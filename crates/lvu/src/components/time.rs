//! The Time window layer (`docs/dialog-system.md` §12.4), converted to the
//! component contract per `docs/component-model.md` §6.2.
//!
//! Time is the first layer to cross the two hard seams. Its drafts live in
//! `ViewState`, not here, so an invalid draft leaves the applied window intact
//! and reopening the dialog shows what the user last typed; and it submits a
//! capture-time constraint through `ctx.views.submit_capture_time`, the one
//! query seam, rather than through `&mut App`.
//!
//! The caret is the dialog's own: `segment_cursor` is a single cursor that
//! follows focus between the six date/clock/zone segments and resets to the end
//! of each. §2.5 suggests a `TextField` per segment; that would give each
//! segment an independent remembered caret, which is a behaviour change, so the
//! single cursor moved across unchanged and `ctx.cursors` is unused here.

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph},
};
use unicode_width::UnicodeWidthStr;

use crate::app::{
    Action, CaptureTimePolicy, CaptureTimeRange, SubmitRefused, TimeBasis, TimeFieldCandidate,
    TimeRecognition, TimeRecognitionRequest, TimeWindowChoice, ViewState, Views,
    format_capture_duration, format_utc_nanos, mark_time_edit, parse_capture_range,
    resolve_capture_time_policy, split_time_draft, time_basis_label, time_field_token_label,
    time_zone_choices,
};
use crate::command_palette::CommandId;
use crate::component::{
    CommandEntry, CommandSpec, Component, Ctx, Event, Outbox, Outcome, RenderCtx, Surface,
};
use crate::dialog_controls::{DialogStyles, button_style, button_width, render_button};
use crate::provider::RowId;
use crate::ui::{
    FIELD_GUTTER, InputSurface, MESSAGE_SENTENCE_COLUMN, MessageState, clipped_width,
    dialog_frame_regions, help_rows, message_rows, packed_button_rows, render_action_row,
    render_help_text, render_message, render_scrollbar, time_input_window, truncated,
    wrap_sentence,
};
use ratatui::widgets::Widget;

/// One recognition request per open, plus room for a re-open before the worker
/// drains: the queue is never allowed to grow with keystrokes (AGENTS.md).
const TIME_OUTBOX_CAP: usize = 8;

/// One row of the Time dialog's basis dropdown. Rendering and selection share
/// this list so the label a user clicks and the basis that is applied cannot
/// drift apart.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TimeBasisEntry {
    Builtin(TimeBasis),
    Field(TimeFieldCandidate),
}

impl TimeBasisEntry {
    pub fn label(&self) -> String {
        match self {
            TimeBasisEntry::Builtin(basis) => time_basis_label(*basis).to_owned(),
            TimeBasisEntry::Field(candidate) => {
                let mut label = if candidate.reading.is_empty() {
                    candidate.label.clone()
                } else {
                    format!("{} · {}", candidate.label, candidate.reading)
                };
                if candidate.first_usable().is_none() {
                    label.push_str(" · blocked");
                } else if candidate.blocked.is_some() || !candidate.assumptions.is_empty() {
                    label.push_str(" · needs an assumption");
                } else if let Some(coverage) = candidate.coverage_percent {
                    label.push_str(&format!(" · {coverage}%"));
                }
                label
            }
        }
    }
}

