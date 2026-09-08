//! The Settings layer (`docs/dialog-system.md` §12.14), converted to the
//! component contract as step 10 of `docs/component-model.md` §6.3.
//!
//! Settings introduces the two seams §6.3 names for it. The first is
//! `ctx.appearance`: a draft theme, delight, reduced-motion or ASCII choice
//! previews by writing the shell's `Appearance` live, and dismissal rolls it
//! back to the effective values the layer opened with. That is the single
//! documented component-specific member of `Ctx` (§2.2). The second is the
//! `SettingsRequest` outbox `lvu-app` drains, fenced by the dialog generation
//! so a completion for an older dialog cannot overwrite a newer draft.
//!
//! The form's text fields are dialog-owned, so their carets live here (one
//! `TextCursor` per editable field, which is what `CursorBank` gave them
//! before) and `ctx.cursors` is unused.

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::Line,
    widgets::{Block, Borders, Clear, Paragraph, Widget},
};
use unicode_width::UnicodeWidthStr;

use crate::app::{
    SettingsContext, SettingsRequest, SettingsValues, time_zone_choices, time_zone_label,
};
use crate::component::{
    Appearance, Component, Ctx, Event, Outbox, Outcome, RenderCtx, Surface, is_typed_char,
};
use crate::dialog_controls::{DialogStyles, button_style};
use crate::text_edit::{EditCommand, EditPolicy, TextCursor, edit};
use crate::theme::{Theme, ThemeId};
use crate::ui::{
    ACTION_GUTTER, FIELD_GUTTER, InputSurface, MessageState, clipped_width, dialog_frame_regions,
    help_rows, message_rows, packed_button_rows, place_input_cursor_at, render_action_row,
    render_help_text, render_message, render_scrollbar, truncated,
};

/// A save and its retry can be outstanding at once; the cap only has to stop an
/// unbounded queue if `lvu-app` stops draining (AGENTS.md). The dialog's own
/// "save queue is full" refusal fires at two, as it did on `App`.
const SETTINGS_OUTBOX_CAP: usize = 16;

/// The refusal threshold the legacy `Action::SaveSettings` arm used.
const SETTINGS_QUEUE_LIMIT: usize = 2;

/// Bytes a settings text field accepts.
const SETTINGS_FIELD_BYTES: usize = 256;

/// Logical body rows of the Settings form (§12.14). The form is always shown in
/// full; the body window follows the focused control, so no field is ever
/// hidden behind a paging button.
const SETTINGS_FORM_ROWS: u16 = 17;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SettingsField {
    Provider,
    Mode,
    Thinking,
    Theme,
    /// The fixed UTC offset the log viewport shows times in.
    DisplayZone,
    Delight,
    ReducedMotion,
    Ascii,
    RowCache,
    Membership,
    DiskTotal,
    IndexPerSource,
}

impl SettingsField {
    pub const ALL: [Self; 12] = [
        Self::Provider,
        Self::Mode,
        Self::Thinking,
        Self::Theme,
        Self::DisplayZone,
        Self::Delight,
        Self::ReducedMotion,
        Self::Ascii,
        Self::RowCache,
        Self::Membership,
        Self::DiskTotal,
        Self::IndexPerSource,
    ];

    /// The caret slot for an editable field. The toggles and the dropdown take
    /// no text, so they have none.
    fn text_slot(self) -> Option<usize> {
        Some(match self {
            Self::Provider => 0,
            Self::Mode => 1,
            Self::Thinking => 2,
            Self::RowCache => 3,
            Self::Membership => 4,
            Self::DiskTotal => 5,
            Self::IndexPerSource => 6,
            Self::Theme | Self::DisplayZone | Self::Delight | Self::ReducedMotion | Self::Ascii => {
                return None;
            }
        })
    }
}

/// The rows a field's dropdown offers, in the order it draws them.
fn settings_choices(field: SettingsField) -> Vec<String> {
    match field {
        // The token, not the pretty label: it is what the settings file
        // stores, and showing the two spellings apart would invite a bug
        // report about which one is "the" theme name.
        SettingsField::Theme => ThemeId::ALL
            .iter()
            .map(|theme| theme.as_str().to_owned())
            .collect(),
        SettingsField::DisplayZone => time_zone_choices()
            .iter()
            .map(|(label, _)| (*label).to_owned())
            .collect(),
        _ => Vec::new(),
    }
}

/// Where a stored zone token sits in the offered list. A token this build does
/// not know reads as UTC, so an unrecognised setting still shows a legible log.
fn zone_index(token: &str) -> usize {
    time_zone_choices()
        .iter()
        .position(|(_, value)| *value == token)
        .unwrap_or(0)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SettingsControl {
    Field(SettingsField),
    Save,
    More,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SettingsStatus {
    #[default]
    Saved,
    Pending,
    Error,
}

/// The dialog's own data, moved off `App` unchanged.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SettingsDialogState {
    pub generation: u64,
    pub selected: usize,
    pub focus: SettingsControl,
    pub draft: SettingsValues,
    pub context: SettingsContext,
    pub saving: bool,
    pub status_kind: SettingsStatus,
    pub status: String,
    /// Which field's choice list is open, if any. Two fields offer one now, and
    /// a third would cost nothing.
    pub dropdown: Option<SettingsField>,
    pub choice_selected: usize,
    pub details_scroll: usize,
    pub details_scroll_limit: usize,
}

/// Everything Settings draws that can be clicked (§5.1).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SettingsHit {
    Control(SettingsControl),
    Choice(usize),
}

/// Recorded by `render`, consumed by `hit()`. Every rect here was painted this
/// frame.
#[derive(Clone, Debug, Default)]
struct SettingsGeometry {
    controls: Vec<(Rect, SettingsControl)>,
    choices: Vec<(Rect, usize)>,
}

