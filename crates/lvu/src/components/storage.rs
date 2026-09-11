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
use crate::dialog_controls::{ActionRow, DialogStyles, render_role_button, stable_action_rows};
use crate::dialog_layout::{DialogSpec, PresentationKind, ScrollViewport, plan_list};
use crate::ui::{
    ELLIPSIS, FIELD_GUTTER, MessageState, clipped_width, render_message, render_scrollbar,
    truncated,
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
    /// A row of the generic overflow `More ▾` menu (20x6 floor only).
    /// Hit-tested first: §10 puts it on top.
    Menu(usize),
    /// The generic overflow `More ▾` button itself (20x6 floor only).
    More,
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
    menu: Vec<(Rect, usize)>,
    more: Vec<(Rect, ())>,
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
    /// Generic overflow `More ▾` menu (20x6 floor only, same-layer anchored
    /// popup, not a child). Preserves the two-step confirmation when the
    /// shared band collapses to one row.
    menu_open: bool,
    menu_selected: usize,
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
            menu_open: false,
            menu_selected: 0,
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

    /// Geometry recorded by the last `render` for the generic overflow menu
    /// (20x6 floor only). Same-layer anchored popup, not a child.
    pub fn menu_rects(&self) -> &[(Rect, usize)] {
        &self.geometry.menu
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
        if self.menu_open {
            // Generic overflow menu has at most the hidden actions (Refresh /
            // Cleanup); same-layer, not a child. Length comes from the last
            // painted menu so wheel/keys never disagree with paint.
            let len = self.geometry.menu.len().max(1);
            self.menu_selected =
                (self.menu_selected as i32 + delta).rem_euclid(len as i32) as usize;
            return;
        }
        if self.snapshot.entries.is_empty() {
            return;
        }
        self.selected =
            (self.selected as i32 + delta).rem_euclid(self.snapshot.entries.len() as i32) as usize;
        self.confirm_clear = false;
    }

    fn choose_menu(&mut self, _index: usize) -> Outcome {
        // Same-layer overflow menu holds only the hidden Cleanup/Confirm:
        // Refresh (the default) always survives in the band, so the menu
        // never needs to distinguish rows. No child, no Replace.
        self.menu_open = false;
        self.menu_selected = 0;
        self.clear();
        Outcome::Consumed
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
        // §10: an open anchored menu owns arrows and Enter.
        if self.menu_open {
            match key.code {
                KeyCode::Up => self.move_selection(-1),
                KeyCode::Down => self.move_selection(1),
                KeyCode::Enter => {
                    let index = self.menu_selected;
                    return self.choose_menu(index);
                }
                KeyCode::Esc => {
                    self.menu_open = false;
                    self.menu_selected = 0;
                    return Outcome::Consumed;
                }
                _ => return Outcome::Consumed,
            }
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
            // When the shared band overflows (20x6 floor), Enter still runs the
            // default; hidden Cleanup is reached via the same-layer More menu.
            KeyCode::Enter => {
                if self.menu_open {
                    let index = self.menu_selected;
                    return self.choose_menu(index);
                }
                self.refresh()
            }
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
        // §10: the open menu takes the click first (same-layer, not a child).
        if pressed && let Some(StorageHit::Menu(index)) = hit {
            return self.choose_menu(index);
        }
        if matches!(kind, MouseEventKind::ScrollUp | MouseEventKind::ScrollDown) && self.menu_open {
            self.move_selection(if matches!(kind, MouseEventKind::ScrollUp) {
                -1
            } else {
                1
            });
            return Outcome::Consumed;
        }
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
            (true, Some(StorageHit::More)) => {
                self.menu_open = !self.menu_open;
                self.menu_selected = 0;
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

/// Stable LongContent budgets: outer size is policy-only, never scan counts.
/// Header 0, body minimum 3, message 2 stable max, no help, actions from the
/// stable width budget so pending/scanning/populated/error frames share one
/// frame and sticky tail. Hand-rolled layout here is presentation-only
/// folding, never query membership (AGENTS.md).
fn storage_spec_for(area: Rect, buttons: &[&str]) -> DialogSpec {
    let (policy_w, _) = crate::dialog_layout::policy_size(area, PresentationKind::LongContent);
    let estimate = policy_w.saturating_sub(4).max(1);
    let action_rows = stable_action_rows(estimate, buttons).clamp(1, 2);
    DialogSpec::new(PresentationKind::LongContent, 0, 3, 2, 0, action_rows)
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
        self.menu_open = false;
        self.menu_selected = 0;
        self.geometry = StorageGeometry::default();
        self.start_scan("scanning application-owned storage…");
    }

    fn handle(&mut self, event: Event<StorageHit>, _ctx: &mut Ctx<'_>) -> Outcome {
        match event {
            Event::Key(key) => self.key(key),
            Event::Mouse { kind, hit, .. } => self.mouse(kind, hit),
            Event::Dismiss => {
                // §10: the open menu absorbs dismissal rather than the layer.
                if self.menu_open {
                    self.menu_open = false;
                    self.menu_selected = 0;
                    return Outcome::Consumed;
                }
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
        // §10: the open menu is drawn last and takes the point first.
        if let Some((_, index)) = g.menu.iter().find(|(rect, _)| contains(*rect, point)) {
            return Some(StorageHit::Menu(*index));
        }
        if let Some((_, _)) = g.more.iter().find(|(rect, _)| contains(*rect, point)) {
            return Some(StorageHit::More);
        }
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
        let theme = ctx.theme;
        let ascii = ctx.ascii;
        let styles = DialogStyles::new(theme);
        let mut rows: Vec<(Rect, usize)> = Vec::new();
        let mut diagnostics_rect: Option<Rect> = None;
        let mut scroll = self.scroll;
        let mut scroll_limit = 0usize;

        let snapshot = self.snapshot.clone();
        let action_labels = self.buttons();
        // Stable budgets: outer frame is policy-only; scan counts, diagnostics
        // length and errors size only scroll extents, never the frame.
        let spec = storage_spec_for(area, &action_labels);
        let Ok(resolved) =
            crate::dialog_layout::resolve_dialog(area, &spec, 1, &action_labels, Some(0), None)
        else {
            // Below the 20x6 floor the tiny fallback owns the frame.
            self.geometry = StorageGeometry::default();
            self.scroll_limit = 0;
            self.surface = Surface {
                popup: Rect::default(),
                interior: Rect::default(),
                caret: None,
                scrollable: false,
                text_focus: false,
            };
            return self.surface;
        };
        crate::ui::render_responsive_frame(frame, &resolved, "Storage", ctx.active, theme);
        let mut surface = Surface {
            popup: resolved.frame,
            interior: resolved.interior,
            caret: None,
            scrollable: false,
            // Storage has no text field, so `q` dismisses it (§1).
            text_focus: false,
        };
        let body = resolved.body.viewport;
        if body.width == 0 || body.height == 0 {
            self.geometry = StorageGeometry::default();
            self.scroll_limit = 0;
            self.surface = surface;
            return surface;
        }
        let message_rect = resolved.message;
        let action_geom = resolved.actions.clone();
        let roomy = !resolved.compact;
        let width = resolved.content.width;

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
        let summary_rows: u16 = (if paired { 2 } else { 4 }) + 1;

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

        let entries = snapshot.entries.len();
        // Body owns surplus: summary fixed, list Fill, diagnostics bounded with
        // roomy gaps and compact zero gaps. At tiny pressure the focused pane
        // wins (list vs diagnostics) so paint, wheel and hitboxes never
        // disagree; summary hides at the floor but list/diagnostics/actions
        // stay reachable. Hand-rolled here is presentation-only folding.
        let gap = u16::from(roomy);
        let spare = body.height;
        let diag_wanted: u16 = if diagnostics.is_empty() { 0 } else { 4 };
        let full_need = summary_rows
            .saturating_add(1)
            .saturating_add(diag_wanted)
            .saturating_add(gap.saturating_mul(2));
        let (show_summary, list_h, diag_h) = if spare == 0 {
            (false, 0, 0)
        } else if full_need <= spare {
            (
                true,
                spare
                    .saturating_sub(summary_rows)
                    .saturating_sub(1)
                    .saturating_sub(diag_wanted)
                    .saturating_sub(gap.saturating_mul(2))
                    .max(1),
                diag_wanted,
            )
        } else if self.scroll_focused && diag_wanted > 0 {
            (false, 0, spare)
        } else {
            (false, spare, 0)
        };
        let label_width = u16::try_from(UnicodeWidthStr::width("Derived disk")).unwrap_or(12);
        let column = body.width / 2;
        if show_summary {
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
            if let Some(y) = Some(body.y.saturating_add(summary_rows.saturating_sub(1)))
                .filter(|y| *y < body.bottom())
            {
                frame.render_widget(
                    Paragraph::new(truncated(STORAGE_CAPTION, usize::from(body.width)))
                        .style(styles.description),
                    Rect::new(body.x, y, body.width, 1),
                );
            }
        }
        // Gaps only between shown sections (roomy 1, compact 0).
        let list_y = if show_summary {
            body.y
                .saturating_add(summary_rows)
                .saturating_add(1)
                .saturating_add(gap)
        } else {
            body.y
        };
        let list_area = Rect::new(
            body.x,
            list_y,
            body.width,
            list_h.min(body.bottom().saturating_sub(list_y)),
        );
        if list_area.height > 0 {
            let count = format!(
                "{} of {} · {} reclaimable",
                usize::from(list_area.height.saturating_sub(1)).min(entries),
                entries,
                format_storage_bytes(snapshot.reclaimable_bytes)
            );
            let count = truncated(&count, usize::from(list_area.width / 2));
            let selected_init = self.selected.min(entries.saturating_sub(1));
            let rects = plan_list(
                list_area,
                u16::try_from(UnicodeWidthStr::width(count.as_str())).unwrap_or(0),
                entries,
                (!snapshot.entries.is_empty()).then_some(selected_init),
                0,
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
            let visible = rects.row_rects.len();
            let selected = self.selected.min(entries.saturating_sub(1));
            let top = rects.first_row;
            for (offset, row) in rects.row_rects.iter().copied().enumerate() {
                let index = top.saturating_add(offset);
                let Some(entry) = snapshot.entries.get(index) else {
                    continue;
                };
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
                    entries.saturating_sub(visible.max(1)),
                    theme,
                    ascii,
                );
                surface.scrollable = true;
            }
        }

        // Scan/result diagnostics scroll inside the body (shared viewport)
        // and keep actions sticky; confirmation stays same-layer, not a child.
        // Hand-rolled wrapping here is presentation-only folding.
        if diag_h > 0 {
            let area = Rect::new(
                body.x,
                list_area
                    .bottom()
                    .saturating_add(gap.min(body.bottom().saturating_sub(list_area.bottom()))),
                body.width,
                diag_h.min(body.bottom().saturating_sub(list_area.bottom())),
            );
            // Heading 1 row + viewport below, indent shared with list panes.
            let heading = Rect::new(area.x, area.y, area.width, 1.min(area.height));
            let viewport_probe = Rect::new(
                area.x
                    .saturating_add(crate::dialog_layout::PANE_INDENT.min(area.width)),
                area.y.saturating_add(1),
                area.width
                    .saturating_sub(crate::dialog_layout::PANE_INDENT.min(area.width)),
                area.height.saturating_sub(1),
            );
            let text = Paragraph::new(diagnostics.clone())
                .wrap(Wrap { trim: false })
                .style(styles.error);
            let wrapped = text.line_count(viewport_probe.width.max(1));
            let viewport = ScrollViewport::new(viewport_probe, wrapped, scroll);
            let rects_viewport = viewport.viewport;
            let rects = crate::dialog_layout::ListGeometry {
                heading,
                count: Rect::new(area.right(), area.y, 0, 0),
                viewport: rects_viewport,
                scrollbar: viewport.scrollbar,
                first_row: viewport.first_row,
                row_rects: Vec::new(),
            };
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
            // Reveal the stored offset via the shared viewport; same rects
            // drive paint, scrollbar and mouse.
            let scrolled = ScrollViewport::new(viewport_probe, wrapped, scroll);
            let bar_w = u16::from(scrolled.scrollbar.is_some());
            let text_area = Rect::new(
                scrolled.viewport.x,
                scrolled.viewport.y,
                scrolled.viewport.width.saturating_sub(bar_w),
                scrolled.viewport.height,
            );
            frame.render_widget(
                text.scroll((scrolled.first_row.min(u16::MAX as usize) as u16, 0)),
                text_area,
            );
            if let Some(bar) = scrolled.scrollbar {
                render_scrollbar(frame, bar, scrolled.first_row, limit, theme, ascii);
                surface.scrollable = true;
            }
            scroll = scrolled.first_row;
            diagnostics_rect = Some(area);
        }

        render_message(frame, message_rect, state, &sentence, theme, ascii);
        // §8.2: cleanup is destructive once it is the confirming action.
        // Default is Refresh even while Confirm shows; destructive never default.
        let destructive = if self.confirm_clear {
            vec![1]
        } else {
            Vec::new()
        };
        let action_row = ActionRow {
            labels: &action_labels,
            default: Some(0),
            destructive: &destructive,
            focused: None,
        };
        let mut action_hit: Vec<(usize, Rect)> = Vec::new();
        let mut more_hit: Vec<(Rect, ())> = Vec::new();
        let mut menu_anchor: Option<Rect> = None;
        for (orig, rect) in action_geom.buttons.iter().copied() {
            let role = action_row.role(orig);
            render_role_button(frame, rect, action_labels[orig], role, false, theme);
            action_hit.push((orig, rect));
        }
        // Pressure order: wrap to two rows first; beyond that collapse to one
        // row with a same-layer More ▾ (never a child). The hidden Cleanup
        // stays reachable via the anchored menu, preserving the two-step.
        if !action_geom.overflow.is_empty()
            && let Some(more_rect) = action_geom.more
        {
            let more_label = if ascii { "More v" } else { "More \u{25be}" };
            render_role_button(
                frame,
                more_rect,
                more_label,
                crate::dialog_controls::ButtonRole::Normal,
                false,
                theme,
            );
            menu_anchor = Some(more_rect);
            more_hit.push((more_rect, ()));
        }
        // Same-layer anchored overflow menu for the hidden Cleanup/Confirm.
        // Frame-bounded to the terminal frame, exact union into the popup.
        let mut menu_hit: Vec<(Rect, usize)> = Vec::new();
        if self.menu_open
            && let Some(anchor) = menu_anchor
            && !action_geom.overflow.is_empty()
        {
            let hidden: Vec<usize> = action_geom.overflow.clone();
            let longest = hidden
                .iter()
                .filter_map(|idx| action_labels.get(*idx))
                .map(|label| UnicodeWidthStr::width(*label))
                .max()
                .unwrap_or(8);
            let preferred = u16::try_from(longest.saturating_add(4))
                .unwrap_or(12)
                .max(12);
            let spec = crate::dialog_layout::AnchoredSpec::new(hidden.len(), None, preferred, 0);
            let selected = self.menu_selected.min(hidden.len().saturating_sub(1));
            let pop = crate::dialog_layout::anchored_geometry(area, anchor, &spec, selected, 0);
            surface.popup = surface.popup.union(pop.popup);
            crate::ui::clear_themed(frame, pop.popup, theme);
            frame.render_widget(
                ratatui::widgets::Block::default()
                    .borders(ratatui::widgets::Borders::ALL)
                    .border_style(styles.label),
                pop.popup,
            );
            let bar_w = u16::from(pop.scrollbar.is_some());
            for (offset, orig) in hidden
                .iter()
                .copied()
                .skip(pop.first_item)
                .take(usize::from(pop.viewport.height))
                .enumerate()
            {
                let idx = pop.first_item.saturating_add(offset);
                let Some(label) = action_labels.get(orig) else {
                    continue;
                };
                let row = Rect::new(
                    pop.viewport.x,
                    pop.viewport.y.saturating_add(offset as u16),
                    pop.viewport.width.saturating_sub(bar_w),
                    1,
                );
                let style = if idx == selected {
                    styles.selection
                } else {
                    crate::dialog_controls::button_style(theme, false, false)
                };
                frame.render_widget(Paragraph::new((*label).to_owned()).style(style), row);
                // Store the original action index so choosing runs Refresh (0)
                // or Cleanup/Confirm (1) directly, same-layer.
                menu_hit.push((row, orig));
            }
            if let Some(bar) = pop.scrollbar {
                render_scrollbar(
                    frame,
                    bar,
                    pop.first_item,
                    hidden
                        .len()
                        .saturating_sub(usize::from(pop.viewport.height)),
                    theme,
                    ascii,
                );
                surface.scrollable = true;
            }
        }

        self.scroll = scroll;
        self.scroll_limit = scroll_limit;
        // Clamp the overflow selection to the last painted menu so keys and
        // mouse never disagree after a resize.
        if !menu_hit.is_empty() {
            self.menu_selected = self.menu_selected.min(menu_hit.len().saturating_sub(1));
        }
        self.geometry = StorageGeometry {
            body,
            rows,
            actions: action_hit,
            diagnostics: diagnostics_rect,
            menu: menu_hit,
            more: more_hit,
        };
        self.surface = surface;
        surface
    }
}
