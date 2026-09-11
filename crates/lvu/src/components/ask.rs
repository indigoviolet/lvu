//! The Ask 🧠 layer (`docs/dialog-system.md` §12.17), converted per
//! `docs/component-model.md` §6.3 step 12.
//!
//! Ask is the first layer with a long-running remote stage: between `Submit`
//! and a proposal it is waiting on a snapshot, a session and an agent turn, and
//! it must stay open, cancellable and unable to submit again. That lifecycle is
//! the component's — `Outbox<AskAiRequest>` carries the request out and
//! `complete`/`progress` bring the answer back behind the generation fence the
//! dialog already had.
//!
//! Applying a proposal is *not* the component's: it writes a draft into the
//! advanced-filter or enrichment editor, or a whole recipe into the view, and
//! all three are still legacy. The component validates the proposal against the
//! view it was frozen for and hands the rest to the shell through
//! `Outcome::Defer`, which keeps the layer up so a refusal can be shown in the
//! dialog exactly as it was before.

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Widget},
};
use unicode_width::UnicodeWidthStr;

use crate::app::{
    Action, AskAiDialogState, AskAiKind, AskAiRequest, AskAiStage, AskAnswer, AskControl,
    AskSample, AskSampleTier, AskTask, RecipeConfig, RecipeOutcome,
};
use crate::command_palette::CommandId;
use crate::component::{Component, Ctx, Event, Outbox, Outcome, RenderCtx, Surface};
use crate::dialog_controls::{ActionRow, DialogStyles, render_role_button, stable_action_rows};
use crate::dialog_layout::{
    AnchoredSpec, DialogSpec, PresentationKind, anchored_geometry, policy_size, resolve_dialog,
};
use crate::text_edit::{EditCommand, EditPolicy, TextCursor, edit};
use crate::theme::Theme;
use crate::ui::{
    FIELD_GUTTER, InputSurface, MessageState, render_help_text, render_message, render_placeholder,
    render_responsive_frame, render_scrollbar, truncated, wrap_sentence,
};

/// Matches the legacy `MAX_AI_REQUESTS`: two in flight is the refusal
/// threshold the dialog words itself.
const MAX_ASK_REQUESTS: usize = 2;
/// The legacy `MAX_AI_PROMPT_BYTES`.
const MAX_ASK_PROMPT_BYTES: usize = 8 * 1024;
/// The proposal-instruction ceiling the bridge enforces.
const MAX_INSTRUCTION_BYTES: usize = 131_072;

const ASK_KINDS: [AskAiKind; 2] = [AskAiKind::Filter, AskAiKind::Enrichment];

fn ask_kind_index(kind: AskAiKind) -> usize {
    ASK_KINDS
        .iter()
        .position(|candidate| *candidate == kind)
        .unwrap_or(0)
}

/// §12.17: the wider re-run is offered on either signal — the preparation
/// left rows out, or the answer said it needed more — and only from a finished
/// answer that used the standard tier (`docs/larger-ask-sample.md`).
fn widen_offered(dialog: &AskAiDialogState) -> bool {
    matches!(dialog.stage, AskAiStage::Proposal | AskAiStage::Error)
        && dialog
            .answer_sample
            .or(dialog.sample)
            .is_some_and(|sample| {
                sample.tier == AskSampleTier::Standard && (sample.omitted() || dialog.needs_more)
            })
}

fn ask_controls(dialog: &AskAiDialogState) -> Vec<AskControl> {
    let mut controls = match dialog.stage {
        AskAiStage::Input if dialog.recipe.is_some() || dialog.task.is_some() => {
            vec![AskControl::Prompt, AskControl::Submit]
        }
        AskAiStage::Input => {
            vec![AskControl::Kind, AskControl::Prompt, AskControl::Submit]
        }
        AskAiStage::Error => vec![AskControl::Prompt, AskControl::Submit],
        AskAiStage::Proposal => vec![AskControl::Apply],
        // Waiting is not a dead end: cancelling the request is a real action,
        // so it is reachable as a button and not only through Escape.
        AskAiStage::Snapshot | AskAiStage::StartingSession | AskAiStage::Proposing => {
            vec![AskControl::Cancel]
        }
    };
    if widen_offered(dialog) {
        controls.push(AskControl::Widen);
    }
    if dialog.review_scroll_limit > 0 {
        controls.push(AskControl::More);
    }
    controls
}

/// How the layer was opened. The three entry points differ only in what they
/// prepare, so they are data rather than three `open` methods.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AskOpen {
    /// `A`: the user picks the kind and writes the request.
    Generic,
    /// A prepared task from another layer. The kind is decided, the request is
    /// prefilled, and the dialog says both (§12.17).
    Task(AskTask),
    /// Recipe adaptation, from the Recipes dialog.
    Recipe {
        config: Box<RecipeConfig>,
        outcome: RecipeOutcome,
        prompt: String,
    },
}

/// Everything the Ask layer draws that can be clicked.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AskHit {
    Control(AskControl),
    KindChoice(usize),
    Body,
}

/// Recorded by `render`, consumed by `hit()` (§5.1).
#[derive(Clone, Debug, Default)]
struct AskGeometry {
    controls: Vec<(Rect, AskControl)>,
    kind_choices: Vec<(Rect, usize)>,
    body: Option<Rect>,
}

/// The multi-line Request field. Dialog-owned, so its caret lives here rather
/// than in `ctx.cursors` (§2.5); `open` resets it, which is what bumping the
/// dialog generation used to do to the `CursorBank` slot.
#[derive(Clone, Debug, Default)]
struct PromptField {
    value: String,
    cursor: TextCursor,
}

#[derive(Debug)]
pub struct AskDialog {
    open: bool,
    state: Option<AskAiDialogState>,
    prompt: PromptField,
    geometry: AskGeometry,
    surface: Surface,
    /// The agent defaults the shell holds; seeded by `App::configure_ai`.
    defaults: (String, String, String),
    pub outbox: Outbox<AskAiRequest>,
}

impl Default for AskDialog {
    fn default() -> Self {
        Self {
            open: false,
            state: None,
            prompt: PromptField::default(),
            geometry: AskGeometry::default(),
            surface: Surface::default(),
            // The same agent defaults `App::new` starts from; `configure_ai`
            // replaces them once `lvu-app` resolves the real settings.
            defaults: (
                "codex/gpt-5.6-sol".to_owned(),
                "full-access".to_owned(),
                "medium".to_owned(),
            ),
            // The legacy queue was `MAX_AI_REQUESTS` deep and the dialog words
            // its own refusal at that depth.
            outbox: Outbox::new(MAX_ASK_REQUESTS),
        }
    }
}

fn contains(rect: Rect, point: (u16, u16)) -> bool {
    point.0 >= rect.x && point.0 < rect.right() && point.1 >= rect.y && point.1 < rect.bottom()
}