/// What the shell still has to do with a successful save: the appearance to
/// show — a newer dialog's unsaved preview outranks the saved values — and the
/// status-line notice for a completion no open dialog is showing.
#[derive(Clone, Debug)]
pub struct SaveCompletion {
    pub appearance: Appearance,
    pub notice: Option<String>,
}

#[derive(Debug)]
pub struct SettingsDialog {
    /// Whether the layer is on the stack. The slot is permanent so a completion
    /// arriving after close still lands somewhere.
    open: bool,
    /// The effective-settings snapshot `lvu-app` supplies. Without it there is
    /// nothing to show, and the shell refuses to push the layer.
    context: Option<SettingsContext>,
    next_generation: u64,
    state: Option<SettingsDialogState>,
    /// One caret per editable field, indexed by `SettingsField::text_slot`.
    /// `None` means the field has not been edited yet, so its caret is the end
    /// of the value — what `CursorBank::get_or_end` did for it before.
    cursors: [Option<TextCursor>; 7],
    geometry: SettingsGeometry,
    surface: Surface,
    pub outbox: Outbox<SettingsRequest>,
}

impl Default for SettingsDialog {
    fn default() -> Self {
        Self {
            open: false,
            context: None,
            next_generation: 1,
            state: None,
            cursors: [None; 7],
            geometry: SettingsGeometry::default(),
            surface: Surface::default(),
            outbox: Outbox::new(SETTINGS_OUTBOX_CAP),
        }
    }
}

impl SettingsDialog {
    /// The effective settings `lvu-app` resolved at startup and after each
    /// save. Replaces `App::configure_settings`' write to `settings_context`.
    pub fn configure(&mut self, context: SettingsContext) {
        self.context = Some(context);
    }

