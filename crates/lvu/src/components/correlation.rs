//! Correlate across sources (`docs/dialog-system.md` §12.21) as a component
//! (`docs/component-model.md` §6.3 step 14, the last conversion).
//!
//! Fields hands this layer the frozen record identity and the field with
//! `Outcome::Replace`, and from then on everything about the correlation is
//! here: the bounded request queue, the completion fences, the pending lookup
//! and the per-source mapping. The shell keeps nothing but forwarders for
//! `lvu-app`'s completion calls, so `Ctx::correlating` — the §7.2 exception
//! that let Fields freeze itself while the shell owned the queue — is gone.
//!
//! Two shapes worth naming. The lookup is a live region (§5.2.1): the source
//! list arrives asynchronously, so its rows are reserved from the open sources
//! at open time and the popup rect does not move when the result lands. And
//! `[ Cancel ]` is a verb, not a dismissal (§7.5): it aborts the lookup or the
//! accept in flight, and only when nothing is running does it merely close.

use std::collections::{BTreeMap, HashSet};

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use lvu_core::{ExactScalar, FieldCorrelation};
use ratatui::{
    Frame,
    layout::{Margin, Rect},
    style::{Modifier, Style},
    widgets::{Block, Borders, Paragraph},
};

use crate::app::Views;
use crate::command_palette::CommandId;
use crate::component::{
    CommandEntry, CommandSpec, Component, Ctx, Event, Outbox, Outcome, RenderCtx, Surface,
};
use crate::dialog_controls::{ActionRow, DialogStyles};
use crate::dialog_layout::{DialogClass, DialogContent, MIN_LIVE_ROWS, content_width, pane};
use crate::provider::RowId;
use crate::theme::Theme;
use crate::ui::{
    MessageState, clear_themed, clipped_width, dialog_frame_regions, help_rows, message_rows,
    packed_button_rows, render_actions, render_help_text, render_message, truncated,
};

/// The legacy `MAX_CORRELATION_REQUESTS`: distinct generations that may be in
/// flight — queued, delivered or awaiting a cancellation — at once.
pub const MAX_CORRELATION_REQUESTS: usize = 8;

/// The explicit "this source does not carry the value" choice.
pub const NOT_CORRELATED: &str = "Not correlated";

const HELP: &str =
    "Sources name the same identity differently; unmapped sources contribute no records.";

/// §8.10: the action row, `&` markers included. One list for `render`, the
/// shell's mnemonic resolver and `press_action`, so the underlined letter and
/// the key that presses the button cannot disagree.
const BUTTONS: [&str; 2] = ["Co&rrelate", "&Cancel"];
const BUTTON_CONTROLS: [CorrelationControl; 2] =
    [CorrelationControl::Correlate, CorrelationControl::Cancel];

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CorrelationRequest {
    Resolve {
        generation: u64,
        origin_view_id: String,
        row_id: RowId,
        field: String,
    },
    Cancel {
        generation: u64,
        origin_view_id: String,
    },
    /// The user accepted an explicit per-source mapping. The controller opens
    /// the correlated view; nothing about the origin view changes.
    Accept {
        generation: u64,
        origin_view_id: String,
        name: String,
        correlation: FieldCorrelation,
    },
}

impl CorrelationRequest {
    fn generation(&self) -> u64 {
        match self {
            CorrelationRequest::Resolve { generation, .. }
            | CorrelationRequest::Cancel { generation, .. }
            | CorrelationRequest::Accept { generation, .. } => *generation,
        }
    }
}

/// One source's choice in the mapping. `chosen` is `None` until the user picks
/// a field: a correlation never infers a name for a source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CorrelationSourceChoice {
    pub source_id: String,
    pub name: String,
    /// Field names observed in a bounded sample of this source.
    pub fields: Vec<String>,
    pub chosen: Option<String>,
    /// The sample stopped before the journal ended, so `fields` may omit a
    /// field this source really has.
    pub incomplete: bool,
}

/// Which control has focus. UI-only state, so it lives here (§7.3).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CorrelationControl {
    Sources,
    Correlate,
    Cancel,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CorrelationHit {
    Row(usize),
    Control(CorrelationControl),
    /// One option in the anchored field popup.
    Choice(usize),
}

/// What Fields resolved before handing over: the frozen record and the field
/// whose value is followed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CorrelationOpen {
    pub row: RowId,
    pub field: String,
}

/// The per-source mapping once the lookup has answered.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CorrelationMapping {
    pub value: ExactScalar,
    /// How the value reads in the dialog. Presentation only; the predicate
    /// always uses the typed `value`.
    pub value_label: String,
    pub sources: Vec<CorrelationSourceChoice>,
    pub selected: usize,
    /// The §8.3 popup for the selected source, and its highlighted option.
    /// Option 0 is always `Not correlated`.
    pub popup: Option<usize>,
    /// Set while the controller is opening the accepted view.
    pub submitting: bool,
}