impl AskDialog {
    /// The agent defaults the shell holds. Seeded on open, exactly as the
    /// legacy `Action::OpenAskAi` arm read `App::ai_*`.
    pub fn configure(&mut self, provider: String, mode: String, thinking: String) {
        self.defaults = (provider, mode, thinking);
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Read-only view of the dialog, for the shell's completion delegation and
    /// for tests. Nothing outside may mutate it.
    pub fn state(&self) -> Option<&AskAiDialogState> {
        self.state.as_ref()
    }

    /// Whether the layer is in a stage that accepts a new request. The
    /// timestamp task refuses to reopen over a request in flight, exactly as
    /// the legacy `Action::OpenTimestampAssistant` guard did.
    pub fn accepts_new_request(&self) -> bool {
        self.state
            .as_ref()
            .is_none_or(|dialog| matches!(dialog.stage, AskAiStage::Input | AskAiStage::Error))
    }

    /// Whether a remote turn is in flight. The activity indicator asks the
    /// shell, which asks here: a stage is component state, not something
    /// `terminal.rs` may read off a public field (§7.8).
    pub fn is_working(&self) -> bool {
        self.state.as_ref().is_some_and(|dialog| {
            matches!(
                dialog.stage,
                AskAiStage::Snapshot | AskAiStage::StartingSession | AskAiStage::Proposing
            )
        })
    }

    fn editing(&self) -> bool {
        self.state.as_ref().is_some_and(|dialog| {
            !dialog.kind_dropdown
                && dialog.focus == AskControl::Prompt
                && matches!(dialog.stage, AskAiStage::Input | AskAiStage::Error)
        })
    }

    fn policy(&self) -> EditPolicy {
        EditPolicy {
            max_bytes: MAX_ASK_PROMPT_BYTES,
            multiline: true,
        }
    }

    /// §8.1: Up/Down move by the rows the render actually drew, so the wrap
    /// width the last frame recorded is what the caret walks.
    fn wrap_width(&self) -> Option<usize> {
        self.state
            .as_ref()
            .filter(|dialog| dialog.prompt_width > 0)
            .map(|dialog| usize::from(dialog.prompt_width))
    }

    fn edit_prompt(&mut self, command: EditCommand<'_>) -> Outcome {
        if !self.editing() {
            return Outcome::Ignored;
        }
        let policy = self.policy();
        edit(
            &mut self.prompt.value,
            &mut self.prompt.cursor,
            command,
            policy,
        );
        if let Some(dialog) = &mut self.state {
            dialog.prompt = self.prompt.value.clone();
        }
        Outcome::Consumed
    }

    fn move_control(&mut self, delta: i32) -> Outcome {
        if let Some(dialog) = &mut self.state {
            let controls = ask_controls(dialog);
            if !controls.is_empty() {
                let index = controls
                    .iter()
                    .position(|control| *control == dialog.focus)
                    .unwrap_or(0);
                dialog.focus =
                    controls[(index as i32 + delta).rem_euclid(controls.len() as i32) as usize];
            }
        }
        Outcome::Consumed
    }

    fn focus_control(&mut self, control: AskControl) {
        if let Some(dialog) = &mut self.state
            && ask_controls(dialog).contains(&control)
        {
            dialog.focus = control;
        }
    }

    fn open_kind(&mut self) -> Outcome {
        if let Some(dialog) = &mut self.state
            && dialog.recipe.is_none()
            && dialog.task.is_none()
            && dialog.stage == AskAiStage::Input
        {
            dialog.kind_selected = ask_kind_index(dialog.kind);
            dialog.kind_dropdown = true;
        }
        Outcome::Consumed
    }

    fn move_kind(&mut self, delta: i32) -> Outcome {
        if let Some(dialog) = &mut self.state
            && dialog.kind_dropdown
        {
            dialog.kind_selected =
                (dialog.kind_selected as i32 + delta).rem_euclid(ASK_KINDS.len() as i32) as usize;
        }
        Outcome::Consumed
    }

    fn choose_kind(&mut self, index: usize) -> Outcome {
        if let Some(kind) = ASK_KINDS.get(index).copied() {
            let _ = self.select_kind(kind);
            if let Some(dialog) = &mut self.state {
                dialog.kind_selected = index;
                dialog.kind_dropdown = false;
            }
        }
        Outcome::Consumed
    }

    fn close_kind(&mut self) -> Outcome {
        if let Some(dialog) = &mut self.state {
            dialog.kind_selected = ask_kind_index(dialog.kind);
            dialog.kind_dropdown = false;
        }
        Outcome::Consumed
    }

    fn select_kind(&mut self, kind: AskAiKind) -> Outcome {
        if let Some(dialog) = &mut self.state
            && dialog.stage == AskAiStage::Input
            && dialog.recipe.is_none()
            && dialog.task.is_none()
            && kind != AskAiKind::Recipe
        {
            dialog.kind = kind;
            dialog.progress = match kind {
                AskAiKind::Filter => "describe the desired filter",
                AskAiKind::Enrichment => "describe the field to derive",
                AskAiKind::Recipe => "describe how to adapt the suggested recipe",
            }
            .into();
        }
        Outcome::Consumed
    }

    fn scroll(&mut self, delta: i32) -> Outcome {
        if let Some(dialog) = &mut self.state {
            dialog.review_scroll = (i32::from(dialog.review_scroll) + delta)
                .clamp(0, i32::from(dialog.review_scroll_limit))
                as u16;
        }
        Outcome::Consumed
    }

    fn activate(&mut self, ctx: &mut Ctx<'_>) -> Outcome {
        match self.state.as_ref().map(|dialog| dialog.focus) {
            Some(AskControl::Kind) => self.open_kind(),
            Some(AskControl::Submit) => self.submit(ctx),
            Some(AskControl::Apply) => self.apply(ctx),
            Some(AskControl::Widen) => self.widen(ctx),
            Some(AskControl::Cancel) => self.dismiss(),
            Some(AskControl::Prompt | AskControl::More) | None => Outcome::Consumed,
        }
    }

    /// Escape and the `Cancel request` button take the same path: the layer
    /// closes and a request already in flight is cancelled.
    fn dismiss(&mut self) -> Outcome {
        self.open = false;
        let cancel = self
            .state
            .take()
            .filter(|dialog| !matches!(dialog.stage, AskAiStage::Input | AskAiStage::Error))
            .map(|dialog| dialog.generation);
        self.prompt = PromptField::default();
        if let Some(generation) = cancel {
            let _ = self.outbox.push(AskAiRequest::Cancel { generation });
        }
        Outcome::Close
    }

    fn submit(&mut self, ctx: &mut Ctx<'_>) -> Outcome {
        let wider_retry = self.state.as_ref().is_some_and(|dialog| {
            dialog.stage == AskAiStage::Error
                && dialog
                    .previous_answer
                    .as_ref()
                    .is_some_and(|answer| answer.request == dialog.prompt)
        });
        self.start(ctx, wider_retry)
    }

    /// Re-run the request that is on screen against the wider bounded sample.
    /// Same text, new generation, fenced exactly as the first turn (§2.4).
    fn widen(&mut self, ctx: &mut Ctx<'_>) -> Outcome {
        let Some(dialog) = &mut self.state else {
            return Outcome::Consumed;
        };
        if !widen_offered(dialog) {
            return Outcome::Consumed;
        }
        dialog.previous_answer = current_answer(dialog);
        dialog.generation = self.outbox.next_generation();
        dialog.stage = AskAiStage::Input;
        self.start(ctx, true)
    }

    fn start(&mut self, ctx: &mut Ctx<'_>, wider: bool) -> Outcome {
        if !wider
            && self
                .state
                .as_ref()
                .is_some_and(|dialog| dialog.stage == AskAiStage::Proposal)
        {
            return self.apply(ctx);
        }
        let pending = self.outbox.len();
        // The view the dialog was frozen for, not whichever is active now.
        let busy = self.state.as_ref().is_some_and(|dialog| {
            ctx.views.state(&dialog.view_id).is_some_and(|state| {
                state.search.pending_generation.is_some()
                    || state.advanced.pending_generation.is_some()
                    || state.enrichment.pending_generation.is_some()
            })
        });
        let Some(dialog) = &mut self.state else {
            return Outcome::Consumed;
        };
        if !matches!(dialog.stage, AskAiStage::Input | AskAiStage::Error) {
            return Outcome::Consumed;
        }
        if dialog.prompt.trim().is_empty() {
            dialog.stage = AskAiStage::Error;
            dialog.progress = "request cannot be empty".into();
            return Outcome::Consumed;
        }
        if busy {
            dialog.stage = AskAiStage::Error;
            dialog.progress = "wait for the current view definition to finish applying".into();
            return Outcome::Consumed;
        }
        if pending >= MAX_ASK_REQUESTS {
            dialog.stage = AskAiStage::Error;
            dialog.progress = "agent request queue is full".into();
            return Outcome::Consumed;
        }
        let instruction = if let Some(recipe) = &dialog.recipe {
            let stages = recipe
                .enrichments
                .iter()
                .map(|stage| format!("id={:?} source={:?}", stage.id.0, stage.source))
                .collect::<Vec<_>>()
                .join("\n");
            format!(
                "{}\nReviewed advanced filter: {:?}\nReviewed ordered enrichment chain:\n{}\nLegacy enrichment: {:?}\nAdapt the advanced filter and, if needed, the complete ordered enrichment chain. Preserve all other settings. Return empty recipe_stage_revisions.",
                dialog.prompt, recipe.advanced, stages, recipe.enrichment
            )
        } else {
            dialog.prompt.clone()
        };
        if instruction.len() > MAX_INSTRUCTION_BYTES {
            dialog.stage = AskAiStage::Error;
            dialog.progress = "recipe context exceeds the 128 KiB proposal limit".into();
            return Outcome::Consumed;
        }
        dialog.review_scroll = 0;
        dialog.review_scroll_limit = 0;
        if !wider {
            dialog.previous_answer = None;
        }
        dialog.stage = AskAiStage::Snapshot;
        dialog.progress = "freezing applied view snapshot".into();
        dialog.expression = None;
        dialog.explanation = None;
        dialog.sample = None;
        dialog.answer_sample = None;
        dialog.needs_more = false;
        let request = AskAiRequest::Start {
            generation: dialog.generation,
            view_id: dialog.view_id.clone(),
            definition_revision: dialog.definition_revision,
            kind: dialog.kind,
            instruction,
            provider: dialog.provider.clone(),
            mode: dialog.mode.clone(),
            thinking: dialog.thinking.clone(),
            wider,
        };
        let _ = self.outbox.push(request);
        Outcome::Consumed
    }

    /// The proposal is checked against the view it was frozen for; everything
    /// that writes a draft or a recipe is still legacy, so the shell does it
    /// and the layer stays up to show a refusal (§6.4).
    fn apply(&mut self, ctx: &mut Ctx<'_>) -> Outcome {
        let Some(dialog) = self.state.as_ref() else {
            return Outcome::Consumed;
        };
        if dialog.stage != AskAiStage::Proposal {
            return Outcome::Consumed;
        }
        let view_id = dialog.view_id.clone();
        let revision = dialog.definition_revision;
        let kind = dialog.kind;
        let expression = dialog.expression.clone().unwrap_or_default();
        let recipe = dialog.recipe.clone();
        let outcome = dialog.recipe_outcome.clone();
        if ctx.views.active_id() != Some(view_id.as_str())
            || ctx.views.definition_revision(&view_id) != Some(revision)
        {
            if let Some(dialog) = &mut self.state {
                dialog.stage = AskAiStage::Error;
                dialog.progress = "view changed; request a fresh proposal".into();
            }
            return Outcome::Consumed;
        }
        Outcome::Defer(Action::ApplyAskProposal {
            kind,
            expression,
            recipe: recipe.map(Box::new),
            outcome,
            task: dialog.task,
        })
    }

    /// The shell could not apply the proposal. The dialog keeps the proposal
    /// and words the refusal itself, as it did before the conversion.
    pub fn fail_apply(&mut self, message: &str) {
        if let Some(dialog) = &mut self.state {
            dialog.stage = AskAiStage::Error;
            dialog.progress = message.to_owned();
        }
    }

    /// The shell applied the proposal; the layer is done.
    pub fn finish_apply(&mut self) {
        self.open = false;
        self.state = None;
        self.prompt = PromptField::default();
    }

    /// Progress from the worker, behind the dialog's own generation fence.
    /// What the bounded preparation admitted, reported before the prompt is
    /// sent. Fenced by generation like every other completion (§2.4).
    pub fn sampled(&mut self, generation: u64, sample: AskSample) -> bool {
        let Some(dialog) = self
            .state
            .as_mut()
            .filter(|dialog| dialog.generation == generation)
        else {
            return false;
        };
        dialog.sample = Some(sample);
        true
    }

    /// The agent said the sample was not enough. Independent of `sampled`: a
    /// complete sample can still be the wrong data to answer with.
    pub fn needs_more(&mut self, generation: u64) -> bool {
        let Some(dialog) = self
            .state
            .as_mut()
            .filter(|dialog| dialog.generation == generation)
        else {
            return false;
        };
        dialog.needs_more = true;
        true
    }

    pub fn progress(
        &mut self,
        generation: u64,
        stage: AskAiStage,
        progress: String,
        session_id: Option<String>,
        snapshot_dir: Option<String>,
    ) -> bool {
        let Some(dialog) = self
            .state
            .as_mut()
            .filter(|dialog| dialog.generation == generation)
        else {
            return false;
        };
        dialog.stage = stage;
        dialog.progress = progress;
        if session_id.is_some() {
            dialog.session_id = session_id;
        }
        if snapshot_dir.is_some() {
            dialog.snapshot_dir = snapshot_dir;
        }
        true
    }

    /// A finished proposal, or the failure that ended the turn. `current` is
    /// the shell's answer to "is the view still on the revision this proposal
    /// was frozen against"; the component cannot ask, because completions
    /// arrive outside a `Ctx` (§2.4), and the shell already holds `Views`.
    pub fn complete(
        &mut self,
        generation: u64,
        view_id: &str,
        revision: u64,
        current: bool,
        result: Result<(String, String), String>,
    ) -> bool {
        let Some(dialog) = self.state.as_mut().filter(|dialog| {
            dialog.generation == generation
                && dialog.view_id == view_id
                && dialog.definition_revision == revision
        }) else {
            return false;
        };
        if !current {
            dialog.stage = AskAiStage::Error;
            dialog.focus = AskControl::Prompt;
            dialog.progress = "view definition changed; request a fresh proposal".into();
            return false;
        }
        match result {
            Ok((value, explanation)) => {
                dialog.expression = Some(value);
                dialog.explanation = Some(explanation);
                // The answer keeps the sample it was built from, so a wider
                // re-run can be told from the first attempt.
                dialog.answer_sample = dialog.sample;
                dialog.stage = AskAiStage::Proposal;
                dialog.focus = AskControl::Apply;
                dialog.progress =
                    "proposal ready; Apply validates it before changing the view".into();
            }
            Err(message) => {
                dialog.stage = AskAiStage::Error;
                dialog.focus = AskControl::Prompt;
                dialog.progress = message;
            }
        }
        true
    }

    /// The recipe variant carries the adapted stage list with the expression.
    pub fn complete_recipe(
        &mut self,
        generation: u64,
        view_id: &str,
        revision: u64,
        current: bool,
        result: Result<(String, String), String>,
        chain: Option<Vec<crate::app::EnrichmentDefinition>>,
    ) -> bool {
        let accepted = self.complete(generation, view_id, revision, current, result);
        if accepted
            && let Some(dialog) = &mut self.state
            && dialog.kind == AskAiKind::Recipe
            && dialog.stage == AskAiStage::Proposal
            && let Some(chain) = chain
            && let Some(config) = &mut dialog.recipe
        {
            config.enrichments = chain;
            config.enrichment.clear();
        }
        accepted
    }

    fn key(&mut self, key: KeyEvent, ctx: &mut Ctx<'_>) -> Outcome {
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return Outcome::Ignored;
        }
        // An open dropdown takes the keys first (§10): it is the innermost
        // surface, and it never sees a bare character as text.
        if self
            .state
            .as_ref()
            .is_some_and(|dialog| dialog.kind_dropdown)
        {
            if !key.modifiers.is_empty() {
                return Outcome::Consumed;
            }
            return match key.code {
                KeyCode::Up => self.move_kind(-1),
                KeyCode::Down => self.move_kind(1),
                KeyCode::Enter => {
                    let selected = self.state.as_ref().map_or(0, |d| d.kind_selected);
                    self.choose_kind(selected)
                }
                _ => Outcome::Consumed,
            };
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            let command = match key.code {
                KeyCode::Char('a') => Some(EditCommand::StartOfLine),
                KeyCode::Char('e') => Some(EditCommand::EndOfLine),
                KeyCode::Char('k') => Some(EditCommand::KillToEndOfLine),
                _ => None,
            };
            if let Some(command) = command {
                return self.edit_prompt(command);
            }
            return Outcome::Ignored;
        }
        if key.modifiers.contains(KeyModifiers::ALT) {
            return match key.code {
                // §8.1 newline accelerator for the multi-line Request field.
                KeyCode::Char('n') => self.edit_prompt(EditCommand::Insert("\n")),
                KeyCode::Char('t') => {
                    if self.accepts_new_request() {
                        Outcome::Replace(crate::component::Open::Ask(AskOpen::Task(
                            AskTask::TimestampColumn,
                        )))
                    } else {
                        Outcome::Consumed
                    }
                }
                KeyCode::Char('f') => self.select_kind(AskAiKind::Filter),
                KeyCode::Char('e') => self.select_kind(AskAiKind::Enrichment),
                _ => Outcome::Ignored,
            };
        }
        // The panes take the arrows while they hold focus, as `ScrollAskAi` did.
        if self
            .state
            .as_ref()
            .is_some_and(|dialog| dialog.focus == AskControl::More)
            && matches!(key.code, KeyCode::Up | KeyCode::Down)
        {
            return self.scroll(if key.code == KeyCode::Up { -1 } else { 1 });
        }
        // While the Request field is editing, the caret owns Left/Right/Up/Down
        // and Enter inserts a newline; §8.1 Up/Down are visual rows.
        if self.editing() {
            match key.code {
                KeyCode::Left => return self.edit_prompt(EditCommand::MoveLeft),
                KeyCode::Right => return self.edit_prompt(EditCommand::MoveRight),
                KeyCode::Up => {
                    let command = match self.wrap_width() {
                        Some(width) => EditCommand::MoveUpWrapped(width),
                        None => EditCommand::MoveUp,
                    };
                    return self.edit_prompt(command);
                }
                KeyCode::Down => {
                    let command = match self.wrap_width() {
                        Some(width) => EditCommand::MoveDownWrapped(width),
                        None => EditCommand::MoveDown,
                    };
                    return self.edit_prompt(command);
                }
                KeyCode::Enter => return self.edit_prompt(EditCommand::Insert("\n")),
                KeyCode::Backspace => return self.edit_prompt(EditCommand::Backspace),
                KeyCode::Char(character) => {
                    let mut buffer = [0u8; 4];
                    return self
                        .edit_prompt(EditCommand::Insert(character.encode_utf8(&mut buffer)));
                }
                _ => {}
            }
        }
        match key.code {
            KeyCode::Tab | KeyCode::Down => self.move_control(1),
            KeyCode::BackTab | KeyCode::Up => self.move_control(-1),
            KeyCode::Enter | KeyCode::Char(' ') => self.activate(ctx),
            KeyCode::Backspace => self.edit_prompt(EditCommand::Backspace),
            _ => Outcome::Ignored,
        }
    }

