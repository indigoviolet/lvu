//! The Details pane's structured view of one record (docs/dialog-system.md
//! §8.11, §12.19).
//!
//! A JSON record is shown as a tree: each top-level key on its own row, an
//! object or array collapsed to `{3 keys}` / `[12]` and opened in place
//! when its path is in the view's expansion memory. Scalars are the record's
//! own bytes, styled by the JSON kind the log line uses. A record that is
//! not one JSON value keeps the flat `key: value` rows.

use std::collections::BTreeSet;

use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};

use crate::dialog_controls::DialogStyles;
use crate::json_spans::JsonKind;
use crate::json_tree::{JsonTree, RowShape, TreeRow};
use crate::provider::DisplayRow;
use crate::theme::Theme;

/// Rows the `stable display id` and `raw` lines occupy above the tree.
pub const HEADER_LINES: usize = 2;

pub struct DetailsView {
    pub lines: Vec<Line<'static>>,
    /// The tree rows in the order drawn, for the cursor to walk.
    pub rows: Vec<TreeRow>,
    /// Line index of the cursor row, when the record is a tree.
    pub cursor_line: Option<usize>,
}

/// The style the log line gives a scalar of `kind`; the tree uses the same
/// vocabulary so the two never disagree about what a number looks like.
pub fn json_kind_style(kind: &JsonKind, theme: Theme) -> Style {
    let colour = match kind {
        JsonKind::Key(identity) => theme.value_color(identity),
        JsonKind::String => theme.json.string,
        JsonKind::Number => theme.json.number,
        JsonKind::Boolean => theme.json.boolean,
        JsonKind::Null => theme.json.null,
        JsonKind::Punctuation => theme.json.punctuation,
    };
    Style::default().fg(colour)
}

/// The disclosure glyph of a container row.
pub fn disclosure(expanded: bool, ascii: bool) -> &'static str {
    match (expanded, ascii) {
        (true, false) => "▾",
        (false, false) => "▸",
        (true, true) => "v",
        (false, true) => ">",
    }
}

/// Builds the pane's lines. `cursor` indexes `rows` and is clamped.
pub fn details_view(
    row: &DisplayRow,
    expanded: &BTreeSet<String>,
    cursor: usize,
    focused: bool,
    theme: Theme,
    ascii: bool,
) -> DetailsView {
    let styles = DialogStyles::new(theme);
    let mut lines = Vec::new();
    lines.push(Line::from(vec![
        Span::styled("stable display id: ", styles.label),
        Span::styled(row.id.to_string(), styles.description),
    ]));
    lines.push(Line::from(vec![
        Span::styled("raw: ", styles.label),
        Span::styled(row.text.clone(), styles.description),
    ]));

    let tree = JsonTree::parse(&row.text).filter(JsonTree::is_object);
    let mut rows = Vec::new();
    let mut cursor_line = None;
    match tree {
        Some(tree) => {
            rows = tree.rows(&|path| expanded.contains(path));
            let cursor = cursor.min(rows.len().saturating_sub(1));
            for (index, tree_row) in rows.iter().enumerate() {
                let at_cursor = focused && index == cursor;
                if index == cursor {
                    cursor_line = Some(lines.len());
                }
                lines.push(tree_line(
                    &tree, &row.text, tree_row, at_cursor, theme, ascii,
                ));
            }
        }
        None => {
            for (key, value) in &row.fields {
                lines.push(Line::from(vec![
                    Span::styled(format!("{key}: "), styles.label),
                    Span::styled(value.clone(), styles.description),
                ]));
            }
        }
    }
    for (key, value) in &row.details {
        let status = key == "command.status";
        let value_style = if status
            && value
                .split_whitespace()
                .next()
                .is_some_and(|word| word.eq_ignore_ascii_case("pending"))
        {
            styles.pending
        } else if status {
            styles.applied
        } else {
            styles.description
        };
        lines.push(Line::from(vec![
            Span::styled(format!("{key}: "), styles.label),
            Span::styled(value.clone(), value_style),
        ]));
    }
    DetailsView {
        lines,
        rows,
        cursor_line,
    }
}

/// One tree row: cursor marker, indent, disclosure, key, then the bytes or
/// the summary.
pub fn tree_line(
    tree: &JsonTree,
    text: &str,
    row: &TreeRow,
    at_cursor: bool,
    theme: Theme,
    ascii: bool,
) -> Line<'static> {
    let styles = DialogStyles::new(theme);
    let base = if at_cursor {
        styles.selection
    } else {
        styles.label
    };
    let marker = if at_cursor {
        if ascii { "> " } else { "› " }
    } else {
        "  "
    };
    let indent = "  ".repeat(row.depth);
    let mut spans = vec![Span::styled(format!("{marker}{indent}"), base)];
    match &row.shape {
        RowShape::Container { expanded, .. } => {
            spans.push(Span::styled(
                format!("{} {}: ", disclosure(*expanded, ascii), row.label),
                base.add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::styled(
                tree.summary(row.node).unwrap_or_default(),
                if at_cursor {
                    base
                } else {
                    styles.description.add_modifier(Modifier::ITALIC)
                },
            ));
        }
        RowShape::Scalar(kind) => {
            spans.push(Span::styled(format!("  {}: ", row.label), base));
            let value = tree.scalar_text(text, row.node).unwrap_or_default();
            spans.push(Span::styled(
                value.to_owned(),
                if at_cursor {
                    base
                } else {
                    json_kind_style(kind, theme)
                },
            ));
        }
    }
    Line::from(spans)
}