/// The basis dropdown: the three built-in bases, then every recognized
/// candidate, ranked as the recognizer ranked them.
pub fn time_basis_entries(dialog: &TimeState) -> Vec<TimeBasisEntry> {
    let mut entries = vec![
        TimeBasisEntry::Builtin(TimeBasis::Capture),
        TimeBasisEntry::Builtin(TimeBasis::Event),
        TimeBasisEntry::Builtin(TimeBasis::Extracted),
    ];
    entries.extend(
        dialog
            .recognition
            .candidates
            .iter()
            .cloned()
            .map(TimeBasisEntry::Field),
    );
    entries
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TimeState {
    pub focus: TimeControl,
    pub dropdown: Option<TimeDropdown>,
    pub window: TimeWindowChoice,
    pub scroll: usize,
    pub reveal_focus: bool,
    pub has_overflow: bool,
    pub dropdown_scroll: usize,
    pub segment_cursor: usize,
    pub start_date: String,
    pub start_clock: String,
    pub start_zone: String,
    pub end_date: String,
    pub end_clock: String,
    pub end_zone: String,
    pub start_zone_custom: bool,
    pub end_zone_custom: bool,
    pub highlighted: usize,
    pub window_choices: Vec<TimeWindowChoice>,
    pub anchored_row: Option<RowId>,
    pub anchored_capture_nanos: Option<i64>,
    pub anchored_event_nanos: Option<i64>,
    pub anchored_extracted_nanos: Option<i64>,
    pub anchored_selected_nanos: Option<i64>,
    pub basis: TimeBasis,
    /// Accepted `TimeFieldSelection` token backing `TimeBasis::Selected`.
    pub field_token: Option<String>,
    /// Readable name of the accepted token, so the field reads as a field name
    /// rather than as an encoding.
    pub field_label: String,
    pub recognition: TimeRecognition,
    pub recognition_generation: u64,
    /// A candidate the user picked that needs an assumption accepted first.
    /// Nothing is applied while this is set — that is the whole point of it.
    pub pending_field: Option<TimeFieldCandidate>,
    /// Which reading of `pending_field` is offered: 0 is the recognizer's own,
    /// the rest are its alternatives.
    pub pending_reading: usize,
    /// Why the last pick could not be taken, when it could not.
    pub field_error: Option<String>,
}

impl TimeState {
    /// The reading the confirmation step is currently offering.
    pub fn pending_reading(&self) -> Option<&TimeFieldCandidate> {
        let candidate = self.pending_field.as_ref()?;
        if self.pending_reading == 0 {
            return Some(candidate);
        }
        candidate.alternatives.get(self.pending_reading - 1)
    }

    /// Every reading on offer for the pending candidate, the recognizer's first.
    pub fn pending_readings(&self) -> Vec<&TimeFieldCandidate> {
        self.pending_field.as_ref().map_or_else(Vec::new, |c| {
            std::iter::once(c).chain(c.alternatives.iter()).collect()
        })
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TimeControl {
    #[default]
    Basis,
    Window,
    StartDate,
    StartClock,
    StartZone,
    StartZoneMenu,
    EndDate,
    EndClock,
    EndZone,
    EndZoneMenu,
    Reading,
    AcceptField,
    Apply,
    Clear,
    Recognize,
    ScrollUp,
    ScrollDown,
}

impl TimeControl {
    pub(crate) fn focusable(
        has_overflow: bool,
        start_custom: bool,
        end_custom: bool,
        confirming_field: bool,
    ) -> Vec<Self> {
        let mut controls = vec![Self::Basis];
        if confirming_field {
            // The confirmation step sits directly under the basis it qualifies,
            // and its two controls are the only way past it.
            controls.extend([Self::Reading, Self::AcceptField]);
        }
        controls.extend([Self::Window, Self::StartDate, Self::StartClock]);
        if start_custom {
            controls.push(Self::StartZone);
        }
        controls.extend([Self::StartZoneMenu, Self::EndDate, Self::EndClock]);
        if end_custom {
            controls.push(Self::EndZone);
        }
        controls.extend([Self::EndZoneMenu, Self::Apply, Self::Clear, Self::Recognize]);
        if has_overflow {
            controls.extend([Self::ScrollUp, Self::ScrollDown]);
        }
        controls
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimeDropdown {
    Basis,
    Window,
    StartZone,
    EndZone,
    /// Readings of the candidate awaiting confirmation: the override path.
    Reading,
}

#[derive(Clone)]
enum EitherTimeChoice {
    Basis(TimeBasis),
    /// Index into `time_basis_entries`, resolved against the same list the
    /// dropdown rendered.
    Field(usize),
    /// Index into `TimeState::pending_readings`.
    Reading(usize),
    Window(TimeWindowChoice),
    Zone(String),
    CustomZone,
}

fn is_time_zone_preset(value: &str) -> bool {
    time_zone_choices()
        .iter()
        .any(|(_, preset)| *preset == value)
}

fn dialog_time_segment(dialog: &TimeState, control: TimeControl) -> &str {
    match control {
        TimeControl::StartDate => &dialog.start_date,
        TimeControl::StartClock => &dialog.start_clock,
        TimeControl::StartZone => &dialog.start_zone,
        TimeControl::EndDate => &dialog.end_date,
        TimeControl::EndClock => &dialog.end_clock,
        TimeControl::EndZone => &dialog.end_zone,
        _ => "",
    }
}

fn edit_dialog_time_segment(dialog: &mut TimeState, input: Option<char>) -> bool {
    let control = dialog.focus;
    if matches!(control, TimeControl::StartZone) && !dialog.start_zone_custom
        || matches!(control, TimeControl::EndZone) && !dialog.end_zone_custom
    {
        return false;
    }
    let cursor = dialog.segment_cursor;
    let part = match control {
        TimeControl::StartDate => &mut dialog.start_date,
        TimeControl::StartClock => &mut dialog.start_clock,
        TimeControl::StartZone => &mut dialog.start_zone,
        TimeControl::EndDate => &mut dialog.end_date,
        TimeControl::EndClock => &mut dialog.end_clock,
        TimeControl::EndZone => &mut dialog.end_zone,
        _ => return false,
    };
    let char_len = part.chars().count();
    let at = cursor.min(char_len);
    let byte_at = part
        .char_indices()
        .nth(at)
        .map_or(part.len(), |(index, _)| index);
    match input {
        Some(ch) if !ch.is_control() && part.len().saturating_add(ch.len_utf8()) <= 32 => {
            part.insert(byte_at, ch);
            dialog.segment_cursor = at + 1;
            dialog.window = TimeWindowChoice::Absolute;
            return true;
        }
        None if at > 0 => {
            let previous = part
                .char_indices()
                .nth(at - 1)
                .map_or(0, |(index, _)| index);
            part.replace_range(previous..byte_at, "");
            dialog.segment_cursor = at - 1;
            dialog.window = TimeWindowChoice::Absolute;
            return true;
        }
        _ => {}
    }
    false
}

fn dialog_time_drafts(dialog: &TimeState) -> (String, String, [String; 6]) {
    (
        format!(
            "{}T{}{}",
            dialog.start_date, dialog.start_clock, dialog.start_zone
        ),
        format!(
            "{}T{}{}",
            dialog.end_date, dialog.end_clock, dialog.end_zone
        ),
        [
            dialog.start_date.clone(),
            dialog.start_clock.clone(),
            dialog.start_zone.clone(),
            dialog.end_date.clone(),
            dialog.end_clock.clone(),
            dialog.end_zone.clone(),
        ],
    )
}

fn store_time_drafts(state: &mut ViewState, start: String, end: String, parts: [String; 6]) {
    state.time_start_draft = start;
    state.time_end_draft = end;
    let [sd, sc, sz, ed, ec, ez] = parts;
    state.time_start_date_draft = sd;
    state.time_start_clock_draft = sc;
    state.time_start_zone_draft = sz;
    state.time_end_date_draft = ed;
    state.time_end_clock_draft = ec;
    state.time_end_zone_draft = ez;
    state.time_structured_draft_present = true;
}

fn time_window_choices(current: TimeWindowChoice) -> Vec<TimeWindowChoice> {
    let mut values = vec![
        TimeWindowChoice::All,
        TimeWindowChoice::Absolute,
        TimeWindowChoice::Recent(300),
        TimeWindowChoice::Recent(900),
        TimeWindowChoice::Recent(3600),
        TimeWindowChoice::AroundSelected,
    ];
    if let TimeWindowChoice::Recent(seconds) = current
        && !matches!(seconds, 300 | 900 | 3600)
    {
        values.insert(5, current);
    }
    values
}

/// How many preset zones the zone dropdown offers before `Custom offset…`.
pub fn time_zone_choice_count() -> usize {
    time_zone_choices().len()
}

/// Everything the Time layer draws that can be clicked.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimeHit {
    Control(TimeControl),
    /// A row of the open anchored dropdown, by index into its choice list.
    Choice(usize),
}

/// Recorded by `render`, consumed by `hit()` (§5.1). Anchored dropdowns are
/// drawn by this component, so their rows are part of its geometry rather than
/// a layer of their own (§5.2).
#[derive(Clone, Debug, Default)]
struct TimeGeometry {
    controls: Vec<(Rect, TimeControl)>,
    choices: Vec<(Rect, usize)>,
}

#[derive(Debug)]
pub struct TimeDialog {
    /// Whether the layer is on the stack. The slot is permanent, so a late
    /// recognition report after close has somewhere to land and be fenced out.
    open: bool,
    state: TimeState,
    geometry: TimeGeometry,
    surface: Surface,
    pub outbox: Outbox<TimeRecognitionRequest>,
}

impl Default for TimeDialog {
    fn default() -> Self {
        Self {
            open: false,
            state: TimeState::default(),
            geometry: TimeGeometry::default(),
            surface: Surface::default(),
            outbox: Outbox::new(TIME_OUTBOX_CAP),
        }
    }
}

impl TimeDialog {
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Read-only view of the dialog's own state. Drafts are not here: they are
    /// the view's, read through `Views` (§4.2).
    pub fn state(&self) -> &TimeState {
        &self.state
    }

    /// Preselect a dropdown row. The mouse does this from `hit()`; it is also
    /// how a test names a row without depending on where it was drawn.
    pub fn highlight(&mut self, index: usize) {
        self.state.highlighted = index;
    }

    /// Geometry recorded by the last `render`; `hit()` is how input reaches it.
    pub fn control_rects(&self) -> &[(Rect, TimeControl)] {
        &self.geometry.controls
    }

    pub fn choice_rects(&self) -> &[(Rect, usize)] {
        &self.geometry.choices
    }

    /// Recognizer output for the open dialog. Fenced: a report for a dialog
    /// that has since closed or been reopened is dropped.
    pub fn complete_recognition(&mut self, generation: u64, recognition: TimeRecognition) -> bool {
        if !self.open || self.state.recognition_generation != generation {
            return false;
        }
        self.state.anchored_selected_nanos = recognition.anchored_selected_nanos;
        self.state.recognition = recognition;
        // A pending confirmation names a candidate by value, so a fresh report
        // must not silently swap the reading under the user's decision.
        true
    }

    /// Which segment, if any, is taking text. `None` while a dropdown is open
    /// or a button holds focus, which is also what makes bare `q` dismiss.
    pub fn editing_segment(&self) -> Option<TimeControl> {
        if self.state.dropdown.is_some() {
            return None;
        }
        match self.state.focus {
            TimeControl::StartDate
            | TimeControl::StartClock
            | TimeControl::EndDate
            | TimeControl::EndClock => Some(self.state.focus),
            TimeControl::StartZone if self.state.start_zone_custom => Some(self.state.focus),
            TimeControl::EndZone if self.state.end_zone_custom => Some(self.state.focus),
            _ => None,
        }
    }
}

impl TimeDialog {
    /// Seeds the drafts the view owns, then asks the recognizer what fields the
    /// sample offers. Never submits a query (§1).
    fn open_dialog(&mut self, ctx: &mut Ctx<'_>) {
        let anchored_row = ctx.views.active().and_then(|state| state.selected.clone());
        let anchored = ctx
            .views
            .active_id()
            .map(str::to_owned)
            .zip(anchored_row.as_ref())
            .and_then(|(view, id)| ctx.provider.row_by_id(&view, id));
        let anchored_capture_nanos = anchored.as_ref().and_then(|row| row.captured_at_unix_nanos);
        let anchored_event_nanos = anchored.as_ref().and_then(|row| {
            row.details
                .iter()
                .find(|(name, _)| name == "event_time_utc_nanos")
                .and_then(|(_, value)| value.parse().ok())
        });
        let anchored_extracted_nanos = anchored.as_ref().and_then(|row| {
            row.details
                .iter()
                .find(|(name, _)| name == "derived.timestamp_utc")
                .and_then(|(_, value)| crate::app::parse_utc_nanos(value).ok())
        });
        let (basis, policy, applied, may_seed) =
            ctx.views
                .active()
                .map_or((TimeBasis::Capture, None, None, true), |state| {
                    (
                        if state.time_draft_touched {
                            state.time_basis_draft
                        } else {
                            state.applied_time_basis
                        },
                        state.applied_capture_time_policy,
                        state.applied_capture_time,
                        !state.time_draft_touched,
                    )
                });
        if may_seed {
            let seed = applied
                .or_else(|| {
                    let center = match basis {
                        TimeBasis::Capture => anchored_capture_nanos,
                        TimeBasis::Event => anchored_event_nanos,
                        TimeBasis::Extracted => anchored_extracted_nanos,
                        // The chosen field is read outside this crate, so no
                        // seed is available until the recognizer reports back.
                        TimeBasis::Selected => None,
                    };
                    center.map(|center| CaptureTimeRange {
                        start_unix_nanos: center.saturating_sub(30_000_000_000),
                        end_unix_nanos: center.saturating_add(30_000_000_000),
                    })
                })
                .unwrap_or(CaptureTimeRange {
                    start_unix_nanos: ctx.clock.now_unix_nanos.saturating_sub(30_000_000_000),
                    end_unix_nanos: ctx.clock.now_unix_nanos,
                });
            if let Some(state) = ctx.views.active_mut() {
                state.time_start_draft = format_utc_nanos(seed.start_unix_nanos);
                state.time_end_draft = format_utc_nanos(seed.end_unix_nanos);
            }
        }
        if let Some(state) = ctx.views.active_mut()
            && (may_seed || !state.time_structured_draft_present)
        {
            let start = split_time_draft(&state.time_start_draft);
            let end = split_time_draft(&state.time_end_draft);
            state.time_start_date_draft = start.0;
            state.time_start_clock_draft = start.1;
            state.time_start_zone_draft = start.2;
            state.time_end_date_draft = end.0;
            state.time_end_clock_draft = end.1;
            state.time_end_zone_draft = end.2;
            state.time_structured_draft_present = true;
        }
        let (start_date, start_clock, start_zone, end_date, end_clock, end_zone) = ctx
            .views
            .active()
            .map(|state| {
                if state.time_structured_draft_present {
                    return (
                        state.time_start_date_draft.clone(),
                        state.time_start_clock_draft.clone(),
                        state.time_start_zone_draft.clone(),
                        state.time_end_date_draft.clone(),
                        state.time_end_clock_draft.clone(),
                        state.time_end_zone_draft.clone(),
                    );
                }
                let start = split_time_draft(&state.time_start_draft);
                let end = split_time_draft(&state.time_end_draft);
                (start.0, start.1, start.2, end.0, end.1, end.2)
            })
            .unwrap_or_default();
        let window = ctx.views.active().map_or(TimeWindowChoice::All, |state| {
            if state.time_draft_touched {
                state.time_window_draft
            } else {
                match policy {
                    None => TimeWindowChoice::All,
                    Some(CaptureTimePolicy::Absolute(_)) => TimeWindowChoice::Absolute,
                    Some(CaptureTimePolicy::Recent { seconds }) => {
                        TimeWindowChoice::Recent(seconds)
                    }
                }
            }
        });
        let field_token = ctx.views.active().and_then(|state| {
            if state.time_draft_touched {
                state.time_field_draft.clone()
            } else {
                state.applied_time_field.clone()
            }
        });
        let generation = self.outbox.next_generation();
        self.state = TimeState {
            focus: TimeControl::Basis,
            dropdown: None,
            window,
            scroll: 0,
            reveal_focus: true,
            has_overflow: false,
            dropdown_scroll: 0,
            segment_cursor: usize::MAX,
            start_date,
            start_clock,
            start_zone_custom: !is_time_zone_preset(&start_zone),
            start_zone,
            end_date,
            end_clock,
            end_zone_custom: !is_time_zone_preset(&end_zone),
            end_zone,
            highlighted: 0,
            window_choices: time_window_choices(window),
            anchored_row,
            anchored_capture_nanos,
            anchored_event_nanos,
            anchored_extracted_nanos,
            anchored_selected_nanos: None,
            basis,
            field_token: field_token.clone(),
            field_label: field_token
                .as_deref()
                .map(time_field_token_label)
                .unwrap_or_default(),
            recognition: TimeRecognition::default(),
            recognition_generation: generation,
            pending_field: None,
            pending_reading: 0,
            field_error: None,
        };
        self.geometry = TimeGeometry::default();
        if let Some(view_id) = ctx.views.active_id().map(str::to_owned) {
            let _ = self.outbox.push(TimeRecognitionRequest {
                generation,
                view_id,
                token: field_token,
                anchored_row: self.state.anchored_row.clone(),
            });
        }
    }

    /// Jump between the Start and End rows. No key or palette entry produces
    /// this: `Action::SwitchTimeField` was reachable only from tests, and it
    /// stays reachable the same way.
    pub fn switch_field(&mut self) {
        self.state.focus = if matches!(
            self.state.focus,
            TimeControl::EndDate
                | TimeControl::EndClock
                | TimeControl::EndZone
                | TimeControl::EndZoneMenu
        ) {
            TimeControl::StartDate
        } else {
            TimeControl::EndDate
        };
        self.state.segment_cursor = usize::MAX;
        self.state.reveal_focus = true;
    }

    fn move_focus(&mut self, delta: i32) {
        self.state.dropdown = None;
        let controls = TimeControl::focusable(
            self.state.has_overflow,
            self.state.start_zone_custom,
            self.state.end_zone_custom,
            self.state.pending_field.is_some(),
        );
        let at = controls
            .iter()
            .position(|item| *item == self.state.focus)
            .unwrap_or(0);
        self.state.focus =
            controls[(at as isize + delta as isize).rem_euclid(controls.len() as isize) as usize];
        self.state.segment_cursor = usize::MAX;
        self.state.reveal_focus = true;
    }

    fn focus_control(&mut self, control: TimeControl) {
        self.state.focus = control;
        self.state.dropdown = None;
        self.state.segment_cursor = usize::MAX;
        self.state.reveal_focus = true;
    }

    /// Enter on the focused control: a dropdown control opens its list, an
    /// action control runs, and a list already open commits its highlight.
    fn open_focused(&mut self, ctx: &mut Ctx<'_>) -> Outcome {
        if self.state.dropdown.is_some() {
            return self.choose(ctx);
        }
        let action = match self.state.focus {
            TimeControl::Basis
            | TimeControl::Window
            | TimeControl::StartZoneMenu
            | TimeControl::EndZoneMenu
            | TimeControl::Reading => None,
            TimeControl::AcceptField => Some(TimeAction::AcceptField),
            TimeControl::Apply => Some(TimeAction::Submit),
            TimeControl::Clear => Some(TimeAction::Clear),
            TimeControl::Recognize => Some(TimeAction::Recognize),
            TimeControl::ScrollUp => Some(TimeAction::Scroll(-1)),
            TimeControl::ScrollDown => Some(TimeAction::Scroll(1)),
            _ => Some(TimeAction::None),
        };
        self.state.dropdown = match self.state.focus {
            TimeControl::Basis => Some(TimeDropdown::Basis),
            TimeControl::Window => Some(TimeDropdown::Window),
            TimeControl::StartZoneMenu => Some(TimeDropdown::StartZone),
            TimeControl::EndZoneMenu => Some(TimeDropdown::EndZone),
            TimeControl::Reading => Some(TimeDropdown::Reading),
            _ => self.state.dropdown,
        };
        self.state.highlighted = match self.state.dropdown {
            Some(TimeDropdown::Basis) => time_basis_entries(&self.state)
                .iter()
                .position(|entry| match entry {
                    TimeBasisEntry::Builtin(basis) => {
                        self.state.basis != TimeBasis::Selected && *basis == self.state.basis
                    }
                    TimeBasisEntry::Field(candidate) => {
                        self.state.field_token.as_deref() == Some(candidate.token.as_str())
                    }
                })
                .unwrap_or(0),
            Some(TimeDropdown::Reading) => self.state.pending_reading,
            Some(TimeDropdown::Window) => self
                .state
                .window_choices
                .iter()
                .position(|v| *v == self.state.window)
                .unwrap_or(0),
            Some(TimeDropdown::StartZone) => time_zone_choices()
                .iter()
                .position(|(_, value)| *value == self.state.start_zone)
                .unwrap_or(time_zone_choices().len()),
            Some(TimeDropdown::EndZone) => time_zone_choices()
                .iter()
                .position(|(_, value)| *value == self.state.end_zone)
                .unwrap_or(time_zone_choices().len()),
            None => 0,
        };
        match action {
            Some(action) => self.run(action, ctx),
            None => Outcome::Consumed,
        }
    }

    fn set_basis(&mut self, basis: TimeBasis, ctx: &mut Ctx<'_>) {
        self.state.basis = basis;
        // Leaving a chosen field abandons both the accepted token and any
        // confirmation in flight; a stale token must never outlive the basis
        // that gave it meaning.
        self.state.field_token = None;
        self.state.field_label.clear();
        self.state.pending_field = None;
        self.state.pending_reading = 0;
        self.state.field_error = None;
        if let Some(state) = ctx.views.active_mut() {
            mark_time_edit(state);
            state.time_error = None;
            state.time_basis_draft = basis;
            state.time_field_draft = None;
            state.time_draft_touched = true;
        }
    }

    fn choose_field(&mut self, index: usize, ctx: &mut Ctx<'_>) -> Outcome {
        let candidate = match time_basis_entries(&self.state).into_iter().nth(index) {
            Some(TimeBasisEntry::Field(candidate)) => candidate,
            _ => return Outcome::Consumed,
        };
        let Some(usable) = candidate.first_usable() else {
            // A candidate no reading can rescue stays visible so the user can
            // see why, but choosing it changes nothing.
            self.state.dropdown = None;
            self.state.pending_field = None;
            self.state.field_error = candidate.blocked.clone();
            return Outcome::Consumed;
        };
        self.state.dropdown = None;
        // Land on the first reading that could be applied, never on one the
        // recognizer already refused.
        self.state.pending_reading = usable;
        self.state.field_error = None;
        self.state.pending_field = Some(candidate);
        self.state.focus = TimeControl::AcceptField;
        self.state.reveal_focus = true;
        // A reading that rests on no assumption has nothing to confirm, so it
        // is accepted directly. One that does waits for the user.
        let clean = self
            .state
            .pending_reading()
            .is_some_and(|reading| reading.assumptions.is_empty());
        if clean {
            self.accept_field(ctx);
        }
        Outcome::Consumed
    }

    fn accept_field(&mut self, ctx: &mut Ctx<'_>) {
        let Some(accepted) = self.state.pending_reading().cloned() else {
            return;
        };
        if let Some(reason) = accepted.blocked {
            self.state.field_error = Some(reason);
            return;
        }
        self.state.basis = TimeBasis::Selected;
        self.state.field_token = Some(accepted.token.clone());
        self.state.field_label = accepted.label.clone();
        self.state.pending_field = None;
        self.state.pending_reading = 0;
        self.state.field_error = None;
        self.state.focus = TimeControl::Basis;
        self.state.reveal_focus = true;
        if let Some(state) = ctx.views.active_mut() {
            mark_time_edit(state);
            state.time_error = None;
            state.time_basis_draft = TimeBasis::Selected;
            state.time_field_draft = Some(accepted.token);
            state.time_draft_touched = true;
        }
    }

    fn move_choice(&mut self, delta: i32) {
        match self.state.dropdown {
            Some(TimeDropdown::Basis) => {
                let count = time_basis_entries(&self.state).len() as isize;
                self.state.highlighted =
                    (self.state.highlighted as isize + delta as isize).rem_euclid(count) as usize;
            }
            Some(TimeDropdown::Reading) => {
                let count = self.state.pending_readings().len().max(1) as isize;
                self.state.highlighted =
                    (self.state.highlighted as isize + delta as isize).rem_euclid(count) as usize;
            }
            Some(TimeDropdown::Window) => {
                self.state.highlighted = (self.state.highlighted as isize + delta as isize)
                    .rem_euclid(self.state.window_choices.len() as isize)
                    as usize;
            }
            Some(TimeDropdown::StartZone) | Some(TimeDropdown::EndZone) => {
                let choices = time_zone_choices().len() + 1;
                self.state.highlighted = (self.state.highlighted as isize + delta as isize)
                    .rem_euclid(choices as isize) as usize;
            }
            None if matches!(
                self.state.focus,
                TimeControl::ScrollUp | TimeControl::ScrollDown
            ) =>
            {
                self.state.scroll = self.state.scroll.saturating_add_signed(delta as isize);
                self.state.reveal_focus = false;
            }
            None => {}
        }
    }

    fn choose(&mut self, ctx: &mut Ctx<'_>) -> Outcome {
        let selected = match self.state.dropdown {
            Some(TimeDropdown::Basis) => {
                match time_basis_entries(&self.state).get(self.state.highlighted) {
                    Some(TimeBasisEntry::Builtin(basis)) => Some(EitherTimeChoice::Basis(*basis)),
                    Some(TimeBasisEntry::Field(_)) => {
                        Some(EitherTimeChoice::Field(self.state.highlighted))
                    }
                    None => None,
                }
            }
            Some(TimeDropdown::Reading) => Some(EitherTimeChoice::Reading(self.state.highlighted)),
            Some(TimeDropdown::Window) => self
                .state
                .window_choices
                .get(self.state.highlighted)
                .copied()
                .map(EitherTimeChoice::Window),
            Some(TimeDropdown::StartZone) | Some(TimeDropdown::EndZone) => time_zone_choices()
                .get(self.state.highlighted)
                .map(|(_, value)| EitherTimeChoice::Zone((*value).into()))
                .or(Some(EitherTimeChoice::CustomZone)),
            None => None,
        };
        self.state.dropdown = None;
        match selected {
            Some(EitherTimeChoice::Basis(basis)) => {
                self.set_basis(basis, ctx);
                Outcome::Consumed
            }
            Some(EitherTimeChoice::Field(index)) => self.choose_field(index, ctx),
            Some(EitherTimeChoice::Reading(index)) => {
                self.state.pending_reading =
                    index.min(self.state.pending_readings().len().saturating_sub(1));
                self.state.field_error = None;
                self.state.focus = TimeControl::AcceptField;
                self.state.reveal_focus = true;
                Outcome::Consumed
            }
            Some(EitherTimeChoice::Window(window)) => {
                let unavailable =
                    window == TimeWindowChoice::AroundSelected && !self.anchor_available();
                if unavailable {
                    if let Some(state) = ctx.views.active_mut() {
                        state.time_error = Some("Around selected is unavailable: the opening record has no timestamp in this basis".into());
                    }
                    return Outcome::Consumed;
                }
                if window == TimeWindowChoice::AroundSelected {
                    self.around_selected(ctx);
                    return Outcome::Consumed;
                }
                self.state.window = window;
                if let Some(state) = ctx.views.active_mut() {
                    mark_time_edit(state);
                    state.time_error = None;
                    state.time_window_draft = window;
                    state.time_draft_touched = true;
                }
                Outcome::Consumed
            }
            Some(EitherTimeChoice::Zone(zone)) => {
                match self.state.focus {
                    TimeControl::StartZone | TimeControl::StartZoneMenu => {
                        self.state.start_zone = zone;
                        self.state.start_zone_custom = false;
                    }
                    TimeControl::EndZone | TimeControl::EndZoneMenu => {
                        self.state.end_zone = zone;
                        self.state.end_zone_custom = false;
                    }
                    _ => return Outcome::Consumed,
                }
                self.state.window = TimeWindowChoice::Absolute;
                let (start, end, parts) = dialog_time_drafts(&self.state);
                if let Some(state) = ctx.views.active_mut() {
                    store_time_drafts(state, start, end, parts);
                    state.time_window_draft = TimeWindowChoice::Absolute;
                    state.time_draft_touched = true;
                    state.time_error = None;
                    mark_time_edit(state);
                }
                Outcome::Consumed
            }
            Some(EitherTimeChoice::CustomZone) => {
                match self.state.focus {
                    TimeControl::StartZone | TimeControl::StartZoneMenu => {
                        self.state.start_zone_custom = true;
                        self.state.focus = TimeControl::StartZone;
                    }
                    TimeControl::EndZone | TimeControl::EndZoneMenu => {
                        self.state.end_zone_custom = true;
                        self.state.focus = TimeControl::EndZone;
                    }
                    _ => {}
                }
                self.state.segment_cursor = usize::MAX;
                Outcome::Consumed
            }
            None => Outcome::Consumed,
        }
    }

    /// Whether the opening record carries a timestamp in the current basis.
    fn anchor_available(&self) -> bool {
        match self.state.basis {
            TimeBasis::Capture => self.state.anchored_capture_nanos.is_some(),
            TimeBasis::Event => self.state.anchored_event_nanos.is_some(),
            TimeBasis::Extracted => self.state.anchored_extracted_nanos.is_some(),
            TimeBasis::Selected => self.state.anchored_selected_nanos.is_some(),
        }
    }

    fn scroll_body(&mut self, delta: i32) {
        self.state.scroll = self.state.scroll.saturating_add_signed(delta as isize);
        self.state.reveal_focus = false;
    }

    fn move_cursor(&mut self, delta: i32) {
        let len = dialog_time_segment(&self.state, self.state.focus)
            .chars()
            .count();
        let at = self.state.segment_cursor.min(len);
        self.state.segment_cursor = at.saturating_add_signed(delta as isize).min(len);
    }

    /// One typed character, or one backspace when `input` is `None`.
    fn edit_segment(&mut self, input: Option<char>, ctx: &mut Ctx<'_>) {
        if !edit_dialog_time_segment(&mut self.state, input) {
            return;
        }
        let (start, end, parts) = dialog_time_drafts(&self.state);
        if let Some(state) = ctx.views.active_mut() {
            store_time_drafts(state, start, end, parts);
            state.time_window_draft = TimeWindowChoice::Absolute;
            mark_time_edit(state);
            state.time_error = None;
            state.time_draft_touched = true;
        }
    }

    fn clear(&mut self, ctx: &mut Ctx<'_>) -> Outcome {
        if let Some(state) = ctx.views.active_mut() {
            state.time_start_draft.clear();
            state.time_end_draft.clear();
            state.time_recent_draft.clear();
            state.time_start_date_draft.clear();
            state.time_start_clock_draft.clear();
            state.time_start_zone_draft.clear();
            state.time_end_date_draft.clear();
            state.time_end_clock_draft.clear();
            state.time_end_zone_draft.clear();
            state.time_structured_draft_present = true;
            mark_time_edit(state);
            state.time_draft_touched = true;
            state.time_window_draft = TimeWindowChoice::All;
        }
        self.state.start_date.clear();
        self.state.start_clock.clear();
        self.state.start_zone.clear();
        self.state.end_date.clear();
        self.state.end_clock.clear();
        self.state.end_zone.clear();
        self.state.start_zone_custom = true;
        self.state.end_zone_custom = true;
        self.state.window = TimeWindowChoice::All;
        self.submit_window(None, None, ctx)
    }

    fn around_selected(&mut self, ctx: &mut Ctx<'_>) {
        self.state.window = TimeWindowChoice::AroundSelected;
        if let Some(state) = ctx.views.active_mut() {
            mark_time_edit(state);
            state.time_window_draft = TimeWindowChoice::AroundSelected;
            state.time_draft_touched = true;
        }
        let basis = self.state.basis;
        let center = match basis {
            TimeBasis::Capture => self.state.anchored_capture_nanos,
            TimeBasis::Extracted => self.state.anchored_extracted_nanos,
            TimeBasis::Event => self.state.anchored_event_nanos,
            TimeBasis::Selected => self.state.anchored_selected_nanos,
        };
        let Some(center) = center else {
            if let Some(state) = ctx.views.active_mut() {
                state.time_error = Some(match basis {
                    TimeBasis::Capture => "selected record has no capture timestamp".into(),
                    TimeBasis::Extracted => {
                        "selected record has no valid extracted timestamp_utc".into()
                    }
                    TimeBasis::Event => "selected record has no recognized event timestamp".into(),
                    TimeBasis::Selected => {
                        "selected record has no value in the chosen field".into()
                    }
                });
            }
            return;
        };
        let start = center.saturating_sub(30_000_000_000);
        let end = center.saturating_add(30_000_000_000);
        let start_parts = split_time_draft(&format_utc_nanos(start));
        let end_parts = split_time_draft(&format_utc_nanos(end));
        (
            self.state.start_date,
            self.state.start_clock,
            self.state.start_zone,
        ) = start_parts;
        (
            self.state.end_date,
            self.state.end_clock,
            self.state.end_zone,
        ) = end_parts;
        self.state.start_zone_custom = !is_time_zone_preset(&self.state.start_zone);
        self.state.end_zone_custom = !is_time_zone_preset(&self.state.end_zone);
        let (start_draft, end_draft, parts) = dialog_time_drafts(&self.state);
        if let Some(state) = ctx.views.active_mut() {
            store_time_drafts(state, start_draft, end_draft, parts);
            state.time_error = None;
            state.time_draft_touched = true;
        }
    }

    fn submit(&mut self, ctx: &mut Ctx<'_>) -> Outcome {
        if let Some(state) = ctx.views.active_mut() {
            mark_time_edit(state);
        }
        let choice = self.state.window;
        if choice == TimeWindowChoice::All {
            return self.clear(ctx);
        }
        if let TimeWindowChoice::Recent(seconds) = choice {
            return self.set_recent(seconds, ctx);
        }
        if choice == TimeWindowChoice::AroundSelected {
            if !self.anchor_available() {
                if let Some(state) = ctx.views.active_mut() {
                    state.time_error =
                        Some("opening record has no timestamp in the chosen basis".into());
                }
                return Outcome::Consumed;
            }
            self.around_selected(ctx);
        }
        // Drafts are per-view state, so they are read from the view, not from
        // `self`; an invalid draft leaves the applied view intact.
        let parsed = ctx
            .views
            .active()
            .map(|state| parse_capture_range(&state.time_start_draft, &state.time_end_draft));
        match parsed {
            Some(Ok(window)) => {
                self.submit_window(Some(window), Some(CaptureTimePolicy::Absolute(window)), ctx)
            }
            Some(Err(error)) => {
                if let Some(state) = ctx.views.active_mut() {
                    state.time_error = Some(error);
                }
                Outcome::Consumed
            }
            None => Outcome::Consumed,
        }
    }

    fn set_recent(&mut self, seconds: u64, ctx: &mut Ctx<'_>) -> Outcome {
        let Some(window) = resolve_capture_time_policy(
            CaptureTimePolicy::Recent { seconds },
            ctx.clock.now_unix_nanos,
        ) else {
            return Outcome::Consumed;
        };
        if let Some(state) = ctx.views.active_mut() {
            state.time_recent_draft = format_capture_duration(seconds);
            state.time_error = None;
            state.time_draft_touched = true;
            state.time_window_draft = TimeWindowChoice::Recent(seconds);
            mark_time_edit(state);
        }
        self.submit_window(
            Some(window),
            Some(CaptureTimePolicy::Recent { seconds }),
            ctx,
        )
    }

    /// The one query seam (§2.3). A full queue keeps the draft and says so in
    /// the dialog's own message row; the applied window is untouched.
    fn submit_window(
        &mut self,
        window: Option<CaptureTimeRange>,
        policy: Option<CaptureTimePolicy>,
        ctx: &mut Ctx<'_>,
    ) -> Outcome {
        let Some(view_id) = ctx.views.active_id().map(str::to_owned) else {
            return Outcome::Consumed;
        };
        let basis = self.state.basis;
        match ctx
            .views
            .submit_capture_time(&view_id, window, policy, basis)
        {
            Ok(_revision) => {
                self.open = false;
                Outcome::Close
            }
            // The seam recorded the desired window and refused to apply it in
            // place: All events is never filtered, it forks. Staging the fork
            // is still the shell's, so the layer hands it back (§6.4).
            Err(SubmitRefused::DefinitionFixed) => {
                self.open = false;
                Outcome::Legacy(Action::StageForkedTimeWindow)
            }
            Err(SubmitRefused::QueueFull) => {
                if let Some(state) = ctx.views.active_mut() {
                    state.time_error =
                        Some("query submission queue is full; last window preserved".into());
                }
                Outcome::Consumed
            }
        }
    }
}

/// The controls' effects, named so `open_focused` can pick one without the
/// recursive `self.handle(Action::…)` the shell used to need (§2.5).
#[derive(Clone, Copy)]
enum TimeAction {
    None,
    AcceptField,
    Submit,
    Clear,
    Recognize,
    Scroll(i32),
}

impl TimeDialog {
    fn run(&mut self, action: TimeAction, ctx: &mut Ctx<'_>) -> Outcome {
        match action {
            TimeAction::None => Outcome::Consumed,
            TimeAction::AcceptField => {
                self.accept_field(ctx);
                Outcome::Consumed
            }
            TimeAction::Submit => self.submit(ctx),
            TimeAction::Clear => self.clear(ctx),
            // Ask is converted last (§6.3), so the assistant is still a legacy
            // dialog; this becomes `Replace(Open::Ask { .. })` then. The layer
            // leaves the stack the same way it does on `Close`.
            TimeAction::Recognize => {
                self.open = false;
                Outcome::Legacy(Action::OpenTimestampAssistant)
            }
            TimeAction::Scroll(delta) => {
                self.scroll_body(delta);
                Outcome::Consumed
            }
        }
    }
}

impl TimeDialog {
    /// The component's keymap. The order is the shell's old one: the universal
    /// line-editing chords first, then the arrow keys that belong to the caret
    /// while a segment is taking text, then the dialog's own table.
    fn key(&mut self, key: KeyEvent, ctx: &mut Ctx<'_>) -> Outcome {
        use crate::text_edit::EditCommand;
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return Outcome::Ignored;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            let command = match key.code {
                KeyCode::Char('a') => Some(EditCommand::StartOfLine),
                KeyCode::Char('e') => Some(EditCommand::EndOfLine),
                KeyCode::Char('k') => Some(EditCommand::KillToEndOfLine),
                _ => None,
            };
            if let Some(command) = command {
                self.edit_command(command, ctx);
                return Outcome::Consumed;
            }
        }
        if self.editing_segment().is_some() && key.modifiers.is_empty() {
            let command = match key.code {
                KeyCode::Left => Some(EditCommand::MoveLeft),
                KeyCode::Right => Some(EditCommand::MoveRight),
                KeyCode::Up => Some(EditCommand::MoveUp),
                KeyCode::Down => Some(EditCommand::MoveDown),
                _ => None,
            };
            if let Some(command) = command {
                self.edit_command(command, ctx);
                return Outcome::Consumed;
            }
        }
        match key.code {
            KeyCode::Char('t') if key.modifiers.contains(KeyModifiers::ALT) => {
                self.run(TimeAction::Recognize, ctx)
            }
            KeyCode::Tab if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.move_focus(-1);
                Outcome::Consumed
            }
            KeyCode::BackTab => {
                self.move_focus(-1);
                Outcome::Consumed
            }
            KeyCode::Tab => {
                self.move_focus(1);
                Outcome::Consumed
            }
            KeyCode::Up => {
                self.move_choice(-1);
                Outcome::Consumed
            }
            KeyCode::Down => {
                self.move_choice(1);
                Outcome::Consumed
            }
            KeyCode::Left => {
                self.move_cursor(-1);
                Outcome::Consumed
            }
            KeyCode::Right => {
                self.move_cursor(1);
                Outcome::Consumed
            }
            KeyCode::Enter => self.open_focused(ctx),
            KeyCode::Backspace => {
                self.edit_segment(None, ctx);
                Outcome::Consumed
            }
            KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::ALT) => {
                self.around_selected(ctx);
                Outcome::Consumed
            }
            KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::ALT) => {
                self.set_basis(TimeBasis::Capture, ctx);
                Outcome::Consumed
            }
            KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::ALT) => {
                self.set_basis(TimeBasis::Event, ctx);
                Outcome::Consumed
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::ALT) => {
                self.set_basis(TimeBasis::Extracted, ctx);
                Outcome::Consumed
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::ALT) => self.clear(ctx),
            KeyCode::Char('5') if key.modifiers.contains(KeyModifiers::ALT) => {
                self.set_recent(5 * 60, ctx)
            }
            KeyCode::Char('m') if key.modifiers.contains(KeyModifiers::ALT) => {
                self.set_recent(15 * 60, ctx)
            }
            KeyCode::Char('h') if key.modifiers.contains(KeyModifiers::ALT) => {
                self.set_recent(60 * 60, ctx)
            }
            KeyCode::Char(ch) => {
                self.edit_segment(Some(ch), ctx);
                Outcome::Consumed
            }
            _ => Outcome::Ignored,
        }
    }

    /// Line editing against the focused segment, using the dialog's own cursor.
    fn edit_command(&mut self, command: crate::text_edit::EditCommand<'_>, ctx: &mut Ctx<'_>) {
        use crate::text_edit::{EditPolicy, TextCursor, edit};
        if self.editing_segment().is_none() {
            return;
        }
        let mut value = dialog_time_segment(&self.state, self.state.focus).to_owned();
        let mut cursor = TextCursor {
            char_index: self.state.segment_cursor.min(value.chars().count()),
        };
        let outcome = edit(
            &mut value,
            &mut cursor,
            command,
            EditPolicy {
                max_bytes: 32,
                multiline: false,
            },
        );
        self.state.segment_cursor = cursor.char_index;
        if !outcome.changed {
            return;
        }
        match self.state.focus {
            TimeControl::StartDate => self.state.start_date = value,
            TimeControl::StartClock => self.state.start_clock = value,
            TimeControl::StartZone => self.state.start_zone = value,
            TimeControl::EndDate => self.state.end_date = value,
            TimeControl::EndClock => self.state.end_clock = value,
            TimeControl::EndZone => self.state.end_zone = value,
            _ => return,
        }
        self.state.window = TimeWindowChoice::Absolute;
        let (start, end, parts) = dialog_time_drafts(&self.state);
        if let Some(state) = ctx.views.active_mut() {
            store_time_drafts(state, start, end, parts);
            state.time_window_draft = TimeWindowChoice::Absolute;
            mark_time_edit(state);
            state.time_error = None;
            state.time_draft_touched = true;
        }
    }

    /// A pasted whole bound (`…T…`) replaces the focused Start or End row; any
    /// other paste types into the focused segment. Both are bounded.
    fn paste(&mut self, text: String, ctx: &mut Ctx<'_>) {
        let whole_bound = text.contains('T');
        let segment_length = dialog_time_segment(&self.state, self.state.focus).len();
        if text.chars().any(char::is_control)
            || text.len() > 64
            || (!whole_bound && segment_length.saturating_add(text.len()) > 32)
        {
            if let Some(state) = ctx.views.active_mut() {
                state.time_error = Some("Pasted time exceeds the field limit or contains control characters; draft retained".into());
            }
            return;
        }
        if whole_bound {
            let parts = split_time_draft(&text);
            if matches!(
                self.state.focus,
                TimeControl::EndDate
                    | TimeControl::EndClock
                    | TimeControl::EndZone
                    | TimeControl::EndZoneMenu
            ) {
                (
                    self.state.end_date,
                    self.state.end_clock,
                    self.state.end_zone,
                ) = parts;
                self.state.end_zone_custom = !is_time_zone_preset(&self.state.end_zone);
            } else {
                (
                    self.state.start_date,
                    self.state.start_clock,
                    self.state.start_zone,
                ) = parts;
                self.state.start_zone_custom = !is_time_zone_preset(&self.state.start_zone);
                self.state.focus = TimeControl::StartDate;
            }
            self.state.segment_cursor = usize::MAX;
        } else {
            if !matches!(
                self.state.focus,
                TimeControl::StartDate
                    | TimeControl::StartClock
                    | TimeControl::StartZone
                    | TimeControl::EndDate
                    | TimeControl::EndClock
                    | TimeControl::EndZone
            ) {
                return;
            }
            for ch in text.chars() {
                edit_dialog_time_segment(&mut self.state, Some(ch));
            }
        }
        self.state.window = TimeWindowChoice::Absolute;
        let (start, end, parts) = dialog_time_drafts(&self.state);
        if let Some(state) = ctx.views.active_mut() {
            store_time_drafts(state, start, end, parts);
            state.time_window_draft = TimeWindowChoice::Absolute;
            state.time_draft_touched = true;
            state.time_error = None;
            mark_time_edit(state);
        }
    }

    fn mouse(&mut self, kind: MouseEventKind, hit: Option<TimeHit>, ctx: &mut Ctx<'_>) -> Outcome {
        match kind {
            MouseEventKind::Down(MouseButton::Left) => match hit {
                // A click on a dropdown row names that row; the list is the
                // component's own geometry, not a layer (§5.2).
                Some(TimeHit::Choice(index)) => {
                    self.state.highlighted = index;
                    self.choose(ctx)
                }
                Some(TimeHit::Control(control)) => {
                    self.focus_control(control);
                    self.open_focused(ctx)
                }
                None => Outcome::Consumed,
            },
            MouseEventKind::ScrollUp => {
                self.scroll_body(-1);
                Outcome::Consumed
            }
            MouseEventKind::ScrollDown => {
                self.scroll_body(1);
                Outcome::Consumed
            }
            _ => Outcome::Consumed,
        }
    }
}