impl CorrelationMapping {
    /// The mapping as it stands, or the reason it cannot be accepted yet.
    pub fn correlation(&self, field: &str) -> Result<FieldCorrelation, String> {
        let mapped: BTreeMap<String, String> = self
            .sources
            .iter()
            .filter_map(|source| {
                source
                    .chosen
                    .clone()
                    .map(|field| (source.source_id.clone(), field))
            })
            .collect();
        if mapped.is_empty() {
            return Err("choose the field that carries this value in at least one source".into());
        }
        FieldCorrelation::new(field.to_owned(), self.value.clone(), mapped)
            .map_err(|error| error.to_string())
    }

    /// `Not correlated` plus this source's observed names.
    pub fn options(&self, index: usize) -> Vec<String> {
        let mut options = vec![NOT_CORRELATED.to_owned()];
        if let Some(source) = self.sources.get(index) {
            options.extend(source.fields.iter().cloned());
        }
        options
    }

    pub fn mapped_sources(&self) -> usize {
        self.sources
            .iter()
            .filter(|source| source.chosen.is_some())
            .count()
    }
}

/// The lookup this layer is waiting on, kept until the adapter answers it so a
/// stale completion — for a superseded generation or another view — is
/// recognised and dropped without releasing capacity early.
#[derive(Clone, Debug, Eq, PartialEq)]
struct PendingLookup {
    generation: u64,
    origin_view_id: String,
}

#[derive(Clone, Debug, Default)]
struct Geometry {
    rows: Vec<(Rect, usize)>,
    controls: Vec<(Rect, CorrelationControl)>,
    choices: Vec<(Rect, usize)>,
    field_rects: Vec<Rect>,
    body: Option<Rect>,
}

#[derive(Debug)]
pub struct CorrelationDialog {
    /// Drained by `lvu-app`; completions come back through `finish`,
    /// `resolved`, `accepted` and `accept_failed` (§2.4).
    pub outbox: Outbox<CorrelationRequest>,
    open: bool,
    /// The fence of the session on screen: first its lookup, then its mapping.
    generation: u64,
    origin_view_id: String,
    /// The field name in the source the record came from.
    field: String,
    /// The lookup still unanswered for the session on screen, if any.
    lookup: Option<PendingLookup>,
    /// Every generation the adapter has not answered yet, including ones this
    /// layer already abandoned; it is what the request bound counts.
    unanswered: HashSet<u64>,
    mapping: Option<CorrelationMapping>,
    control: CorrelationControl,
    error: Option<String>,
    /// §5.2.1: body rows reserved at open from the sources the view could map,
    /// so the frame does not change height when the lookup answers.
    reserved_rows: usize,
    /// First visible source row, so the selection stays inside the reserved
    /// rows when there are more sources than rows.
    top: usize,
    geometry: Geometry,
    surface: Surface,
}

impl Default for CorrelationDialog {
    fn default() -> Self {
        Self {
            outbox: Outbox::new(MAX_CORRELATION_REQUESTS),
            open: false,
            generation: 0,
            origin_view_id: String::new(),
            field: String::new(),
            lookup: None,
            unanswered: HashSet::new(),
            mapping: None,
            control: CorrelationControl::Sources,
            error: None,
            reserved_rows: 0,
            top: 0,
            geometry: Geometry::default(),
            surface: Surface::default(),
        }
    }
}

fn contains(rect: Rect, point: (u16, u16)) -> bool {
    point.0 >= rect.x && point.0 < rect.right() && point.1 >= rect.y && point.1 < rect.bottom()
}

/// `service = "api"`, bounded so a long value cannot become a view name that
/// no list can render.
pub fn correlation_view_name(field: &str, value_label: &str) -> String {
    let mut name = format!("{field} = {value_label}");
    if name.chars().count() > 48 {
        name = name.chars().take(47).collect::<String>();
        name.push('…');
    }
    name
}

impl CorrelationDialog {
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// The lookup for the session on screen has not answered yet.
    pub fn lookup_pending(&self) -> bool {
        self.open && self.lookup.is_some()
    }

    /// The per-source mapping, once the lookup has answered.
    pub fn mapping(&self) -> Option<&CorrelationMapping> {
        self.open.then_some(self.mapping.as_ref()).flatten()
    }