    /// Whether the layer has anything to show. The shell asks before pushing,
    /// because a component cannot decline its own `open`.
    pub fn is_configured(&self) -> bool {
        self.context.is_some()
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn state(&self) -> Option<&SettingsDialogState> {
        self.state.as_ref()
    }

    /// Geometry recorded by the last `render`; `hit()` is how input reaches it.
    /// The accessors exist so tests can assert that what was painted is what is
    /// hit-tested.
    pub fn control_rects(&self) -> &[(Rect, SettingsControl)] {
        &self.geometry.controls
    }

    pub fn theme_choice_rects(&self) -> &[(Rect, usize)] {
        &self.geometry.choices
    }

    /// A successful save. The dialog's own generation fences it: an older
    /// completion advances the rollback baseline of a newer dialog without
    /// replacing that newer draft or briefly exposing stale globals.
    pub fn complete_save(&mut self, generation: u64, context: SettingsContext) -> SaveCompletion {
        let mut notice = None;
        let preview = if let Some(dialog) = &mut self.state {
            if dialog.generation == generation {
                dialog.saving = false;
                dialog.draft = context.saved.clone();
                dialog.context = context.clone();
                dialog.status_kind = SettingsStatus::Saved;
                dialog.status = settings_restart_status(&context);
                dialog.details_scroll = 0;
                None
            } else {
                dialog.context = context.clone();
                notice = Some(settings_restart_status(&context));
                Some(Appearance {
                    theme_id: dialog.draft.theme,
                    delight_enabled: dialog.draft.delight_enabled,
                    reduced_motion: dialog.draft.reduced_motion,
                    ascii: dialog.draft.ascii,
                    display_zone: dialog.draft.display_zone.clone(),
                })
            }
        } else {
            notice = Some(settings_restart_status(&context));
            None
        };
        let appearance = preview.unwrap_or_else(|| Appearance {
            theme_id: context.effective_theme,
            delight_enabled: context.effective_delight_enabled,
            reduced_motion: context.effective_reduced_motion,
            ascii: context.effective_ascii,
            display_zone: context.effective_display_zone.clone(),
        });
        self.context = Some(context);
        SaveCompletion { appearance, notice }
    }

    /// A failed save. Returns whether the open dialog reported it; when it did
    /// not, the shell puts the failure in the status line instead.
    pub fn fail_save(&mut self, generation: u64, error: &str) -> bool {
        let Some(dialog) = &mut self.state else {
            return false;
        };
        if dialog.generation != generation {
            return false;
        }
        dialog.saving = false;
        dialog.status_kind = SettingsStatus::Error;
        dialog.status = format!("save failed: {error}");
        dialog.details_scroll = 0;
        true
    }

    fn move_focus(&mut self, delta: i32) {
        let Some(dialog) = &mut self.state else {
            return;
        };
        let controls = settings_controls(dialog);
        let index = controls
            .iter()
            .position(|control| *control == dialog.focus)
            .unwrap_or(0);
        dialog.focus = controls[(index as i32 + delta).rem_euclid(controls.len() as i32) as usize];
        if let SettingsControl::Field(field) = dialog.focus {
            dialog.selected = SettingsField::ALL
                .iter()
                .position(|candidate| *candidate == field)
                .unwrap_or(0);
        }
    }

    fn focus_control(&mut self, control: SettingsControl) {
        let Some(dialog) = &mut self.state else {
            return;
        };
        if control == SettingsControl::More && dialog.details_scroll_limit == 0 {
            return;
        }
        dialog.focus = control;
        if let SettingsControl::Field(field) = control {
            dialog.selected = SettingsField::ALL
                .iter()
                .position(|candidate| *candidate == field)
                .unwrap_or(0);
        }
    }

    /// The old `Action::CycleSetting`: open the theme dropdown, or flip the
    /// focused toggle and preview it immediately.
    fn cycle(&mut self, ctx: &mut Ctx<'_>) {
        let Some(dialog) = &mut self.state else {
            return;
        };
        let SettingsControl::Field(field) = dialog.focus else {
            return;
        };
        match field {
            SettingsField::Theme => {
                dialog.choice_selected = ThemeId::ALL
                    .iter()
                    .position(|theme| *theme == dialog.draft.theme)
                    .unwrap_or(0);
                dialog.dropdown = Some(SettingsField::Theme);
                return;
            }
            SettingsField::DisplayZone => {
                dialog.choice_selected = zone_index(&dialog.draft.display_zone);
                dialog.dropdown = Some(SettingsField::DisplayZone);
                return;
            }
            SettingsField::Delight => dialog.draft.delight_enabled = !dialog.draft.delight_enabled,
            SettingsField::ReducedMotion => {
                dialog.draft.reduced_motion = !dialog.draft.reduced_motion
            }
            SettingsField::Ascii => dialog.draft.ascii = !dialog.draft.ascii,
            _ => {}
        }
        mark_settings_pending(dialog);
        *ctx.appearance = Appearance {
            theme_id: dialog.draft.theme,
            delight_enabled: dialog.draft.delight_enabled,
            reduced_motion: dialog.draft.reduced_motion,
            ascii: dialog.draft.ascii,
            display_zone: dialog.draft.display_zone.clone(),
        };
    }

    /// Enter (§8.9): the focused control's own activation where it has one —
    /// the theme dropdown opens, a toggle flips, and `More` is a button whose
    /// press is the focus it already holds on the scrolled pane, so it stays
    /// inert rather than saving. Every text field hands Enter to the
    /// default, `Save`.
    fn activate(&mut self, ctx: &mut Ctx<'_>) {
        match self.state.as_ref().map(|dialog| dialog.focus) {
            Some(SettingsControl::Field(
                SettingsField::Theme
                | SettingsField::DisplayZone
                | SettingsField::Delight
                | SettingsField::ReducedMotion
                | SettingsField::Ascii,
            )) => self.cycle(ctx),
            Some(SettingsControl::More) | None => {}
            Some(SettingsControl::Field(_) | SettingsControl::Save) => self.save(),
        }
    }

    fn move_choice(&mut self, delta: i32) {
        let Some(dialog) = &mut self.state else {
            return;
        };
        let Some(field) = dialog.dropdown else {
            return;
        };
        let count = settings_choices(field).len().max(1) as i32;
        dialog.choice_selected = (dialog.choice_selected as i32 + delta).rem_euclid(count) as usize;
    }

    fn choose(&mut self, index: usize, ctx: &mut Ctx<'_>) {
        let Some(dialog) = &mut self.state else {
            return;
        };
        let Some(field) = dialog.dropdown else {
            return;
        };
        match field {
            SettingsField::Theme => {
                let Some(theme) = ThemeId::ALL.get(index).copied() else {
                    return;
                };
                dialog.draft.theme = theme;
                ctx.appearance.theme_id = theme;
            }
            SettingsField::DisplayZone => {
                let Some((_, token)) = time_zone_choices().get(index) else {
                    return;
                };
                dialog.draft.display_zone = (*token).to_owned();
                // Previewed immediately, like the theme: the log behind the
                // dialog is the only honest preview of a time format.
                ctx.appearance.display_zone = (*token).to_owned();
            }
            _ => return,
        }
        dialog.choice_selected = index;
        dialog.dropdown = None;
        mark_settings_pending(dialog);
    }

    fn scroll_details(&mut self, delta: i32) {
        let Some(dialog) = &mut self.state else {
            return;
        };
        if dialog.focus != SettingsControl::More {
            return;
        }
        dialog.details_scroll = dialog
            .details_scroll
            .saturating_add_signed(delta as isize)
            .min(dialog.details_scroll_limit);
    }

    fn save(&mut self) {
        let Some(dialog) = &mut self.state else {
            return;
        };
        if dialog.saving {
            dialog.status_kind = SettingsStatus::Pending;
            dialog.status = "settings save already pending".into();
        } else if self.outbox.len() >= SETTINGS_QUEUE_LIMIT {
            dialog.status_kind = SettingsStatus::Error;
            dialog.status = "settings save queue is full; retry shortly".into();
        } else {
            dialog.saving = true;
            dialog.status_kind = SettingsStatus::Pending;
            dialog.status = "saving global settings…".into();
            let _ = self.outbox.push(SettingsRequest {
                generation: dialog.generation,
                values: dialog.draft.clone(),
            });
        }
    }

    /// Line editing against the focused text field, using the dialog's own
    /// caret. Non-text controls have no field, so a character aimed at one is
    /// dropped — exactly as `edit_setting` dropped it.
    fn edit_field(&mut self, command: EditCommand<'_>) {
        let Some(dialog) = &mut self.state else {
            return;
        };
        let SettingsControl::Field(field) = dialog.focus else {
            return;
        };
        let Some(slot) = field.text_slot() else {
            return;
        };
        let value = setting_field_mut(dialog, field);
        let mut cursor = self.cursors[slot].unwrap_or(TextCursor {
            char_index: value.chars().count(),
        });
        cursor.char_index = cursor.char_index.min(value.chars().count());
        let outcome = edit(
            value,
            &mut cursor,
            command,
            EditPolicy {
                max_bytes: SETTINGS_FIELD_BYTES,
                multiline: false,
            },
        );
        self.cursors[slot] = Some(cursor);
        if outcome.changed {
            mark_settings_pending(dialog);
        }
    }

    /// The component's keymap: the universal line-editing chords the shell's
    /// `key_to_action` gave `Focus::Settings`, then the dialog's own table.
    fn key(&mut self, key: KeyEvent, ctx: &mut Ctx<'_>) -> Outcome {
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return Outcome::Ignored;
        }
        // The dropdown owns every key while it is open.
        if self
            .state
            .as_ref()
            .is_some_and(|dialog| dialog.dropdown.is_some())
        {
            match key.code {
                KeyCode::Up => self.move_choice(-1),
                KeyCode::Down => self.move_choice(1),
                KeyCode::Enter => {
                    let selected = self.state.as_ref().map_or(0, |d| d.choice_selected);
                    self.choose(selected, ctx);
                }
                _ => {}
            }
            return Outcome::Consumed;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            let command = match key.code {
                KeyCode::Char('a') => Some(EditCommand::StartOfLine),
                KeyCode::Char('e') => Some(EditCommand::EndOfLine),
                KeyCode::Char('k') => Some(EditCommand::KillToEndOfLine),
                _ => None,
            };
            if let Some(command) = command {
                self.edit_field(command);
                return Outcome::Consumed;
            }
        }
        // The effective-values pane takes the arrow keys while it holds focus.
        if self
            .state
            .as_ref()
            .is_some_and(|dialog| dialog.focus == SettingsControl::More)
            && matches!(key.code, KeyCode::Up | KeyCode::Down)
        {
            self.scroll_details(if key.code == KeyCode::Up { -1 } else { 1 });
            return Outcome::Consumed;
        }
        // Left/Right are the caret's; the shell routed them through
        // `Action::TextMoveLeft`/`Right` for whichever field was editing.
        match key.code {
            KeyCode::Left => self.edit_field(EditCommand::MoveLeft),
            KeyCode::Right => self.edit_field(EditCommand::MoveRight),
            KeyCode::Up | KeyCode::BackTab => self.move_focus(-1),
            KeyCode::Down | KeyCode::Tab => self.move_focus(1),
            // §8.4: Space toggles the focused checkbox or opens the focused
            // dropdown; it is not a second Enter, so it never saves.
            KeyCode::Char(' ') => self.cycle(ctx),
            KeyCode::Enter => self.activate(ctx),
            KeyCode::Backspace => self.edit_field(EditCommand::Backspace),
            KeyCode::Char(character) if is_typed_char(&key) => {
                let mut buffer = [0u8; 4];
                self.edit_field(EditCommand::Insert(character.encode_utf8(&mut buffer)));
            }
            _ => return Outcome::Ignored,
        }
        Outcome::Consumed
    }