    fn mouse(&mut self, kind: MouseEventKind, hit: Option<AskHit>, ctx: &mut Ctx<'_>) -> Outcome {
        let dropdown = self
            .state
            .as_ref()
            .is_some_and(|dialog| dialog.kind_dropdown);
        if dropdown {
            if let (MouseEventKind::Down(MouseButton::Left), Some(AskHit::KindChoice(index))) =
                (kind, hit)
            {
                return self.choose_kind(index);
            }
            return Outcome::Consumed;
        }
        match (kind, hit) {
            (MouseEventKind::Down(MouseButton::Left), Some(AskHit::Control(control))) => {
                self.focus_control(control);
                if matches!(
                    control,
                    AskControl::Kind
                        | AskControl::Submit
                        | AskControl::Apply
                        | AskControl::Widen
                        | AskControl::Cancel
                ) {
                    return self.activate(ctx);
                }
                Outcome::Consumed
            }
            (MouseEventKind::ScrollUp, Some(AskHit::Body)) => self.scroll(-1),
            (MouseEventKind::ScrollDown, Some(AskHit::Body)) => self.scroll(1),
            _ => Outcome::Consumed,
        }
    }
}

impl Component for AskDialog {
    type Hit = AskHit;
    type Open = AskOpen;

    fn open(&mut self, params: AskOpen, ctx: &mut Ctx<'_>) {
        let Some(view_id) = ctx.views.active_id().map(str::to_owned) else {
            return;
        };
        let generation = self.outbox.next_generation();
        let definition_revision = ctx.views.definition_revision(&view_id).unwrap_or_default();
        let (provider, mode, thinking) = self.defaults.clone();
        let mut dialog = AskAiDialogState {
            generation,
            definition_revision,
            view_id,
            kind: AskAiKind::Filter,
            task: None,
            focus: AskControl::Prompt,
            kind_dropdown: false,
            kind_selected: 0,
            prompt: String::new(),
            provider,
            mode,
            thinking,
            stage: AskAiStage::Input,
            progress: "describe the desired filter".into(),
            expression: None,
            explanation: None,
            session_id: None,
            snapshot_dir: None,
            recipe: None,
            recipe_outcome: None,
            sample: None,
            answer_sample: None,
            previous_answer: None,
            needs_more: false,
            review_scroll: 0,
            review_scroll_limit: 0,
            prompt_scroll: 0,
            prompt_width: 0,
        };
        match params {
            AskOpen::Generic => {}
            AskOpen::Task(task) => {
                // The task fixes the kind, so the dialog states what it will do
                // instead of offering a filter/enrichment choice.
                dialog.task = Some(task);
                dialog.kind = AskAiKind::Enrichment;
                dialog.kind_selected = 1;
                dialog.prompt = task.prompt().into();
                dialog.progress = task.message().into();
            }
            AskOpen::Recipe {
                config,
                outcome,
                prompt,
            } => {
                dialog.kind = AskAiKind::Recipe;
                dialog.prompt = prompt;
                dialog.progress = "review the adaptation request before applying".into();
                dialog.recipe = Some(*config);
                dialog.recipe_outcome = Some(outcome);
            }
        }
        self.prompt = PromptField {
            cursor: TextCursor {
                char_index: dialog.prompt.chars().count(),
            },
            value: dialog.prompt.clone(),
        };
        self.state = Some(dialog);
        self.geometry = AskGeometry::default();
        self.open = true;
    }

