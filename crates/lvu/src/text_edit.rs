//! Bounded, Unicode-aware editing primitives shared by terminal text inputs.

use unicode_width::UnicodeWidthChar;

pub const MAX_CURSOR_SLOTS: usize = 48;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct TextTarget {
    pub identity: String,
    pub field: &'static str,
}

#[derive(Clone, Debug, Default)]
pub struct CursorBank {
    slots: Vec<(TextTarget, TextCursor)>,
}

impl CursorBank {
    pub fn get_or_end(&mut self, target: TextTarget, value: &str) -> TextCursor {
        if let Some(index) = self.slots.iter().position(|(saved, _)| saved == &target) {
            let entry = self.slots.remove(index);
            let mut cursor = entry.1;
            clamp_cursor(value, &mut cursor);
            self.slots.push((entry.0, cursor));
            return cursor;
        }
        let mut cursor = TextCursor::default();
        reset_cursor_to_end(value, &mut cursor);
        if self.slots.len() == MAX_CURSOR_SLOTS {
            self.slots.remove(0);
        }
        self.slots.push((target, cursor));
        cursor
    }

    pub fn store(&mut self, target: TextTarget, cursor: TextCursor) {
        if let Some((_, saved)) = self.slots.iter_mut().find(|(saved, _)| saved == &target) {
            *saved = cursor;
        } else {
            if self.slots.len() == MAX_CURSOR_SLOTS {
                self.slots.remove(0);
            }
            self.slots.push((target, cursor));
        }
    }

    pub fn reset(&mut self, target: TextTarget, value: &str) {
        let mut cursor = TextCursor::default();
        reset_cursor_to_end(value, &mut cursor);
        self.store(target, cursor);
    }

    pub fn prune_identity(&mut self, identity: &str) {
        self.slots.retain(|(target, _)| target.identity != identity);
    }

    pub fn prune_where_identity_contains(&mut self, marker: &str) {
        self.slots
            .retain(|(target, _)| !target.identity.contains(marker));
    }