    fn mouse(
        &mut self,
        kind: MouseEventKind,
        hit: Option<SettingsHit>,
        ctx: &mut Ctx<'_>,
    ) -> Outcome {
        match kind {
            MouseEventKind::Down(MouseButton::Left) => match hit {
                Some(SettingsHit::Choice(index)) => self.choose(index, ctx),
                Some(SettingsHit::Control(control)) => {
                    self.focus_control(control);
                    if matches!(
                        control,
                        SettingsControl::Field(
                            SettingsField::Theme
                                | SettingsField::Delight
                                | SettingsField::ReducedMotion
                                | SettingsField::Ascii
                        ) | SettingsControl::Save
                    ) {
                        self.activate(ctx);
                    }
                }
                None => {}
            },
            MouseEventKind::ScrollUp => self.scroll_details(-1),
            MouseEventKind::ScrollDown => self.scroll_details(1),
            _ => return Outcome::Consumed,
        }
        Outcome::Consumed
    }

    fn record(
        &mut self,
        controls: Vec<(Rect, SettingsControl)>,
        choices: Vec<(Rect, usize)>,
        caret: Option<(u16, u16)>,
        surface: Surface,
    ) -> Surface {
        self.geometry = SettingsGeometry { controls, choices };
        self.surface = Surface { caret, ..surface };
        self.surface
    }

    /// The caret the focused field draws from, or the end of its value the
    /// first time it takes focus.
    fn caret_of(&self, field: SettingsField, value: &str) -> usize {
        field
            .text_slot()
            .and_then(|slot| self.cursors[slot])
            .map_or_else(|| value.chars().count(), |cursor| cursor.char_index)
    }
}

impl Component for SettingsDialog {
    type Hit = SettingsHit;
    type Open = ();

    fn open(&mut self, _params: (), _ctx: &mut Ctx<'_>) {
        let Some(context) = self.context.clone() else {
            return;
        };
        let generation = self.next_generation;
        self.next_generation = generation.saturating_add(1);
        self.state = Some(SettingsDialogState {
            generation,
            selected: 0,
            focus: SettingsControl::Field(SettingsField::Provider),
            draft: context.saved.clone(),
            context,
            saving: false,
            status_kind: SettingsStatus::Saved,
            status: "Saved settings loaded; cache-limit changes apply after restart".into(),
            dropdown: None,
            choice_selected: 0,
            details_scroll: 0,
            details_scroll_limit: 0,
        });
        // A new dialog is a new caret identity, which is what bumping the
        // generation meant for `CursorBank`.
        self.cursors = [None; 7];
        self.geometry = SettingsGeometry::default();
        self.open = true;
    }

    fn handle(&mut self, event: Event<SettingsHit>, ctx: &mut Ctx<'_>) -> Outcome {
        match event {
            Event::Key(key) => self.key(key, ctx),
            Event::Mouse { kind, hit, .. } => self.mouse(kind, hit, ctx),
            // §5.3: the innermost thing closes first, so an open dropdown
            // absorbs the dismissal rather than the whole dialog.
            Event::Dismiss => {
                if let Some(dialog) = &mut self.state
                    && dialog.dropdown.take().is_some()
                {
                    return Outcome::Consumed;
                }
                // An unsaved preview is rolled back to the effective values the
                // layer opened with.
                if let Some(dialog) = self.state.take() {
                    *ctx.appearance = Appearance {
                        theme_id: dialog.context.effective_theme,
                        delight_enabled: dialog.context.effective_delight_enabled,
                        reduced_motion: dialog.context.effective_reduced_motion,
                        ascii: dialog.context.effective_ascii,
                        display_zone: dialog.context.effective_display_zone.clone(),
                    };
                }
                self.open = false;
                Outcome::Close
            }
            Event::Command(_) | Event::Paste(_) | Event::View(_) | Event::Resize => {
                Outcome::Ignored
            }
        }
    }