    fn handle(&mut self, event: Event<AskHit>, ctx: &mut Ctx<'_>) -> Outcome {
        match event {
            Event::Key(key) => self.key(key, ctx),
            Event::Paste(text) => self.edit_prompt(EditCommand::Insert(&text)),
            Event::Mouse { kind, hit, .. } => self.mouse(kind, hit, ctx),
            // §10: the dropdown is the innermost surface and absorbs the first
            // dismissal, keeping the request the user has typed.
            Event::Dismiss => {
                if self
                    .state
                    .as_ref()
                    .is_some_and(|dialog| dialog.kind_dropdown)
                {
                    return self.close_kind();
                }
                self.dismiss()
            }
            Event::Command(CommandId::AskAiFilter) => self.select_kind(AskAiKind::Filter),
            Event::Command(CommandId::AskAiEnrichment) => self.select_kind(AskAiKind::Enrichment),
            Event::Command(_) | Event::View(_) | Event::Resize => Outcome::Ignored,
        }
    }

    /// §4.3: the two kind rows the catalog lists are *taken over* by this layer
    /// while a kind is still choosable. They were `Action::SelectAskAiKind`
    /// gated on `Focus::AskAi` before the conversion, and a prepared task or a
    /// recipe adaptation has already decided the kind, so the rows say why they
    /// are unavailable rather than silently doing nothing.
    fn commands(&self, _views: &crate::app::Views) -> Vec<crate::component::CommandEntry> {
        use crate::component::{CommandEntry, CommandSpec};
        let reason = match self.state.as_ref() {
            None => Some("open Ask agent first"),
            Some(dialog) if dialog.stage != AskAiStage::Input => {
                Some("wait for the current request to finish")
            }
            Some(dialog) if dialog.recipe.is_some() || dialog.task.is_some() => {
                Some("this request already has a kind")
            }
            Some(_) => None,
        };
        vec![
            CommandEntry {
                spec: CommandSpec {
                    id: CommandId::AskAiFilter,
                    name: "Ask agent for filter",
                    description: "Select a filter proposal",
                    category: "Agent",
                    aliases: &["predicate proposal"],
                    shortcut: reason.is_none().then_some("Alt-F"),
                },
                unavailable_reason: reason,
            },
            CommandEntry {
                spec: CommandSpec {
                    id: CommandId::AskAiEnrichment,
                    name: "Ask agent for enrichment",
                    description: "Select an enrichment proposal",
                    category: "Agent",
                    aliases: &["derive proposal"],
                    shortcut: reason.is_none().then_some("Alt-E"),
                },
                unavailable_reason: reason,
            },
        ]
    }

    fn surface(&self) -> Surface {
        self.surface
    }

    /// A key burst that opens the layer and types into it — `Aoffline request`
    /// — reaches `handle` before any frame exists, so the last render has
    /// published nothing and the `q` in `request` would dismiss. `editing()` is
    /// already false while the kind list is open, which is what the render
    /// derives its own flag from.
    fn text_focus(&self) -> bool {
        self.editing()
    }

