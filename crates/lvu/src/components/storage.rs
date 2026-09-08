//! The Storage layer (`docs/dialog-system.md` §12.13), converted to the
//! component contract as the pilot of `docs/component-model.md` §6.1.
//!
//! Everything Storage needs lives here: the usage snapshot and its generation
//! fence, the two-step cleanup confirmation, the keymap, the geometry recorded
//! by `render` and answered by `hit`, and the `StorageRequest` outbox that
//! `lvu-app` drains. It touches neither `Views` nor text editing.

use ratatui::{
    Frame,
    layout::Rect,
    style::Modifier,
    widgets::{Paragraph, Wrap},
};
use unicode_width::UnicodeWidthStr;

use crate::app::{
    StorageCategory, StorageRequest, StorageRequestKind, StorageSnapshot, format_storage_bytes,
};
use crate::command_palette::CommandId;
use crate::component::{
    CommandEntry, CommandSpec, Component, Ctx, Event, Outbox, Outcome, RenderCtx, Surface,
};
use crate::dialog_controls::DialogStyles;
use crate::dialog_layout::DialogRegions;
use crate::ui::{
    ELLIPSIS, FIELD_GUTTER, MessageState, clipped_width, dialog_frame_regions, message_rows,
    packed_button_rows, render_action_row, render_message, render_scrollbar, truncated,
};

/// Two scans plus their cancels can never be outstanding at once; the cap only
/// has to stop an unbounded queue if `lvu-app` stops draining (AGENTS.md).
const STORAGE_OUTBOX_CAP: usize = 16;

/// §12.13 entry columns: kind, right-aligned size, name (fill), status. The
/// status column is the first thing width pressure drops.
const STORAGE_KIND_WIDTH: u16 = 10;
const STORAGE_SIZE_WIDTH: u16 = 10;
const STORAGE_STATUS_WIDTH: u16 = 22;

/// The budgets are not an RSS limit; that caption belongs with the numbers it
/// qualifies, where it survives whatever the message row is reporting.
const STORAGE_CAPTION: &str = "managed budgets only · not a process RSS limit";

/// §4.3: the palette entry Storage owns. Public so a caller can name it
/// without reaching into the component's state.
pub const CLEANUP_COMMAND: CommandSpec = CommandSpec {
    id: CommandId::StorageClear,
    name: "Confirm derived-data cleanup",
    description: "Use the existing two-step storage confirmation",
    category: "Storage",
    aliases: &["clear cache", "delete derived"],
    shortcut: None,
};

/// Everything Storage draws that can be clicked.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StorageHit {
    Row(usize),
    Refresh,
    Cleanup,
    /// The scrollable scan-diagnostics pane.
    Diagnostics,
    Body,
}

/// Recorded by `render`, consumed by `hit()` and by the component's own scroll
/// logic (§5.1). Every rect here was painted this frame.
#[derive(Clone, Debug, Default)]
struct StorageGeometry {
    body: Rect,
    rows: Vec<(Rect, usize)>,
    /// Index 0 refreshes, index 1 previews or confirms cleanup.
    actions: Vec<(usize, Rect)>,
    diagnostics: Option<Rect>,
}

#[derive(Debug)]
pub struct StorageDialog {
    /// Whether the layer is on the stack. The slot is permanent so a cancel
    /// pushed on close and a late completion after close both land somewhere.
    open: bool,
    generation: u64,
    snapshot: StorageSnapshot,
    selected: usize,
    scanning: bool,
    confirm_clear: bool,
    status: String,
    /// Body scroll for the diagnostics pane; `scroll_focused` is what Tab
    /// hands the arrow keys to.
    scroll: usize,
    scroll_limit: usize,
    scroll_focused: bool,
    geometry: StorageGeometry,
    surface: Surface,
    pub outbox: Outbox<StorageRequest>,
}

impl Default for StorageDialog {
    fn default() -> Self {
        Self {
            open: false,
            generation: 0,
            snapshot: StorageSnapshot::default(),
            selected: 0,
            scanning: false,
            confirm_clear: false,
            status: String::new(),
            scroll: 0,
            scroll_limit: 0,
            scroll_focused: false,
            geometry: StorageGeometry::default(),
            surface: Surface::default(),
            outbox: Outbox::new(STORAGE_OUTBOX_CAP),
        }
    }
}