    fn surface(&self) -> Surface {
        self.surface
    }

    fn hit(&self, point: (u16, u16)) -> Option<SettingsHit> {
        // The anchored dropdown is drawn last and takes the point first.
        self.geometry
            .choices
            .iter()
            .find_map(|(rect, index)| contains(*rect, point).then_some(SettingsHit::Choice(*index)))
            .or_else(|| {
                self.geometry.controls.iter().find_map(|(rect, control)| {
                    contains(*rect, point).then_some(SettingsHit::Control(*control))
                })
            })
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &RenderCtx<'_>) -> Surface {
        use crate::dialog_layout::{DialogClass, DialogContent, content_width};
        use SettingsControl as Control;
        use SettingsField as Field;

        let theme = ctx.theme;
        let ascii = ctx.ascii;
        let styles = DialogStyles::new(theme);
        let mut controls_hit: Vec<(Rect, Control)> = Vec::new();
        let mut choices_hit: Vec<(Rect, usize)> = Vec::new();
        let mut caret_cell: Option<(u16, u16)> = None;
        let Some(dialog) = self.state.clone() else {
            return self.record(controls_hit, choices_hit, None, Surface::default());
        };
        let values = dialog.draft.clone();
        let agent_label = if ascii { "Agent" } else { "🧠" };
        let width = content_width(area, DialogClass::L);

        // §7.4: one message row, one vocabulary, and a sentence that does not
        // repeat the state word.
        let (state, sentence) = match dialog.status_kind {
            SettingsStatus::Saved if dialog.status.contains("restart") => (
                MessageState::Saved,
                "appearance applies now · cache limits apply after restart".to_owned(),
            ),
            SettingsStatus::Saved => (MessageState::Saved, "saved and applied".to_owned()),
            SettingsStatus::Pending if dialog.saving => {
                (MessageState::Pending, "saving settings".to_owned())
            }
            SettingsStatus::Pending => (MessageState::Pending, "changes are not saved".to_owned()),
            // The failure text is the sentence: an error the user cannot read is
            // not a diagnostic, and the pane it used to live in scrolls.
            SettingsStatus::Error => (MessageState::Error, dialog.status.clone()),
        };
        // Wrap the effective values before measuring: a settings path is longer
        // than the pane at every terminal width, and it has to stay readable.
        // Measure at the width the rows are actually rendered at: the body loses a
        // column to the scrollbar once it overflows, and measuring wider than that
        // under-counts the wrapped rows, leaving the last paths unreachable.
        let detail_width = usize::from(
            width
                .saturating_sub(crate::dialog_layout::PANE_INDENT)
                .saturating_sub(1),
        )
        .max(1);
        let details: Vec<String> = settings_detail_lines(&dialog, agent_label)
            .iter()
            .flat_map(|line| wrap_value(&line.to_string(), detail_width))
            .collect();
        let natural_body =
            SETTINGS_FORM_ROWS.saturating_add(u16::try_from(details.len()).unwrap_or(0));

        let save_label = if dialog.saving { "Saving…" } else { "Save" };
        // §3: the anatomy has a help row, and this is the one thing about
        // Settings a user cannot discover from the form itself. There is no
        // timezone database in this build, so a display zone is a fixed offset
        // and daylight saving is not applied — saying so is the difference
        // between a limitation and a bug report.
        let help = "Display zones are fixed UTC offsets: no timezone database, so daylight saving is not applied. Times always show their offset.";
        let content = DialogContent {
            header: 0,
            body: natural_body,
            message: message_rows(&sentence, width),
            help: help_rows(help, width),
            actions: packed_button_rows(width, &[save_label, "More"]),
        };
        let regions =
            dialog_frame_regions(frame, area, DialogClass::L, "Settings", &content, theme);
        let mut surface = Surface {
            popup: regions.popup,
            interior: regions.interior,
            caret: None,
            scrollable: true,
            // Settings' keymap consumes every bare character — the form's text
            // fields take them and its other controls drop them — so `q` is
            // never a dismissal outside the dropdown, which is what the legacy
            // `Focus::Settings` key table did.
            text_focus: dialog.dropdown.is_none(),
        };
        let body = regions.body;
        if body.width == 0 || body.height == 0 {
            return self.record(controls_hit, choices_hit, caret_cell, surface);
        }

        // §8.8: the window follows focus, which is what keeps every field reachable
        // at 54x16 without a paging control or a change of information architecture.
        let visible = body.height;
        let max_offset = natural_body.saturating_sub(visible);
        let focus_row = settings_focus_row(dialog.focus).unwrap_or(0);
        let base_offset = focus_row
            .saturating_sub(visible.saturating_sub(1))
            .min(max_offset);
        // §8.8: the wheel scrolls the body whatever holds focus, and focusing the
        // effective-values pane hands it the arrow keys. Both feed one offset, so
        // the rows below the pane heading are reachable by either device.
        let offset = base_offset
            .saturating_add(u16::try_from(dialog.details_scroll).unwrap_or(u16::MAX))
            .min(max_offset);
        let overflows = natural_body > visible;
        let bar_width = u16::from(overflows);
        let form = Rect::new(
            body.x,
            body.y,
            body.width.saturating_sub(bar_width),
            body.height,
        );
        if overflows {
            render_scrollbar(
                frame,
                Rect::new(body.right().saturating_sub(1), body.y, 1, body.height),
                usize::from(offset),
                usize::from(max_offset),
                theme,
                ascii,
            );
        }

        // Row index -> screen rect, or None when scrolled out of the window.
        let row_rect = |index: u16| -> Option<Rect> {
            (index >= offset && index < offset.saturating_add(visible))
                .then(|| Rect::new(form.x, form.y.saturating_add(index - offset), form.width, 1))
        };

        let label_width = u16::try_from(UnicodeWidthStr::width("Provider / model")).unwrap_or(16);
        let mut dropdown_anchors: Vec<(Field, Rect)> = Vec::new();

        let section = |frame: &mut Frame<'_>, rect: Option<Rect>, text: &str| {
            if let Some(rect) = rect {
                frame.render_widget(
                    Paragraph::new(text.to_owned())
                        .style(styles.label.add_modifier(Modifier::BOLD)),
                    rect,
                );
            }
        };