    pub fn control(&self) -> CorrelationControl {
        self.control
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn field(&self) -> &str {
        &self.field
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn origin_view_id(&self) -> &str {
        &self.origin_view_id
    }

    /// Distinct generations in flight: unanswered lookups plus whatever is
    /// still queued. The bound counts sessions, not messages.
    fn in_flight(&self) -> usize {
        // The outbox is drained by `lvu-app`, and a generation whose Resolve is
        // still queued is also in `unanswered`, so the set is the count; the
        // queue only adds generations whose completion was already dropped.
        self.unanswered.len()
    }

    /// The §8.9 default: the one verb in the row. Read by both `render` (which
    /// button carries the fill) and the Enter arm, so they cannot disagree.
    pub fn default_control() -> CorrelationControl {
        CorrelationControl::Correlate
    }

    /// §2.4's fence, extended by the view: a completion for the generation on
    /// screen counts only while the origin view is still the active one.
    pub fn is_current(&self, generation: u64, origin_view_id: &str, active: Option<&str>) -> bool {
        self.open
            && self.generation == generation
            && self.origin_view_id == origin_view_id
            && active == Some(origin_view_id)
            && self
                .lookup
                .as_ref()
                .is_some_and(|lookup| lookup.generation == generation)
    }

    /// Drop the queued messages of a generation the adapter will never see.
    fn forget_queued(&mut self, generation: u64) {
        let keep: Vec<CorrelationRequest> = self
            .outbox
            .take()
            .into_iter()
            .filter(|request| request.generation() != generation)
            .collect();
        for request in keep {
            // Re-queuing what was already accepted cannot exceed the bound.
            let _ = self.outbox.push(request);
        }
    }

    /// The lookup ended without a mapping. `Err` is the adapter's reason and
    /// stays on screen as the dialog's error state; `Ok` is a notice the shell
    /// shows after closing the layer. Returns whether the completion was
    /// current; a stale one only releases its capacity.
    pub fn finish(
        &mut self,
        generation: u64,
        origin_view_id: &str,
        active: Option<&str>,
        result: Result<String, String>,
    ) -> Option<Result<String, String>> {
        if !self.unanswered.remove(&generation) {
            return None;
        }
        self.forget_queued(generation);
        let current = self.is_current(generation, origin_view_id, active);
        if !current {
            return None;
        }
        self.lookup = None;
        match result {
            Ok(notice) => {
                self.open = false;
                Some(Ok(notice))
            }
            Err(error) => {
                self.error = Some(format!("correlation unavailable: {error}"));
                self.control = CorrelationControl::Cancel;
                Some(Err(error))
            }
        }
    }

    /// The lookup resolved: the layer moves from pending to mapping. Fenced
    /// like `finish`; an answer with no source to map is a failure with a
    /// reason, not an empty list.
    #[allow(clippy::too_many_arguments)]
    pub fn resolved(
        &mut self,
        generation: u64,
        origin_view_id: &str,
        active: Option<&str>,
        field: String,
        value: ExactScalar,
        value_label: String,
        sources: Vec<CorrelationSourceChoice>,
    ) -> bool {
        if !self.is_current(generation, origin_view_id, active) {
            // Still an answer: the capacity it held is released.
            self.unanswered.remove(&generation);
            self.forget_queued(generation);
            return false;
        }
        if sources.is_empty() {
            return self
                .finish(
                    generation,
                    origin_view_id,
                    active,
                    Err("no open source can be correlated".into()),
                )
                .is_some();
        }
        self.unanswered.remove(&generation);
        self.lookup = None;
        self.field = field;
        self.error = None;
        self.control = CorrelationControl::Sources;
        self.top = 0;
        self.mapping = Some(CorrelationMapping {
            value,
            value_label,
            sources,
            selected: 0,
            popup: None,
            submitting: false,
        });
        true
    }

    /// The controller could not open the accepted view. The dialog keeps the
    /// whole mapping so the user can adjust it; the origin view is untouched.
    pub fn accept_failed(&mut self, generation: u64, message: String) -> bool {
        if !self.open || self.generation != generation {
            return false;
        }
        let Some(mapping) = self.mapping.as_mut() else {
            return false;
        };
        mapping.submitting = false;
        self.error = Some(message);
        true
    }

    /// The correlated view exists: the layer is done. The shell pops it and
    /// shows the notice.
    pub fn accepted(&mut self, generation: u64) -> bool {
        if !self.open || self.generation != generation || self.mapping.is_none() {
            return false;
        }
        self.open = false;
        self.mapping = None;
        true
    }

    /// The origin view is going away or is no longer active. A lookup for it
    /// is abandoned; a mapping stays, because it is accepted state or nothing
    /// and the controller fences the accept against the view. Returns whether
    /// the layer has nothing left to show.
    pub fn cancel_for_view(&mut self, view_id: &str) -> bool {
        if self
            .lookup
            .as_ref()
            .is_some_and(|lookup| lookup.origin_view_id == view_id)
        {
            self.abandon_lookup();
            self.open = false;
            return true;
        }
        false
    }

    /// Abort the lookup on screen. A Resolve the adapter has not been handed
    /// is simply withdrawn; one it has is followed by a Cancel, and its
    /// generation stays counted until the adapter answers.
    fn abandon_lookup(&mut self) {
        let Some(lookup) = self.lookup.take() else {
            return;
        };
        let queued = self.outbox.take();
        let mut withdrawn = false;
        let mut cancel_queued = false;
        for request in queued {
            match &request {
                CorrelationRequest::Resolve { generation, .. }
                    if *generation == lookup.generation =>
                {
                    withdrawn = true;
                }
                CorrelationRequest::Cancel { generation, .. }
                    if *generation == lookup.generation =>
                {
                    cancel_queued = true;
                    let _ = self.outbox.push(request);
                }
                _ => {
                    let _ = self.outbox.push(request);
                }
            }
        }
        if withdrawn {
            self.unanswered.remove(&lookup.generation);
        } else if !cancel_queued {
            let _ = self.outbox.push(CorrelationRequest::Cancel {
                generation: lookup.generation,
                origin_view_id: lookup.origin_view_id,
            });
        }
    }

    /// `[ Cancel ]` and Escape: a verb while something runs, a close otherwise.
    fn cancel(&mut self) -> Outcome {
        self.abandon_lookup();
        if let Some(mapping) = self.mapping.as_mut()
            && mapping.submitting
        {
            // The accept is a synchronous controller call answered in the same
            // drain; there is nothing to withdraw, so the layer closes and the
            // late answer is fenced by the generation it names.
            mapping.submitting = false;
        }
        self.open = false;
        self.mapping = None;
        Outcome::Close
    }

    fn submit(&mut self, ctx: &mut Ctx<'_>) -> Outcome {
        let Some(mapping) = self.mapping.as_mut() else {
            // Nothing to accept while the lookup runs; the message says so.
            return Outcome::Consumed;
        };
        if mapping.submitting {
            return Outcome::Consumed;
        }
        let correlation = match mapping.correlation(&self.field) {
            Ok(value) => value,
            Err(error) => {
                self.error = Some(error);
                return Outcome::Consumed;
            }
        };
        let name = correlation_view_name(&self.field, &mapping.value_label);
        let request = CorrelationRequest::Accept {
            generation: self.generation,
            origin_view_id: self.origin_view_id.clone(),
            name,
            correlation,
        };
        if self.outbox.push(request).is_err() {
            ctx.notice("correlation request queue is full; try again shortly");
            return Outcome::Consumed;
        }
        mapping.submitting = true;
        self.error = None;
        Outcome::Consumed
    }

    fn move_selection(&mut self, delta: i32) {
        let Some(mapping) = self.mapping.as_mut() else {
            return;
        };
        if mapping.submitting {
            return;
        }
        match mapping.popup {
            Some(highlighted) => {
                let count = mapping
                    .sources
                    .get(mapping.selected)
                    .map_or(1, |source| source.fields.len() + 1);
                mapping.popup = Some(
                    (highlighted as i32 + delta).clamp(0, count.saturating_sub(1) as i32) as usize,
                );
            }
            None if self.control == CorrelationControl::Sources => {
                let count = mapping.sources.len();
                mapping.selected = (mapping.selected as i32 + delta)
                    .clamp(0, count.saturating_sub(1) as i32)
                    as usize;
            }
            None => {}
        }
    }

    fn move_control(&mut self, delta: i32) {
        if self
            .mapping
            .as_ref()
            .is_some_and(|mapping| mapping.submitting || mapping.popup.is_some())
        {
            return;
        }
        let order = [
            CorrelationControl::Sources,
            CorrelationControl::Correlate,
            CorrelationControl::Cancel,
        ];
        let index = order
            .iter()
            .position(|control| *control == self.control)
            .unwrap_or(0) as i32;
        self.control = order[(index + delta).rem_euclid(order.len() as i32) as usize];
    }

    fn open_popup(&mut self) {
        let Some(mapping) = self.mapping.as_mut() else {
            return;
        };
        let current = mapping
            .sources
            .get(mapping.selected)
            .and_then(|source| {
                source.chosen.as_ref().and_then(|chosen| {
                    source
                        .fields
                        .iter()
                        .position(|field| field == chosen)
                        .map(|index| index + 1)
                })
            })
            .unwrap_or(0);
        mapping.popup = Some(current);
    }

    fn commit_popup(&mut self, highlighted: usize) {
        let Some(mapping) = self.mapping.as_mut() else {
            return;
        };
        mapping.popup = None;
        let index = mapping.selected;
        let chosen = (highlighted > 0)
            .then(|| {
                mapping
                    .sources
                    .get(index)
                    .and_then(|source| source.fields.get(highlighted - 1).cloned())
            })
            .flatten();
        if let Some(source) = mapping.sources.get_mut(index) {
            source.chosen = chosen;
        }
        self.error = None;
    }

    /// Enter: the popup commits, a source row opens its popup, a button runs,
    /// and anything else runs the default (§8.9).
    fn activate(&mut self, ctx: &mut Ctx<'_>) -> Outcome {
        if self
            .mapping
            .as_ref()
            .is_some_and(|mapping| mapping.submitting)
        {
            return Outcome::Consumed;
        }
        if let Some(highlighted) = self.mapping.as_ref().and_then(|mapping| mapping.popup) {
            self.commit_popup(highlighted);
            return Outcome::Consumed;
        }
        match self.control {
            CorrelationControl::Sources if self.mapping.is_some() => {
                self.open_popup();
                Outcome::Consumed
            }
            CorrelationControl::Sources => self.activate_control(Self::default_control(), ctx),
            control => self.activate_control(control, ctx),
        }
    }

    fn activate_control(&mut self, control: CorrelationControl, ctx: &mut Ctx<'_>) -> Outcome {
        match control {
            CorrelationControl::Sources => Outcome::Consumed,
            CorrelationControl::Correlate => self.submit(ctx),
            CorrelationControl::Cancel => self.cancel(),
        }
    }

    fn key(&mut self, key: KeyEvent, ctx: &mut Ctx<'_>) -> Outcome {
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return Outcome::Ignored;
        }
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Down | KeyCode::Char('j') if !alt => self.move_selection(1),
            KeyCode::Up | KeyCode::Char('k') if !alt => self.move_selection(-1),
            KeyCode::Tab if key.modifiers.contains(KeyModifiers::SHIFT) => self.move_control(-1),
            KeyCode::BackTab | KeyCode::Left => self.move_control(-1),
            KeyCode::Tab | KeyCode::Right => self.move_control(1),
            KeyCode::Enter => return self.activate(ctx),
            _ => return Outcome::Ignored,
        }
        Outcome::Consumed
    }