impl StorageDialog {
    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    pub fn confirm_clear(&self) -> bool {
        self.confirm_clear
    }

    pub fn status(&self) -> &str {
        &self.status
    }

    pub fn snapshot(&self) -> &StorageSnapshot {
        &self.snapshot
    }

    /// Geometry recorded by the last `render`. Read-only: `hit()` is the way
    /// input reaches these rects; the accessors exist so tests can assert that
    /// what was painted is what is hit-tested.
    pub fn row_rects(&self) -> &[(Rect, usize)] {
        &self.geometry.rows
    }

    pub fn action_rects(&self) -> &[(usize, Rect)] {
        &self.geometry.actions
    }

    pub fn scroll(&self) -> usize {
        self.scroll
    }

    pub fn scroll_limit(&self) -> usize {
        self.scroll_limit
    }

    /// Completion from the worker. Ignored unless `generation` matches; that
    /// fence is the component's.
    pub fn complete(
        &mut self,
        generation: u64,
        snapshot: StorageSnapshot,
        status: String,
        complete: bool,
    ) -> bool {
        if !self.open || self.generation != generation {
            return false;
        }
        self.snapshot = snapshot;
        self.selected = self
            .selected
            .min(self.snapshot.entries.len().saturating_sub(1));
        self.scanning = !complete;
        self.status = status;
        if complete {
            self.confirm_clear = false;
        }
        true
    }

    fn cancel_in_flight(&mut self) {
        if self.scanning {
            let generation = self.generation;
            let _ = self.outbox.push(StorageRequest {
                generation,
                kind: StorageRequestKind::Cancel,
            });
        }
    }

    fn start_scan(&mut self, status: &str) {
        self.cancel_in_flight();
        let generation = self.outbox.next_generation();
        self.generation = generation;
        self.scanning = true;
        self.confirm_clear = false;
        self.status = status.into();
        let _ = self.outbox.push(StorageRequest {
            generation,
            kind: StorageRequestKind::Scan,
        });
    }

    fn refresh(&mut self) {
        self.start_scan("refreshing storage usage…");
    }

    /// §8.9/§8.10: the action row, from the one function `render`, the shell's
    /// mnemonic lookup and `press_action` all read. The second button relabels
    /// with the confirmation state (§12.13) and its letter travels with it.
    fn buttons(&self) -> [&'static str; 2] {
        [
            "&Refresh",
            if self.confirm_clear {
                "Confirm &cleanup"
            } else {
                "Preview &cleanup"
            },
        ]
    }

    /// The two-step cleanup: the first press names the amount, the second
    /// submits. Ownership-aware refusals arrive as the worker's status text.
    fn clear(&mut self) {
        if self.scanning || self.snapshot.reclaimable_bytes == 0 {
            self.status = if self.scanning {
                "wait for the current storage scan".into()
            } else {
                "no unused derived indexes are reclaimable".into()
            };
        } else if !self.confirm_clear {
            self.confirm_clear = true;
            self.status = format!(
                "clear {} of unused recomputable derived indexes? press c again",
                format_storage_bytes(self.snapshot.reclaimable_bytes)
            );
        } else {
            self.scanning = true;
            self.confirm_clear = false;
            self.status = "clearing unused derived indexes…".into();
            let generation = self.generation;
            let _ = self.outbox.push(StorageRequest {
                generation,
                kind: StorageRequestKind::ClearUnusedDerived,
            });
        }
    }

    fn move_selection(&mut self, delta: i32) {
        if self.snapshot.entries.is_empty() {
            return;
        }
        self.selected =
            (self.selected as i32 + delta).rem_euclid(self.snapshot.entries.len() as i32) as usize;
        self.confirm_clear = false;
    }

    fn scroll_body(&mut self, delta: i32) {
        self.scroll = self
            .scroll
            .saturating_add_signed(delta as isize)
            .min(self.scroll_limit);
    }

    fn select_row(&mut self, index: usize) {
        self.selected = index;
        self.confirm_clear = false;
    }