        section(frame, row_rect(0), &format!("{agent_label} Agent"));
        section(frame, row_rect(5), "Appearance");
        section(frame, row_rect(10), "Cache limits (MiB)");

        for (index, field, label, value) in [
            (1u16, Field::Provider, "Provider / model", &values.provider),
            (2, Field::Mode, "Mode", &values.mode),
            (3, Field::Thinking, "Thinking", &values.thinking),
            (11, Field::RowCache, "Rows", &values.rows_mib),
            (12, Field::Membership, "Membership", &values.membership_mib),
            (
                13,
                Field::DiskTotal,
                "Derived total",
                &values.disk_total_mib,
            ),
            (
                14,
                Field::IndexPerSource,
                "Per source",
                &values.index_per_source_mib,
            ),
        ] {
            let Some(rect) = row_rect(index) else {
                continue;
            };
            let focused = dialog.focus == Control::Field(field);
            let caret = focused.then(|| self.caret_of(field, value));
            let placed = render_labelled_field(
                frame,
                &mut controls_hit,
                rect,
                label_width,
                label,
                value,
                Control::Field(field),
                focused,
                caret,
                theme,
            );
            if focused {
                caret_cell = placed;
            }
        }

        // §8.3: the theme is a dropdown field, drawn in the field column like the
        // text fields rather than as a button with its label inside.
        for (index, field, label, value) in [
            (
                6u16,
                Field::Theme,
                "Theme",
                values.theme.as_str().to_owned(),
            ),
            (
                7,
                Field::DisplayZone,
                "Times shown in",
                time_zone_label(&values.display_zone),
            ),
        ] {
            let Some(rect) = row_rect(index) else {
                continue;
            };
            let control = Control::Field(field);
            let anchor = render_dropdown_field(
                frame,
                &mut controls_hit,
                rect,
                label_width,
                label,
                &value,
                control,
                dialog.focus == control,
                ascii,
                theme,
            );
            dropdown_anchors.push((field, anchor));
        }

        // §8.4: toggles are checkboxes sharing a row, not buttons with state in the
        // label.
        if let Some(rect) = row_rect(8) {
            let mut x = rect.x;
            for (field, label, on) in [
                (Field::Delight, "Delight", values.delight_enabled),
                (
                    Field::ReducedMotion,
                    "Reduced motion",
                    values.reduced_motion,
                ),
                (Field::Ascii, "ASCII", values.ascii),
            ] {
                let text = format!("[{}] {label}", if on { "x" } else { " " });
                let text_width = u16::try_from(UnicodeWidthStr::width(text.as_str())).unwrap_or(0);
                if x.saturating_add(text_width) > rect.right() {
                    break;
                }
                let control = Control::Field(field);
                let cell = Rect::new(x, rect.y, text_width, 1);
                frame.render_widget(
                    Paragraph::new(text).style(if dialog.focus == control {
                        styles.selection.add_modifier(Modifier::BOLD)
                    } else {
                        styles.label
                    }),
                    cell,
                );
                controls_hit.push((cell, control));
                x = x
                    .saturating_add(text_width)
                    .saturating_add(ACTION_GUTTER + 1);
            }
        }

        // §8.7: the effective values are a pane — a bold heading and indented rows,
        // no border, and they scroll with the rest of the body.
        if let Some(rect) = row_rect(SETTINGS_FORM_ROWS.saturating_sub(1)) {
            frame.render_widget(
                Paragraph::new("Effective values and paths")
                    .style(styles.label.add_modifier(Modifier::BOLD)),
                rect,
            );
        }
        for (index, line) in details.iter().enumerate() {
            let row = SETTINGS_FORM_ROWS.saturating_add(u16::try_from(index).unwrap_or(0));
            let Some(rect) = row_rect(row) else { continue };
            let indent = crate::dialog_layout::PANE_INDENT.min(rect.width);
            frame.render_widget(
                Paragraph::new(line.clone()).style(styles.description),
                Rect::new(
                    rect.x.saturating_add(indent),
                    rect.y,
                    rect.width.saturating_sub(indent),
                    1,
                ),
            );
        }

        // `More` no longer pages the form; it exists only while the body genuinely
        // overflows, and it moves focus into the scrolled region.
        if let Some(state) = &mut self.state {
            state.details_scroll_limit = usize::from(max_offset);
            state.details_scroll = state.details_scroll.min(state.details_scroll_limit);
            if !overflows && state.focus == Control::More {
                state.focus = Control::Save;
            }
        }
        let mut controls = vec![(Control::Save, save_label)];
        if overflows {
            controls.push((Control::More, "More"));
        }
        let labels: Vec<&str> = controls.iter().map(|(_, label)| *label).collect();
        let focused = controls
            .iter()
            .position(|(control, _)| *control == dialog.focus);
        for (index, rect) in render_action_row(frame, regions.actions, &labels, focused, &[], theme)
        {
            controls_hit.push((rect, controls[index].0));
        }