/// §4.3: the palette entries Time owns. The palette no longer needs to know
/// which dialog is focused to decide whether these apply.
const TIME_COMMANDS: &[(CommandId, CommandSpec)] = &[
    (
        CommandId::TimeClear,
        CommandSpec {
            id: CommandId::TimeClear,
            name: "Clear time window",
            description: "Remove the applied time constraint",
            category: "Time",
            aliases: &["all time", "reset range"],
            shortcut: None,
        },
    ),
    (
        CommandId::TimeAroundSelected,
        CommandSpec {
            id: CommandId::TimeAroundSelected,
            name: "Time around selected row",
            description: "Center a time range on the selected record",
            category: "Time",
            aliases: &["around", "context"],
            shortcut: None,
        },
    ),
    (
        CommandId::TimeBasisCapture,
        CommandSpec {
            id: CommandId::TimeBasisCapture,
            name: "Use capture time",
            description: "Filter using ingestion timestamps",
            category: "Time",
            aliases: &["received", "arrival"],
            shortcut: None,
        },
    ),
    (
        CommandId::TimeBasisEvent,
        CommandSpec {
            id: CommandId::TimeBasisEvent,
            name: "Use event time",
            description: "Filter using parsed event timestamps",
            category: "Time",
            aliases: &["timestamp", "parsed"],
            shortcut: None,
        },
    ),
    (
        CommandId::TimeBasisExtracted,
        CommandSpec {
            id: CommandId::TimeBasisExtracted,
            name: "Use extracted timestamp_utc",
            description: "Alt-U: filter using the accepted UTC timestamp enrichment",
            category: "Time",
            aliases: &["timestamp", "enrichment", "derived"],
            shortcut: None,
        },
    ),
    (
        CommandId::TimeRecentFive,
        CommandSpec {
            id: CommandId::TimeRecentFive,
            name: "Recent 5 minutes",
            description: "Apply the five-minute preset",
            category: "Time",
            aliases: &["5m", "five"],
            shortcut: None,
        },
    ),
    (
        CommandId::TimeRecentFifteen,
        CommandSpec {
            id: CommandId::TimeRecentFifteen,
            name: "Recent 15 minutes",
            description: "Apply the fifteen-minute preset",
            category: "Time",
            aliases: &["15m", "quarter hour"],
            shortcut: None,
        },
    ),
    (
        CommandId::TimeRecentHour,
        CommandSpec {
            id: CommandId::TimeRecentHour,
            name: "Recent 1 hour",
            description: "Apply the one-hour preset",
            category: "Time",
            aliases: &["60m", "1h", "hour"],
            shortcut: None,
        },
    ),
];

