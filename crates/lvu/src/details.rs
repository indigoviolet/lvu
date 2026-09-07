//! The Details pane's structured view of one record (docs/dialog-system.md
//! §8.11, §12.19).
//!
//! A JSON record is shown as a tree: each top-level key on its own row, an
//! object or array collapsed to `{3 keys}` / `[12]` and opened in place
//! when its path is in the view's expansion memory. Scalars are the record's
//! own bytes, styled by the JSON kind the log line uses. A record that is
//! not one JSON value keeps the flat `key: value` rows.
//!
//! **The pane colours a record exactly as the log pane colours it** (§12.20).
//! Three things carry that: [`json_kind_style`] is the one function that turns
//! a JSON token into a colour, and `ui::styled_event_line_with_tokens` calls it
//! too, so a number cannot look like one thing in the line and another in the
//! tree; a key's label takes the identity colour that key carries inside the
//! raw line, because it names the same column; and `base` is the record's own
//! row style from `ui::record_style` — its severity, or the hashed colour of
//! the field the view is coloured by — which everything that is not a JSON
//! token inherits.

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
#[allow(clippy::too_many_arguments)]
pub fn details_view(
    row: &DisplayRow,
    expanded: &BTreeSet<String>,
    cursor: usize,
    focused: bool,
    base: Style,
    theme: Theme,
    ascii: bool,
) -> DetailsView {
    let styles = DialogStyles::new(theme);
    let mut lines = Vec::new();
    // `stable display id` is the pane's own caption, not a column of the
    // record, so it stays chrome.
    lines.push(Line::from(vec![
        Span::styled("stable display id: ", styles.label),
        Span::styled(row.id.to_string(), base),
    ]));
    // `raw` is a real column — the one the editor completion inserts as
    // `pl.col('raw')` — so its name takes the identity colour its key carries,
    // and its value goes through the log pane's own text styler. The line the
    // pane shows and the line the log shows are then the same line.
    lines.push(Line::from(
        std::iter::once(Span::styled("raw: ", column_label_style("raw", theme)))
            .chain(crate::ui::styled_record_text(&row.text, base, theme).spans)
            .collect::<Vec<_>>(),
    ));

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
                    &tree, &row.text, tree_row, at_cursor, base, theme, ascii,
                ));
            }
        }
        None => {
            // Not one JSON value, so there is no tree; the columns are still
            // columns and take the colours they take everywhere else.
            for (key, value) in &row.fields {
                lines.push(detail_row(key, value, base, base, theme));
            }
        }
    }
    for (key, value) in &row.details {
        // The one thing Details says that the log does not: whether a command
        // run has finished. That is run state, not a colour of the record, so
        // it keeps its own treatment.
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
            base
        };
        lines.push(detail_row(key, value, base, value_style, theme));
    }
    DetailsView {
        lines,
        rows,
        cursor_line,
    }
}

/// The colour a column's name carries. A JSON key in the log's raw line is
/// painted `Theme::value_color(key)`, so `ms:` in this pane and `"ms"` in that
/// line are one colour: they name the same column.
pub fn column_label_style(name: &str, theme: Theme) -> Style {
    json_kind_style(&JsonKind::Key(name.to_owned()), theme)
}

/// One flat `name: value` row, for a record with no tree and for the
/// presentation details below one. The value goes through the log's text
/// styler, so a bare `503` is a number and `null` is null exactly as they are
/// inside a raw line.
fn detail_row(
    name: &str,
    value: &str,
    base: Style,
    value_style: Style,
    theme: Theme,
) -> Line<'static> {
    Line::from(
        std::iter::once(Span::styled(
            format!("{name}: "),
            column_label_style(name, theme),
        ))
        .chain(crate::ui::styled_record_text(value, value_style, theme).spans)
        .collect::<Vec<_>>(),
    )
    .style(base)
}

/// One tree row: cursor marker, indent, disclosure, key, then the bytes or
/// the summary.
#[allow(clippy::too_many_arguments)]
pub fn tree_line(
    tree: &JsonTree,
    text: &str,
    row: &TreeRow,
    at_cursor: bool,
    record: Style,
    theme: Theme,
    ascii: bool,
) -> Line<'static> {
    let styles = DialogStyles::new(theme);
    // The cursor wins, exactly as the log's selection does; otherwise the row
    // is the record's own colour.
    let base = if at_cursor { styles.selection } else { record };
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
                if at_cursor {
                    base.add_modifier(Modifier::BOLD)
                } else {
                    column_label_style(&row.label, theme).add_modifier(Modifier::BOLD)
                },
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
            spans.push(Span::styled(
                format!("  {}: ", row.label),
                if at_cursor {
                    base
                } else {
                    column_label_style(&row.label, theme)
                },
            ));
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