        render_message(frame, regions.message, state, &sentence, theme, ascii);
        render_help_text(frame, regions.help, help, theme);

        if let Some(field) = dialog.dropdown
            && let Some(anchor) = dropdown_anchors
                .iter()
                .find(|(candidate, _)| *candidate == field)
                .map(|(_, rect)| *rect)
            && anchor.width > 0
        {
            let box_area = render_settings_dropdown(
                frame,
                &mut choices_hit,
                regions.popup,
                anchor,
                &settings_choices(field),
                dialog.choice_selected,
                theme,
            );
            // §5.2 containment is measured against everything the layer drew.
            surface.popup = surface.popup.union(box_area);
        }

        self.record(controls_hit, choices_hit, caret_cell, surface)
    }
}

fn contains(area: Rect, point: (u16, u16)) -> bool {
    point.0 >= area.x && point.0 < area.right() && point.1 >= area.y && point.1 < area.bottom()
}

fn mark_settings_pending(dialog: &mut SettingsDialogState) {
    dialog.status_kind = SettingsStatus::Pending;
    dialog.status = "Changes are not saved".into();
}

fn settings_controls(dialog: &SettingsDialogState) -> Vec<SettingsControl> {
    let mut controls = SettingsField::ALL
        .iter()
        .copied()
        .map(SettingsControl::Field)
        .collect::<Vec<_>>();
    controls.push(SettingsControl::Save);
    if dialog.details_scroll_limit > 0 {
        controls.push(SettingsControl::More);
    }
    controls
}

fn setting_field_mut(dialog: &mut SettingsDialogState, field: SettingsField) -> &mut String {
    match field {
        SettingsField::Provider => &mut dialog.draft.provider,
        SettingsField::Mode => &mut dialog.draft.mode,
        SettingsField::Thinking => &mut dialog.draft.thinking,
        SettingsField::RowCache => &mut dialog.draft.rows_mib,
        SettingsField::Membership => &mut dialog.draft.membership_mib,
        SettingsField::DiskTotal => &mut dialog.draft.disk_total_mib,
        SettingsField::IndexPerSource => &mut dialog.draft.index_per_source_mib,
        SettingsField::Theme
        | SettingsField::DisplayZone
        | SettingsField::Delight
        | SettingsField::ReducedMotion
        | SettingsField::Ascii => unreachable!("only editable fields have a text slot"),
    }
}

pub(crate) fn settings_restart_status(context: &SettingsContext) -> String {
    let saved = &context.saved;
    let changed = saved.rows_mib.parse::<u64>().ok() != Some(context.applied_rows_mib)
        || saved.membership_mib.parse::<u64>().ok() != Some(context.applied_membership_mib)
        || saved.disk_total_mib.parse::<u64>().ok() != Some(context.applied_disk_total_mib)
        || saved.index_per_source_mib.parse::<u64>().ok()
            != Some(context.applied_index_per_source_mib);
    if changed {
        "saved; cache limits require restart (no raw data was evicted). Other active lvu processes with a different global disk cap can refuse index growth until all restart".into()
    } else {
        "saved and applied; existing investigations retain their agent session model".into()
    }
}

fn settings_focus_row(focus: SettingsControl) -> Option<u16> {
    use SettingsControl as Control;
    use SettingsField as Field;
    Some(match focus {
        Control::Field(Field::Provider) => 1,
        Control::Field(Field::Mode) => 2,
        Control::Field(Field::Thinking) => 3,
        Control::Field(Field::Theme) => 6,
        Control::Field(Field::DisplayZone) => 7,
        Control::Field(Field::Delight)
        | Control::Field(Field::ReducedMotion)
        | Control::Field(Field::Ascii) => 8,
        Control::Field(Field::RowCache) => 11,
        Control::Field(Field::Membership) => 12,
        Control::Field(Field::DiskTotal) => 13,
        Control::Field(Field::IndexPerSource) => 14,
        Control::More => SETTINGS_FORM_ROWS.saturating_sub(1),
        Control::Save => return None,
    })
}

/// §4.2: label column, then the field column at a fixed x. The painted input
/// rect is exactly the field. Returns the caret cell when it drew one.
#[allow(clippy::too_many_arguments)]
fn render_labelled_field(
    frame: &mut Frame<'_>,
    controls: &mut Vec<(Rect, SettingsControl)>,
    row: Rect,
    label_width: u16,
    label: &str,
    value: &str,
    control: SettingsControl,
    focused: bool,
    cursor: Option<usize>,
    theme: Theme,
) -> Option<(u16, u16)> {
    let styles = DialogStyles::new(theme);
    frame.render_widget(
        Paragraph::new(label.to_owned()).style(if focused {
            styles.shortcut
        } else {
            styles.label
        }),
        Rect::new(row.x, row.y, label_width.min(row.width), 1),
    );
    let field_x = row
        .x
        .saturating_add(label_width)
        .saturating_add(FIELD_GUTTER);
    if field_x >= row.right() {
        return None;
    }
    let field = Rect::new(field_x, row.y, row.right().saturating_sub(field_x), 1);
    controls.push((field, control));
    if focused {
        place_input_cursor_at(
            frame,
            field,
            0,
            0,
            value,
            cursor.unwrap_or_else(|| value.chars().count()),
            theme,
        )
    } else {
        InputSurface {
            style: styles.input,
        }
        .render(field, frame.buffer_mut());
        // §11: an unfocused value is identified by its head, so it truncates at
        // the end. Showing the tail rendered `fixture/provider` as
        // `xture/provider`.
        frame.render_widget(
            Paragraph::new(truncated(value, usize::from(field.width))).style(styles.input),
            field,
        );
        None
    }
}

