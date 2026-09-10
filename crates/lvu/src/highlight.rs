//! Where inside a rendered line a search or a rule's pattern matched.
//!
//! This is *not* a second predicate evaluator. The query engine already decided
//! which rows are in the view and which colour rule paints each one; nothing
//! here can add or remove a row, and a pattern that fails to compile simply
//! highlights nothing. What this does is re-locate the match inside the one
//! line that is about to be drawn, so the eye can find it.
//!
//! **The raw-bytes invariant.** `DisplayRow::text` is already
//! `String::from_utf8_lossy` of the captured bytes: invalid sequences became
//! U+FFFD *before* the terminal ever saw them. Spans are computed against that
//! exact string — the same bytes the renderer is about to clip into columns —
//! so a replacement character can never shift a highlight off the text it
//! belongs to. Anything that searched the original bytes and reported offsets
//! into them would drift, because one invalid byte becomes three.

use std::ops::Range;

use crate::app::ColorRule;

/// Highlighting is a reading aid, not a scan: a line with more matches than
/// this is already unreadable and the rest are dropped.
const MAX_SPANS_PER_LINE: usize = 64;

/// A pattern is compiled once per frame; this caps what one frame will hold.
const MAX_PATTERNS: usize = 17;

/// Compiled patterns worth locating inside a line.
#[derive(Clone, Debug, Default)]
pub struct Highlights {
    patterns: Vec<Pattern>,
}

#[derive(Clone, Debug)]
enum Pattern {
    /// A case-insensitive literal, which is what the search box means by plain
    /// text. Located with the same case folding the engine filtered with.
    Literal(String),
    Regex(regex::Regex),
}

impl Highlights {
    /// The applied search plus every rule predicate, in that order.
    ///
    /// Only the *pattern* forms are located: a `field: value` search and a
    /// `pl.…` expression describe a column, not a run of characters in the
    /// rendered line, and underlining an arbitrary substring of the line
    /// because a column matched would point at the wrong thing.
    pub fn compile(search: &str, rules: &[ColorRule]) -> Self {
        let mut patterns = Vec::new();
        for source in
            std::iter::once(search).chain(rules.iter().map(|rule| rule.predicate.as_str()))
        {
            if patterns.len() >= MAX_PATTERNS {
                break;
            }
            if let Some(pattern) = Pattern::compile(source.trim()) {
                patterns.push(pattern);
            }
        }
        Self { patterns }
    }

    pub fn is_empty(&self) -> bool {
        self.patterns.is_empty()
    }

    /// Byte ranges inside `line` to emphasise, ordered and non-overlapping.
    ///
    /// Ranges always fall on character boundaries of `line`, because that is
    /// what both `str::find` and `regex` guarantee over a `&str`; the renderer
    /// can slice with them directly.
    pub fn spans(&self, line: &str) -> Vec<Range<usize>> {
        if self.patterns.is_empty() || line.is_empty() {
            return Vec::new();
        }
        let mut spans: Vec<Range<usize>> = Vec::new();
        for pattern in &self.patterns {
            pattern.locate(line, &mut spans);
            if spans.len() >= MAX_SPANS_PER_LINE {
                break;
            }
        }
        merge(spans)
    }
}

impl Pattern {
    fn compile(source: &str) -> Option<Self> {
        if source.is_empty() {
            return None;
        }
        // `\/literal` is the search box's escape for a leading slash; it is a
        // literal, not a pattern.
        if let Some(literal) = source.strip_prefix(r"\/") {
            return Some(Self::Literal(format!("/{literal}").to_lowercase()));
        }
        if source.starts_with("pl.") || source.starts_with("(pl.") {
            return None;
        }
        // `field: value` names a column. The column's value is not necessarily
        // a substring of the rendered line, so there is nothing to underline.
        if let Some((field, _)) = source.split_once(": ")
            && !field.is_empty()
            && field.len() <= 64
            && field
                .chars()
                .all(|c| c.is_alphanumeric() || "_.-".contains(c))
        {
            return None;
        }
        if let Some(body) = source.strip_prefix('/') {
            let (pattern, flags) = body.rsplit_once('/').unwrap_or((body, ""));
            if pattern.is_empty() {
                return None;
            }
            let source = if flags.is_empty() {
                pattern.to_owned()
            } else {
                format!("(?{flags}){pattern}")
            };
            return regex::RegexBuilder::new(&source)
                .size_limit(1024 * 1024)
                .nest_limit(64)
                .build()
                .ok()
                .map(Self::Regex);
        }
        Some(Self::Literal(source.to_lowercase()))
    }

    fn locate(&self, line: &str, spans: &mut Vec<Range<usize>>) {
        match self {
            Self::Literal(needle) => {
                // The engine lowercases both sides for a literal search, and so
                // does this, so what is underlined is what matched. Lowercasing
                // can change byte length, so the haystack is searched with the
                // needle's own folded form over a folded copy and the offsets
                // are mapped back by walking both at once.
                let mut at = 0usize;
                for (start, character) in line.char_indices() {
                    if spans.len() >= MAX_SPANS_PER_LINE {
                        return;
                    }
                    if at > start {
                        continue;
                    }
                    let _ = character;
                    let tail = &line[start..];
                    if fold_starts_with(tail, needle) {
                        let end = fold_prefix_len(tail, needle);
                        if end > 0 {
                            spans.push(start..start + end);
                            at = start + end;
                        }
                    }
                }
            }
            Self::Regex(regex) => {
                for found in regex.find_iter(line) {
                    if spans.len() >= MAX_SPANS_PER_LINE {
                        return;
                    }
                    if found.start() < found.end() {
                        spans.push(found.range());
                    }
                }
            }
        }
    }
}