    fn key(&mut self, key: crossterm::event::KeyEvent) -> Outcome {
        use crossterm::event::{KeyCode, KeyEventKind};
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return Outcome::Ignored;
        }
        match key.code {
            KeyCode::Tab => self.scroll_focused = !self.scroll_focused,
            KeyCode::Up | KeyCode::Down => {
                let delta = if key.code == KeyCode::Up { -1 } else { 1 };
                if self.scroll_focused {
                    self.scroll_body(delta);
                } else {
                    self.move_selection(delta);
                }
            }
            KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Char('j') => self.move_selection(1),
            // §8.9: the entry list has no row action, so Enter is the default
            // button, `Refresh`. `r` and `c` are the §8.10 mnemonics of the two
            // buttons and the shell resolves them before this point; they used
            // to be spelled out here, which is the duplication that let a
            // dialog's underline and its keymap drift apart.
            KeyCode::Enter => self.refresh(),
            _ => return Outcome::Ignored,
        }
        Outcome::Consumed
    }

    fn mouse(
        &mut self,
        kind: crossterm::event::MouseEventKind,
        hit: Option<StorageHit>,
    ) -> Outcome {
        use crossterm::event::{MouseButton, MouseEventKind};
        let pressed = matches!(kind, MouseEventKind::Down(MouseButton::Left));
        // The diagnostics pane takes the event before the list does, so a long
        // scan report stays scrollable even over the entry rows' column.
        if hit == Some(StorageHit::Diagnostics) {
            match kind {
                MouseEventKind::Down(MouseButton::Left) => self.scroll_focused = true,
                MouseEventKind::ScrollUp => self.scroll_body(-1),
                MouseEventKind::ScrollDown => self.scroll_body(1),
                _ => return Outcome::Ignored,
            }
            return Outcome::Consumed;
        }
        if pressed {
            self.scroll_focused = false;
        }
        match (pressed, hit) {
            (true, Some(StorageHit::Refresh)) => {
                self.refresh();
                return Outcome::Consumed;
            }
            (true, Some(StorageHit::Cleanup)) => {
                self.clear();
                return Outcome::Consumed;
            }
            (true, Some(StorageHit::Row(index))) => self.select_row(index),
            _ => {}
        }
        match kind {
            MouseEventKind::ScrollUp => self.move_selection(-1),
            MouseEventKind::ScrollDown => self.move_selection(1),
            _ => {}
        }
        Outcome::Consumed
    }
}

fn contains(area: Rect, point: (u16, u16)) -> bool {
    point.0 >= area.x && point.0 < area.right() && point.1 >= area.y && point.1 < area.bottom()
}

/// §9: keep both ends of a long identifier by eliding the middle.
fn elide_middle(value: &str, maximum: usize) -> String {
    if maximum == 0 {
        return String::new();
    }
    if UnicodeWidthStr::width(value) <= maximum {
        return value.to_owned();
    }
    if maximum <= 2 {
        return clipped_width(value, maximum);
    }
    let head = maximum.div_euclid(2);
    let tail = maximum.saturating_sub(head).saturating_sub(1);
    let prefix = clipped_width(value, head);
    let mut suffix = String::new();
    let mut width = 0usize;
    for character in value.chars().rev() {
        let step = UnicodeWidthStr::width(character.encode_utf8(&mut [0; 4]));
        if width + step > tail {
            break;
        }
        width += step;
        suffix.insert(0, character);
    }
    format!("{prefix}{ELLIPSIS}{suffix}")
}

fn surface_of(regions: &DialogRegions) -> Surface {
    Surface {
        popup: regions.popup,
        interior: regions.interior,
        caret: None,
        scrollable: true,
        // Storage has no text field, so `q` dismisses it (§1).
        text_focus: false,
    }
}

impl Component for StorageDialog {
    type Hit = StorageHit;
    type Open = ();

    fn open(&mut self, _params: (), _ctx: &mut Ctx<'_>) {
        self.open = true;
        self.selected = 0;
        self.snapshot = StorageSnapshot::default();
        self.scroll = 0;
        self.scroll_limit = 0;
        self.scroll_focused = false;
        self.geometry = StorageGeometry::default();
        self.start_scan("scanning application-owned storage…");
    }