    fn action_labels(&self, _ctx: &Ctx<'_>) -> Vec<&'static str> {
        self.state
            .as_ref()
            .map(ask_action_labels)
            .unwrap_or_default()
    }

    fn press_action(&mut self, index: usize, ctx: &mut Ctx<'_>) -> Outcome {
        let Some(control) = self
            .state
            .as_ref()
            .map(ask_action_controls)
            .and_then(|controls| controls.get(index).copied())
        else {
            return Outcome::Ignored;
        };
        // The focus ring stays where the user left it: an accelerator fires a
        // verb, it does not move focus (§8.10).
        match control {
            AskControl::Submit => self.submit(ctx),
            AskControl::Apply => self.apply(ctx),
            AskControl::Widen => self.widen(ctx),
            AskControl::Cancel => self.dismiss(),
            AskControl::Kind | AskControl::Prompt | AskControl::More => Outcome::Ignored,
        }
    }

    fn hit(&self, point: (u16, u16)) -> Option<AskHit> {
        // The dropdown is drawn last and may overlap the dialog, so its rows
        // are tested first (§5.3).
        self.geometry
            .kind_choices
            .iter()
            .find_map(|(rect, index)| contains(*rect, point).then_some(AskHit::KindChoice(*index)))
            .or_else(|| {
                self.geometry.controls.iter().find_map(|(rect, control)| {
                    contains(*rect, point).then_some(AskHit::Control(*control))
                })
            })
            .or_else(|| {
                self.geometry
                    .body
                    .filter(|rect| contains(*rect, point))
                    .map(|_| AskHit::Body)
            })
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &RenderCtx<'_>) -> Surface {
        let mut controls = Vec::new();
        let mut kind_choices = Vec::new();
        let mut body = None;
        let cursor = self.editing().then_some(self.prompt.cursor.char_index);
        let Some(state) = &mut self.state else {
            self.surface = Surface::default();
            return self.surface;
        };
        let surface = draw(
            state,
            frame,
            area,
            ctx,
            cursor,
            &mut controls,
            &mut kind_choices,
            &mut body,
        );
        self.geometry = AskGeometry {
            controls,
            kind_choices,
            body,
        };
        self.surface = surface;
        surface
    }
}

/// §12.17 Ask 🧠 — class L on the shared anatomy: title, an optional header
/// summary for a prepared task, the Kind/Request form, the Proposal and
/// Activity panes, one message row, help, and the actions last.
///
/// The body is the pre-conversion `ui::render_ask_ai` unchanged; only its
/// boundary moved. Geometry it used to publish into the global `HitRegions` is
/// handed back to the component instead (§5.1).
#[allow(clippy::too_many_arguments)]
fn draw(
    state: &mut AskAiDialogState,
    frame: &mut Frame<'_>,
    area: Rect,
    ctx: &RenderCtx<'_>,
    cursor: Option<usize>,
    controls_hit: &mut Vec<(Rect, crate::app::AskControl)>,
    kind_hit: &mut Vec<(Rect, usize)>,
    body_hit: &mut Option<Rect>,
) -> Surface {
    use crate::app::{AskAiStage as S, AskControl as C};
    use crate::dialog_layout::PANE_INDENT;
    use crate::ui::MESSAGE_SENTENCE_COLUMN;
    let theme = ctx.theme;
    let styles = DialogStyles::new(theme);
    let ascii = ctx.ascii;
    let mut caret = None;
    let mut dialog = state.clone();

    let editable = matches!(dialog.stage, S::Input | S::Error);
    // A prepared task has already decided the kind, so the dialog explains the
    // task in its header instead of offering an irrelevant choice.
    let task = dialog.task;
    let show_kind = task.is_none();
    let choosable_kind = show_kind && dialog.recipe.is_none() && dialog.stage == S::Input;

    let title = match (ascii, task) {
        (true, None) => "Ask Agent".to_owned(),
        (false, None) => "Ask 🧠".to_owned(),
        (true, Some(task)) => format!("Ask Agent · {}", task.object()),
        (false, Some(task)) => format!("Ask 🧠 · {}", task.object()),
    };
    let help = task.map(crate::app::AskTask::help).unwrap_or("");
    let (state_word, sentence) = ask_message(&dialog);

    // Stable LongContent budgets: outer size is policy-only, never the
    // request, proposal, previous answer or activity length. The header is
    // present exactly when a prepared task explains itself (fixed at open),
    // the message keeps its two-row maximum and the actions keep the stable
    // width budget, so the frame and sticky tail origins are identical
    // across input/proposal/transcript/error states. Hand-rolled row
    // assignment below is presentation-only folding, never query membership
    // (AGENTS.md).
    let action_labels = ask_action_labels(&dialog);
    let borrowed: Vec<&str> = action_labels.clone();
    let help_rows_budget = if help.is_empty() {
        0
    } else {
        let (policy_w, _) = policy_size(area, PresentationKind::LongContent);
        let estimate = usize::from(policy_w.saturating_sub(4)).max(1);
        wrap_sentence(help, estimate, 2).len().min(2) as u16
    };
    let spec = ask_spec_for(area, task.is_some(), help_rows_budget);
    let Ok(resolved) = resolve_dialog(area, &spec, 1, &borrowed, Some(0), None) else {
        // Below the floor the tiny fallback owns the frame; stay open with
        // nothing drawn, as the palette does.
        return Surface {
            popup: Rect::default(),
            interior: Rect::default(),
            caret: None,
            scrollable: false,
            text_focus: editable && dialog.focus == C::Prompt && !dialog.kind_dropdown,
        };
    };
    // Shared frame so geometry and paint share one definition; compactness
    // comes from the geometry, never recomputed from the frame.
    render_responsive_frame(frame, &resolved, &title, ctx.active, theme);
    let mut surface = Surface {
        popup: resolved.frame,
        interior: resolved.interior,
        caret: None,
        // Derived below from real pane overflow, never blanket true.
        scrollable: false,
        // While the kind list is open it is the innermost surface and takes
        // the keys, so `q` dismisses it rather than being typed.
        text_focus: editable && dialog.focus == C::Prompt && !dialog.kind_dropdown,
    };
    let mut popup = resolved.frame;
    let body = resolved.body.viewport;
    if body.width == 0 || body.height == 0 {
        return surface;
    }
    let message_rect = resolved.message;
    let help_rect = resolved.help;
    let action_geom = resolved.actions.clone();

    if let Some(task) = task
        && resolved.header.height > 0
    {
        frame.render_widget(
            Paragraph::new(truncated(
                task.summary(),
                usize::from(resolved.header.width),
            ))
            .style(styles.description),
            resolved.header,
        );
    }

    let labels: &[&str] = if show_kind {
        &["Kind", "Request"]
    } else {
        &["Request"]
    };
    let label_width = labels
        .iter()
        .map(|label| u16::try_from(UnicodeWidthStr::width(*label)).unwrap_or(0))
        .max()
        .unwrap_or(0)
        .min(18);
    // §4.2: below this the label no longer fits beside a usable field, so it
    // stacks above it. Stable per size, never per state.
    let stacked = body.width < label_width.saturating_add(FIELD_GUTTER).saturating_add(20);

    let field_width = if stacked {
        body.width
    } else {
        body.width
            .saturating_sub(label_width)
            .saturating_sub(FIELD_GUTTER)
            .max(1)
    };
    // §8.1: the Request field takes the rows its draft needs, up to three, and
    // scrolls internally beyond that. Sizing it to the content is what retires
    // the input-background slab painted over rows the prompt never reaches.
    // The internal scrollbar costs a column, which changes the wrap, so the
    // wrap is measured again once the overflow is known.
    let mut text_width = field_width.max(1);
    let mut wrapped = crate::text_edit::wrapped_text(&dialog.prompt, usize::from(text_width));
    let visible_rows = |wrapped: &crate::text_edit::WrappedText| {
        u16::try_from(wrapped.lines.len())
            .unwrap_or(ASK_REQUEST_ROWS)
            .clamp(1, ASK_REQUEST_ROWS)
    };
    let mut request_rows = visible_rows(&wrapped);
    if wrapped.lines.len() > usize::from(request_rows) && field_width > 1 {
        text_width = field_width.saturating_sub(1);
        wrapped = crate::text_edit::wrapped_text(&dialog.prompt, usize::from(text_width));
        request_rows = visible_rows(&wrapped);
    }
    let request_overflows = wrapped.lines.len() > usize::from(request_rows);

    // The Kind/Request form stays fixed at the top of the body while the
    // panes scroll beneath it, so the input and every action stay reachable
    // however long the transcript grows. Rows before this line never scroll.
    let fixed_end = u16::from(show_kind)
        .saturating_mul(1u16.saturating_add(u16::from(stacked)))
        .saturating_add(u16::from(stacked))
        .saturating_add(request_rows);
    let pane_viewport_h = body.height.saturating_sub(fixed_end);
    // §7.4 caps the message at two rows, but a bridge diagnostic names what
    // failed *and* what to do about it. When it does not fit, the full text
    // becomes body content so the body's own scroll reaches it; truncating the
    // remedy away is not a diagnostic. The wrap accounts for the scrollbar
    // column the panes take on overflow, measured again once it is known, so
    // paint never truncates a line the measure claimed fits.
    let pane_text_full = usize::from(body.width.saturating_sub(PANE_INDENT)).max(1);
    let wrap_panes = |text_width: usize| {
        let sentence_width =
            usize::from(body.width.saturating_sub(MESSAGE_SENTENCE_COLUMN).max(1)).max(1);
        let diagnostic: Vec<PaneLine> =
            if wrap_sentence(&sentence, sentence_width, usize::MAX).len() > 2 {
                wrap_sentence(&sentence, text_width, usize::MAX)
                    .into_iter()
                    .map(|text| PaneLine {
                        text,
                        error: state_word == MessageState::Error,
                    })
                    .collect()
            } else {
                Vec::new()
            };
        let proposal = ask_proposal_lines(&dialog, text_width);
        let activity = ask_activity_lines(&dialog, text_width);
        (diagnostic, proposal, activity)
    };
    let (diagnostic, proposal, activity) = wrap_panes(pane_text_full);
    let pane_total = diagnostic.len() + proposal.len() + activity.len() + 3 * 2;
    let pane_text_width = if pane_total > usize::from(pane_viewport_h) && pane_text_full > 1 {
        pane_text_full.saturating_sub(1).max(1)
    } else {
        pane_text_full
    };
    let (diagnostic, proposal, activity) = if pane_text_width == pane_text_full {
        (diagnostic, proposal, activity)
    } else {
        wrap_panes(pane_text_width)
    };
    let mut panes: Vec<(&str, Option<String>, &Vec<PaneLine>)> = Vec::new();
    if !diagnostic.is_empty() {
        panes.push(("Details", None, &diagnostic));
    }
    panes.push((
        "Proposal",
        (dialog.expression.is_none() && dialog.previous_answer.is_none())
            .then(|| "none yet".to_owned()),
        &proposal,
    ));
    panes.push(("Activity", None, &activity));
    let pane_lines: Vec<usize> = panes.iter().map(|(_, _, lines)| lines.len()).collect();
    let kind_width = ask_kind_width(&dialog);

    let layout = ask_body_layout(
        Rect::new(0, 0, body.width, 1),
        show_kind,
        kind_width,
        stacked,
        label_width,
        request_rows,
        &pane_lines,
    );
    // §9: the panes are the scrolling region and their scrollbar replaces the
    // retired `[ More ]` pseudo-button. The Kind/Request form above
    // `fixed_end` never scrolls; one projection serves paint, cursor,
    // selection, scrollbar and mouse.
    let pane_content = layout.height.saturating_sub(fixed_end);
    let max_scroll = pane_content.saturating_sub(pane_viewport_h);
    dialog.review_scroll_limit = max_scroll;
    dialog.review_scroll = dialog.review_scroll.min(max_scroll);
    if dialog.focus == C::More && max_scroll == 0 {
        dialog.focus = match dialog.stage {
            S::Proposal => C::Apply,
            S::Input | S::Error => C::Submit,
            _ => C::Cancel,
        };
    }
    let scroll = dialog.review_scroll;
    let project = |rect: Rect| -> Option<Rect> {
        if rect.width == 0 || rect.height == 0 {
            return None;
        }
        if rect.bottom() <= fixed_end {
            // Fixed form rows paint where laid out, when the body holds them.
            (rect.bottom() <= body.height).then(|| {
                Rect::new(
                    body.x.saturating_add(rect.x),
                    body.y.saturating_add(rect.y),
                    rect.width.min(body.width.saturating_sub(rect.x)),
                    rect.height,
                )
            })
        } else if rect.y >= fixed_end {
            // Pane rows scroll under the fixed form.
            let y = rect.y.saturating_sub(scroll);
            (y >= fixed_end && y < fixed_end.saturating_add(pane_viewport_h) && y < body.height)
                .then(|| {
                    Rect::new(
                        body.x.saturating_add(rect.x),
                        body.y.saturating_add(y),
                        rect.width.min(body.width.saturating_sub(rect.x)),
                        rect.height,
                    )
                })
        } else {
            None
        }
    };

    if let Some((label, field)) = layout.kind {
        let focused = dialog.focus == C::Kind;
        if let Some(rect) = project(label) {
            frame.render_widget(
                Paragraph::new("Kind").style(if focused {
                    styles.shortcut
                } else {
                    styles.label
                }),
                rect,
            );
        }
        if let Some(rect) = project(field) {
            // §8.3: a dropdown is a field with a chevron in its last cell.
            let style = if focused {
                styles.selection
            } else {
                styles.input
            };
            InputSurface { style }.render(rect, frame.buffer_mut());
            frame.render_widget(
                Paragraph::new(truncated(
                    ask_kind_label(dialog.kind),
                    usize::from(rect.width.saturating_sub(2)),
                ))
                .style(style),
                rect,
            );
            if choosable_kind && rect.width > 0 {
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
                controls_hit.push((rect, C::Kind));
            }
        }
    }

    let request_focused = dialog.focus == C::Prompt;
    if let Some(rect) = project(layout.request_label) {
        frame.render_widget(
            Paragraph::new("Request").style(if request_focused {
                styles.shortcut
            } else {
                styles.label
            }),
            rect,
        );
    }
    let editing = request_focused && editable;
    let (caret_row, caret_column) = cursor.map_or((0, 0), |cursor| {
        let mut logical = crate::text_edit::TextCursor { char_index: cursor };
        crate::text_edit::wrapped_cursor(&dialog.prompt, &mut logical, usize::from(text_width))
    });
    let visible = usize::from(layout.request_field.height);
    let mut prompt_scroll =
        usize::from(dialog.prompt_scroll).min(wrapped.lines.len().saturating_sub(visible.max(1)));
    if editing {
        if caret_row < prompt_scroll {
            prompt_scroll = caret_row;
        }
        if caret_row >= prompt_scroll.saturating_add(visible.max(1)) {
            prompt_scroll = caret_row + 1 - visible.max(1);
        }
    }
    dialog.prompt_scroll = u16::try_from(prompt_scroll).unwrap_or(u16::MAX);
    dialog.prompt_width = text_width;
    for row in 0..layout.request_field.height {
        let logical = Rect::new(
            layout.request_field.x,
            layout.request_field.y.saturating_add(row),
            layout.request_field.width,
            1,
        );
        let Some(rect) = project(logical) else {
            continue;
        };
        let text_rect = Rect::new(rect.x, rect.y, text_width.min(rect.width), 1);
        InputSurface {
            style: styles.input,
        }
        .render(text_rect, frame.buffer_mut());
        let line = wrapped
            .lines
            .get(prompt_scroll.saturating_add(usize::from(row)))
            .cloned()
            .unwrap_or_default();
        frame.render_widget(
            Paragraph::new(truncated(&line, usize::from(text_rect.width))).style(styles.input),
            text_rect,
        );
        if editable {
            controls_hit.push((rect, C::Prompt));
        }
    }
    if dialog.prompt.is_empty()
        && let Some(rect) = project(Rect::new(
            layout.request_field.x,
            layout.request_field.y,
            text_width,
            1,
        ))
    {
        render_placeholder(frame, rect, ask_placeholder(&dialog), theme);
    }
    if request_overflows
        && let Some(rect) = project(Rect::new(
            layout.request_field.x.saturating_add(text_width),
            layout.request_field.y,
            1,
            layout.request_field.height,
        ))
    {
        render_scrollbar(
            frame,
            rect,
            prompt_scroll,
            wrapped.lines.len().saturating_sub(visible.max(1)),
            theme,
            ascii,
        );
    }
    if editing
        && let Some(rect) = project(Rect::new(
            layout.request_field.x,
            layout.request_field.y.saturating_add(
                u16::try_from(caret_row.saturating_sub(prompt_scroll)).unwrap_or(0),
            ),
            text_width,
            1,
        ))
        && rect.width > 0
    {
        let x = rect
            .x
            .saturating_add(u16::try_from(caret_column).unwrap_or(0))
            .min(rect.right().saturating_sub(1));
        frame.buffer_mut()[(x, rect.y)]
            .set_style(Style::default().bg(theme.cursor).fg(theme.input_fg));
        frame.set_cursor_position((x, rect.y));
        caret = Some((x, rect.y));
    }

    let pane_focused = dialog.focus == C::More;
    for ((heading, count, lines), rows) in panes.into_iter().zip(layout.panes.iter().copied()) {
        if let Some(rect) = project(Rect::new(rows.x, rows.y, rows.width, 1)) {
            let style = if pane_focused {
                styles.shortcut.add_modifier(Modifier::BOLD)
            } else {
                styles.label.add_modifier(Modifier::BOLD)
            };
            let mut spans = vec![Span::styled(heading.to_owned(), style)];
            if let Some(count) = count {
                let used = UnicodeWidthStr::width(heading)
                    .saturating_add(UnicodeWidthStr::width(count.as_str()));
                spans.push(Span::raw(
                    " ".repeat(usize::from(rect.width).saturating_sub(used)),
                ));
                spans.push(Span::styled(count, styles.description));
            }
            frame.render_widget(Paragraph::new(Line::from(spans)), rect);
        }
        for (index, line) in lines.iter().enumerate() {
            let logical = Rect::new(
                rows.x.saturating_add(PANE_INDENT),
                rows.y
                    .saturating_add(1)
                    .saturating_add(u16::try_from(index).unwrap_or(u16::MAX)),
                rows.width.saturating_sub(PANE_INDENT),
                1,
            );
            if let Some(rect) = project(logical) {
                frame.render_widget(
                    Paragraph::new(line.text.clone()).style(if line.error {
                        styles.error
                    } else {
                        styles.description
                    }),
                    rect,
                );
            }
        }
    }

    // The scrollbar covers the scrolling pane region only; the fixed form
    // above it never scrolls. It replaces the retired `[ More ]`
    // pseudo-button, and the same projected rects drive paint and mouse.
    if max_scroll > 0 && pane_viewport_h > 0 {
        *body_hit = Some(body);
        surface.scrollable = true;
        render_scrollbar(
            frame,
            Rect::new(
                body.right().saturating_sub(1),
                body.y.saturating_add(fixed_end),
                1,
                pane_viewport_h,
            ),
            usize::from(scroll),
            usize::from(max_scroll),
            theme,
            ascii,
        );
    } else if pane_viewport_h > 0 {
        *body_hit = Some(body);
    }

    render_message(frame, message_rect, state_word, &sentence, theme, ascii);
    render_help_text(frame, help_rect, help, theme);
    // §3: the actions come last, after every field they act on. Painted from
    // the shared band plan so paint and mouse share rects; one filled
    // default, never a destructive one.
    let action_controls = ask_action_controls(&dialog);
    let focused_action = action_controls
        .iter()
        .position(|control| *control == dialog.focus);
    let action_row = ActionRow {
        labels: &borrowed,
        default: Some(0),
        destructive: &[],
        focused: focused_action,
    };
    for (index, rect) in action_geom.buttons.iter().copied() {
        let role = action_row.role(index);
        render_role_button(
            frame,
            rect,
            borrowed[index],
            role,
            focused_action == Some(index),
            theme,
        );
        if let Some(control) = action_controls.get(index) {
            controls_hit.push((rect, *control));
        }
    }

    if dialog.kind_dropdown
        && let Some((_, field)) = layout.kind
        && let Some(anchor) = project(field)
    {
        // §5.3: an anchored popup may extend past the dialog, and modal
        // containment tests `Surface.popup`, so it joins the surface. Shared
        // anchored geometry from the actual field rect: the same popup,
        // viewport and rows drive paint, selection and mouse.
        popup = popup.union(render_ask_kind_dropdown(
            frame,
            kind_hit,
            area,
            anchor,
            dialog.kind_selected,
            ascii,
            theme,
        ));
    }
    // Geometry settled during the draw is state: the scroll limit, the focus
    // the render normalised, and the wrap width Up/Down move by.
    state.review_scroll_limit = dialog.review_scroll_limit;
    state.review_scroll = dialog.review_scroll;
    state.focus = dialog.focus;
    state.prompt_scroll = dialog.prompt_scroll;
    state.prompt_width = dialog.prompt_width;
    Surface {
        popup,
        interior: surface.interior,
        caret,
        scrollable: surface.scrollable,
        // While the kind list is open it is the innermost surface and takes the
        // keys, so `q` dismisses it rather than being typed — which is what the
        // legacy `Focus::AskAi` key table did with `kind_dropdown` set.
        text_focus: surface.text_focus,
    }
}
/// §8.1 visible-row cap for the Request field.
const ASK_REQUEST_ROWS: u16 = 3;

struct AskBodyLayout {
    kind: Option<(Rect, Rect)>,
    request_label: Rect,
    request_field: Rect,
    /// One rect per pane, heading row included, in the order supplied.
    panes: Vec<Rect>,
    height: u16,
}

/// §4.2/§4.3 body rows for Ask, in coordinates relative to `content`. Measuring
/// and drawing call this with the same arguments, so the scroll, the hitboxes
/// and the glyphs cannot disagree.
#[allow(clippy::too_many_arguments)]
fn ask_body_layout(
    content: Rect,
    show_kind: bool,
    kind_width: u16,
    stacked: bool,
    label_width: u16,
    request_rows: u16,
    pane_lines: &[usize],
) -> AskBodyLayout {
    let field_x = if stacked {
        0
    } else {
        label_width.saturating_add(FIELD_GUTTER)
    };
    let field_width = content.width.saturating_sub(field_x).max(1);
    let mut y = 0u16;
    let kind = show_kind.then(|| {
        let label = Rect::new(0, y, label_width.min(content.width), 1);
        let field = if stacked {
            Rect::new(0, y.saturating_add(1), kind_width.min(field_width), 1)
        } else {
            Rect::new(field_x, y, kind_width.min(field_width), 1)
        };
        y = y.saturating_add(1 + u16::from(stacked));
        (label, field)
    });
    let request_label = Rect::new(0, y, label_width.min(content.width), 1);
    let request_field = if stacked {
        Rect::new(0, y.saturating_add(1), field_width, request_rows)
    } else {
        Rect::new(field_x, y, field_width, request_rows)
    };
    y = y
        .saturating_add(u16::from(stacked))
        .saturating_add(request_rows);
    let panes = pane_lines
        .iter()
        .map(|lines| {
            // §4.3: one gap row before each pane, then its heading and rows.
            y = y.saturating_add(1);
            let rect = Rect::new(
                0,
                y,
                content.width,
                1u16.saturating_add(u16::try_from(*lines).unwrap_or(u16::MAX)),
            );
            y = y.saturating_add(rect.height);
            rect
        })
        .collect();
    AskBodyLayout {
        kind,
        request_label,
        request_field,
        panes,
        height: y,
    }
}

/// §4.2: a dropdown is `max(longest option) + 4` cells wide, minimum 12.
fn ask_kind_width(dialog: &AskAiDialogState) -> u16 {
    let longest = if dialog.recipe.is_some() {
        UnicodeWidthStr::width(ask_kind_label(AskAiKind::Recipe))
    } else {
        [AskAiKind::Filter, AskAiKind::Enrichment]
            .into_iter()
            .map(|kind| UnicodeWidthStr::width(ask_kind_label(kind)))
            .max()
            .unwrap_or(0)
    };
    u16::try_from(longest)
        .unwrap_or(8)
        .saturating_add(4)
        .max(12)
}

struct PaneLine {
    text: String,
    error: bool,
}

impl PaneLine {
    fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            error: false,
        }
    }
}