    fn mouse(
        &mut self,
        kind: MouseEventKind,
        hit: Option<CorrelationHit>,
        ctx: &mut Ctx<'_>,
    ) -> Outcome {
        if !matches!(kind, MouseEventKind::Down(MouseButton::Left)) {
            return Outcome::Consumed;
        }
        if self
            .mapping
            .as_ref()
            .is_some_and(|mapping| mapping.submitting)
        {
            return Outcome::Consumed;
        }
        let popup_open = self
            .mapping
            .as_ref()
            .is_some_and(|mapping| mapping.popup.is_some());
        match hit {
            Some(CorrelationHit::Choice(index)) if popup_open => {
                self.commit_popup(index);
                Outcome::Consumed
            }
            // A click outside the popup closes it, as Escape does; §8.3.
            _ if popup_open => {
                if let Some(mapping) = self.mapping.as_mut() {
                    mapping.popup = None;
                }
                Outcome::Consumed
            }
            Some(CorrelationHit::Row(index)) => {
                if let Some(mapping) = self.mapping.as_mut() {
                    mapping.selected = index.min(mapping.sources.len().saturating_sub(1));
                }
                self.control = CorrelationControl::Sources;
                self.open_popup();
                Outcome::Consumed
            }
            Some(CorrelationHit::Control(control)) => {
                self.control = control;
                self.activate_control(control, ctx)
            }
            Some(CorrelationHit::Choice(_)) | None => Outcome::Consumed,
        }
    }