/// Whether `haystack` begins with `needle` under the same lowercase folding the
/// engine's literal search uses.
fn fold_starts_with(haystack: &str, needle: &str) -> bool {
    fold_prefix_len(haystack, needle) > 0
}

/// The byte length of the prefix of `haystack` whose lowercase form equals
/// `needle`, or zero. Walking characters rather than slicing keeps this correct
/// where lowercasing changes length, and every returned length is a character
/// boundary of `haystack`.
fn fold_prefix_len(haystack: &str, needle: &str) -> usize {
    if needle.is_empty() {
        return 0;
    }
    let mut wanted = needle.chars();
    let mut consumed = 0usize;
    let mut pending: Option<char> = None;
    for (index, character) in haystack.char_indices() {
        let mut folded = character.to_lowercase();
        while let Some(next) = pending.take().or_else(|| folded.next()) {
            match wanted.next() {
                Some(expected) if expected == next => {}
                _ => return 0,
            }
        }
        consumed = index + character.len_utf8();
        if wanted.clone().next().is_none() {
            return consumed;
        }
    }
    let _ = consumed;
    0
}

/// Orders spans and folds overlaps, so the renderer can walk them once.
fn merge(mut spans: Vec<Range<usize>>) -> Vec<Range<usize>> {
    if spans.len() < 2 {
        return spans;
    }
    spans.sort_by_key(|span| (span.start, span.end));
    let mut merged: Vec<Range<usize>> = Vec::with_capacity(spans.len());
    for span in spans {
        match merged.last_mut() {
            Some(last) if span.start <= last.end => last.end = last.end.max(span.end),
            _ => merged.push(span),
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::RuleColor;

    fn rule(predicate: &str) -> ColorRule {
        ColorRule {
            predicate: predicate.into(),
            color: RuleColor::Red,
            column: None,
            value: None,
        }
    }

    #[test]
    fn a_literal_search_is_located_case_insensitively_and_without_overlap() {
        let highlights = Highlights::compile("error", &[]);
        let line = "ERROR: error and Error";
        let spans = highlights.spans(line);
        assert_eq!(spans.len(), 3);
        for span in &spans {
            assert_eq!(line[span.clone()].to_lowercase(), "error");
        }
    }

    #[test]
    fn a_regex_rule_is_located_and_an_invalid_one_highlights_nothing() {
        let highlights = Highlights::compile("", &[rule(r"/\d+/")]);
        let line = "status 503 in 12ms";
        let spans = highlights.spans(line);
        assert_eq!(
            spans.iter().map(|s| &line[s.clone()]).collect::<Vec<_>>(),
            ["503", "12"]
        );
        assert!(Highlights::compile("", &[rule("/(/")]).is_empty());
    }

    #[test]
    fn column_and_expression_predicates_underline_nothing() {
        // Neither describes a run of characters in the rendered line.
        assert!(Highlights::compile("level: ERROR", &[]).is_empty());
        assert!(Highlights::compile("pl.col('x') == 'y'", &[]).is_empty());
        // The escaped-slash form is a literal, and is located.
        let highlights = Highlights::compile(r"\/usr", &[]);
        assert_eq!(highlights.spans("in /usr/bin"), vec![3..7]);
    }

    #[test]
    fn replacement_characters_do_not_shift_a_span() {
        // What the terminal draws is `from_utf8_lossy` of the captured bytes,
        // so spans are computed on that same string: the three bytes of U+FFFD
        // are counted exactly as they are rendered.
        let lossy = String::from_utf8_lossy(b"a\xffZneedle").into_owned();
        assert!(lossy.contains('\u{fffd}'));
        let spans = Highlights::compile("needle", &[]).spans(&lossy);
        assert_eq!(spans.len(), 1);
        assert_eq!(&lossy[spans[0].clone()], "needle");
        // And the span still falls on character boundaries of the rendered
        // string, which is what lets the renderer slice with it.
        assert!(lossy.is_char_boundary(spans[0].start));
        assert!(lossy.is_char_boundary(spans[0].end));
    }

    #[test]
    fn overlapping_patterns_merge_into_one_run() {
        let highlights = Highlights::compile("abc", &[rule("bcd")]);
        assert_eq!(highlights.spans("xabcdy"), vec![1..5]);
    }

    #[test]
    fn a_line_cannot_be_flooded_with_spans() {
        // Separated matches stay separate spans, so this is the cap and not
        // the merge; a run of adjacent matches folds into one span instead.
        let highlights = Highlights::compile("a", &[]);
        assert_eq!(
            highlights.spans(&"a.".repeat(500)).len(),
            MAX_SPANS_PER_LINE
        );
        // Adjacent matches merge, and the cap applies to what was collected
        // before merging, so a pathological line yields one bounded run rather
        // than 500 spans.
        assert_eq!(
            highlights.spans(&"a".repeat(500)),
            vec![0..MAX_SPANS_PER_LINE]
        );
    }
}