fn ask_placeholder(dialog: &AskAiDialogState) -> &'static str {
    match dialog.kind {
        AskAiKind::Filter => "Show only errors from the worker service",
        AskAiKind::Enrichment => "Derive a duration_ms field from the latency text",
        AskAiKind::Recipe => "Adapt the suggested recipe to this source",
    }
}

fn ask_action_controls(dialog: &AskAiDialogState) -> Vec<AskControl> {
    use crate::app::{AskAiStage as S, AskControl as C};
    let mut controls = match dialog.stage {
        S::Input | S::Error => vec![C::Submit],
        S::Proposal => vec![C::Apply],
        S::Snapshot | S::StartingSession | S::Proposing => vec![C::Cancel],
    };
    if widen_offered(dialog) {
        controls.push(C::Widen);
    }
    controls
}

/// §8.10: the letter each button underlines is the key that presses it, so the
/// marked label here is the one `render` draws and the one the shell's resolver
/// reads. No row claims a letter twice: `&Submit`/`Submit a&gain` never appear
/// beside `&Apply`, and the wider re-run marks `w`, which none of them use.
fn ask_action_labels(dialog: &AskAiDialogState) -> Vec<&'static str> {
    use crate::app::{AskAiStage as S, AskControl as C};
    ask_action_controls(dialog)
        .into_iter()
        .map(|control| match control {
            C::Submit if dialog.stage == S::Error => "Submit a&gain",
            C::Submit => "&Submit",
            C::Apply => "&Apply",
            C::Widen => "Ask again with a &wider sample",
            C::Cancel => "&Cancel request",
            C::Kind | C::Prompt | C::More => "",
        })
        .collect()
}