    /// Keep the selected source inside the reserved rows.
    fn scroll_to_selection(&mut self, rows: usize) {
        let Some(mapping) = self.mapping.as_ref() else {
            self.top = 0;
            return;
        };
        if rows == 0 {
            self.top = 0;
            return;
        }
        if mapping.selected < self.top {
            self.top = mapping.selected;
        } else if mapping.selected >= self.top + rows {
            self.top = mapping.selected + 1 - rows;
        }
        self.top = self.top.min(mapping.sources.len().saturating_sub(rows));
    }

    /// The §7.4 message for the state on screen.
    fn message(&self) -> (MessageState, String) {
        if let Some(error) = self.error.as_deref() {
            return (MessageState::Error, error.to_owned());
        }
        let Some(mapping) = self.mapping.as_ref() else {
            return (
                MessageState::Pending,
                "finding records that share this value".to_owned(),
            );
        };
        let mapped = mapping.mapped_sources();
        let total = mapping.sources.len();
        if mapping.submitting {
            (
                MessageState::Updating,
                "opening the correlated view".to_owned(),
            )
        } else if mapped == 0 {
            (
                MessageState::Disabled,
                "no source is mapped yet, so there is nothing to correlate".to_owned(),
            )
        } else if mapping.sources.iter().any(|source| source.incomplete) {
            (
                MessageState::Scanned,
                format!(
                    "{mapped} of {total} sources mapped · field names come from a bounded sample, \
                     so a rarely used field may be missing"
                ),
            )
        } else {
            (
                MessageState::Applied,
                format!("{mapped} of {total} sources mapped"),
            )
        }
    }
}