/// The Alt chord that reaches each command while the layer is on top; the
/// shortcut column used to be derived from the focus-specific key table.
fn time_command_shortcut(id: CommandId) -> Option<&'static str> {
    match id {
        CommandId::TimeClear => Some("Alt-C"),
        CommandId::TimeAroundSelected => Some("Alt-A"),
        CommandId::TimeBasisCapture => Some("Alt-P"),
        CommandId::TimeBasisEvent => Some("Alt-E"),
        CommandId::TimeBasisExtracted => Some("Alt-U"),
        CommandId::TimeRecentFive => Some("Alt-5"),
        CommandId::TimeRecentFifteen => Some("Alt-M"),
        CommandId::TimeRecentHour => Some("Alt-H"),
        _ => None,
    }
}

struct TimeFieldLayout<'a> {
    control: TimeControl,
    label: Rect,
    input: Rect,
    label_text: &'static str,
    value: &'a str,
    /// A dropdown renders its value with a chevron and takes no caret (§8.3).
    dropdown: bool,
}

struct TimeButtonLayout<'a> {
    control: TimeControl,
    rect: Rect,
    label: &'a str,
}

struct TimeEditorLayout<'a> {
    fields: Vec<TimeFieldLayout<'a>>,
    buttons: Vec<TimeButtonLayout<'a>>,
    controls: Vec<(Rect, TimeControl)>,
    /// Indented read-only lines: what a chosen field assumes, how much of the
    /// sample it covers, and why it is blocked. `true` marks a line the user
    /// has to weigh rather than merely read.
    notes: Vec<(Rect, String, bool)>,
    height: u16,
}