/// Stable LongContent budgets for Ask (see `draw`). The header is present
/// exactly when a prepared task explains itself; the actions keep the
/// stable maximum label set across Submit/Apply/Cancel/Widen relabellings.
fn ask_spec_for(area: Rect, has_header: bool, help_rows: u16) -> DialogSpec {
    // Stable maximum action labels across stages so the band never moves as
    // Submit becomes Apply or the wider re-run appears. Display width via
    // button_width, capped at two rows.
    let max_labels = ["Submit a&gain", "Ask again with a &wider sample"];
    let (policy_w, _) = policy_size(area, PresentationKind::LongContent);
    let estimate = policy_w.saturating_sub(4).max(1);
    let action_rows = stable_action_rows(estimate, &max_labels).clamp(1, 2);
    DialogSpec::new(
        PresentationKind::LongContent,
        u16::from(has_header),
        3,
        2,
        help_rows,
        action_rows,
    )
}

/// §7.4: one message row, one state word from the closed vocabulary.
fn ask_message(dialog: &AskAiDialogState) -> (MessageState, String) {
    use AskAiStage as S;
    let state = match dialog.stage {
        S::Input => MessageState::Ready,
        S::Error => MessageState::Error,
        S::Proposal => MessageState::Ready,
        S::Snapshot | S::StartingSession | S::Proposing => MessageState::Pending,
    };
    (state, dialog.progress.clone())
}