impl Component for CorrelationDialog {
    type Hit = CorrelationHit;
    type Open = CorrelationOpen;

    fn open(&mut self, params: CorrelationOpen, ctx: &mut Ctx<'_>) {
        let Some(origin_view_id) = ctx.views.active_id().map(str::to_owned) else {
            return;
        };
        self.open = true;
        self.mapping = None;
        self.error = None;
        self.control = CorrelationControl::Sources;
        self.top = 0;
        self.field = params.field.clone();
        self.origin_view_id = origin_view_id.clone();
        // §5.2.1: every open source is a candidate row, so that is the
        // reservation; the answer never has more.
        self.reserved_rows = ctx.sources.len().max(usize::from(MIN_LIVE_ROWS));
        // A previous session's lookup that is still on screen is superseded.
        self.abandon_lookup();
        if self.in_flight() >= MAX_CORRELATION_REQUESTS {
            self.error = Some("correlation request queue is full; try again shortly".into());
            self.control = CorrelationControl::Cancel;
            return;
        }
        let generation = self.outbox.next_generation();
        self.generation = generation;
        let request = CorrelationRequest::Resolve {
            generation,
            origin_view_id: origin_view_id.clone(),
            row_id: params.row,
            field: params.field,
        };
        if self.outbox.push(request).is_err() {
            self.error = Some("correlation request queue is full; try again shortly".into());
            self.control = CorrelationControl::Cancel;
            return;
        }
        self.unanswered.insert(generation);
        self.lookup = Some(PendingLookup {
            generation,
            origin_view_id,
        });
    }

    fn handle(&mut self, event: Event<CorrelationHit>, ctx: &mut Ctx<'_>) -> Outcome {
        if !self.open {
            return Outcome::Ignored;
        }
        match event {
            Event::Key(key) => self.key(key, ctx),
            Event::Mouse { kind, hit, .. } => self.mouse(kind, hit, ctx),
            // §10: the field popup absorbs the first Escape, the layer the
            // second. A cancelled mapping leaves the origin view exactly as it
            // was.
            Event::Dismiss => {
                if let Some(mapping) = self.mapping.as_mut()
                    && mapping.popup.take().is_some()
                {
                    return Outcome::Consumed;
                }
                self.cancel()
            }
            Event::Command(CommandId::CorrelationAccept) => {
                self.activate_control(CorrelationControl::Correlate, ctx)
            }
            Event::Command(CommandId::CorrelationCancel) => self.cancel(),
            Event::Command(_) | Event::Paste(_) | Event::View(_) | Event::Resize => {
                Outcome::Ignored
            }
        }
    }

    /// §8.10: the two verbs are palette rows, each saying why it cannot run
    /// right now. The row that opens the layer stays with Fields.
    fn commands(&self, _views: &Views) -> Vec<CommandEntry> {
        let accept_reason = if !self.open {
            Some("open Fields and choose Correlate first")
        } else if self.error.is_some() && self.mapping.is_none() {
            Some("the lookup failed; cancel and try again")
        } else {
            match self.mapping.as_ref() {
                None => Some("wait for the lookup to finish"),
                Some(mapping) if mapping.submitting => Some("the correlated view is opening"),
                Some(mapping) if mapping.mapped_sources() == 0 => {
                    Some("map at least one source first")
                }
                Some(_) => None,
            }
        };
        let cancel_reason = (!self.open).then_some("open Fields and choose Correlate first");
        vec![
            CommandEntry {
                spec: CommandSpec {
                    id: CommandId::CorrelationAccept,
                    name: "Correlate with this mapping",
                    description: "Open the correlated view over the mapped sources",
                    category: "Fields",
                    aliases: &["accept mapping", "open correlated view"],
                    shortcut: accept_reason.is_none().then_some("r"),
                },
                unavailable_reason: accept_reason,
            },
            CommandEntry {
                spec: CommandSpec {
                    id: CommandId::CorrelationCancel,
                    name: "Cancel correlation",
                    description: "Abandon the lookup or the mapping and leave the view as it was",
                    category: "Fields",
                    aliases: &["stop lookup", "abandon mapping"],
                    shortcut: cancel_reason.is_none().then_some("Esc"),
                },
                unavailable_reason: cancel_reason,
            },
        ]
    }

