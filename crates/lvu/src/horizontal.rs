//! Bounded terminal-column slicing for the horizontally scrollable event cell.

use unicode_width::UnicodeWidthChar;

pub fn scroll_columns(text: &str, offset: usize, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let mut result = String::new();
    let mut position = 0usize;
    let mut written = 0usize;
    let mut visible_base = false;
    for character in text.chars() {
        if character.is_control() {
            continue;
        }
        let columns = character.width().unwrap_or(0);
        if columns == 0 {
            if visible_base {
                result.push(character);
            }
            continue;
        }
        let start = position;
        position = position.saturating_add(columns);
        if position <= offset {
            visible_base = false;
            continue;
        }
        if start < offset {
            let remaining = position - offset;
            if written.saturating_add(remaining) > width {
                break;
            }
            result.extend(std::iter::repeat_n(' ', remaining));
            written += remaining;
            visible_base = false;
        } else {
            if written.saturating_add(columns) > width {
                break;
            }
            result.push(character);
            written += columns;
            visible_base = true;
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::scroll_columns;

    #[test]
    fn slices_terminal_columns_without_partial_wide_or_orphan_combining_cells() {
        let text = "a界e\u{301}xyz";
        assert_eq!(scroll_columns(text, 0, 4), "a界e\u{301}");
        assert_eq!(scroll_columns(text, 1, 3), "界e\u{301}");
        assert_eq!(scroll_columns(text, 2, 3), " e\u{301}x");
        assert_eq!(scroll_columns(text, 4, 2), "xy");
        assert_eq!(scroll_columns(text, 100, 5), "");
        assert_eq!(scroll_columns(text, 0, 0), "");
        assert_eq!(scroll_columns("\u{301}界x", 1, 2), " x");
    }
}