    pub fn clear(&mut self) {
        self.slots.clear();
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TextCursor {
    pub char_index: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EditCommand<'a> {
    Insert(&'a str),
    Backspace,
    StartOfLine,
    EndOfLine,
    KillToEndOfLine,
    MoveLeft,
    MoveRight,
    MoveUp,
    MoveDown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EditPolicy {
    pub max_bytes: usize,
    pub multiline: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct EditOutcome {
    pub changed: bool,
    pub moved: bool,
    pub rejected: bool,
}

pub fn clamp_cursor(value: &str, cursor: &mut TextCursor) {
    cursor.char_index = cursor.char_index.min(value.chars().count());
}

pub fn reset_cursor_to_end(value: &str, cursor: &mut TextCursor) {
    cursor.char_index = value.chars().count();
}

fn byte_index(value: &str, char_index: usize) -> usize {
    value
        .char_indices()
        .nth(char_index)
        .map_or(value.len(), |(index, _)| index)
}

fn line_char_bounds(value: &str, at: usize) -> (usize, usize) {
    let chars = value.chars().collect::<Vec<_>>();
    let at = at.min(chars.len());
    let start = chars[..at]
        .iter()
        .rposition(|ch| *ch == '\n')
        .map_or(0, |index| index + 1);
    let end = chars[at..]
        .iter()
        .position(|ch| *ch == '\n')
        .map_or(chars.len(), |index| at + index);
    (start, end)
}

pub fn edit(
    value: &mut String,
    cursor: &mut TextCursor,
    command: EditCommand<'_>,
    policy: EditPolicy,
) -> EditOutcome {
    clamp_cursor(value, cursor);
    let before = cursor.char_index;
    match command {
        EditCommand::StartOfLine => cursor.char_index = line_char_bounds(value, before).0,
        EditCommand::EndOfLine => cursor.char_index = line_char_bounds(value, before).1,
        EditCommand::MoveLeft => cursor.char_index = before.saturating_sub(1),
        EditCommand::MoveRight => cursor.char_index = (before + 1).min(value.chars().count()),
        EditCommand::MoveUp => {
            let (start, _) = line_char_bounds(value, before);
            if start > 0 {
                let column = before - start;
                let (previous_start, previous_end) = line_char_bounds(value, start - 1);
                cursor.char_index = previous_start + column.min(previous_end - previous_start);
            }
        }
        EditCommand::MoveDown => {
            let chars = value.chars().count();
            let (start, end) = line_char_bounds(value, before);
            if end < chars {
                let column = before - start;
                let next_start = end + 1;
                let (_, next_end) = line_char_bounds(value, next_start);
                cursor.char_index = next_start + column.min(next_end - next_start);
            }
        }
        EditCommand::Backspace if before > 0 => {
            let start = byte_index(value, before - 1);
            let end = byte_index(value, before);
            value.replace_range(start..end, "");
            cursor.char_index -= 1;
            return EditOutcome {
                changed: true,
                ..EditOutcome::default()
            };
        }
        EditCommand::KillToEndOfLine => {
            let end_char = line_char_bounds(value, before).1;
            if end_char > before {
                let start = byte_index(value, before);
                let end = byte_index(value, end_char);
                value.replace_range(start..end, "");
                return EditOutcome {
                    changed: true,
                    ..EditOutcome::default()
                };
            }
        }
        EditCommand::Insert(text) => {
            let invalid_control = text
                .chars()
                .any(|ch| ch.is_control() && !(policy.multiline && ch == '\n'));
            let invalid_line = !policy.multiline && text.contains('\n');
            if invalid_control
                || invalid_line
                || value.len().saturating_add(text.len()) > policy.max_bytes
            {
                return EditOutcome {
                    rejected: true,
                    ..EditOutcome::default()
                };
            }
            if !text.is_empty() {
                let at = byte_index(value, before);
                value.insert_str(at, text);
                cursor.char_index += text.chars().count();
                return EditOutcome {
                    changed: true,
                    ..EditOutcome::default()
                };
            }
        }
        EditCommand::Backspace => {}
    }
    EditOutcome {
        moved: cursor.char_index != before,
        ..EditOutcome::default()
    }
}

pub fn cursor_line_prefix<'a>(value: &'a str, cursor: &mut TextCursor) -> &'a str {
    clamp_cursor(value, cursor);
    let at = byte_index(value, cursor.char_index);
    let start = value[..at].rfind('\n').map_or(0, |index| index + 1);
    &value[start..at]
}

/// Returns the display row and cell column for a logical scalar cursor.
pub fn wrapped_cursor(value: &str, cursor: &mut TextCursor, width: usize) -> (usize, usize) {
    clamp_cursor(value, cursor);
    let layout = wrapped_text(value, width);
    layout.cursor_positions[cursor.char_index]
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WrappedText {
    pub lines: Vec<String>,
    cursor_positions: Vec<(usize, usize)>,
}

pub fn wrapped_text(value: &str, width: usize) -> WrappedText {
    let width = width.max(1);
    let mut row = 0;
    let mut column = 0;
    let mut pending_wrap = false;
    let mut lines = vec![String::new()];
    let mut cursor_positions = vec![(0, 0)];
    for ch in value.chars() {
        if ch == '\n' {
            row += 1;
            column = 0;
            pending_wrap = false;
            lines.push(String::new());
            cursor_positions.push((row, column));
            continue;
        }
        let cells = ch.width().unwrap_or(0);
        if pending_wrap && cells > 0 {
            row += 1;
            column = 0;
            lines.push(String::new());
        }
        if column > 0 && column + cells > width {
            row += 1;
            column = 0;
            lines.push(String::new());
        }
        if !ch.is_control() {
            lines.last_mut().expect("wrapped line").push(ch);
        }
        column += cells;
        pending_wrap = column >= width;
        cursor_positions.push(if pending_wrap {
            (row + 1, 0)
        } else {
            (row, column)
        });
    }
    if pending_wrap {
        lines.push(String::new());
    }
    WrappedText {
        lines,
        cursor_positions,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MULTI: EditPolicy = EditPolicy {
        max_bytes: 64,
        multiline: true,
    };

    #[test]
    fn line_commands_are_logical_and_kill_preserves_newline() {
        let mut value = "one\nαβγ\nthree".to_owned();
        let mut cursor = TextCursor { char_index: 7 };
        assert!(edit(&mut value, &mut cursor, EditCommand::StartOfLine, MULTI).moved);
        assert_eq!(cursor.char_index, 4);
        assert!(edit(&mut value, &mut cursor, EditCommand::KillToEndOfLine, MULTI).changed);
        assert_eq!(value, "one\n\nthree");
        assert!(!edit(&mut value, &mut cursor, EditCommand::KillToEndOfLine, MULTI).changed);
    }

    #[test]
    fn insert_and_backspace_use_scalar_boundaries() {
        let mut value = "界e\u{301}".to_owned();
        let mut cursor = TextCursor { char_index: 1 };
        edit(&mut value, &mut cursor, EditCommand::Insert("λ"), MULTI);
        assert_eq!(value, "界λe\u{301}");
        edit(&mut value, &mut cursor, EditCommand::Backspace, MULTI);
        assert_eq!(value, "界e\u{301}");
        assert_eq!(wrapped_cursor(&value, &mut cursor, 8), (0, 2));
    }

    #[test]
    fn insertion_rejects_the_whole_payload_but_deletion_ignores_cap() {
        let policy = EditPolicy {
            max_bytes: 2,
            multiline: false,
        };
        let mut value = "abcd".to_owned();
        let mut cursor = TextCursor { char_index: 2 };
        assert!(edit(&mut value, &mut cursor, EditCommand::Insert("é"), policy).rejected);
        assert_eq!(value, "abcd");
        assert!(edit(&mut value, &mut cursor, EditCommand::Backspace, policy).changed);
        assert_eq!(value, "acd");
    }

    #[test]
    fn wrapping_shares_exact_boundary_newline_wide_and_combining_rules() {
        let layout = wrapped_text("abcd\nq", 4);
        assert_eq!(layout.lines, ["abcd", "q"]);
        let mut cursor = TextCursor { char_index: 5 };
        assert_eq!(wrapped_cursor("abcd\nq", &mut cursor, 4), (1, 0));

        let value = "e\u{301}\nx";
        let layout = wrapped_text(value, 1);
        assert_eq!(layout.lines, ["e\u{301}", "x", ""]);
        let mut after_combining = TextCursor { char_index: 2 };
        assert_eq!(wrapped_cursor(value, &mut after_combining, 1), (1, 0));

        let layout = wrapped_text("界\n", 1);
        assert_eq!(layout.lines, ["界", ""]);
        let mut end = TextCursor { char_index: 2 };
        assert_eq!(wrapped_cursor("界\n", &mut end, 1), (1, 0));
    }

    #[test]
    fn vertical_movement_preserves_scalar_column_and_clamps_short_lines() {
        let mut value = "ab界\nx\ne\u{301}nd".to_owned();
        let mut cursor = TextCursor { char_index: 9 };
        assert!(edit(&mut value, &mut cursor, EditCommand::MoveUp, MULTI).moved);
        assert_eq!(cursor.char_index, 5);
        assert!(edit(&mut value, &mut cursor, EditCommand::MoveUp, MULTI).moved);
        assert_eq!(cursor.char_index, 1);
        assert!(edit(&mut value, &mut cursor, EditCommand::MoveDown, MULTI).moved);
        assert_eq!(cursor.char_index, 5);
    }
}