    fn action_labels(&self, _ctx: &Ctx<'_>) -> Vec<&'static str> {
        if self.open {
            BUTTONS.to_vec()
        } else {
            Vec::new()
        }
    }

    /// An accelerator fires its verb where the user is: the focus ring stays.
    fn press_action(&mut self, index: usize, ctx: &mut Ctx<'_>) -> Outcome {
        if !self.open {
            return Outcome::Ignored;
        }
        if self
            .mapping
            .as_ref()
            .is_some_and(|mapping| mapping.submitting)
        {
            return Outcome::Consumed;
        }
        match BUTTON_CONTROLS.get(index) {
            Some(control) => self.activate_control(*control, ctx),
            None => Outcome::Ignored,
        }
    }

    fn surface(&self) -> Surface {
        self.surface
    }

    fn hit(&self, point: (u16, u16)) -> Option<CorrelationHit> {
        // The popup is drawn last and may overlap the rows, so it is tested
        // first (§5.3).
        self.geometry
            .choices
            .iter()
            .find_map(|(rect, index)| {
                contains(*rect, point).then_some(CorrelationHit::Choice(*index))
            })
            .or_else(|| {
                self.geometry.controls.iter().find_map(|(rect, control)| {
                    contains(*rect, point).then_some(CorrelationHit::Control(*control))
                })
            })
            .or_else(|| {
                self.geometry.rows.iter().find_map(|(rect, index)| {
                    contains(*rect, point).then_some(CorrelationHit::Row(*index))
                })
            })
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &RenderCtx<'_>) -> Surface {
        self.geometry = Geometry::default();
        if !self.open {
            self.surface = Surface::default();
            return self.surface;
        }
        let theme: Theme = ctx.theme;
        let styles = DialogStyles::new(theme);
        let ascii = ctx.ascii;
        let width = content_width(area, DialogClass::M);
        let header = match self.mapping.as_ref() {
            Some(mapping) => format!(
                "{} = {} · from the selected record",
                self.field, mapping.value_label
            ),
            None => format!("{} · from the selected record", self.field),
        };
        let (state, sentence) = self.message();
        let labels = BUTTONS;
        // §5.2.1 again, for the message: the lookup's answer changes which
        // sentence is shown, so the rows are reserved for the longest one the
        // layer can say, and the frame is the same before and after.
        let message = [
            sentence.as_str(),
            "finding records that share this value",
            "no source is mapped yet, so there is nothing to correlate",
            &format!(
                "{0} of {0} sources mapped · field names come from a bounded sample, \
                 so a rarely used field may be missing",
                self.reserved_rows
            ),
        ]
        .into_iter()
        .map(|candidate| message_rows(candidate, width))
        .max()
        .unwrap_or(0);
        let content = DialogContent {
            header: 1,
            // Pane heading plus the reserved rows (§5.2.1), whatever the
            // lookup has answered so far.
            body: u16::try_from(self.reserved_rows.saturating_add(1)).unwrap_or(u16::MAX),
            message,
            help: help_rows(HELP, width),
            actions: packed_button_rows(width, &labels),
        };
        let regions = dialog_frame_regions(
            frame,
            area,
            DialogClass::M,
            "Correlate across sources",
            &content,
            theme,
        );
        if regions.header.height > 0 {
            frame.render_widget(
                Paragraph::new(truncated(&header, usize::from(regions.header.width)))
                    .style(styles.label.add_modifier(Modifier::BOLD)),
                regions.header,
            );
        }
        if regions.body.height > 0 && regions.body.width > 0 {
            self.geometry.body = Some(regions.body);
            let rects = pane(regions.body, 0, self.reserved_rows);
            let source_count = self
                .mapping
                .as_ref()
                .map_or(0, |mapping| mapping.sources.len());
            if rects.heading.height > 0 {
                frame.render_widget(
                    Paragraph::new("Source").style(styles.label.add_modifier(Modifier::BOLD)),
                    rects.heading,
                );
                if let Some(mapping) = self.mapping.as_ref() {
                    let count = format!("{} of {}", mapping.mapped_sources(), source_count);
                    let count_width = u16::try_from(count.chars().count()).unwrap_or(0);
                    if rects.heading.width > count_width {
                        frame.render_widget(
                            Paragraph::new(count).style(styles.description),
                            Rect::new(
                                rects.heading.right().saturating_sub(count_width),
                                rects.heading.y,
                                count_width,
                                1,
                            ),
                        );
                    }
                }
            }
            let visible = usize::from(rects.viewport.height);
            self.scroll_to_selection(visible);
            let name_width = usize::from(rects.viewport.width).saturating_sub(24).max(8);
            if let Some(mapping) = self.mapping.as_ref() {
                for (offset, index) in (self.top..source_count).take(visible).enumerate() {
                    let Some(source) = mapping.sources.get(index) else {
                        continue;
                    };
                    let Some(y) = u16::try_from(offset)
                        .ok()
                        .map(|offset| rects.viewport.y.saturating_add(offset))
                        .filter(|y| *y < rects.viewport.bottom())
                    else {
                        continue;
                    };
                    let row = Rect::new(rects.viewport.x, y, rects.viewport.width, 1);
                    let selected =
                        self.control == CorrelationControl::Sources && index == mapping.selected;
                    let gutter = if selected {
                        if ascii { "> " } else { "› " }
                    } else {
                        "  "
                    };
                    let chosen = source
                        .chosen
                        .clone()
                        .unwrap_or_else(|| NOT_CORRELATED.to_owned());
                    let caret = if ascii { "v" } else { "▾" };
                    let text = format!(
                        "{gutter}{:<name_width$}  {chosen} {caret}",
                        truncated(&source.name, name_width),
                    );
                    frame.render_widget(
                        Paragraph::new(clipped_width(&text, usize::from(row.width))).style(
                            if selected {
                                styles.selection
                            } else if source.chosen.is_some() {
                                styles.applied
                            } else {
                                styles.description
                            },
                        ),
                        row,
                    );
                    self.geometry.rows.push((row, index));
                    let field_x = row
                        .x
                        .saturating_add(u16::try_from(name_width.saturating_add(4)).unwrap_or(0))
                        .min(row.right().saturating_sub(1));
                    self.geometry.field_rects.push(Rect::new(
                        field_x,
                        y,
                        row.right().saturating_sub(field_x),
                        1,
                    ));
                }
            }
        }
        render_message(frame, regions.message, state, &sentence, theme, ascii);
        render_help_text(frame, regions.help, HELP, theme);

        let action_controls = BUTTON_CONTROLS;
        let default_action = action_controls
            .iter()
            .position(|control| *control == Self::default_control());
        let focused = action_controls
            .iter()
            .position(|control| *control == self.control);
        for (index, rect) in render_actions(
            frame,
            regions.actions,
            ActionRow {
                labels: &labels,
                default: default_action,
                destructive: &[],
                focused,
            },
            theme,
        ) {
            self.geometry.controls.push((rect, action_controls[index]));
        }

        let mut popup = regions.popup;
        // §8.3: the field choice is a dropdown, so the options open as an
        // anchored popup over the dialog rather than cycling invisibly.
        if let Some(mapping) = self.mapping.as_ref()
            && let Some(highlighted) = mapping.popup
            && let Some(anchor) = mapping
                .selected
                .checked_sub(self.top)
                .and_then(|offset| self.geometry.field_rects.get(offset))
                .copied()
        {
            let options = mapping.options(mapping.selected);
            let hint = options
                .iter()
                .map(|option| u16::try_from(option.chars().count()).unwrap_or(0))
                .max()
                .unwrap_or(12)
                .saturating_add(4);
            let rect =
                crate::dialog_layout::anchored_rect(regions.interior, anchor, options.len(), hint);
            clear_themed(frame, rect, theme);
            frame.render_widget(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(theme.active_border)),
                rect,
            );
            let inner = rect.inner(Margin::new(1, 1));
            let visible = usize::from(inner.height);
            let top = highlighted.saturating_sub(visible.saturating_sub(1));
            for (offset, index) in (top..options.len()).take(visible).enumerate() {
                let Some(y) = u16::try_from(offset)
                    .ok()
                    .map(|offset| inner.y.saturating_add(offset))
                    .filter(|y| *y < inner.bottom())
                else {
                    continue;
                };
                let choice = Rect::new(inner.x, y, inner.width, 1);
                frame.render_widget(
                    Paragraph::new(truncated(&options[index], usize::from(choice.width))).style(
                        if index == highlighted {
                            styles.selection
                        } else if index == 0 {
                            styles.unavailable
                        } else {
                            styles.description
                        },
                    ),
                    choice,
                );
                self.geometry.choices.push((choice, index));
            }
            popup = popup.union(rect);
        }

        self.surface = Surface {
            popup,
            interior: regions.interior,
            caret: None,
            scrollable: false,
            text_focus: false,
        };
        self.surface
    }
}
