//! ANSI terminal-control removal for record presentation.
//!
//! Captures and query columns keep the original bytes. This module is used only
//! at the terminal presentation boundary, where passing captured control
//! sequences through would let log data control the viewer. In particular, an
//! ESC byte must be consumed with its CSI/OSC payload rather than dropped on its
//! own, which would leave visible tails such as `[2m`.

use std::borrow::Cow;

/// Removes ANSI control sequences introduced by an actual ESC or C1 control.
///
/// CSI and OSC sequences are consumed as units. A truncated sequence consumes
/// the bounded remainder of the display projection, so its parameter bytes do
/// not become misleading text. Ordinary text such as a literal `[2m` has no
/// introducer and is returned unchanged without allocating.
pub(crate) fn without_ansi(text: &str) -> Cow<'_, str> {
    let Some(first) = text
        .char_indices()
        .find(|(_, ch)| is_ansi_introducer(*ch))
        .map(|(index, _)| index)
    else {
        return Cow::Borrowed(text);
    };

    let mut output = String::with_capacity(text.len());
    output.push_str(&text[..first]);
    let mut at = first;
    while at < text.len() {
        let ch = text[at..].chars().next().expect("character boundary");
        if is_ansi_introducer(ch) {
            at += ansi_sequence_len(&text[at..], ch);
        } else {
            output.push(ch);
            at += ch.len_utf8();
        }
    }
    Cow::Owned(output)
}

fn is_ansi_introducer(ch: char) -> bool {
    matches!(
        ch,
        '\u{1b}' | '\u{90}' | '\u{98}' | '\u{9b}' | '\u{9d}' | '\u{9e}' | '\u{9f}'
    )
}

fn ansi_sequence_len(tail: &str, introducer: char) -> usize {
    let introducer_len = introducer.len_utf8();
    let body = &tail[introducer_len..];
    match introducer {
        '\u{9b}' => introducer_len + csi_body_len(body),
        '\u{9d}' => introducer_len + osc_body_len(body),
        '\u{90}' | '\u{98}' | '\u{9e}' | '\u{9f}' => introducer_len + string_control_body_len(body),
        '\u{1b}' if body.starts_with('[') => introducer_len + 1 + csi_body_len(&body[1..]),
        '\u{1b}' if body.starts_with(']') => introducer_len + 1 + osc_body_len(&body[1..]),
        '\u{1b}'
            if body
                .as_bytes()
                .first()
                .is_some_and(|byte| matches!(byte, b'P' | b'X' | b'^' | b'_')) =>
        {
            introducer_len + 1 + string_control_body_len(&body[1..])
        }
        '\u{1b}' => introducer_len + escape_body_len(body),
        _ => unreachable!(),
    }
}

/// Bytes after ESC in an ECMA-48 escape sequence: zero or more intermediate
/// bytes (`0x20..=0x2f`) followed by a final (`0x30..=0x7e`). This consumes
/// charset designators such as `ESC ( B` as one control rather than exposing
/// `(B`, while malformed Unicode/newline input remains ordinary text.
fn escape_body_len(body: &str) -> usize {
    let mut consumed = 0;
    for ch in body.chars() {
        if !ch.is_ascii() {
            break;
        }
        if (' '..='/').contains(&ch) {
            consumed += ch.len_utf8();
            continue;
        }
        if ('0'..='~').contains(&ch) {
            return consumed + ch.len_utf8();
        }
        break;
    }
    consumed
}

fn csi_body_len(body: &str) -> usize {
    for (index, ch) in body.char_indices() {
        if ch.is_ascii() && ('@'..='~').contains(&ch) {
            return index + ch.len_utf8();
        }
        // UTF-8 and bytes outside the parameter/intermediate ranges cannot
        // terminate CSI. Continue to the bounded end rather than exposing a
        // partially parsed control sequence.
    }
    body.len()
}

fn osc_body_len(body: &str) -> usize {
    let bytes = body.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == 0x07 {
            return index + 1;
        }
        if bytes[index] == 0x1b && bytes.get(index + 1) == Some(&b'\\') {
            return index + 2;
        }
        if body[index..].starts_with('\u{9c}') {
            return index + '\u{9c}'.len_utf8();
        }
        index += body[index..]
            .chars()
            .next()
            .expect("character boundary")
            .len_utf8();
    }
    body.len()
}

fn string_control_body_len(body: &str) -> usize {
    let bytes = body.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == 0x1b && bytes.get(index + 1) == Some(&b'\\') {
            return index + 2;
        }
        if body[index..].starts_with('\u{9c}') {
            return index + '\u{9c}'.len_utf8();
        }
        index += body[index..]
            .chars()
            .next()
            .expect("character boundary")
            .len_utf8();
    }
    body.len()
}

#[cfg(test)]
mod tests {
    use super::without_ansi;
    use std::borrow::Cow;

    #[test]
    fn strips_sgr_csi_and_osc_but_not_literal_parameter_text() {
        let text = "[2m literal \u{1b}[2m2026\u{1b}[0m \u{1b}[32;1minfo\u{1b}[0m";
        assert_eq!(without_ansi(text), "[2m literal 2026 info");
        assert!(matches!(without_ansi("plain [32m text"), Cow::Borrowed(_)));

        assert_eq!(
            without_ansi("a\u{1b}[2Kb\u{1b}]8;;https://example.test\u{1b}\\link\u{1b}]8;;\u{7}c"),
            "ablinkc"
        );
        assert_eq!(without_ansi("x\u{9b}31mred\u{9b}0my"), "xredy");
        assert_eq!(without_ansi("a\u{1b}(Bb"), "ab");
        assert_eq!(
            without_ansi("a\u{1b}Pprivate payload\u{1b}\\b\u{9e}more\u{9c}c"),
            "abc"
        );
    }

    #[test]
    fn truncated_sequences_are_bounded_and_unicode_before_them_is_preserved() {
        assert_eq!(without_ansi("東京 e\u{301}\u{1b}[38;2;1"), "東京 e\u{301}");
        assert_eq!(
            without_ansi("before\u{1b}]title without terminator"),
            "before"
        );
        assert_eq!(without_ansi("left\u{1b}"), "left");
        assert_eq!(without_ansi("line\u{1b}\nnext"), "line\nnext");
        assert_eq!(without_ansi("before\u{1b}[31東京"), "before");
    }
}