/// §4.2: the label column width every Time row shares.
const TIME_LABEL_WIDTH: u16 = 11;

fn time_editor_layout<'a>(
    area: Rect,
    dialog: &'a TimeState,
    basis_value: &'a str,
    window_value: &'a str,
    reading_value: &'a str,
) -> TimeEditorLayout<'a> {
    use TimeControl as C;
    let width = area.width.max(1);
    let mut fields = Vec::new();
    let mut buttons = Vec::new();
    let mut controls = Vec::new();
    let mut notes: Vec<(Rect, String, bool)> = Vec::new();
    let mut y = 0;
    let field_x = TIME_LABEL_WIDTH.saturating_add(FIELD_GUTTER).min(width);
    let push_dropdown = |y: u16,
                         control,
                         label_text,
                         value,
                         fields: &mut Vec<TimeFieldLayout<'a>>,
                         controls: &mut Vec<(Rect, C)>| {
        let input = Rect::new(field_x, y, width.saturating_sub(field_x), 1);
        fields.push(TimeFieldLayout {
            control,
            label: Rect::new(0, y, TIME_LABEL_WIDTH.min(width), 1),
            input,
            label_text,
            value,
            dropdown: true,
        });
        controls.push((input, control));
    };
    push_dropdown(
        y,
        C::Basis,
        "Time basis",
        basis_value,
        &mut fields,
        &mut controls,
    );
    y += 1;
    // §3: the confirmation for a chosen field sits directly under the basis it
    // qualifies, because nothing below it applies until it is resolved.
    if let Some(reading) = dialog.pending_reading() {
        push_dropdown(
            y,
            C::Reading,
            "Reading",
            reading_value,
            &mut fields,
            &mut controls,
        );
        y += 1;
        let indent = crate::dialog_layout::PANE_INDENT;
        let note_width = width.saturating_sub(indent).max(1);
        let mut push_note = |y: &mut u16, text: String, warn: bool| {
            notes.push((Rect::new(indent, *y, note_width, 1), text, warn));
            *y += 1;
        };
        push_note(&mut y, format!("Field: {}", reading.label), false);
        push_note(
            &mut y,
            match reading.coverage_percent {
                Some(percent) => format!(
                    "Coverage: {percent}% of {} sampled records",
                    dialog.recognition.sampled_records
                ),
                None => "Coverage: not validated".to_owned(),
            },
            false,
        );
        if let Some(blocked) = &reading.blocked {
            push_note(&mut y, format!("Blocked: {blocked}"), true);
        }
        for assumption in &reading.assumptions {
            push_note(&mut y, format!("Assumes: {assumption}"), true);
        }
        if reading.blocked.is_none() {
            let label = "Accept assumption";
            let rect = Rect::new(indent, y, button_width(label).min(note_width), 1);
            buttons.push(TimeButtonLayout {
                control: C::AcceptField,
                rect,
                label,
            });
            controls.push((rect, C::AcceptField));
            y += 1;
        }
    }
    push_dropdown(
        y,
        C::Window,
        "Window",
        window_value,
        &mut fields,
        &mut controls,
    );
    y += 2;
    let wide = width >= 56;
    for (
        row_label,
        date,
        clock,
        zone,
        zone_menu,
        zone_custom,
        date_value,
        clock_value,
        zone_value,
    ) in [
        (
            "Start",
            C::StartDate,
            C::StartClock,
            C::StartZone,
            C::StartZoneMenu,
            dialog.start_zone_custom,
            dialog.start_date.as_str(),
            dialog.start_clock.as_str(),
            dialog.start_zone.as_str(),
        ),
        (
            "End",
            C::EndDate,
            C::EndClock,
            C::EndZone,
            C::EndZoneMenu,
            dialog.end_zone_custom,
            dialog.end_date.as_str(),
            dialog.end_clock.as_str(),
            dialog.end_zone.as_str(),
        ),
    ] {
        if wide {
            let row_label_width = 6;
            let date_width = 10;
            let zone_width = 14.min(width.saturating_sub(row_label_width + date_width + 2));
            let clock_width = width
                .saturating_sub(row_label_width + date_width + zone_width + 2)
                .max(12);
            let label_rect = Rect::new(0, y, row_label_width, 1);
            fields.push(TimeFieldLayout {
                control: date,
                label: label_rect,
                input: Rect::new(row_label_width, y, date_width, 1),
                label_text: row_label,
                value: date_value,
                dropdown: false,
            });
            fields.push(TimeFieldLayout {
                control: clock,
                label: Rect::new(row_label_width + date_width, y, 1, 1),
                input: Rect::new(row_label_width + date_width + 1, y, clock_width, 1),
                label_text: "",
                value: clock_value,
                dropdown: false,
            });
            fields.push(TimeFieldLayout {
                control: zone,
                label: Rect::new(row_label_width + date_width + clock_width + 1, y, 1, 1),
                input: Rect::new(
                    row_label_width + date_width + clock_width + 2,
                    y,
                    zone_width.saturating_sub(5),
                    1,
                ),
                label_text: "",
                value: zone_value,
                dropdown: false,
            });
            let menu_width = button_width("▾").min(width);
            let menu_rect = Rect::new(width.saturating_sub(menu_width), y, menu_width, 1);
            buttons.push(TimeButtonLayout {
                control: zone_menu,
                rect: menu_rect,
                label: "▾",
            });
            controls.push((menu_rect, zone_menu));
            if !zone_custom {
                controls.retain(|(_, control)| *control != zone);
            }
            y += 1;
        } else {
            let date_label = if row_label == "Start" {
                "Start date"
            } else {
                "End date"
            };
            // §4.2: a reflowed group keeps the full compound label, never a
            // bare `time` or `zone` that no longer says which bound it is.
            let (clock_label, zone_label) = if row_label == "Start" {
                ("Start time", "Start zone")
            } else {
                ("End time", "End zone")
            };
            for (index, (control, label, value)) in [
                (date, date_label, date_value),
                (clock, clock_label, clock_value),
                (zone, zone_label, zone_value),
            ]
            .into_iter()
            .enumerate()
            {
                // One label column for the whole reflowed group, wide enough
                // for its longest compound label (§4.2/§4.4).
                let label_width = ([date_label, clock_label, zone_label]
                    .iter()
                    .map(|text| text.width() as u16)
                    .max()
                    .unwrap_or(6)
                    + 2)
                .min(width);
                let _ = index;
                fields.push(TimeFieldLayout {
                    control,
                    label: Rect::new(0, y, label_width, 1),
                    input: Rect::new(
                        label_width,
                        y,
                        width.saturating_sub(label_width + if control == zone { 5 } else { 0 }),
                        1,
                    ),
                    label_text: label,
                    value,
                    dropdown: false,
                });
                if control == zone {
                    let menu_width = button_width("▾").min(width);
                    let menu_rect = Rect::new(width.saturating_sub(menu_width), y, menu_width, 1);
                    buttons.push(TimeButtonLayout {
                        control: zone_menu,
                        rect: menu_rect,
                        label: "▾",
                    });
                    controls.push((menu_rect, zone_menu));
                    if !zone_custom {
                        controls.retain(|(_, saved)| *saved != zone);
                    }
                }
                y += 1;
            }
        }
    }
    for field in &fields {
        let editable = match field.control {
            C::StartZone => dialog.start_zone_custom,
            C::EndZone => dialog.end_zone_custom,
            _ => true,
        };
        if editable {
            controls.push((field.input, field.control));
        }
    }
    TimeEditorLayout {
        fields,
        buttons,
        controls,
        notes,
        height: y,
    }
}

