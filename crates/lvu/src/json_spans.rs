//! Bounded, lossless JSON lexical classification for visible log messages.

use std::ops::Range;

pub const MAX_JSON_CHARS: usize = 16 * 1024;
pub const MAX_JSON_TOKENS: usize = 2 * 1024;
pub const MAX_JSON_DEPTH: usize = 64;
const MAX_DECODED_KEY_CHARS: usize = 512;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum JsonKind {
    Key(String),
    String,
    Number,
    Boolean,
    Null,
    Punctuation,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JsonSpan {
    pub bytes: Range<usize>,
    pub kind: JsonKind,
}

/// Returns spans only when the complete input is valid JSON and all limits hold.
/// The ranges always refer to the original text; callers never reserialize it.
pub fn classify(text: &str) -> Option<Vec<JsonSpan>> {
    if text.chars().take(MAX_JSON_CHARS + 1).count() > MAX_JSON_CHARS {
        return None;
    }
    let mut parser = Parser {
        text,
        at: 0,
        spans: Vec::new(),
        depth: 0,
    };
    parser.ws();
    parser.value()?;
    parser.ws();
    (parser.at == text.len()).then_some(parser.spans)
}

struct Parser<'a> {
    text: &'a str,
    at: usize,
    spans: Vec<JsonSpan>,
    depth: usize,
}