    fn handle(&mut self, event: Event<StorageHit>, _ctx: &mut Ctx<'_>) -> Outcome {
        match event {
            Event::Key(key) => self.key(key),
            Event::Mouse { kind, hit, .. } => self.mouse(kind, hit),
            Event::Dismiss => {
                // Cleanup that needs the worker happens before `Close`.
                self.cancel_in_flight();
                self.open = false;
                self.scanning = false;
                Outcome::Close
            }
            Event::Command(CommandId::StorageClear) => {
                self.clear();
                Outcome::Consumed
            }
            Event::Command(_) | Event::Paste(_) | Event::View(_) | Event::Resize => {
                Outcome::Ignored
            }
        }
    }

    fn commands(&self, _views: &crate::app::Views) -> Vec<CommandEntry> {
        vec![CommandEntry {
            spec: CommandSpec {
                // `c` reaches this command only while the layer is on top.
                shortcut: self.open.then_some("Alt-C"),
                ..CLEANUP_COMMAND
            },
            unavailable_reason: (!self.open || !self.confirm_clear)
                .then_some("confirm cleanup in Storage preview first"),
        }]
    }

    fn action_labels(&self, _ctx: &Ctx<'_>) -> Vec<&'static str> {
        self.buttons().to_vec()
    }

    fn press_action(&mut self, index: usize, _ctx: &mut Ctx<'_>) -> Outcome {
        match index {
            0 => self.refresh(),
            1 => self.clear(),
            _ => return Outcome::Ignored,
        }
        Outcome::Consumed
    }

    fn surface(&self) -> Surface {
        self.surface
    }

    fn hit(&self, point: (u16, u16)) -> Option<StorageHit> {
        let g = &self.geometry;
        if g.diagnostics.is_some_and(|area| contains(area, point)) {
            return Some(StorageHit::Diagnostics);
        }
        g.actions
            .iter()
            .find_map(|(index, area)| {
                contains(*area, point).then_some(if *index == 0 {
                    StorageHit::Refresh
                } else {
                    StorageHit::Cleanup
                })
            })
            .or_else(|| {
                g.rows.iter().find_map(|(area, index)| {
                    contains(*area, point).then_some(StorageHit::Row(*index))
                })
            })
            .or_else(|| contains(g.body, point).then_some(StorageHit::Body))
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &RenderCtx<'_>) -> Surface {
        use crate::dialog_layout::{DialogClass, DialogContent, content_width, pane};

        let theme = ctx.theme;
        let ascii = ctx.ascii;
        let styles = DialogStyles::new(theme);
        let mut rows: Vec<(Rect, usize)> = Vec::new();
        let mut diagnostics_rect: Option<Rect> = None;
        let mut scroll = self.scroll;
        let mut scroll_limit = 0usize;

        let snapshot = &self.snapshot;
        let width = content_width(area, DialogClass::L);

        // §12.13: the totals move out of the title into labelled summary rows.
        let summary = [
            (
                "Row cache",
                format!(
                    "{} of {}",
                    format_storage_bytes(snapshot.row_cache_bytes),
                    format_storage_bytes(snapshot.row_cache_limit)
                ),
            ),
            (
                "Membership",
                format!(
                    "{} of {}",
                    format_storage_bytes(snapshot.query_index_bytes),
                    format_storage_bytes(snapshot.query_index_limit)
                ),
            ),
            (
                "Derived disk",
                format!(
                    "{} of {}",
                    format_storage_bytes(snapshot.total_bytes),
                    format_storage_bytes(snapshot.derived_index_limit_total)
                ),
            ),
            (
                "Per source",
                format!(
                    "{} cap",
                    format_storage_bytes(snapshot.derived_index_limit_per_source)
                ),
            ),
        ];
        // Two pairs per row when there is room, otherwise one per row (§4.2).
        let paired = width >= 72;
        let summary_rows = if paired { 2 } else { 4 } + 1;

        // A scan diagnostic can be far longer than the two rows §7.4 allows, so
        // the message states the outcome and the full text lives in a
        // scrollable pane (§9). Clipping a diagnostic is not an inspection path.
        let diagnostics = snapshot.errors.join("\n");
        let (state, sentence) = if !snapshot.errors.is_empty() {
            (
                MessageState::Error,
                format!(
                    "{} problem{} in the last scan · details below",
                    snapshot.errors.len(),
                    if snapshot.errors.len() == 1 { "" } else { "s" }
                ),
            )
        } else if self.confirm_clear {
            // The confirmation wording is the app's, not the layout's: it names
            // the exact amount the next keypress would delete.
            (MessageState::Pending, self.status.clone())
        } else if self.scanning {
            (MessageState::Updating, self.status.clone())
        } else {
            (MessageState::Scanned, self.status.clone())
        };

        let action_labels = self.buttons();
        let entries = snapshot.entries.len();
        let diagnostic_rows = if diagnostics.is_empty() { 0 } else { 4 };
        let content = DialogContent {
            header: 0,
            body: summary_rows
                + 1
                + u16::try_from(entries.clamp(1, 12)).unwrap_or(1)
                + 1
                + diagnostic_rows,
            message: message_rows(&sentence, width),
            help: 0,
            actions: packed_button_rows(width, &action_labels),
        };
        let regions = dialog_frame_regions(frame, area, DialogClass::L, "Storage", &content, theme);
        let surface = surface_of(&regions);
        let body = regions.body;
        if body.width == 0 || body.height == 0 {
            self.geometry = StorageGeometry::default();
            self.scroll_limit = 0;
            self.surface = surface;
            return surface;
        }

        let label_width = u16::try_from(UnicodeWidthStr::width("Derived disk")).unwrap_or(12);
        let column = body.width / 2;
        for (index, (label, value)) in summary.iter().enumerate() {
            let (row, x, cell_width) = if paired {
                (
                    index / 2,
                    body.x
                        .saturating_add(if index % 2 == 0 { 0 } else { column }),
                    column,
                )
            } else {
                (index, body.x, body.width)
            };
            let Some(y) = u16::try_from(row)
                .ok()
                .map(|row| body.y.saturating_add(row))
                .filter(|y| *y < body.bottom())
            else {
                continue;
            };
            frame.render_widget(
                Paragraph::new(*label).style(styles.label),
                Rect::new(x, y, label_width.min(cell_width), 1),
            );
            let value_x = x.saturating_add(label_width).saturating_add(FIELD_GUTTER);
            if value_x < x.saturating_add(cell_width) {
                frame.render_widget(
                    Paragraph::new(truncated(
                        value,
                        usize::from(x.saturating_add(cell_width).saturating_sub(value_x)),
                    ))
                    .style(styles.description),
                    Rect::new(
                        value_x,
                        y,
                        x.saturating_add(cell_width).saturating_sub(value_x),
                        1,
                    ),
                );
            }
        }

        let available = body.height.saturating_sub(summary_rows).saturating_sub(1);
        let diagnostic_height = if diagnostics.is_empty() {
            0
        } else {
            diagnostic_rows.min(available.saturating_sub(2))
        };
        if let Some(y) = Some(body.y.saturating_add(summary_rows.saturating_sub(1)))
            .filter(|y| *y < body.bottom())
        {
            frame.render_widget(
                Paragraph::new(truncated(STORAGE_CAPTION, usize::from(body.width)))
                    .style(styles.description),
                Rect::new(body.x, y, body.width, 1),
            );
        }

        let list_area = Rect::new(
            body.x,
            body.y.saturating_add(summary_rows).saturating_add(1),
            body.width,
            available.saturating_sub(diagnostic_height),
        );
        if list_area.height > 0 {
            let count = format!(
                "{} of {} · {} reclaimable",
                usize::from(list_area.height.saturating_sub(1)).min(entries),
                entries,
                format_storage_bytes(snapshot.reclaimable_bytes)
            );
            let count = truncated(&count, usize::from(list_area.width / 2));
            let rects = pane(
                list_area,
                u16::try_from(UnicodeWidthStr::width(count.as_str())).unwrap_or(0),
                entries,
            );
            frame.render_widget(
                Paragraph::new("Entries").style(styles.label.add_modifier(Modifier::BOLD)),
                rects.heading,
            );
            if rects.count.width > 0 {
                frame.render_widget(
                    Paragraph::new(count)
                        .style(styles.description)
                        .right_aligned(),
                    rects.count,
                );
            }

            // §5.4 width pressure: the status column is dropped before the name
            // is elided, because the name is the identifier.
            let viewport = rects.viewport;
            let columns = viewport
                .width
                .saturating_sub(2)
                .saturating_sub(STORAGE_KIND_WIDTH)
                .saturating_sub(STORAGE_SIZE_WIDTH);
            let (name_width, status_width) = if columns >= STORAGE_STATUS_WIDTH * 2 {
                let name = (columns / 2).min(60);
                (name, columns.saturating_sub(name))
            } else {
                (columns, 0)
            };
            let visible = usize::from(viewport.height);
            let selected = self.selected.min(entries.saturating_sub(1));
            let top = selected.saturating_sub(visible.saturating_sub(1));
            for (offset, (index, entry)) in snapshot
                .entries
                .iter()
                .enumerate()
                .skip(top)
                .take(visible)
                .enumerate()
            {
                let kind = match entry.category {
                    StorageCategory::Capture => "capture",
                    StorageCategory::Derived => "derived",
                    StorageCategory::Workspace => "workspace",
                    StorageCategory::Investigation => "exports",
                };
                let chosen = index == selected;
                let marker = if chosen {
                    if ascii { "> " } else { "› " }
                } else {
                    "  "
                };
                let mut text = format!(
                    "{marker}{kind:<kind_width$}{:>size_width$}  {:<name_width$}",
                    format_storage_bytes(entry.bytes),
                    elide_middle(&entry.label, usize::from(name_width).saturating_sub(1)),
                    kind_width = usize::from(STORAGE_KIND_WIDTH),
                    size_width = usize::from(STORAGE_SIZE_WIDTH).saturating_sub(2),
                    name_width = usize::from(name_width),
                );
                if status_width > 0 {
                    let reclaim = if entry.reclaimable > 0 {
                        " · reclaimable"
                    } else {
                        ""
                    };
                    text.push_str(&truncated(
                        &format!("{}{reclaim}", entry.status),
                        usize::from(status_width),
                    ));
                }
                let row = Rect::new(
                    viewport.x,
                    viewport.y.saturating_add(offset as u16),
                    viewport.width,
                    1,
                );
                frame.render_widget(
                    Paragraph::new(truncated(&text, usize::from(row.width))).style(if chosen {
                        styles.selection
                    } else {
                        styles.description
                    }),
                    row,
                );
                rows.push((row, index));
            }
            if let Some(bar) = rects.scrollbar {
                render_scrollbar(
                    frame,
                    bar,
                    top,
                    entries.saturating_sub(visible),
                    theme,
                    ascii,
                );
            }
        }

        // The diagnostics pane owns the body scroll, which is the target Tab
        // hands the arrow keys to, so a long scan report stays reachable.
        if diagnostic_height > 0 {
            let area = Rect::new(
                body.x,
                list_area.bottom(),
                body.width,
                body.bottom().saturating_sub(list_area.bottom()),
            );
            let text = Paragraph::new(diagnostics.clone())
                .wrap(Wrap { trim: false })
                .style(styles.error);
            let probe = pane(area, 0, usize::MAX);
            let wrapped = text.line_count(probe.viewport.width.max(1));
            let rects = pane(area, 0, wrapped);
            frame.render_widget(
                Paragraph::new("Diagnostics").style(if self.scroll_focused {
                    styles.shortcut.add_modifier(Modifier::BOLD)
                } else {
                    styles.label.add_modifier(Modifier::BOLD)
                }),
                rects.heading,
            );
            let limit = wrapped.saturating_sub(usize::from(rects.viewport.height));
            scroll_limit = limit;
            scroll = scroll.min(limit);
            frame.render_widget(
                text.scroll((scroll.min(u16::MAX as usize) as u16, 0)),
                rects.viewport,
            );
            if let Some(bar) = rects.scrollbar {
                render_scrollbar(frame, bar, scroll, limit, theme, ascii);
            }
            diagnostics_rect = Some(area);
        }

        render_message(frame, regions.message, state, &sentence, theme, ascii);
        // §8.2: cleanup is destructive once it is the confirming action.
        let destructive = if self.confirm_clear {
            vec![1]
        } else {
            Vec::new()
        };
        let actions = render_action_row(
            frame,
            regions.actions,
            &action_labels,
            None,
            &destructive,
            theme,
        );

        self.scroll = scroll;
        self.scroll_limit = scroll_limit;
        self.geometry = StorageGeometry {
            body,
            rows,
            actions,
            diagnostics: diagnostics_rect,
        };
        self.surface = surface;
        surface
    }
}