impl TimeDialog {
    fn record(
        &mut self,
        controls: Vec<(Rect, TimeControl)>,
        choices: Vec<(Rect, usize)>,
        caret: Option<(u16, u16)>,
        surface: Surface,
    ) -> Surface {
        self.geometry = TimeGeometry { controls, choices };
        self.surface = Surface { caret, ..surface };
        self.surface
    }
}

impl Component for TimeDialog {
    type Hit = TimeHit;
    type Open = ();

    fn open(&mut self, _params: (), ctx: &mut Ctx<'_>) {
        self.open = true;
        self.open_dialog(ctx);
    }

    fn handle(&mut self, event: Event<TimeHit>, ctx: &mut Ctx<'_>) -> Outcome {
        match event {
            Event::Key(key) => self.key(key, ctx),
            Event::Paste(text) => {
                self.paste(text, ctx);
                Outcome::Consumed
            }
            Event::Mouse { kind, hit, .. } => self.mouse(kind, hit, ctx),
            // §5.3: Escape closes the innermost thing first, so an open
            // dropdown absorbs it rather than the whole dialog.
            Event::Dismiss => {
                if self.state.dropdown.take().is_some() {
                    return Outcome::Consumed;
                }
                self.open = false;
                Outcome::Close
            }
            Event::Command(id) => self.command(id, ctx),
            Event::View(_) | Event::Resize => Outcome::Ignored,
        }
    }

