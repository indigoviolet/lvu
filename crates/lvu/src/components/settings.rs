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
    widgets::{Paragraph, Widget},
};
use unicode_width::UnicodeWidthStr;

use crate::app::{
    SettingsContext, SettingsRequest, SettingsValues, time_zone_choices, time_zone_label,
    validate_display_zone,
};
use crate::component::{
    Appearance, Component, Ctx, Event, Outbox, Outcome, RenderCtx, Surface, is_typed_char,
};
use crate::dialog_controls::{
    ActionRow, DialogStyles, button_style, render_role_button, stable_action_rows,
};
use crate::dialog_layout::{
    AnchoredSpec, DialogSpec, PresentationKind, ScrollViewport, anchored_geometry,
};
use crate::text_edit::{EditCommand, EditPolicy, TextCursor, edit};
use crate::theme::{Theme, ThemeId};
use crate::ui::{
    ACTION_GUTTER, FIELD_GUTTER, InputSurface, MessageState, clipped_width, place_input_cursor_at,
    render_help_text, render_message, render_responsive_frame, render_scrollbar, truncated,
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
    /// The IANA zone or fixed UTC offset the log viewport shows times in.
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

    /// The caret slot for an editable field. A custom display zone is text;
    /// preset display zones use the same field as a dropdown.
    fn text_slot(self) -> Option<usize> {
        Some(match self {
            Self::Provider => 0,
            Self::Mode => 1,
            Self::Thinking => 2,
            Self::DisplayZone => 3,
            Self::RowCache => 4,
            Self::Membership => 5,
            Self::DiskTotal => 6,
            Self::IndexPerSource => 7,
            Self::Theme | Self::Delight | Self::ReducedMotion | Self::Ascii => {
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
            .chain(std::iter::once("Custom IANA zone…".to_owned()))
            .collect(),
        _ => Vec::new(),
    }
}

/// Where a stored zone token sits in the offered list. Valid names use the
/// custom editor row; invalid persisted values are rejected before this layer
/// is configured.
fn zone_index(token: &str) -> usize {
    time_zone_choices()
        .iter()
        .position(|(_, value)| *value == token)
        .unwrap_or(time_zone_choices().len())
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
    /// Named zones are edited as text after choosing the custom row. Presets
    /// remain the compact dropdown behavior existing settings users know.
    pub zone_custom: bool,
    /// Preview authority kept apart from the editable token. A stale save
    /// acknowledgement may rebuild the other preview fields from a newer
    /// draft, but an invalid zone draft must never reach `Appearance`.
    pub last_valid_display_zone: String,
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
    cursors: [Option<TextCursor>; 8],
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
            cursors: [None; 8],
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
                dialog.last_valid_display_zone = context.effective_display_zone.clone();
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
                    display_zone: dialog.last_valid_display_zone.clone(),
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
            Some(SettingsControl::Field(SettingsField::DisplayZone))
                if self.state.as_ref().is_some_and(|dialog| dialog.zone_custom) =>
            {
                self.save()
            }
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
                if let Some((_, token)) = time_zone_choices().get(index) {
                    dialog.zone_custom = false;
                    dialog.draft.display_zone = (*token).to_owned();
                    // Previewed immediately, like the theme: the log behind the
                    // dialog is the only honest preview of a time format.
                    ctx.appearance.display_zone = (*token).to_owned();
                    dialog.last_valid_display_zone = (*token).to_owned();
                } else if index == time_zone_choices().len() {
                    dialog.zone_custom = true;
                    if time_zone_choices()
                        .iter()
                        .any(|(_, token)| *token == dialog.draft.display_zone)
                    {
                        dialog.draft.display_zone.clear();
                    }
                    self.cursors[SettingsField::DisplayZone.text_slot().unwrap()] =
                        Some(TextCursor {
                            char_index: dialog.draft.display_zone.chars().count(),
                        });
                } else {
                    return;
                }
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
        if let Err(error) = validate_display_zone(&dialog.draft.display_zone) {
            dialog.status_kind = SettingsStatus::Error;
            dialog.status = error;
            dialog.details_scroll = 0;
        } else if dialog.saving {
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
    fn edit_field(&mut self, command: EditCommand<'_>, ctx: &mut Ctx<'_>) {
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
            if field == SettingsField::DisplayZone {
                match validate_display_zone(&dialog.draft.display_zone) {
                    Ok(()) => {
                        ctx.appearance.display_zone = dialog.draft.display_zone.clone();
                        dialog.last_valid_display_zone = dialog.draft.display_zone.clone();
                        mark_settings_pending(dialog);
                    }
                    Err(error) => {
                        // An invalid draft is editable, but it cannot replace
                        // the last valid preview behind the dialog.
                        dialog.status_kind = SettingsStatus::Error;
                        dialog.status = error;
                        dialog.details_scroll = 0;
                    }
                }
            } else {
                mark_settings_pending(dialog);
            }
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
                self.edit_field(command, ctx);
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
            KeyCode::Left => self.edit_field(EditCommand::MoveLeft, ctx),
            KeyCode::Right => self.edit_field(EditCommand::MoveRight, ctx),
            KeyCode::Up | KeyCode::BackTab => self.move_focus(-1),
            KeyCode::Down | KeyCode::Tab => self.move_focus(1),
            // §8.4: Space toggles the focused checkbox or opens the focused
            // dropdown; it is not a second Enter, so it never saves.
            KeyCode::Char(' ') => self.cycle(ctx),
            KeyCode::Enter => self.activate(ctx),
            KeyCode::Backspace => self.edit_field(EditCommand::Backspace, ctx),
            KeyCode::Char(character) if is_typed_char(&key) => {
                let mut buffer = [0u8; 4];
                self.edit_field(EditCommand::Insert(character.encode_utf8(&mut buffer)), ctx);
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
                    let activates = matches!(
                        control,
                        SettingsControl::Field(
                            SettingsField::Theme
                                | SettingsField::Delight
                                | SettingsField::ReducedMotion
                                | SettingsField::Ascii
                        ) | SettingsControl::Save
                    ) || (control
                        == SettingsControl::Field(SettingsField::DisplayZone)
                        && self
                            .state
                            .as_ref()
                            .is_some_and(|dialog| !dialog.zone_custom));
                    if activates {
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

/// Stable LongContent budgets: outer size is policy-only, never draft
/// counts. Header 0, body minimum 3, message/help 2 stable maxima, actions
/// from the stable width budget so saving/saved/error frames share one
/// frame and sticky tail. Hand-rolled here is presentation-only folding.
fn settings_spec_for(area: Rect, save_label: &str, with_more: bool) -> DialogSpec {
    let mut labels = vec![save_label];
    if with_more {
        labels.push("More");
    }
    let (policy_w, _) = crate::dialog_layout::policy_size(area, PresentationKind::LongContent);
    let estimate = policy_w.saturating_sub(4).max(1);
    let action_rows = stable_action_rows(estimate, &labels).clamp(1, 2);
    DialogSpec::new(PresentationKind::LongContent, 0, 3, 2, 2, action_rows)
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
        let zone_custom = time_zone_choices()
            .iter()
            .all(|(_, token)| *token != context.saved.display_zone);
        let last_valid_display_zone = context.effective_display_zone.clone();
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
            zone_custom,
            last_valid_display_zone,
            details_scroll: 0,
            details_scroll_limit: 0,
        });
        // A new dialog is a new caret identity, which is what bumping the
        // generation meant for `CursorBank`.
        self.cursors = [None; 8];
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
        let (policy_w, _) = crate::dialog_layout::policy_size(area, PresentationKind::LongContent);
        let width = policy_w.saturating_sub(4).max(1);

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
        // Pure form rows stay content height with roomy spacing (blank rows at
        // 4/9/15) and compact zero gaps (blanks removed); narrow content
        // stacks labels above fields (label row + field row, toggles one per
        // row). All fields stay reachable via the shared body window.
        let roomy_estimate = !crate::dialog_layout::is_compact(area);
        let stacked_estimate = width < 16 + FIELD_GUTTER + 20;
        let form_count: u16 = if stacked_estimate {
            25
        } else if roomy_estimate {
            SETTINGS_FORM_ROWS
        } else {
            SETTINGS_FORM_ROWS - 3
        };
        let natural_body = form_count.saturating_add(u16::try_from(details.len()).unwrap_or(0));

        let save_label = if dialog.saving { "Saving…" } else { "Save" };
        // §3: the anatomy has a help row, and this is the one thing about
        // Settings a user cannot discover from the form itself.
        let help = "Use an IANA zone such as Europe/Berlin for per-instant daylight saving, or a fixed UTC offset. Display only: captured/event instants never change. Times always show their offset.";
        // Two-pass for stable More: estimate overflow with max labels, then
        // resolve with actual labels. Frame is policy-only so both share it.
        let natural_usize = usize::from(natural_body);
        let spec_max = settings_spec_for(area, save_label, true);
        let viewport_estimate = crate::dialog_layout::resolve_dialog(
            area,
            &spec_max,
            natural_usize,
            &[save_label, "More"],
            Some(0),
            None,
        )
        .map(|g| g.body.viewport)
        .unwrap_or_default();
        let overflows_estimate = natural_usize > usize::from(viewport_estimate.height);
        let with_more = overflows_estimate;
        let actual_labels: Vec<&str> = if with_more {
            vec![save_label, "More"]
        } else {
            vec![save_label]
        };
        let spec = settings_spec_for(area, save_label, with_more);
        let Ok(resolved) = crate::dialog_layout::resolve_dialog(
            area,
            &spec,
            natural_usize,
            &actual_labels,
            Some(0),
            None,
        ) else {
            return self.record(
                controls_hit,
                choices_hit,
                None,
                Surface {
                    popup: Rect::default(),
                    interior: Rect::default(),
                    caret: None,
                    scrollable: false,
                    text_focus: dialog.dropdown.is_none(),
                },
            );
        };
        render_responsive_frame(frame, &resolved, "Settings", ctx.active, theme);
        let mut surface = Surface {
            popup: resolved.frame,
            interior: resolved.interior,
            caret: None,
            scrollable: false,
            // Settings' keymap consumes every bare character — the form's text
            // fields take them and its other controls drop them — so `q` is
            // never a dismissal outside the dropdown, which is what the legacy
            // `Focus::Settings` key table did.
            text_focus: dialog.dropdown.is_none(),
        };
        let body = resolved.body.viewport;
        if body.width == 0 || body.height == 0 {
            return self.record(controls_hit, choices_hit, caret_cell, surface);
        }
        let message_rect = resolved.message;
        let help_rect = resolved.help;
        let action_geom = resolved.actions.clone();
        let roomy = !resolved.compact;

        // §8.8: the shared body window follows focus, which keeps every field
        // reachable without a paging control. Roomy keeps blank gaps at 4/9/15;
        // compact removes them. The wheel scrolls the body whatever holds
        // focus, and focusing the effective-values pane hands it the arrows.
        // Hand-rolled index mapping here is presentation-only folding.
        let _compact_body = !roomy;
        let map_row = |roomy_index: u16| -> u16 {
            if roomy {
                roomy_index
            } else {
                roomy_index
                    .saturating_sub(u16::from(roomy_index > 4))
                    .saturating_sub(u16::from(roomy_index > 9))
                    .saturating_sub(u16::from(roomy_index > 15))
            }
        };
        let visible = body.height;
        let max_offset = natural_body.saturating_sub(visible);
        let focus_roomy = settings_focus_row(dialog.focus).unwrap_or(0);
        // Stacked floor (20x6, content <38): field rows, not label rows, own
        // focus so the caret stays painted; labels sit immediately above.
        let stacked = resolved.content.width < 16 + FIELD_GUTTER + 20;
        let focus_row = if stacked {
            use SettingsControl as C2;
            use SettingsField as F2;
            match dialog.focus {
                C2::Field(F2::Provider) => 2,
                C2::Field(F2::Mode) => 4,
                C2::Field(F2::Thinking) => 6,
                C2::Field(F2::Theme) => 9,
                C2::Field(F2::DisplayZone) => 11,
                C2::Field(F2::Delight) => 12,
                C2::Field(F2::ReducedMotion) => 13,
                C2::Field(F2::Ascii) => 14,
                C2::Field(F2::RowCache) => 17,
                C2::Field(F2::Membership) => 19,
                C2::Field(F2::DiskTotal) => 21,
                C2::Field(F2::IndexPerSource) => 23,
                C2::More => 24,
                C2::Save => 0,
            }
        } else {
            map_row(focus_roomy)
        };
        let base_offset = focus_row
            .saturating_sub(visible.saturating_sub(1))
            .min(max_offset);
        let offset = base_offset
            .saturating_add(u16::try_from(dialog.details_scroll).unwrap_or(u16::MAX))
            .min(max_offset);
        // One authoritative body viewport: same rects drive paint, cursor,
        // selection, scrollbar and mouse; no independent projection.
        let body_scroll = ScrollViewport::new(body, natural_usize, usize::from(offset));
        let overflows = body_scroll.overflow() > 0;
        if overflows {
            surface.scrollable = true;
        }
        let form = Rect::new(
            body.x,
            body.y,
            body.width
                .saturating_sub(u16::from(body_scroll.scrollbar.is_some())),
            body.height,
        );
        if let Some(bar) = body_scroll.scrollbar {
            render_scrollbar(
                frame,
                bar,
                body_scroll.first_row,
                body_scroll.overflow(),
                theme,
                ascii,
            );
        }
        // Row index (roomy numbering) -> screen rect via the shared window,
        // or None when scrolled out. Compact gaps collapse to zero.
        let row_rect =
            |index: u16| -> Option<Rect> { body_scroll.project_row(usize::from(map_row(index))) };
        let _ = form;

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

        // Stacked floor: labels above fields (label row bold, then full-width
        // field row, no blank between); toggles one per row. Same shared body
        // window drives paint/cursor/hitboxes; focus reveals the field row.
        // Hand-rolled here is presentation-only folding.
        if stacked {
            let srect = |idx: usize| -> Option<Rect> { body_scroll.project_row(idx) };
            let paint_label = |frame: &mut Frame<'_>, idx: usize, text: &str, focused: bool| {
                if let Some(rect) = srect(idx) {
                    frame.render_widget(
                        Paragraph::new(text.to_owned()).style(if focused {
                            styles.shortcut
                        } else {
                            styles.label.add_modifier(Modifier::BOLD)
                        }),
                        rect,
                    );
                }
            };
            // Sections.
            paint_label(frame, 0, &format!("{agent_label} Agent"), false);
            paint_label(frame, 7, "Appearance", false);
            paint_label(frame, 15, "Cache limits (MiB)", false);
            paint_label(frame, 24, "Effective values and paths", false);
            // Text fields: (label_idx, field_idx, field, label, value).
            for (li, fi, field, label, value) in [
                (
                    1usize,
                    2usize,
                    Field::Provider,
                    "Provider / model",
                    values.provider.as_str(),
                ),
                (3, 4, Field::Mode, "Mode", values.mode.as_str()),
                (5, 6, Field::Thinking, "Thinking", values.thinking.as_str()),
                (16, 17, Field::RowCache, "Rows", values.rows_mib.as_str()),
                (
                    18,
                    19,
                    Field::Membership,
                    "Membership",
                    values.membership_mib.as_str(),
                ),
                (
                    20,
                    21,
                    Field::DiskTotal,
                    "Derived total",
                    values.disk_total_mib.as_str(),
                ),
                (
                    22,
                    23,
                    Field::IndexPerSource,
                    "Per source",
                    values.index_per_source_mib.as_str(),
                ),
            ] {
                let focused = dialog.focus == Control::Field(field);
                paint_label(frame, li, label, focused);
                if let Some(rect) = srect(fi) {
                    let caret = focused.then(|| self.caret_of(field, value));
                    let placed = place_input_cursor_at(
                        frame,
                        rect,
                        0,
                        0,
                        value,
                        caret.unwrap_or_else(|| value.chars().count()),
                        theme,
                    );
                    // place_input_cursor_at paints input tone + caret when
                    // focused; when unfocused it still paints the tone with no
                    // caret. Record the full-width field for hit-testing.
                    controls_hit.push((rect, Control::Field(field)));
                    if focused {
                        caret_cell = placed;
                    }
                }
            }
            // Dropdowns: label above, full-width value + chevron below.
            // Theme (preset, always dropdown).
            {
                let focused = dialog.focus == Control::Field(Field::Theme);
                paint_label(frame, 8, "Theme", focused);
                if let Some(rect) = srect(9) {
                    controls_hit.push((rect, Control::Field(Field::Theme)));
                    InputSurface {
                        style: if focused {
                            styles.selection
                        } else {
                            styles.input
                        },
                    }
                    .render(rect, frame.buffer_mut());
                    frame.render_widget(
                        Paragraph::new(truncated(
                            values.theme.as_str(),
                            usize::from(rect.width.saturating_sub(1)),
                        ))
                        .style(if focused {
                            styles.selection
                        } else {
                            styles.input
                        }),
                        Rect::new(rect.x, rect.y, rect.width.saturating_sub(1).max(1), 1),
                    );
                    frame.render_widget(
                        Paragraph::new(if ascii { "v" } else { "▾" }).style(
                            Style::default().fg(theme.accent).bg(if focused {
                                theme.selection_bg
                            } else {
                                theme.input_bg
                            }),
                        ),
                        Rect::new(rect.right().saturating_sub(1), rect.y, 1, 1),
                    );
                    dropdown_anchors.push((Field::Theme, rect));
                    if focused {
                        // Caret for dropdown fields: end of value (dropdown
                        // opens on Enter/Space; arrows move choices when open).
                        caret_cell = Some((rect.x.min(rect.right().saturating_sub(1)), rect.y));
                    }
                }
            }
            // DisplayZone: preset dropdown or custom text, same stacked rows.
            {
                let focused = dialog.focus == Control::Field(Field::DisplayZone);
                paint_label(frame, 10, "Times shown in", focused);
                if let Some(rect) = srect(11) {
                    if dialog.zone_custom {
                        let caret = focused
                            .then(|| self.caret_of(Field::DisplayZone, &values.display_zone));
                        let placed = place_input_cursor_at(
                            frame,
                            rect,
                            0,
                            0,
                            &values.display_zone,
                            caret.unwrap_or_else(|| values.display_zone.chars().count()),
                            theme,
                        );
                        controls_hit.push((rect, Control::Field(Field::DisplayZone)));
                        if focused {
                            caret_cell = placed;
                        }
                    } else {
                        controls_hit.push((rect, Control::Field(Field::DisplayZone)));
                        InputSurface {
                            style: if focused {
                                styles.selection
                            } else {
                                styles.input
                            },
                        }
                        .render(rect, frame.buffer_mut());
                        frame.render_widget(
                            Paragraph::new(truncated(
                                &time_zone_label(&values.display_zone),
                                usize::from(rect.width.saturating_sub(1)),
                            ))
                            .style(if focused {
                                styles.selection
                            } else {
                                styles.input
                            }),
                            Rect::new(rect.x, rect.y, rect.width.saturating_sub(1).max(1), 1),
                        );
                        frame.render_widget(
                            Paragraph::new(if ascii { "v" } else { "▾" }).style(
                                Style::default().fg(theme.accent).bg(if focused {
                                    theme.selection_bg
                                } else {
                                    theme.input_bg
                                }),
                            ),
                            Rect::new(rect.right().saturating_sub(1), rect.y, 1, 1),
                        );
                        dropdown_anchors.push((Field::DisplayZone, rect));
                    }
                }
            }
            // Toggles, one per row when stacked.
            for (idx, field, label, on) in [
                (12usize, Field::Delight, "Delight", values.delight_enabled),
                (
                    13,
                    Field::ReducedMotion,
                    "Reduced motion",
                    values.reduced_motion,
                ),
                (14, Field::Ascii, "ASCII", values.ascii),
            ] {
                let Some(rect) = srect(idx) else {
                    continue;
                };
                let text = format!("[{}] {label}", if on { "x" } else { " " });
                frame.render_widget(
                    Paragraph::new(text).style(if dialog.focus == Control::Field(field) {
                        styles.selection.add_modifier(Modifier::BOLD)
                    } else {
                        styles.label
                    }),
                    rect,
                );
                controls_hit.push((rect, Control::Field(field)));
            }
            // Effective details, indented, via the same shared window.
            for (offset, line) in details.iter().enumerate() {
                let Some(rect) = srect(25usize.saturating_add(offset)) else {
                    continue;
                };
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
        } else {
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

            // §8.3: presets are dropdown fields. A named zone uses the same row as
            // an editable field after the user chooses Custom IANA zone.
            for (index, field, label, value) in [(
                6u16,
                Field::Theme,
                "Theme",
                values.theme.as_str().to_owned(),
            )]
            .into_iter()
            .chain((!dialog.zone_custom).then(|| {
                (
                    7,
                    Field::DisplayZone,
                    "Times shown in",
                    time_zone_label(&values.display_zone),
                )
            })) {
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

            if dialog.zone_custom
                && let Some(rect) = row_rect(7)
            {
                let field = Field::DisplayZone;
                let control = Control::Field(field);
                let focused = dialog.focus == control;
                let caret = focused.then(|| self.caret_of(field, &values.display_zone));
                let placed = render_labelled_field(
                    frame,
                    &mut controls_hit,
                    rect,
                    label_width,
                    "Times shown in",
                    &values.display_zone,
                    control,
                    focused,
                    caret,
                    theme,
                );
                if focused {
                    caret_cell = placed;
                }
                if let Some((anchor, _)) = controls_hit
                    .iter()
                    .rev()
                    .find(|(_, candidate)| *candidate == control)
                {
                    dropdown_anchors.push((field, *anchor));
                }
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
                    let text_width =
                        u16::try_from(UnicodeWidthStr::width(text.as_str())).unwrap_or(0);
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
        } // end side-by-side body; stacked painted above

        // `More` no longer pages the form; it exists only while the body genuinely
        // overflows, and it moves focus into the scrolled region.
        let max_offset_usize = natural_usize.saturating_sub(usize::from(body.height));
        let _max_offset = u16::try_from(max_offset_usize).unwrap_or(u16::MAX);
        if let Some(state) = &mut self.state {
            state.details_scroll_limit = max_offset_usize;
            state.details_scroll = state.details_scroll.min(state.details_scroll_limit);
            if !overflows && state.focus == Control::More {
                state.focus = Control::Save;
            }
        }
        // One authoritative action plan from the shared geometry: sticky band,
        // filled Save default, same rects for paint/mouse. No independent rows.
        let mut controls = vec![(Control::Save, save_label)];
        if overflows {
            controls.push((Control::More, "More"));
        }
        let labels: Vec<&str> = controls.iter().map(|(_, label)| *label).collect();
        let focused = controls
            .iter()
            .position(|(control, _)| *control == dialog.focus);
        let action_row = ActionRow {
            labels: &labels,
            default: Some(0),
            destructive: &[],
            focused,
        };
        // Reconcile shared band with actual labels: when the estimate showed
        // overflow but the final band is narrower, More may hide; the focus
        // reset above keeps Save reachable. Paint exactly the shared band.
        for (orig, rect) in action_geom.buttons.iter().copied() {
            let Some((control, label)) = controls.get(orig) else {
                continue;
            };
            let role = action_row.role(orig);
            let is_focused = focused == Some(orig);
            render_role_button(frame, rect, label, role, is_focused, theme);
            controls_hit.push((rect, *control));
        }

        render_message(frame, message_rect, state, &sentence, theme, ascii);
        render_help_text(frame, help_rect, help, theme);

        // Theme/zone dropdowns use the shared Anchored geometry against the
        // actual field anchor and the terminal frame (frame-bounded, not
        // dialog-confined); Unicode paths/carets stay correct via display
        // width. Same rects drive paint, selection, scrollbar and mouse.
        if let Some(field) = dialog.dropdown
            && let Some(anchor) = dropdown_anchors
                .iter()
                .find(|(candidate, _)| *candidate == field)
                .map(|(_, rect)| *rect)
            && anchor.width > 0
        {
            let choices = settings_choices(field);
            let longest = choices
                .iter()
                .map(|value| UnicodeWidthStr::width(value.as_str()))
                .max()
                .unwrap_or(8);
            let preferred = u16::try_from(longest.saturating_add(4))
                .unwrap_or(12)
                .max(12);
            let spec = AnchoredSpec::new(choices.len(), None, preferred, 0);
            let selected = dialog.choice_selected.min(choices.len().saturating_sub(1));
            let pop = anchored_geometry(area, anchor, &spec, selected, 0);
            // Exact union: frame-bounded to the terminal frame, may extend
            // past the dialog; containment, selection and hitboxes share it.
            surface.popup = surface.popup.union(pop.popup);
            crate::ui::clear_themed(frame, pop.popup, theme);
            frame.render_widget(
                ratatui::widgets::Block::default()
                    .borders(ratatui::widgets::Borders::ALL)
                    .border_style(styles.label),
                pop.popup,
            );
            let bar_w = u16::from(pop.scrollbar.is_some());
            for (offset, _) in choices
                .iter()
                .enumerate()
                .skip(pop.first_item)
                .take(usize::from(pop.viewport.height))
                .enumerate()
            {
                let idx = pop.first_item.saturating_add(offset);
                let Some(value) = choices.get(idx) else {
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
                    button_style(theme, false, false)
                };
                frame.render_widget(Paragraph::new(value.clone()).style(style), row);
                choices_hit.push((row, idx));
            }
            if let Some(bar) = pop.scrollbar {
                render_scrollbar(
                    frame,
                    bar,
                    pop.first_item,
                    choices
                        .len()
                        .saturating_sub(usize::from(pop.viewport.height)),
                    theme,
                    ascii,
                );
                surface.scrollable = true;
            }
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
        SettingsField::DisplayZone => &mut dialog.draft.display_zone,
        SettingsField::RowCache => &mut dialog.draft.rows_mib,
        SettingsField::Membership => &mut dialog.draft.membership_mib,
        SettingsField::DiskTotal => &mut dialog.draft.disk_total_mib,
        SettingsField::IndexPerSource => &mut dialog.draft.index_per_source_mib,
        SettingsField::Theme
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