impl Parser<'_> {
    fn value(&mut self) -> Option<()> {
        self.ws();
        match self.peek()? {
            b'{' => self.object(),
            b'[' => self.array(),
            b'"' => self.string(false),
            b't' => self.literal("true", JsonKind::Boolean),
            b'f' => self.literal("false", JsonKind::Boolean),
            b'n' => self.literal("null", JsonKind::Null),
            b'-' | b'0'..=b'9' => self.number(),
            _ => None,
        }
    }

    fn object(&mut self) -> Option<()> {
        self.enter()?;
        self.punctuation(b'{')?;
        self.ws();
        if self.peek() == Some(b'}') {
            self.punctuation(b'}')?;
            return self.leave();
        }
        loop {
            self.string(true)?;
            self.ws();
            self.punctuation(b':')?;
            self.value()?;
            self.ws();
            match self.peek()? {
                b',' => {
                    self.punctuation(b',')?;
                    self.ws();
                }
                b'}' => {
                    self.punctuation(b'}')?;
                    return self.leave();
                }
                _ => return None,
            }
        }
    }

    fn array(&mut self) -> Option<()> {
        self.enter()?;
        self.punctuation(b'[')?;
        self.ws();
        if self.peek() == Some(b']') {
            self.punctuation(b']')?;
            return self.leave();
        }
        loop {
            self.value()?;
            self.ws();
            match self.peek()? {
                b',' => {
                    self.punctuation(b',')?;
                    self.ws();
                }
                b']' => {
                    self.punctuation(b']')?;
                    return self.leave();
                }
                _ => return None,
            }
        }
    }

    fn string(&mut self, key: bool) -> Option<()> {
        let start = self.at;
        self.expect(b'"')?;
        let mut decoded = key.then(String::new);
        while let Some(byte) = self.peek() {
            match byte {
                b'"' => {
                    self.at += 1;
                    let kind = if let Some(decoded) = decoded {
                        JsonKind::Key(decoded)
                    } else {
                        JsonKind::String
                    };
                    return self.push(start, kind);
                }
                b'\\' => {
                    self.at += 1;
                    let escape = self.peek()?;
                    self.at += 1;
                    let character = match escape {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => self.unicode_escape()?,
                        _ => return None,
                    };
                    if let Some(value) = decoded.as_mut() {
                        value.push(character);
                        if value.chars().count() > MAX_DECODED_KEY_CHARS {
                            return None;
                        }
                    }
                }
                0x00..=0x1f => return None,
                _ => {
                    let ch = self.text[self.at..].chars().next()?;
                    self.at += ch.len_utf8();
                    if let Some(value) = decoded.as_mut() {
                        value.push(ch);
                        if value.chars().count() > MAX_DECODED_KEY_CHARS {
                            return None;
                        }
                    }
                }
            }
        }
        None
    }

    fn unicode_escape(&mut self) -> Option<char> {
        let first = self.hex4()?;
        if (0xd800..=0xdbff).contains(&first) {
            self.expect(b'\\')?;
            self.expect(b'u')?;
            let second = self.hex4()?;
            if !(0xdc00..=0xdfff).contains(&second) {
                return None;
            }
            char::from_u32(
                0x10000 + ((u32::from(first) - 0xd800) << 10) + u32::from(second) - 0xdc00,
            )
        } else if (0xdc00..=0xdfff).contains(&first) {
            None
        } else {
            char::from_u32(u32::from(first))
        }
    }

    fn hex4(&mut self) -> Option<u16> {
        let end = self.at.checked_add(4)?;
        let digits = self.text.as_bytes().get(self.at..end)?;
        let mut value = 0_u16;
        for digit in digits {
            value = value.checked_mul(16)?.checked_add(u16::from(match digit {
                b'0'..=b'9' => digit - b'0',
                b'a'..=b'f' => digit - b'a' + 10,
                b'A'..=b'F' => digit - b'A' + 10,
                _ => return None,
            }))?;
        }
        self.at = end;
        Some(value)
    }

    fn number(&mut self) -> Option<()> {
        let start = self.at;
        if self.peek() == Some(b'-') {
            self.at += 1;
        }
        match self.peek()? {
            b'0' => self.at += 1,
            b'1'..=b'9' => {
                while matches!(self.peek(), Some(b'0'..=b'9')) {
                    self.at += 1;
                }
            }
            _ => return None,
        }
        if self.peek() == Some(b'.') {
            self.at += 1;
            let before = self.at;
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.at += 1;
            }
            if self.at == before {
                return None;
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.at += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.at += 1;
            }
            let before = self.at;
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.at += 1;
            }
            if self.at == before {
                return None;
            }
        }
        self.push(start, JsonKind::Number)
    }

    fn literal(&mut self, literal: &str, kind: JsonKind) -> Option<()> {
        let start = self.at;
        self.text.get(start..)?.starts_with(literal).then_some(())?;
        self.at += literal.len();
        self.push(start, kind)
    }

    fn punctuation(&mut self, byte: u8) -> Option<()> {
        let start = self.at;
        self.expect(byte)?;
        self.push(start, JsonKind::Punctuation)
    }

    fn push(&mut self, start: usize, kind: JsonKind) -> Option<()> {
        if self.spans.len() >= MAX_JSON_TOKENS {
            return None;
        }
        self.spans.push(JsonSpan {
            bytes: start..self.at,
            kind,
        });
        Some(())
    }

    fn enter(&mut self) -> Option<()> {
        if self.depth >= MAX_JSON_DEPTH {
            return None;
        }
        self.depth += 1;
        Some(())
    }

    fn leave(&mut self) -> Option<()> {
        self.depth = self.depth.checked_sub(1)?;
        Some(())
    }

    fn ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\n' | b'\r' | b'\t')) {
            self.at += 1;
        }
    }

    fn expect(&mut self, byte: u8) -> Option<()> {
        (self.peek()? == byte).then(|| self.at += 1)
    }

    fn peek(&self) -> Option<u8> {
        self.text.as_bytes().get(self.at).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_ranges_and_decodes_equivalent_keys() {
        let text = r#" {"a": 1, "\u0061": [true, null, "x\\ny"]} "#;
        let spans = classify(text).unwrap();
        assert_eq!(
            spans
                .iter()
                .filter_map(|span| match &span.kind {
                    JsonKind::Key(k) => Some(k.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>(),
            ["a", "a"]
        );
        let rebuilt = spans
            .iter()
            .map(|span| &text[span.bytes.clone()])
            .collect::<Vec<_>>();
        assert!(rebuilt.contains(&r#""\u0061""#));
    }

    #[test]
    fn invalid_or_over_limit_json_has_no_partial_spans() {
        assert!(classify(r#"{"ok": true, broken}"#).is_none());
        assert!(
            classify(&format!(
                "{}0{}",
                "[".repeat(MAX_JSON_DEPTH + 1),
                "]".repeat(MAX_JSON_DEPTH + 1)
            ))
            .is_none()
        );
        assert!(classify(&" ".repeat(MAX_JSON_CHARS + 1)).is_none());
        assert!(
            classify(&format!(
                "[{}]",
                std::iter::repeat_n("0", MAX_JSON_TOKENS)
                    .collect::<Vec<_>>()
                    .join(",")
            ))
            .is_none()
        );
    }
}