    fn commands(&self, views: &Views) -> Vec<CommandEntry> {
        let has_selected_row = views.active().is_some_and(|state| state.selected.is_some());
        TIME_COMMANDS
            .iter()
            .map(|(id, spec)| CommandEntry {
                spec: CommandSpec {
                    shortcut: self.open.then(|| time_command_shortcut(*id)).flatten(),
                    ..*spec
                },
                unavailable_reason: (!self.open)
                    .then_some("open Time window first")
                    .or((*id == CommandId::TimeAroundSelected && !has_selected_row)
                        .then_some("select a row first")),
            })
            .collect()
    }

    fn surface(&self) -> Surface {
        self.surface
    }

    fn hit(&self, point: (u16, u16)) -> Option<TimeHit> {
        // The anchored dropdown is drawn last and takes the point first.
        self.geometry
            .choices
            .iter()
            .find_map(|(rect, index)| contains(*rect, point).then_some(TimeHit::Choice(*index)))
            .or_else(|| {
                self.geometry.controls.iter().find_map(|(rect, control)| {
                    contains(*rect, point).then_some(TimeHit::Control(*control))
                })
            })
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &RenderCtx<'_>) -> Surface {
        use crate::dialog_layout::{DialogClass, DialogContent, content_width};
        use {TimeControl as C, TimeDropdown as D, TimeWindowChoice as W};
        let theme = ctx.theme;
        let styles = DialogStyles::new(theme);
        let ascii = ctx.ascii;
        let mut controls_hit: Vec<(Rect, TimeControl)> = Vec::new();
        let mut choices_hit: Vec<(Rect, usize)> = Vec::new();
        let mut caret_cell: Option<(u16, u16)> = None;
        // The dialog's own state is snapshotted for the frame because the layout
        // borrows its strings while the scroll it computes is written back.
        let dialog = self.state.clone();
        let Some(state) = ctx.views.active() else {
            self.geometry = TimeGeometry::default();
            self.surface = Surface::default();
            return self.surface;
        };
        let basis = match dialog.basis {
            TimeBasis::Capture => "Capture".to_owned(),
            TimeBasis::Extracted => "Extracted timestamp_utc (UTC RFC3339)".to_owned(),
            TimeBasis::Event => "Recognized event (RFC3339 normalized to UTC)".to_owned(),
            TimeBasis::Selected if dialog.field_label.is_empty() => "Chosen field".to_owned(),
            TimeBasis::Selected => format!("Field {}", dialog.field_label),
        };
        // The reading under review, named the way the dropdown names it.
        let reading_value = dialog
            .pending_reading()
            .map_or_else(String::new, |reading| {
                if reading.reading.is_empty() {
                    reading.label.clone()
                } else {
                    reading.reading.clone()
                }
            });
        let applied = match state.applied_capture_time_policy {
            Some(CaptureTimePolicy::Recent { seconds }) => {
                format!("rolling last {}", format_capture_duration(seconds))
            }
            Some(CaptureTimePolicy::Absolute(_)) => state.applied_capture_time.map_or_else(
                || "absolute pending".into(),
                |w| {
                    format!(
                        "absolute {} .. {}",
                        format_utc_nanos(w.start_unix_nanos),
                        format_utc_nanos(w.end_unix_nanos)
                    )
                },
            ),
            None => "all times".into(),
        };
        let window = match dialog.window {
            W::All => "All time".into(),
            W::Absolute => "Absolute".into(),
            W::Recent(s) if matches!(s, 300 | 900 | 3600) => {
                format!("Last {}", format_capture_duration(s))
            }
            W::Recent(s) => format!("Custom last {}", format_capture_duration(s)),
            W::AroundSelected => "Around selected".into(),
        };
        let updating = state.time_update_pending();
        let missing = match dialog.basis {
            TimeBasis::Capture => dialog.anchored_capture_nanos.is_none(),
            TimeBasis::Event => dialog.anchored_event_nanos.is_none(),
            TimeBasis::Extracted => dialog.anchored_extracted_nanos.is_none(),
            TimeBasis::Selected => dialog.anchored_selected_nanos.is_none(),
        };
        let reason = if dialog.window == W::AroundSelected && missing {
            "Around selected is disabled: the opening record has no timestamp in the chosen basis."
        } else {
            "Bounds are half-open. UTC and numeric offsets are normalized to UTC; named zones are not supported."
        };
        // §7.4: one message row and one state word, retiring the boxed
        // `Applied: Applied` stutter.
        let (state_word, sentence) = if let Some(error) = &dialog.field_error {
            (MessageState::Error, error.clone())
        } else if dialog.pending_field.is_some() {
            (
                MessageState::Pending,
                "this reading is not applied until you accept its assumption".to_owned(),
            )
        } else if let Some(error) = &state.time_error {
            (MessageState::Error, error.clone())
        } else if updating {
            (
                MessageState::Updating,
                "the last applied window stays active".to_owned(),
            )
        } else {
            (MessageState::Applied, applied)
        };
        let recognize = if ascii {
            "Agent Recognize timestamp"
        } else {
            "🧠 Recognize timestamp"
        };
        let width = content_width(area, DialogClass::M);
        // §7.4 caps the message at two rows, but a rejected window carries a long
        // diagnostic. When it does not fit, the full text becomes body content so
        // the body's own scroll reaches it; clipping it away is not an inspection
        // path.
        let message_width = usize::from(width.saturating_sub(MESSAGE_SENTENCE_COLUMN)).max(1);
        let wrapped_message = wrap_sentence(&sentence, message_width, usize::MAX);
        let message_overflows = wrapped_message.len() > usize::from(message_rows(&sentence, width));
        let diagnostic_lines: Vec<String> = if message_overflows {
            wrap_sentence(
                &sentence,
                usize::from(width.saturating_sub(2)).max(1),
                usize::MAX,
            )
        } else {
            Vec::new()
        };
        // Measure the body at the class content width before the popup exists.
        let measured = time_editor_layout(
            Rect::new(0, 0, width, 1),
            &dialog,
            basis.as_str(),
            window.as_str(),
            reading_value.as_str(),
        );
        let diagnostic_rows = if diagnostic_lines.is_empty() {
            0
        } else {
            u16::try_from(diagnostic_lines.len())
                .unwrap_or(u16::MAX)
                .saturating_add(2)
        };
        let action_labels = ["Apply", "Clear", recognize];
        let content = DialogContent {
            header: 0,
            body: measured.height.saturating_add(diagnostic_rows),
            message: message_rows(&sentence, width),
            help: help_rows(reason, width),
            actions: packed_button_rows(width, &action_labels),
        };
        let regions =
            dialog_frame_regions(frame, area, DialogClass::M, "Time window", &content, theme);
        // §5.2 containment is measured against everything the layer drew. An
        // anchored dropdown is allowed to extend past the dialog (§5.3), so the
        // popup rect is the union; `interior` stays the dialog's, because that is
        // the text-selection surface.
        let mut surface = Surface {
            popup: regions.popup,
            interior: regions.interior,
            caret: None,
            scrollable: true,
            text_focus: self.editing_segment().is_some(),
        };
        let inner = regions.content;
        let time_layout = time_editor_layout(
            Rect::new(inner.x, inner.y, inner.width, 1),
            &dialog,
            basis.as_str(),
            window.as_str(),
            reading_value.as_str(),
        );
        // §9: the body scrolls under a scrollbar; the `▲ Scroll up` /
        // `▼ Scroll down` pseudo-buttons are retired.
        let body_height = time_layout.height.saturating_add(diagnostic_rows);
        let overflowing = body_height > regions.body.height;
        let viewport = Rect::new(
            regions.body.x,
            regions.body.y,
            regions.body.width.saturating_sub(u16::from(overflowing)),
            regions.body.height,
        );
        let max_scroll = usize::from(body_height.saturating_sub(viewport.height));
        let focus_row = time_layout
            .controls
            .iter()
            .find(|(_, control)| *control == dialog.focus)
            .map(|(rect, _)| usize::from(rect.y));
        let mut scroll = dialog.scroll.min(max_scroll);
        if dialog.reveal_focus
            && let Some(focus_row) = focus_row
        {
            if focus_row < scroll {
                scroll = focus_row;
            }
            if focus_row >= scroll.saturating_add(usize::from(viewport.height)) {
                scroll = focus_row + 1 - usize::from(viewport.height);
            }
        }
        self.state.scroll = scroll;
        self.state.reveal_focus = false;
        self.state.has_overflow = max_scroll > 0;
        let project = |rect: Rect| {
            let y = usize::from(rect.y);
            (y >= scroll && y < scroll + usize::from(viewport.height)).then(|| {
                Rect::new(
                    viewport.x + rect.x,
                    viewport.y + (y - scroll) as u16,
                    rect.width.min(viewport.width.saturating_sub(rect.x)),
                    1,
                )
            })
        };
        for field in &time_layout.fields {
            let Some(label_rect) = project(field.label) else {
                continue;
            };
            let Some(input_rect) = project(field.input) else {
                continue;
            };
            let focused = dialog.focus == field.control;
            frame.render_widget(
                Paragraph::new(field.label_text).style(if focused {
                    styles.shortcut
                } else {
                    styles.label
                }),
                label_rect,
            );
            if field.dropdown {
                // §8.3: a dropdown is a field with a chevron in its last cell, in
                // the same column as every other field.
                let style = if focused {
                    styles.selection
                } else {
                    styles.input
                };
                InputSurface { style }.render(input_rect, frame.buffer_mut());
                frame.render_widget(
                    Paragraph::new(truncated(
                        field.value,
                        usize::from(input_rect.width.saturating_sub(2)),
                    ))
                    .style(style),
                    input_rect,
                );
                frame.render_widget(
                    Paragraph::new(if ascii { "v" } else { "▾" }).style(
                        Style::default().fg(theme.accent).bg(if focused {
                            theme.selection_bg
                        } else {
                            theme.input_bg
                        }),
                    ),
                    Rect::new(input_rect.right().saturating_sub(1), input_rect.y, 1, 1),
                );
                controls_hit.push((input_rect, field.control));
                continue;
            }
            let is_custom_zone = match field.control {
                C::StartZone => dialog.start_zone_custom,
                C::EndZone => dialog.end_zone_custom,
                _ => true,
            };
            let display = if is_custom_zone {
                field.value.to_owned()
            } else if field.value == "Z" {
                "UTC".into()
            } else {
                format!("UTC{}", field.value)
            };
            let caret = if focused {
                dialog.segment_cursor.min(field.value.chars().count())
            } else {
                field.value.chars().count()
            };
            let (visible_value, caret_column) = if focused {
                time_input_window(&display, caret, usize::from(input_rect.width))
            } else {
                (clipped_width(&display, usize::from(input_rect.width)), 0)
            };
            let style = if is_custom_zone {
                styles.input
            } else {
                styles.description
            };
            frame.render_widget(Paragraph::new(visible_value).style(style), input_rect);
            if focused && is_custom_zone && dialog.dropdown.is_none() && input_rect.width > 0 {
                let x = input_rect
                    .x
                    .saturating_add(caret_column as u16)
                    .min(input_rect.right().saturating_sub(1));
                frame.render_widget(
                    Block::default().style(Style::default().bg(theme.cursor)),
                    Rect::new(x, input_rect.y, 1, 1),
                );
                frame.set_cursor_position((x, input_rect.y));
                caret_cell = Some((x, input_rect.y));
            }
            if is_custom_zone {
                controls_hit.push((input_rect, field.control));
            }
        }
        for button in &time_layout.buttons {
            let Some(rect) = project(button.rect) else {
                continue;
            };
            let focused = dialog.focus == button.control;
            render_button(frame, rect, button.label, focused, false, theme);
            controls_hit.push((rect, button.control));
        }
        for (logical, text, warn) in &time_layout.notes {
            let Some(rect) = project(*logical) else {
                continue;
            };
            frame.render_widget(
                Paragraph::new(truncated(text, usize::from(rect.width))).style(if *warn {
                    styles.error
                } else {
                    styles.description
                }),
                rect,
            );
        }
        if !diagnostic_lines.is_empty() {
            let heading = Rect::new(0, time_layout.height.saturating_add(1), viewport.width, 1);
            if let Some(rect) = project(heading) {
                frame.render_widget(
                    Paragraph::new("Diagnostics").style(styles.label.add_modifier(Modifier::BOLD)),
                    rect,
                );
            }
            for (index, line) in diagnostic_lines.iter().enumerate() {
                let logical = Rect::new(
                    crate::dialog_layout::PANE_INDENT,
                    time_layout
                        .height
                        .saturating_add(2)
                        .saturating_add(u16::try_from(index).unwrap_or(u16::MAX)),
                    viewport
                        .width
                        .saturating_sub(crate::dialog_layout::PANE_INDENT),
                    1,
                );
                if let Some(rect) = project(logical) {
                    frame.render_widget(Paragraph::new(line.clone()).style(styles.error), rect);
                }
            }
        }
        if overflowing {
            render_scrollbar(
                frame,
                Rect::new(
                    regions.body.right().saturating_sub(1),
                    regions.body.y,
                    1,
                    regions.body.height,
                ),
                scroll,
                max_scroll,
                theme,
                ascii,
            );
        }
        render_message(frame, regions.message, state_word, &sentence, theme, ascii);
        render_help_text(frame, regions.help, reason, theme);
        // §3: the actions live in their own region, after the fields they act on.
        let action_controls = [C::Apply, C::Clear, C::Recognize];
        let focused_action = action_controls
            .iter()
            .position(|control| *control == dialog.focus);
        for (index, rect) in render_action_row(
            frame,
            regions.actions,
            &action_labels,
            focused_action,
            &[],
            theme,
        ) {
            controls_hit.push((rect, action_controls[index]));
        }
        if let Some(dropdown) = dialog.dropdown {
            let choices: Vec<String> = match dropdown {
                D::Basis => time_basis_entries(&dialog)
                    .iter()
                    .map(TimeBasisEntry::label)
                    .collect(),
                D::Reading => dialog
                    .pending_readings()
                    .iter()
                    .map(|reading| {
                        let mut label = if reading.reading.is_empty() {
                            reading.label.clone()
                        } else {
                            reading.reading.clone()
                        };
                        if reading.blocked.is_some() {
                            label.push_str(" · blocked");
                        } else if reading.assumptions.is_empty() {
                            label.push_str(" · no assumption");
                        } else {
                            label.push_str(" · needs an assumption");
                        }
                        label
                    })
                    .collect(),
                D::Window => dialog
                    .window_choices
                    .iter()
                    .map(|choice| match choice {
                        W::All => "All time".into(),
                        W::Absolute => "Absolute".into(),
                        W::Recent(s) if matches!(s, 300 | 900 | 3600) => {
                            format!("Last {}", format_capture_duration(*s))
                        }
                        W::Recent(s) => format!("Custom last {}", format_capture_duration(*s)),
                        W::AroundSelected => "Around selected".into(),
                    })
                    .collect(),
                D::StartZone | D::EndZone => time_zone_choices()
                    .iter()
                    .map(|(label, _)| (*label).into())
                    .chain(std::iter::once("Custom offset…".into()))
                    .collect(),
            };
            let selected = dialog.highlighted.min(choices.len().saturating_sub(1));
            let anchor = time_layout
                .controls
                .iter()
                .find(|(_, control)| {
                    *control
                        == match dropdown {
                            D::Basis => C::Basis,
                            D::Window => C::Window,
                            D::StartZone => C::StartZoneMenu,
                            D::EndZone => C::EndZoneMenu,
                            D::Reading => C::Reading,
                        }
                })
                .map_or(Rect::default(), |(rect, _)| *rect);
            let anchor_y = viewport.y + usize::from(anchor.y).saturating_sub(scroll) as u16;
            let w = choices
                .iter()
                .map(|s| s.as_str().width())
                .max()
                .unwrap_or(1) as u16
                + 4;
            // §10: an anchored popup is not a dialog. It is drawn last and may
            // extend past the dialog it belongs to, so it is bounded by the frame
            // rather than by a body that now follows its content.
            let frame_area = area;
            let box_width = w.min(frame_area.width).max(3.min(frame_area.width));
            let dropdown_x = viewport
                .x
                .saturating_add(anchor.x)
                .min(frame_area.right().saturating_sub(box_width));
            let below = frame_area
                .bottom()
                .saturating_sub(anchor_y.saturating_add(1));
            let above = anchor_y.saturating_sub(frame_area.y);
            // §5.1 class A: at most eight options plus the border.
            let desired_height = (choices.len().min(8) as u16 + 2).min(frame_area.height);
            let place_below = below >= desired_height || below >= above;
            let available = if place_below { below } else { above };
            let mut box_height = desired_height.min(available);
            let mut dropdown_y = if place_below {
                anchor_y.saturating_add(1)
            } else {
                anchor_y.saturating_sub(box_height)
            };
            if box_height < 3 && frame_area.height >= 3 {
                box_height = desired_height.min(frame_area.height).max(3);
                dropdown_y = anchor_y
                    .saturating_add(1)
                    .min(frame_area.bottom().saturating_sub(box_height))
                    .max(frame_area.y);
            }
            let box_area = Rect::new(dropdown_x, dropdown_y, box_width, box_height);
            if box_area.width < 3 || box_area.height < 3 {
                return self.record(controls_hit, choices_hit, caret_cell, surface);
            }
            surface.popup = surface.popup.union(box_area);
            frame.render_widget(Clear, box_area);
            let choice_height = usize::from(box_area.height - 2);
            let choice_scroll = selected.saturating_add(1).saturating_sub(choice_height);
            frame.render_widget(
                List::new(
                    choices
                        .iter()
                        .enumerate()
                        .skip(choice_scroll)
                        .map(|(index, value)| {
                            ListItem::new(value.as_str()).style(if index == selected {
                                styles.selection
                            } else {
                                button_style(theme, false, false)
                            })
                        })
                        .collect::<Vec<_>>(),
                )
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(styles.label),
                ),
                box_area,
            );
            for (offset, i) in (choice_scroll..choices.len())
                .take(choice_height)
                .enumerate()
            {
                choices_hit.push((
                    Rect::new(
                        box_area.x + 1,
                        box_area.y + 1 + offset as u16,
                        box_area.width.saturating_sub(2),
                        1,
                    ),
                    i,
                ));
            }
        }
        self.record(controls_hit, choices_hit, caret_cell, surface)
    }
}

fn contains(area: Rect, point: (u16, u16)) -> bool {
    point.0 >= area.x && point.0 < area.right() && point.1 >= area.y && point.1 < area.bottom()
}

impl TimeDialog {
    /// A palette command reaches the layer as `Event::Command` (§4.3).
    fn command(&mut self, id: CommandId, ctx: &mut Ctx<'_>) -> Outcome {
        match id {
            CommandId::TimeClear => self.clear(ctx),
            CommandId::TimeAroundSelected => {
                self.around_selected(ctx);
                Outcome::Consumed
            }
            CommandId::TimeBasisCapture => {
                self.set_basis(TimeBasis::Capture, ctx);
                Outcome::Consumed
            }
            CommandId::TimeBasisEvent => {
                self.set_basis(TimeBasis::Event, ctx);
                Outcome::Consumed
            }
            CommandId::TimeBasisExtracted => {
                self.set_basis(TimeBasis::Extracted, ctx);
                Outcome::Consumed
            }
            CommandId::TimeRecentFive => self.set_recent(300, ctx),
            CommandId::TimeRecentFifteen => self.set_recent(900, ctx),
            CommandId::TimeRecentHour => self.set_recent(3600, ctx),
            _ => Outcome::Ignored,
        }
    }
}