fn ask_proposal_lines(dialog: &AskAiDialogState, width: usize) -> Vec<PaneLine> {
    let width = width.max(1);
    let mut lines = Vec::new();
    if let Some(answer) = &dialog.previous_answer {
        append_answer_lines(
            &mut lines,
            dialog.kind,
            &answer.expression,
            &answer.explanation,
            Some(answer.sample),
            answer.recipe.as_ref(),
            width,
        );
    }
    if let Some(expression) = &dialog.expression {
        if !lines.is_empty() {
            lines.push(PaneLine::plain(String::new()));
        }
        if let Some(explanation) = &dialog.explanation {
            append_answer_lines(
                &mut lines,
                dialog.kind,
                expression,
                explanation,
                dialog.answer_sample,
                dialog.recipe.as_ref(),
                width,
            );
        }
    }
    if lines.is_empty() {
        lines.extend(
            wrap_sentence(
                "A proposal appears here for review; nothing is applied until you accept it",
                width,
                2,
            )
            .into_iter()
            .map(PaneLine::plain),
        );
    }
    lines
}

fn current_answer(dialog: &AskAiDialogState) -> Option<AskAnswer> {
    Some(AskAnswer {
        request: dialog.prompt.clone(),
        expression: dialog.expression.clone()?,
        explanation: dialog.explanation.clone()?,
        sample: dialog.answer_sample?,
        recipe: (dialog.kind == AskAiKind::Recipe)
            .then(|| dialog.recipe.clone())
            .flatten(),
    })
}

fn append_answer_lines(
    lines: &mut Vec<PaneLine>,
    kind: AskAiKind,
    expression: &str,
    explanation: &str,
    sample: Option<AskSample>,
    recipe: Option<&RecipeConfig>,
    width: usize,
) {
    if let Some(sample) = sample {
        lines.push(PaneLine::plain(truncated(
            &format!("{} answer · {}", sample.tier.label(), sample.summary()),
            width,
        )));
    }
    lines.extend(
        wrap_sentence(expression, width, 6)
            .into_iter()
            .map(PaneLine::plain),
    );
    if kind == AskAiKind::Recipe
        && let Some(recipe) = recipe
    {
        for (index, stage) in recipe.enrichments.iter().enumerate() {
            lines.push(PaneLine::plain(truncated(
                &format!("{}. [{}] {}", index + 1, stage.id.0, stage.source),
                width,
            )));
        }
        if recipe.enrichments.is_empty() && !recipe.enrichment.is_empty() {
            lines.push(PaneLine::plain(truncated(&recipe.enrichment, width)));
        }
        lines.extend(wrap_sentence(
            "Advanced filter and ordered enrichments may change; search, pins, colors, time and grouping are retained",
            width, 3,
        ).into_iter().map(PaneLine::plain));
    }
    lines.extend(
        wrap_sentence(explanation, width, 6)
            .into_iter()
            .map(PaneLine::plain),
    );
}

fn ask_activity_lines(dialog: &AskAiDialogState, width: usize) -> Vec<PaneLine> {
    let width = width.max(1);
    let mut lines = vec![PaneLine::plain(truncated(
        &format!(
            "{} · {} · thinking {}",
            dialog.provider, dialog.mode, dialog.thinking
        ),
        width,
    ))];
    if !matches!(dialog.stage, AskAiStage::Input | AskAiStage::Error) {
        lines.push(PaneLine::plain(truncated(
            &format!("request: {}", dialog.prompt.replace('\n', " ")),
            width,
        )));
    }
    // §12.17: always, not only on shortfall. "128 of 128 rows" tells the user
    // the answer saw everything; the line's absence would not.
    if let Some(sample) = dialog.answer_sample.or(dialog.sample) {
        lines.push(PaneLine::plain(truncated(
            &format!("sample {}", sample.summary()),
            width,
        )));
    }
    if let Some(session) = &dialog.session_id {
        lines.push(PaneLine::plain(truncated(
            &format!("session {session}"),
            width,
        )));
    }
    if let Some(directory) = &dialog.snapshot_dir {
        lines.push(PaneLine::plain(truncated(
            &format!("snapshot {directory}"),
            width,
        )));
    }
    lines
}

fn ask_kind_label(kind: AskAiKind) -> &'static str {
    match kind {
        AskAiKind::Filter => "Filter",
        AskAiKind::Enrichment => "Enrichment",
        AskAiKind::Recipe => "Recipe adaptation",
    }
}

/// The kind list is anchored to the field that opened it, with no scrim and
/// no breadcrumb, drawn last so it sits above the dialog. Shared anchored
/// geometry from the actual field rect: the same popup, viewport and rows
/// drive paint, selection, scrollbar and mouse.
fn render_ask_kind_dropdown(
    frame: &mut Frame<'_>,
    kind_hit: &mut Vec<(Rect, usize)>,
    area: Rect,
    anchor: Rect,
    selected: usize,
    ascii: bool,
    theme: Theme,
) -> Rect {
    let styles = DialogStyles::new(theme);
    let kinds = [AskAiKind::Filter, AskAiKind::Enrichment];
    let preferred = kinds
        .iter()
        .map(|kind| UnicodeWidthStr::width(ask_kind_label(*kind)))
        .max()
        .unwrap_or(0) as u16;
    let spec = AnchoredSpec::new(kinds.len(), None, preferred.saturating_add(4).max(12), 0);
    let placed = anchored_geometry(area, anchor, &spec, selected.min(kinds.len() - 1), 0);
    let rect = placed.popup;
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.active_border))
            .style(Style::default().fg(theme.base_fg).bg(theme.dialog_bg)),
        rect,
    );
    let bar_w = u16::from(placed.scrollbar.is_some());
    for (offset, kind) in kinds
        .into_iter()
        .enumerate()
        .skip(placed.first_item)
        .take(usize::from(placed.viewport.height))
    {
        let index = placed.first_item.saturating_add(offset);
        let row = Rect::new(
            placed.viewport.x,
            placed.viewport.y.saturating_add(offset as u16),
            placed.viewport.width.saturating_sub(bar_w),
            1,
        );
        kind_hit.push((row, index));
        frame.render_widget(
            Paragraph::new(ask_kind_label(kind)).style(if index == selected {
                styles.selection
            } else {
                styles.label
            }),
            row,
        );
    }
    if let Some(bar) = placed.scrollbar {
        crate::ui::render_scrollbar(
            frame,
            bar,
            placed.first_item,
            kinds
                .len()
                .saturating_sub(usize::from(placed.viewport.height)),
            theme,
            ascii,
        );
    }
    rect
}