/// §8.3: a dropdown is a field with a chevron in its last cell. Returns the
/// field rect so the popup can anchor to it.
#[allow(clippy::too_many_arguments)]
fn render_dropdown_field(
    frame: &mut Frame<'_>,
    controls: &mut Vec<(Rect, SettingsControl)>,
    row: Rect,
    label_width: u16,
    label: &str,
    value: &str,
    control: SettingsControl,
    focused: bool,
    ascii: bool,
    theme: Theme,
) -> Rect {
    let styles = DialogStyles::new(theme);
    frame.render_widget(
        Paragraph::new(label.to_owned()).style(if focused {
            styles.shortcut
        } else {
            styles.label
        }),
        Rect::new(row.x, row.y, label_width.min(row.width), 1),
    );
    let field_x = row
        .x
        .saturating_add(label_width)
        .saturating_add(FIELD_GUTTER);
    if field_x >= row.right() {
        return Rect::default();
    }
    let longest = ThemeId::ALL
        .iter()
        .map(|id| UnicodeWidthStr::width(id.as_str()))
        .max()
        .unwrap_or(12);
    let field_width = u16::try_from(longest + 4)
        .unwrap_or(16)
        .max(12)
        .min(row.right().saturating_sub(field_x));
    let field = Rect::new(field_x, row.y, field_width, 1);
    controls.push((field, control));
    InputSurface {
        style: if focused {
            styles.selection
        } else {
            styles.input
        },
    }
    .render(field, frame.buffer_mut());
    frame.render_widget(
        Paragraph::new(truncated(value, usize::from(field.width.saturating_sub(2)))).style(
            if focused {
                styles.selection
            } else {
                styles.input
            },
        ),
        field,
    );
    frame.render_widget(
        Paragraph::new(if ascii { "v" } else { "▾" }).style(Style::default().fg(theme.accent).bg(
            if focused {
                theme.selection_bg
            } else {
                theme.input_bg
            },
        )),
        Rect::new(field.right().saturating_sub(1), field.y, 1, 1),
    );
    field
}

fn settings_detail_lines(dialog: &SettingsDialogState, agent_label: &str) -> Vec<Line<'static>> {
    vec![
        Line::raw(format!("State detail: {}", dialog.status)),
        Line::raw(format!(
            "Effective {agent_label}: {} [{}] · {} [{}] · {} [{}]",
            dialog.context.effective_provider,
            dialog.context.provider_source,
            dialog.context.effective_mode,
            dialog.context.mode_source,
            dialog.context.effective_thinking,
            dialog.context.thinking_source
        )),
        Line::raw(format!(
            "Effective appearance: theme {} · delight {} [{}] · motion {} [{}] · ASCII {} [{}]",
            dialog.context.effective_theme.as_str(),
            dialog.context.effective_delight_enabled,
            dialog.context.delight_source,
            dialog.context.effective_reduced_motion,
            dialog.context.reduced_motion_source,
            dialog.context.effective_ascii,
            dialog.context.ascii_source
        )),
        Line::raw(format!("Settings: {}", dialog.context.settings_path)),
        Line::raw(format!("Data: {}", dialog.context.data_path)),
        Line::raw(format!("Cache: {}", dialog.context.cache_path)),
        Line::raw(format!("Capture: {}", dialog.context.capture_path)),
        Line::raw(format!(
            "Times shown in: {} ({})",
            time_zone_label(&dialog.context.effective_display_zone),
            dialog.context.display_zone_source
        )),
        Line::raw(
            "Cache-limit changes take effect after restart; appearance previews immediately.",
        ),
    ]
}

/// One anchored choice list. Shared by the theme and the display zone, so the
/// two cannot drift apart in geometry or in behaviour.
fn render_settings_dropdown(
    frame: &mut Frame<'_>,
    choices: &mut Vec<(Rect, usize)>,
    popup: Rect,
    anchor: Rect,
    labels: &[String],
    selected: usize,
    theme: Theme,
) -> Rect {
    let styles = DialogStyles::new(theme);
    let width = labels
        .iter()
        .map(|value| UnicodeWidthStr::width(value.as_str()))
        .max()
        .unwrap_or(1) as u16
        + 2;
    let height = labels
        .len()
        .min(usize::from(popup.height.saturating_sub(4))) as u16
        + 2;
    let x = anchor
        .x
        .min(popup.right().saturating_sub(width).saturating_sub(1));
    let y = anchor
        .bottom()
        .min(popup.bottom().saturating_sub(height).saturating_sub(1));
    let area = Rect::new(x, y, width.min(popup.width.saturating_sub(2)), height);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_style(styles.label),
        area,
    );
    let choice_height = usize::from(area.height.saturating_sub(2));
    let selected = selected.min(labels.len().saturating_sub(1));
    let choice_scroll = selected.saturating_add(1).saturating_sub(choice_height);
    for (offset, (index, value)) in labels
        .iter()
        .enumerate()
        .skip(choice_scroll)
        .take(choice_height)
        .enumerate()
    {
        let rect = Rect::new(
            area.x + 1,
            area.y + 1 + offset as u16,
            area.width.saturating_sub(2),
            1,
        );
        choices.push((rect, index));
        frame.render_widget(
            Paragraph::new(value.clone()).style(if index == selected {
                styles.selection
            } else {
                button_style(theme, false, false)
            }),
            rect,
        );
    }
    area
}

/// Hard-wrap on display width. `wrap_sentence` truncates a token that is wider
/// than the line, which would silently shorten a path; §9 only allows that for
/// list cells, never for a value the user has to read.
fn wrap_value(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return Vec::new();
    }
    let mut rows = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        let head = clipped_width(rest, width);
        if head.is_empty() {
            break;
        }
        rest = &rest[head.len()..];
        rows.push(head);
    }
    if rows.is_empty() {
        rows.push(String::new());
    }
    rows
}
