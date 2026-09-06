//! Selection of the composited visible screen, never hidden dialog contents.
use ratatui::{buffer::Buffer, layout::Position, style::Style};
use unicode_width::UnicodeWidthStr;

const MAX_CELLS: usize = 128 * 1024;
const MAX_COPY_BYTES: usize = 64 * 1024;

#[derive(Default)]
pub(crate) struct TextSelection {
    screen: Option<Buffer>,
    anchor: Position,
    end: Position,
    pub dragging: bool,
    pub selected: bool,
}

impl TextSelection {
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    pub fn begin(&mut self, screen: &Buffer, position: Position) {
        self.clear();
        if screen.content.len() <= MAX_CELLS && screen.area.contains(position) {
            self.screen = Some(screen.clone());
            self.anchor = position;
            self.end = position;
            self.dragging = true;
        }
    }

    pub fn extend(&mut self, position: Position) {
        let Some(screen) = &self.screen else {
            return;
        };
        if !self.dragging {
            return;
        }
        self.end = Position::new(
            position
                .x
                .clamp(screen.area.x, screen.area.right().saturating_sub(1)),
            position
                .y
                .clamp(screen.area.y, screen.area.bottom().saturating_sub(1)),
        );
        self.selected |= self.end != self.anchor;
    }

    pub fn finish(&mut self) {
        self.dragging = false;
        if !self.selected {
            self.clear();
        }
    }

    fn endpoints(&self) -> (Position, Position) {
        if (self.anchor.y, self.anchor.x) <= (self.end.y, self.end.x) {
            (self.anchor, self.end)
        } else {
            (self.end, self.anchor)
        }
    }

    pub fn paint(&self, buffer: &mut Buffer, style: Style) {
        let Some(screen) = &self.screen else {
            return;
        };
        if !self.selected || screen.area != buffer.area {
            return;
        }
        // Freeze the selected visible frame while acquisition and queries continue.
        *buffer = screen.clone();
        let (start, end) = self.endpoints();
        for y in start.y..=end.y {
            let left = if y == start.y { start.x } else { screen.area.x };
            let right = if y == end.y {
                end.x
            } else {
                screen.area.right() - 1
            };
            for x in left..=right {
                buffer[(x, y)].set_style(style);
            }
        }
    }

    pub fn text(&self) -> Result<String, &'static str> {
        let Some(screen) = &self.screen else {
            return Ok(String::new());
        };
        let (start, end) = self.endpoints();
        let mut text = String::new();
        for y in start.y..=end.y {
            let left = if y == start.y { start.x } else { screen.area.x };
            let right = if y == end.y {
                end.x
            } else {
                screen.area.right() - 1
            };
            let mut line = String::new();
            let mut x = screen.area.x;
            while x < screen.area.right() {
                let symbol = screen[(x, y)].symbol();
                let width = UnicodeWidthStr::width(symbol).max(1) as u16;
                if x <= right && x.saturating_add(width) > left {
                    line.push_str(symbol);
                }
                x = x.saturating_add(width);
            }
            if y != start.y {
                text.push('\n');
            }
            text.push_str(line.trim_end());
            if text.len() > MAX_COPY_BYTES {
                return Err("Selection exceeds 64 KiB; select a smaller area");
            }
        }
        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::layout::Rect;
    #[test]
    fn copies_visible_overlay_unicode_and_clears_before_background_selection() {
        let mut background = Buffer::empty(Rect::new(0, 0, 18, 3));
        background.set_string(0, 0, "background record", Style::default());
        let mut dialog = background.clone();
        dialog.set_string(0, 0, "dialog 界e\u{301}      ", Style::default());
        let mut selection = TextSelection::default();
        selection.begin(&dialog, Position::new(9, 0));
        selection.extend(Position::new(0, 0));
        selection.finish();
        assert_eq!(selection.text().unwrap(), "dialog 界e\u{301}");
        selection.clear();
        selection.begin(&background, Position::new(0, 0));
        selection.extend(Position::new(9, 0));
        selection.finish();
        assert_eq!(selection.text().unwrap(), "background");
    }
    #[test]
    fn selection_retains_original_frame_and_trims_each_visual_line() {
        let mut screen = Buffer::empty(Rect::new(0, 0, 8, 2));
        screen.set_string(0, 0, "first", Style::default());
        screen.set_string(0, 1, "second", Style::default());
        let mut selection = TextSelection::default();
        selection.begin(&screen, Position::new(0, 0));
        selection.extend(Position::new(5, 1));
        selection.finish();
        screen.set_string(0, 0, "changed", Style::default());
        selection.paint(&mut screen, Style::default());
        assert_eq!(selection.text().unwrap(), "first\nsecond");
        assert_eq!(screen[(0, 0)].symbol(), "f");
    }
}
