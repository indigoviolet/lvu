//! The Recipes layer and its History child (`docs/dialog-system.md` §12.9),
//! converted to the component contract as step 9 of `docs/component-model.md`
//! §6.3.
//!
//! Recipes introduces the two seams §6.3 names for it. The first is
//! `Views::apply_recipe`: a recipe is a whole definition, so applying one
//! replaces every draft and the desired constraints at once and enqueues a
//! single query, and a refusal leaves the applied view exactly as it was. The
//! second is the `RecipeRequest` outbox, whose completions are fenced by
//! `RecipeRequestMeta { request_id, dialog_id, dialog_revision }` — the same
//! triple `App` used, now owned by the slot that issued the request.
//!
//! History is its own layer, reached and left by `Outcome::Replace` in both
//! directions (§1, §6.5) — never a child. One layer is on the stack at a time,
//! so nothing is scrimmed twice, Escape closes the dialog as it always did, and
//! applying a revision needs no second pop. Every mode this dialog has ever had
//! is one state machine over one slot, which is what lets a `Replace` carry the
//! list, the selection, the name field and the `RecipeRequestMeta` fence id
//! across the transition unchanged.

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use ratatui::{
    Frame,
    layout::Rect,
    style::Modifier,
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph},
};
use unicode_width::UnicodeWidthStr;

use crate::app::{
    Action, MAX_EDITOR_BYTES, RECIPE_MORE_ITEMS, RecipeConfig, RecipeDialogControl,
    RecipeDialogMode, RecipeItem, RecipeOutcome, RecipeRejected, RecipeRequest, RecipeRequestMeta,
    RecipeSuggestion, Views, move_control, move_index,
};
use crate::command_palette::CommandId;
use crate::component::{
    CommandEntry, CommandSpec, Component, Ctx, Event, Open, Outbox, Outcome, RenderCtx, Surface,
    is_typed_char,
};
use crate::dialog_controls::{DialogStyles, button_style};
use crate::text_edit::{EditCommand, EditPolicy, TextCursor, edit};
use crate::ui::{
    FIELD_GUTTER, MessageState, dialog_frame_regions, help_rows, message_rows, packed_button_rows,
    render_action_row, render_form_field, render_help_text, render_message, render_scrollbar,
    truncated, wrap_sentence,
};

/// The queue depth every recipe request checked against `App::recipe_requests`.
/// It is the dialog's own refusal threshold, not the outbox cap.
const RECIPE_QUEUE_LIMIT: usize = 8;

/// Only the queue limit above ever refuses; the cap exists so a stalled
/// `lvu-app` cannot grow the queue without bound (AGENTS.md).
const RECIPE_OUTBOX_CAP: usize = 32;

/// §12.9: the name column, wide enough for a readable recipe name without
/// crowding out the summary that distinguishes two revisions.
const RECIPE_NAME_WIDTH: u16 = 22;
/// §12.9's date column: `YYYY-MM-DD`, ten cells. It answers the question a
/// list of saved recipes actually raises — which of these is recent — where the
/// revision id it replaced answered none. The id is still what History, Export
/// and the message row name, so the identity the user acts on is not lost.
const RECIPE_DATE_WIDTH: u16 = 10;
/// What an undated revision shows: one written before the stored document
/// carried a date. Guessing one from the file's mtime would be a different
/// fact wearing this one's clothes.
const RECIPE_NO_DATE: &str = "—";
const RECIPE_LABEL_WIDTH: u16 = 11;

/// §4.3: the palette entries Recipes owns. Every one of them used to reach
/// inside the dialog through an `Action` the palette gated on `Focus::Recipes`
/// and a peek at `recipe_dialog.mode`.
const RECIPE_COMMANDS: [(CommandId, CommandSpec); 10] = [
    (
        CommandId::RecipeBrowse,
        CommandSpec {
            id: CommandId::RecipeBrowse,
            name: "Browse recipes",
            description: "List saved recipes for this workspace",
            category: "Recipes",
            aliases: &["list recipes"],
            shortcut: None,
        },
    ),
    (
        CommandId::RecipeSave,
        CommandSpec {
            id: CommandId::RecipeSave,
            name: "Save recipe",
            description: "Save the active view's accepted settings as a recipe",
            category: "Recipes",
            aliases: &["store view"],
            shortcut: None,
        },
    ),
    (
        CommandId::RecipeImport,
        CommandSpec {
            id: CommandId::RecipeImport,
            name: "Import recipe",
            description: "Install a recipe TOML for review",
            category: "Recipes",
            aliases: &["load toml"],
            shortcut: None,
        },
    ),
    (
        CommandId::RecipeExport,
        CommandSpec {
            id: CommandId::RecipeExport,
            name: "Export recipe TOML",
            description: "Export the selected immutable recipe to a new file",
            category: "Recipes",
            aliases: &["write toml"],
            shortcut: None,
        },
    ),
    (
        CommandId::RecipeHistory,
        CommandSpec {
            id: CommandId::RecipeHistory,
            name: "Recipe revision history",
            description: "Show earlier revisions of the selected recipe",
            category: "Recipes",
            aliases: &["revisions"],
            shortcut: None,
        },
    ),
    (
        CommandId::RecipeUpdate,
        CommandSpec {
            id: CommandId::RecipeUpdate,
            name: "Update selected recipe",
            description: "Save a new revision of the selected recipe",
            category: "Recipes",
            aliases: &["new revision"],
            shortcut: None,
        },
    ),
    (
        CommandId::RecipeApply,
        CommandSpec {
            id: CommandId::RecipeApply,
            name: "Apply selected recipe",
            description: "Apply the selected compatible recipe",
            category: "Recipes",
            aliases: &["load recipe", "use saved view"],
            shortcut: None,
        },
    ),
    (
        CommandId::RecipeRefreshSuggestions,
        CommandSpec {
            id: CommandId::RecipeRefreshSuggestions,
            name: "Refresh recipe suggestions",
            description: "Rank similar-source recipes from bounded evidence",
            category: "Recipes",
            aliases: &["similar source", "recommend recipe"],
            shortcut: None,
        },
    ),
    (
        CommandId::RecipeAdaptSuggestion,
        CommandSpec {
            id: CommandId::RecipeAdaptSuggestion,
            name: "Adapt suggested recipe with agent",
            description: "Open the agent with the suggestion's evidence",
            category: "Recipes",
            aliases: &["fix recipe"],
            shortcut: None,
        },
    ),
    (
        CommandId::RecipeRejectSuggestion,
        CommandSpec {
            id: CommandId::RecipeRejectSuggestion,
            name: "Reject selected recipe suggestion",
            description: "Record rejection without deleting the saved recipe",
            category: "Recipes",
            aliases: &["dismiss suggestion"],
            shortcut: None,
        },
    ),
];

/// How the layer was opened or re-opened. A fresh `Browse` starts a new dialog
/// generation; every other variant is a `Replace` that continues the one that
/// is already there, which is why they are not a reset.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RecipesOpen {
    /// `r`, or the palette. Starts a new dialog and asks for the list.
    Browse,
    /// A `Replace` back into one of the dialog's modes, from History.
    Mode(RecipeDialogMode),
    /// A `Replace` into the revision list for one recipe.
    History {
        recipe_id: String,
        recipe_name: String,
    },
}

/// Everything Recipes draws that can be clicked (§5.1).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecipeHit {
    /// A row of the open `[ More ▾ ]` menu. Hit-tested first: §10 puts it on top.
    Menu(usize),
    Row(usize),
    Control(RecipeDialogControl),
}

/// Recorded by `render`, consumed by `hit()`. Every rect here was painted this
/// frame.
#[derive(Clone, Debug, Default)]
struct RecipeGeometry {
    controls: Vec<(Rect, RecipeDialogControl)>,
    rows: Vec<(Rect, usize)>,
    menu: Vec<(Rect, usize)>,
}

/// The dialog's own data, moved off `App` unchanged.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RecipeDialogState {
    pub id: u64,
    pub interaction_revision: u64,
    pub pending_request_id: Option<u64>,
    pub mode: RecipeDialogMode,
    pub control: RecipeDialogControl,
    pub name: String,
    pub items: Vec<RecipeItem>,
    pub suggestions: Vec<RecipeSuggestion>,
    pub selected: usize,
    pub status: String,
    pub loading: bool,
    /// Whether `[ More ▾ ]` is showing its menu, and which entry is highlighted.
    pub menu_open: bool,
    pub menu_selected: usize,
}

#[derive(Debug)]
pub struct RecipesDialog {
    /// Whether this slot is on the stack. Permanent, so a completion arriving
    /// after close still lands somewhere.
    open: bool,
    state: RecipeDialogState,
    /// Allocates `RecipeRequestMeta::request_id`, and — in the parent — the
    /// child's `dialog_id`, so the two slots can never collide in a fence.
    next_id: u64,
    /// The recipe whose revisions are being shown, and its breadcrumb. Both
    /// survive a `Replace` into a mode that acts on one of those revisions —
    /// `Alt-E` from History exports the selected revision and leaves the layer
    /// in Export mode, exactly as it did before the conversion.
    recipe_id: String,
    recipe_name: String,
    /// The Name/TOML-path field's caret. Dialog-owned, so it does not use
    /// `ctx.cursors` (§2.2).
    cursor: TextCursor,
    geometry: RecipeGeometry,
    surface: Surface,
    pub outbox: Outbox<RecipeRequest>,
}

impl Default for RecipesDialog {
    fn default() -> Self {
        Self {
            open: false,
            state: RecipeDialogState::default(),
            next_id: 1,
            recipe_id: String::new(),
            recipe_name: String::new(),
            cursor: TextCursor::default(),
            geometry: RecipeGeometry::default(),
            surface: Surface::default(),
            outbox: Outbox::new(RECIPE_OUTBOX_CAP),
        }
    }
}

impl RecipesDialog {
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Marks the layer open without a `Ctx`, so a palette test can ask for the
    /// entries it contributes while it is on the stack.
    pub fn open_for_test(&mut self) {
        self.open = true;
    }

    pub fn state(&self) -> &RecipeDialogState {
        &self.state
    }

    /// Geometry recorded by the last `render`; `hit()` is how input reaches it.
    pub fn control_rects(&self) -> &[(Rect, RecipeDialogControl)] {
        &self.geometry.controls
    }

    pub fn row_rects(&self) -> &[(Rect, usize)] {
        &self.geometry.rows
    }

    pub fn menu_rects(&self) -> &[(Rect, usize)] {
        &self.geometry.menu
    }

    fn is_history(&self) -> bool {
        self.state.mode == RecipeDialogMode::History
    }

    /// Which layer this surface is, for the shell's stack. The state is one
    /// dialog's; the identity is the surface the user is looking at (§6.5).
    pub fn layer(&self) -> crate::component::LayerId {
        if self.is_history() {
            crate::component::LayerId::RecipeHistory
        } else {
            crate::component::LayerId::Recipes
        }
    }

    fn next_meta(&mut self) -> RecipeRequestMeta {
        let request_id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        RecipeRequestMeta {
            request_id,
            dialog_id: self.state.id,
            dialog_revision: self.state.interaction_revision,
        }
    }

    fn fences(&self, meta: RecipeRequestMeta) -> bool {
        self.open
            && self.state.id == meta.dialog_id
            && self.state.interaction_revision == meta.dialog_revision
            && self.state.pending_request_id == Some(meta.request_id)
    }

    /// The worker's answer to `List` or `History`.
    pub fn set_recipes(
        &mut self,
        meta: RecipeRequestMeta,
        items: Vec<RecipeItem>,
        suggestions: Vec<RecipeSuggestion>,
        error: Option<String>,
    ) -> bool {
        if !self.fences(meta) {
            return false;
        }
        let history = self.is_history();
        let dialog = &mut self.state;
        dialog.items = items.into_iter().take(128).collect();
        dialog.suggestions = suggestions.into_iter().take(16).collect();
        dialog.selected = dialog.selected.min(dialog.items.len().saturating_sub(1));
        dialog.loading = false;
        dialog.pending_request_id = None;
        dialog.status = error.unwrap_or_else(|| {
            if history {
                format!(
                    "{} revisions (newest first; at most 100)",
                    dialog.items.len()
                )
            } else {
                format!("{} saved recipes", dialog.items.len())
            }
        });
        true
    }

    /// A completed save. Returns to Browse and relists, exactly as the shell's
    /// `recipe_saved` did; `false` means the message belongs in the status line.
    pub fn saved(&mut self, meta: RecipeRequestMeta, message: String) -> bool {
        if !self.fences(meta) {
            return false;
        }
        self.state.mode = RecipeDialogMode::Browse;
        self.state.status = message;
        self.state.loading = true;
        let list_meta = self.next_meta();
        self.state.pending_request_id = Some(list_meta.request_id);
        let _ = self.outbox.push(RecipeRequest::List { meta: list_meta });
        true
    }

    pub fn exported(&mut self, meta: RecipeRequestMeta, message: String) -> bool {
        if !self.fences(meta) {
            return false;
        }
        self.state.loading = false;
        self.state.pending_request_id = None;
        self.state.status = message;
        true
    }

    /// A failed request. The status line always reports it; the return says
    /// whether this slot also showed it in its message row.
    pub fn failed(&mut self, meta: RecipeRequestMeta, message: &str) -> bool {
        if !self.fences(meta) {
            return false;
        }
        self.state.loading = false;
        self.state.pending_request_id = None;
        self.state.status = message.to_owned();
        true
    }

    /// An accepted recipe's outcome, produced by the query completion the
    /// shell routes (§4.2 would make this a `ViewEvent`; see the doc's §2).
    pub fn record_outcome(&mut self, outcome: RecipeOutcome) {
        let _ = self.outbox.push(RecipeRequest::Outcome(outcome));
    }

    fn queue_full(&self) -> bool {
        self.outbox.len() >= RECIPE_QUEUE_LIMIT
    }

    fn selected_item(&self) -> Option<&RecipeItem> {
        self.state.items.get(self.state.selected)
    }

    fn suggestion_for_selected(&self) -> Option<&RecipeSuggestion> {
        let item = self.selected_item()?;
        self.state
            .suggestions
            .iter()
            .find(|value| value.recipe_id == item.id)
    }

    fn touch(&mut self) {
        self.state.interaction_revision = self.state.interaction_revision.saturating_add(1);
    }

    /// `Action::SelectRecipeMode`, minus History: choosing History is what
    /// opens the child, so it is handled by the caller that can return an
    /// `Outcome`.
    fn select_mode(&mut self, mode: RecipeDialogMode) {
        if self.queue_full() {
            self.state.status = "recipe request queue is full".into();
            return;
        }
        if mode == RecipeDialogMode::Update
            && (self.state.loading || self.selected_item().is_none())
        {
            self.state.status = "select a loaded recipe first".into();
            return;
        }
        if mode == RecipeDialogMode::Update {
            self.state.name = self.state.items[self.state.selected].name.clone();
            self.cursor.char_index = self.state.name.chars().count();
        }
        if mode == RecipeDialogMode::Export && self.state.mode != mode {
            self.state.name.clear();
            self.cursor.char_index = 0;
        }
        self.state.mode = mode;
        self.state.control = if mode.is_editable() {
            RecipeDialogControl::Input
        } else {
            RecipeDialogControl::List
        };
        self.state.loading = false;
        self.state.pending_request_id = None;
        self.state.status.clear();
        self.touch();
        if mode == RecipeDialogMode::Browse && !self.queue_full() {
            self.state.loading = true;
            let meta = self.next_meta();
            self.state.pending_request_id = Some(meta.request_id);
            let _ = self.outbox.push(RecipeRequest::List { meta });
        }
    }

    /// The `Replace` that shows the selected recipe's revisions, or `None` with
    /// the refusal the mode switch used to word. From History it names the
    /// selected revision's own recipe, which is the reload `Alt-H` always did.
    fn history_open(&mut self) -> Option<Open> {
        if self.queue_full() {
            self.state.status = "recipe request queue is full".into();
            return None;
        }
        if self.state.loading || self.selected_item().is_none() {
            self.state.status = "select a loaded recipe first".into();
            return None;
        }
        let item = self.selected_item().expect("selected recipe");
        Some(Open::RecipeHistory {
            recipe_id: item.id.clone(),
            recipe_name: item.name.clone(),
        })
    }

    /// The `Replace` back into `mode` from History. The queue check is the one
    /// `select_mode` makes, so a refusal reads the same either way.
    fn mode_open(&mut self, mode: RecipeDialogMode) -> Option<Open> {
        if self.queue_full() {
            self.state.status = "recipe request queue is full".into();
            return None;
        }
        if mode == RecipeDialogMode::Update
            && (self.state.loading || self.selected_item().is_none())
        {
            self.state.status = "select a loaded recipe first".into();
            return None;
        }
        Some(Open::Recipes { mode })
    }

    /// `Alt-H`/`Alt-S`/… from History are `Replace`s; from any other mode they
    /// are state changes on the layer that is already open. One helper, so the
    /// keymap and the palette agree about which is which.
    fn switch(&mut self, mode: RecipeDialogMode) -> Outcome {
        if mode == RecipeDialogMode::History {
            return match self.history_open() {
                Some(open) => Outcome::Replace(open),
                None => Outcome::Consumed,
            };
        }
        if self.is_history() {
            return match self.mode_open(mode) {
                Some(open) => Outcome::Replace(open),
                None => Outcome::Consumed,
            };
        }
        self.select_mode(mode);
        Outcome::Consumed
    }

    fn refresh_suggestions(&mut self) {
        if self.state.mode != RecipeDialogMode::Browse || self.queue_full() {
            return;
        }
        self.touch();
        self.state.loading = true;
        self.state.status = "refreshing similar-source suggestions…".into();
        let meta = self.next_meta();
        self.state.pending_request_id = Some(meta.request_id);
        let _ = self.outbox.push(RecipeRequest::List { meta });
    }

    fn reject_suggestion(&mut self, ctx: &mut Ctx<'_>) {
        let Some(source_id) = ctx.views.active_source_id().map(str::to_owned) else {
            return;
        };
        let Some(item) = self.selected_item() else {
            return;
        };
        if self.suggestion_for_selected().is_none() {
            return;
        }
        let outcome = RecipeOutcome {
            source_id,
            recipe_id: item.id.clone(),
            revision: item.revision.clone(),
            accepted: false,
        };
        let _ = self.outbox.push(RecipeRequest::Outcome(outcome.clone()));
        self.state
            .suggestions
            .retain(|value| value.recipe_id != outcome.recipe_id);
        self.state.status = "suggestion rejected; recipe remains available".into();
    }

    /// `Alt-a`. The agent dialog is converted last (§6.3 step 12), so the
    /// selection travels to it as plain data on a migration-only `Action`. The
    /// preconditions are checked here so a refusal leaves the layer open, as
    /// it did when the whole arm lived in `App`.
    fn adapt_suggestion(&mut self, ctx: &mut Ctx<'_>) -> Outcome {
        let Some(suggestion) = self.suggestion_for_selected().cloned() else {
            return Outcome::Consumed;
        };
        let Some(item) = self.selected_item().cloned() else {
            return Outcome::Consumed;
        };
        if item.incompatibility.is_some() || ctx.views.active_id().is_none() {
            return Outcome::Consumed;
        }
        Outcome::Legacy(Action::AdaptRecipe {
            item: Box::new(item),
            suggestion: Box::new(suggestion),
        })
    }

    /// `Action::SubmitRecipe`: apply the selected recipe in a list mode, or
    /// submit the name field in an editable one.
    fn submit(&mut self, ctx: &mut Ctx<'_>) -> Outcome {
        if self.state.mode.is_list() {
            let Some(item) = self.selected_item().cloned() else {
                return Outcome::Consumed;
            };
            if let Some(error) = item.incompatibility {
                self.state.status = error;
                return Outcome::Consumed;
            }
            let outcome = ctx.views.active_source_id().and_then(|source_id| {
                let source_id = source_id.to_owned();
                self.suggestion_for_selected().map(|_| RecipeOutcome {
                    source_id,
                    recipe_id: item.id.clone(),
                    revision: item.revision.clone(),
                    accepted: true,
                })
            });
            let Some(view_id) = ctx.views.active_id().map(str::to_owned) else {
                return Outcome::Consumed;
            };
            let now = ctx.clock.now_unix_nanos;
            return match ctx.views.apply_recipe(&view_id, item.config.clone(), now) {
                Ok(_) => {
                    if let Some(state) = ctx.views.active_mut()
                        && let Some(pending) = &mut state.pending_recipe
                    {
                        pending.suggestion = outcome;
                    }
                    self.open = false;
                    Outcome::Close
                }
                Err(RecipeRejected::InvalidStages | RecipeRejected::QueueFull) => {
                    self.state.status = "query queue is full; recipe draft was preserved".into();
                    Outcome::Consumed
                }
            };
        }
        let Some(view_id) = ctx.views.active_id().map(str::to_owned) else {
            return Outcome::Consumed;
        };
        let mode = self.state.mode;
        let name = self.state.name.trim().to_owned();
        if mode == RecipeDialogMode::Import && !name.is_empty() && !self.queue_full() {
            let meta = self.next_meta();
            let _ = self.outbox.push(RecipeRequest::Import { meta, path: name });
            self.state.loading = true;
            self.state.pending_request_id = Some(meta.request_id);
            self.state.status = "importing for review…".into();
        } else if mode == RecipeDialogMode::Export && !name.is_empty() && !self.queue_full() {
            if let Some(item) = self.selected_item().cloned() {
                let meta = self.next_meta();
                let _ = self.outbox.push(RecipeRequest::Export {
                    meta,
                    path: name,
                    recipe_id: item.id,
                    revision: item.revision,
                });
                self.state.loading = true;
                self.state.pending_request_id = Some(meta.request_id);
                self.state.status = "exporting reviewed revision…".into();
            } else {
                self.state.status = "select a saved recipe before exporting".into();
            }
        } else if matches!(mode, RecipeDialogMode::Save | RecipeDialogMode::Update)
            && !name.is_empty()
            && !self.queue_full()
        {
            let config = ctx
                .views
                .applied_recipe_config(&view_id)
                .unwrap_or_default();
            let update = if mode == RecipeDialogMode::Update {
                self.selected_item()
                    .map(|item| (item.id.clone(), item.revision.clone()))
            } else {
                None
            };
            let meta = self.next_meta();
            let _ = self.outbox.push(RecipeRequest::Save {
                update,
                meta,
                name,
                view_id,
                config: Box::new(config),
            });
            self.state.loading = true;
            self.state.pending_request_id = Some(meta.request_id);
            self.state.status = "saving recipe…".into();
        } else {
            self.state.status = "enter a recipe name or select a recipe".into();
        }
        Outcome::Consumed
    }

    fn toggle_menu(&mut self) {
        self.state.menu_open = !self.state.menu_open;
        self.state.menu_selected = 0;
        self.state.control = RecipeDialogControl::More;
    }

    fn choose_menu(&mut self, index: usize) -> Outcome {
        let chosen = RECIPE_MORE_ITEMS.get(index).map(|(_, control)| *control);
        self.state.menu_open = false;
        self.state.menu_selected = 0;
        match chosen {
            Some(RecipeDialogControl::Mode(mode)) => return self.switch(mode),
            Some(RecipeDialogControl::Refresh) => self.refresh_suggestions(),
            _ => {}
        }
        Outcome::Consumed
    }

    fn move_selection(&mut self, delta: i32) {
        if self.state.menu_open {
            self.state.menu_selected =
                move_index(self.state.menu_selected, RECIPE_MORE_ITEMS.len(), delta);
            return;
        }
        self.state.selected = move_index(self.state.selected, self.state.items.len(), delta);
        self.touch();
    }

    fn select_row(&mut self, index: usize) {
        if index < self.state.items.len() {
            self.state.selected = index;
            self.touch();
        }
    }

    fn focus_control(&mut self, control: RecipeDialogControl) {
        if self.controls().contains(&control) {
            self.state.control = control;
        }
    }

    /// Focus order for the Recipes dialog: the list, then the name it feeds,
    /// then the actions in the order they are drawn. Moved from `App`.
    fn controls(&self) -> Vec<RecipeDialogControl> {
        let dialog = &self.state;
        let mut controls = Vec::new();
        if dialog.mode.is_list() || dialog.mode == RecipeDialogMode::Export {
            controls.push(RecipeDialogControl::List);
        }
        controls.push(RecipeDialogControl::Input);
        controls.push(RecipeDialogControl::Apply);
        if dialog.mode.is_editable() {
            controls.push(RecipeDialogControl::Cancel);
        }
        let suggested = dialog.items.get(dialog.selected).is_some_and(|item| {
            dialog
                .suggestions
                .iter()
                .any(|suggestion| suggestion.recipe_id == item.id)
        });
        if suggested && !dialog.mode.is_editable() {
            controls.extend([RecipeDialogControl::Adapt, RecipeDialogControl::Reject]);
        }
        if dialog.mode == RecipeDialogMode::Browse {
            controls.extend([
                RecipeDialogControl::Save,
                RecipeDialogControl::Update,
                RecipeDialogControl::History,
                RecipeDialogControl::More,
            ]);
        }
        controls
    }

    /// `Enter`, and a click on anything that is not the list or the field.
    fn activate(&mut self, ctx: &mut Ctx<'_>) -> Outcome {
        // §10: while an anchored menu is open it owns Enter.
        if self.state.menu_open {
            let index = self.state.menu_selected;
            return self.choose_menu(index);
        }
        match self.state.control {
            RecipeDialogControl::History => return self.switch(RecipeDialogMode::History),
            RecipeDialogControl::Mode(mode) => return self.switch(mode),
            RecipeDialogControl::Apply | RecipeDialogControl::Input | RecipeDialogControl::List => {
                return self.submit(ctx);
            }
            RecipeDialogControl::Refresh => self.refresh_suggestions(),
            RecipeDialogControl::Adapt => return self.adapt_suggestion(ctx),
            RecipeDialogControl::Reject => self.reject_suggestion(ctx),
            RecipeDialogControl::Save => return self.switch(RecipeDialogMode::Save),
            RecipeDialogControl::Update => return self.switch(RecipeDialogMode::Update),
            RecipeDialogControl::Cancel => return self.switch(RecipeDialogMode::Browse),
            RecipeDialogControl::More => self.toggle_menu(),
        }
        Outcome::Consumed
    }

    /// Whether the name field is taking characters. Also the `q`-dismissal
    /// rule: outside an editable mode the old key table mapped every bare
    /// character to `Action::RecipeInput`, which a non-editable mode dropped,
    /// so `q` never closed the dialog.
    fn editing(&self) -> bool {
        !self.state.menu_open
    }

    fn edit_name(&mut self, command: EditCommand<'_>) {
        if !self.state.mode.is_editable() || self.state.control != RecipeDialogControl::Input {
            return;
        }
        self.cursor.char_index = self.cursor.char_index.min(self.state.name.chars().count());
        let outcome = edit(
            &mut self.state.name,
            &mut self.cursor,
            command,
            EditPolicy {
                max_bytes: MAX_EDITOR_BYTES,
                multiline: false,
            },
        );
        if outcome.changed {
            self.touch();
        }
    }

    fn paste(&mut self, text: &str) {
        if !self.state.mode.is_editable() || self.state.control != RecipeDialogControl::Input {
            return;
        }
        if text.chars().any(char::is_control)
            || self.state.name.len().saturating_add(text.len()) > MAX_EDITOR_BYTES
        {
            self.state.status = "recipe name/path must be a bounded single line".into();
            return;
        }
        self.edit_name(EditCommand::Insert(text));
    }

    /// The component's keymap, in the order the shell's `key_to_action` had it:
    /// the universal line-editing chords, then the dialog's own table.
    fn key(&mut self, key: KeyEvent, ctx: &mut Ctx<'_>) -> Outcome {
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
                self.edit_name(command);
                return Outcome::Consumed;
            }
        }
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Tab if key.modifiers.contains(KeyModifiers::SHIFT) => {
                let controls = self.controls();
                self.state.control = move_control(self.state.control, &controls, -1);
            }
            KeyCode::BackTab => {
                let controls = self.controls();
                self.state.control = move_control(self.state.control, &controls, -1);
            }
            KeyCode::Tab => {
                let controls = self.controls();
                self.state.control = move_control(self.state.control, &controls, 1);
            }
            KeyCode::Up => self.move_selection(-1),
            KeyCode::Down => self.move_selection(1),
            KeyCode::Enter => return self.activate(ctx),
            KeyCode::Char('a') if alt => return self.adapt_suggestion(ctx),
            KeyCode::Char('g') if alt => self.refresh_suggestions(),
            KeyCode::Char('h') if alt => return self.switch(RecipeDialogMode::History),
            KeyCode::Char('u') if alt => return self.switch(RecipeDialogMode::Update),
            KeyCode::Backspace => self.edit_name(EditCommand::Backspace),
            KeyCode::Char('s') if alt => return self.switch(RecipeDialogMode::Save),
            KeyCode::Char('b') if alt => return self.switch(RecipeDialogMode::Browse),
            KeyCode::Char('i') if alt => return self.switch(RecipeDialogMode::Import),
            KeyCode::Char('e') if alt => return self.switch(RecipeDialogMode::Export),
            KeyCode::Char('x') => self.reject_suggestion(ctx),
            KeyCode::Char(ch) if is_typed_char(&key) => {
                let mut buffer = [0u8; 4];
                self.edit_name(EditCommand::Insert(ch.encode_utf8(&mut buffer)));
            }
            _ => return Outcome::Ignored,
        }
        Outcome::Consumed
    }

    fn mouse(
        &mut self,
        kind: MouseEventKind,
        hit: Option<RecipeHit>,
        ctx: &mut Ctx<'_>,
    ) -> Outcome {
        match kind {
            MouseEventKind::Down(MouseButton::Left) => match hit {
                Some(RecipeHit::Menu(index)) => return self.choose_menu(index),
                Some(RecipeHit::Row(index)) => {
                    self.focus_control(RecipeDialogControl::List);
                    self.select_row(index);
                }
                Some(RecipeHit::Control(control)) => {
                    self.focus_control(control);
                    if !matches!(
                        control,
                        RecipeDialogControl::Input | RecipeDialogControl::List
                    ) {
                        return self.activate(ctx);
                    }
                }
                None => {}
            },
            MouseEventKind::ScrollUp => self.move_selection(-1),
            MouseEventKind::ScrollDown => self.move_selection(1),
            _ => {}
        }
        Outcome::Consumed
    }

    fn record(
        &mut self,
        geometry: RecipeGeometry,
        caret: Option<(u16, u16)>,
        surface: Surface,
    ) -> Surface {
        self.geometry = geometry;
        self.surface = Surface { caret, ..surface };
        self.surface
    }
}

fn contains(area: Rect, point: (u16, u16)) -> bool {
    point.0 >= area.x && point.0 < area.right() && point.1 >= area.y && point.1 < area.bottom()
}

/// §12.9's date cell for one revision, in the reader's display zone.
pub(crate) fn recipe_date(saved_at_unix_nanos: Option<i64>, zone: &str) -> String {
    saved_at_unix_nanos.map_or_else(
        || RECIPE_NO_DATE.to_owned(),
        |nanos| crate::app::format_display_date(nanos, zone),
    )
}

/// What a saved revision would restore, in one line. Replaces the
/// implementation-shaped `Preview search=… advanced=false …` row (§11).
fn recipe_summary(config: &RecipeConfig) -> String {
    let mut parts = Vec::new();
    if !config.search.is_empty() {
        parts.push(format!("search={:?}", config.search));
    }
    if !config.advanced.is_empty() {
        parts.push("advanced filter".to_owned());
    }
    let stages = config.enrichments.len() + usize::from(!config.enrichment.is_empty());
    if stages > 0 {
        parts.push(format!(
            "{stages} enrichment{}",
            if stages == 1 { "" } else { "s" }
        ));
    }
    if !config.grouping.is_empty() {
        parts.push("grouping".to_owned());
    }
    if !config.pinned_columns.is_empty() {
        parts.push(format!("{} pinned", config.pinned_columns.len()));
    }
    match config.capture_time_policy {
        Some(crate::CaptureTimePolicy::Recent { .. }) => parts.push("rolling window".to_owned()),
        Some(crate::CaptureTimePolicy::Absolute(_)) => parts.push("fixed window".to_owned()),
        None => {}
    }
    if parts.is_empty() {
        return "no filters".to_owned();
    }
    parts.join(" · ")
}

impl Component for RecipesDialog {
    type Hit = RecipeHit;
    type Open = RecipesOpen;

    fn open(&mut self, params: RecipesOpen, _ctx: &mut Ctx<'_>) {
        match params {
            // `r`: a new dialog, and the only variant that resets. Its
            // generation is the fence every request it issues carries.
            RecipesOpen::Browse => {
                let dialog_id = self.next_id;
                self.next_id = self.next_id.saturating_add(1);
                self.state = RecipeDialogState {
                    id: dialog_id,
                    status: "loading recipes…".into(),
                    control: RecipeDialogControl::List,
                    loading: true,
                    ..RecipeDialogState::default()
                };
                self.cursor = TextCursor::default();
                self.geometry = RecipeGeometry::default();
                self.recipe_id.clear();
                self.recipe_name.clear();
                self.open = true;
                if !self.queue_full() {
                    let meta = self.next_meta();
                    self.state.pending_request_id = Some(meta.request_id);
                    let _ = self.outbox.push(RecipeRequest::List { meta });
                }
            }
            // A `Replace` out of History. It continues the dialog that is
            // already here — the list, the selection and the name field are the
            // ones the user was looking at, because this was one mode switch on
            // one dialog before the conversion and still is (§6.5).
            RecipesOpen::Mode(mode) => {
                self.open = true;
                self.select_mode(mode);
            }
            // A `Replace` into the revision list. Same dialog, same fence id;
            // the revisions replace the list, exactly as the mode switch did.
            RecipesOpen::History {
                recipe_id,
                recipe_name,
            } => {
                self.open = true;
                self.recipe_id = recipe_id.clone();
                self.recipe_name = recipe_name;
                self.state.mode = RecipeDialogMode::History;
                self.state.control = RecipeDialogControl::List;
                self.state.loading = false;
                self.state.pending_request_id = None;
                self.state.status.clear();
                self.touch();
                if !self.queue_full() {
                    self.state.loading = true;
                    let meta = self.next_meta();
                    self.state.pending_request_id = Some(meta.request_id);
                    self.state.selected = 0;
                    self.state.items.clear();
                    self.state.suggestions.clear();
                    let _ = self.outbox.push(RecipeRequest::History { meta, recipe_id });
                }
            }
        }
    }

    fn handle(&mut self, event: Event<RecipeHit>, ctx: &mut Ctx<'_>) -> Outcome {
        match event {
            Event::Key(key) => self.key(key, ctx),
            Event::Paste(text) => {
                self.paste(&text);
                Outcome::Consumed
            }
            Event::Mouse { kind, hit, .. } => self.mouse(kind, hit, ctx),
            // §5.3: the innermost thing closes first, so an open menu absorbs
            // the dismissal rather than the whole layer.
            Event::Dismiss => {
                if self.state.menu_open {
                    self.state.menu_open = false;
                    self.state.menu_selected = 0;
                    return Outcome::Consumed;
                }
                self.open = false;
                Outcome::Close
            }
            Event::Command(id) => self.command(id, ctx),
            Event::View(_) | Event::Resize => Outcome::Ignored,
        }
    }

    fn commands(&self, _views: &Views) -> Vec<CommandEntry> {
        RECIPE_COMMANDS
            .iter()
            .map(|(id, spec)| CommandEntry {
                spec: CommandSpec {
                    shortcut: self.open.then(|| recipe_command_shortcut(*id)).flatten(),
                    ..*spec
                },
                unavailable_reason: (!self.open).then_some("open Recipes first").or({
                    (*id == CommandId::RecipeApply && !self.state.mode.is_list())
                        .then_some("open Recipes in browse mode first")
                }),
            })
            .collect()
    }

    fn surface(&self) -> Surface {
        self.surface
    }

    fn hit(&self, point: (u16, u16)) -> Option<RecipeHit> {
        // §10: the open menu is drawn last and takes the point first.
        self.geometry
            .menu
            .iter()
            .find_map(|(rect, index)| contains(*rect, point).then_some(RecipeHit::Menu(*index)))
            .or_else(|| {
                self.geometry.rows.iter().find_map(|(rect, index)| {
                    contains(*rect, point).then_some(RecipeHit::Row(*index))
                })
            })
            .or_else(|| {
                self.geometry.controls.iter().find_map(|(rect, control)| {
                    contains(*rect, point).then_some(RecipeHit::Control(*control))
                })
            })
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &RenderCtx<'_>) -> Surface {
        use crate::dialog_layout::{DialogClass, DialogContent, content_width, pane};
        use RecipeDialogControl as C;
        use RecipeDialogMode as M;

        let theme = ctx.theme;
        let styles = DialogStyles::new(theme);
        let ascii = ctx.ascii;
        let mut geometry = RecipeGeometry::default();
        let mut caret_cell: Option<(u16, u16)> = None;
        let dialog = self.state.clone();
        let width = content_width(area, DialogClass::M);
        let history = dialog.mode == M::History;
        let title = if history {
            format!("Recipes › {} history", self.recipe_name)
        } else {
            "Recipes".to_owned()
        };
        let heading = if history {
            "Revisions"
        } else {
            "Saved recipes"
        };

        // §3: the message row says what just happened; the standing consequence
        // of the current mode is help, so it stays visible once a status exists.
        let help = match dialog.mode {
            M::Save => "Only accepted settings are saved; unfinished drafts are excluded.",
            M::Update => concat!(
                "Saving creates a NEW revision from the active view's accepted settings. ",
                "Old revisions remain; a concurrent update is rejected, so reload and retry."
            ),
            M::Import => {
                "Import installs a canonical copy for preview; Apply is a separate action."
            }
            M::Export => concat!(
                "Exports the selected revision, including source paths and environment. ",
                "It never overwrites an existing file."
            ),
            M::History => "Applying a revision leaves the recipe's own pointer unchanged.",
            M::Browse => "Apply restores a recipe's filters and enrichments.",
        };
        let (state, sentence) = if dialog.loading {
            (MessageState::Updating, dialog.status.clone())
        } else if dialog.status.is_empty() {
            (MessageState::Ready, String::new())
        } else if dialog.status.contains("fail") || dialog.status.contains("full") {
            (MessageState::Error, dialog.status.clone())
        } else {
            (MessageState::Applied, dialog.status.clone())
        };

        // Notes that qualify the selected recipe: why it was suggested, what a
        // suggestion could not confirm, and why it could not be applied.
        let selected_item = dialog.items.get(dialog.selected);
        let mut notes: Vec<(String, bool)> = Vec::new();
        // A mode that acts on one revision names it, because the list's marker
        // is easy to lose track of once the caret is in the field below it.
        if matches!(dialog.mode, M::Export | M::Update)
            && let Some(item) = selected_item
        {
            notes.push((
                format!("Selected: {} · {}", item.name, item.revision),
                false,
            ));
        }
        if !dialog.mode.is_editable()
            && let Some(item) = selected_item
        {
            if let Some(suggestion) = dialog
                .suggestions
                .iter()
                .find(|value| value.recipe_id == item.id)
            {
                notes.push((
                    format!("Suggested because: {}", suggestion.evidence.join("; ")),
                    false,
                ));
                if !suggestion.missing_fields.is_empty() {
                    notes.push((
                        format!(
                            "Not observed in the sampled rows: {}",
                            suggestion.missing_fields.join(", ")
                        ),
                        true,
                    ));
                }
            }
            if let Some(error) = &item.incompatibility {
                notes.push((format!("Cannot apply: {error}"), true));
            }
        }

        let label = if matches!(dialog.mode, M::Import | M::Export) {
            "TOML path"
        } else {
            "Name"
        };
        let primary = match dialog.mode {
            M::Save | M::Update => "Save revision",
            M::Import => "Review import",
            M::Export => "Export revision",
            M::History => "Apply revision",
            M::Browse => "Apply",
        };
        let more = if ascii { "More v" } else { "More ▾" };
        let mut actions: Vec<(&str, C)> = vec![(primary, C::Apply)];
        if dialog.mode.is_editable() {
            actions.push(("Cancel", C::Cancel));
        } else {
            let suggested = selected_item.is_some_and(|item| {
                dialog
                    .suggestions
                    .iter()
                    .any(|value| value.recipe_id == item.id)
            });
            if suggested {
                actions.extend([("&Adapt", C::Adapt), ("Reject", C::Reject)]);
            }
            if dialog.mode == M::Browse {
                actions.extend([
                    ("&Save", C::Save),
                    ("&Update", C::Update),
                    ("&History", C::History),
                    (more, C::More),
                ]);
            }
        }
        let action_labels = actions.iter().map(|(label, _)| *label).collect::<Vec<_>>();

        // A note explains why a recipe was suggested or cannot be applied.
        // Truncating that to one line loses the reason, so it wraps (§9).
        let note_width =
            usize::from(width.saturating_sub(crate::dialog_layout::PANE_INDENT)).max(1);
        let note_lines: Vec<(String, bool)> = notes
            .iter()
            .flat_map(|(text, warn)| {
                wrap_sentence(text, note_width, 2)
                    .into_iter()
                    .map(move |line| (line, *warn))
            })
            .collect();
        let list_rows = dialog.items.len().clamp(1, 12);
        let body = u16::try_from(list_rows + 1 + note_lines.len())
            .unwrap_or(u16::MAX)
            // A blank row, then the name field.
            .saturating_add(2);
        let content = DialogContent {
            header: 0,
            body,
            message: message_rows(&sentence, width).max(1),
            help: help_rows(help, width),
            actions: packed_button_rows(width, &action_labels),
        };
        let regions = dialog_frame_regions(frame, area, DialogClass::M, &title, &content, theme);
        let mut surface = Surface {
            popup: regions.popup,
            interior: regions.interior,
            caret: None,
            scrollable: true,
            text_focus: self.editing(),
        };
        let inner = regions.body;
        if inner.width == 0 || inner.height == 0 {
            return self.record(geometry, caret_cell, surface);
        }

        // §8.5/§8.7: a list is a pane — heading, count, indented rows, scrollbar.
        let note_rows = u16::try_from(note_lines.len())
            .unwrap_or(0)
            .min(inner.height);
        let field_rows = 2u16.min(inner.height.saturating_sub(note_rows));
        let list_area = Rect::new(
            inner.x,
            inner.y,
            inner.width,
            inner
                .height
                .saturating_sub(note_rows)
                .saturating_sub(field_rows),
        );
        let count = format!(
            "{} of {}",
            dialog.selected.saturating_add(1).min(dialog.items.len()),
            dialog.items.len()
        );
        let rects = pane(
            list_area,
            u16::try_from(UnicodeWidthStr::width(count.as_str())).unwrap_or(0),
            dialog.items.len(),
        );
        if rects.heading.height > 0 {
            frame.render_widget(
                Paragraph::new(heading).style(styles.label.add_modifier(Modifier::BOLD)),
                rects.heading,
            );
            if rects.count.width > 0 {
                frame.render_widget(
                    Paragraph::new(count.clone()).style(styles.description),
                    rects.count,
                );
            }
        }
        let visible = usize::from(rects.viewport.height);
        let first = dialog
            .selected
            .saturating_add(1)
            .saturating_sub(visible.max(1));
        if dialog.items.is_empty() {
            if rects.viewport.height > 0 {
                frame.render_widget(
                    Paragraph::new(truncated(
                        if history {
                            "No revisions"
                        } else {
                            "No saved recipes yet · Save stores the current filters"
                        },
                        usize::from(rects.viewport.width),
                    ))
                    .style(styles.description),
                    Rect::new(rects.viewport.x, rects.viewport.y, rects.viewport.width, 1),
                );
            }
        } else {
            for (offset, (index, item)) in dialog
                .items
                .iter()
                .enumerate()
                .skip(first)
                .take(visible)
                .enumerate()
            {
                let y = rects.viewport.y.saturating_add(offset as u16);
                let row = Rect::new(rects.viewport.x, y, rects.viewport.width, 1);
                let focused = index == dialog.selected;
                let suggested = dialog
                    .suggestions
                    .iter()
                    .any(|value| value.recipe_id == item.id);
                let marker = if focused {
                    if ascii { "> " } else { "› " }
                } else {
                    "  "
                };
                let name = format!("{marker}{}{}", if suggested { "★ " } else { "" }, item.name);
                let name_width = RECIPE_NAME_WIDTH.min(row.width);
                frame.render_widget(
                    Paragraph::new(truncated(&name, usize::from(name_width))).style(if focused {
                        styles.selection
                    } else {
                        styles.label
                    }),
                    Rect::new(row.x, y, name_width, 1),
                );
                let summary_x = row
                    .x
                    .saturating_add(name_width)
                    .saturating_add(FIELD_GUTTER);
                // The date only earns its column when a readable summary still
                // fits beside it; otherwise the summary is the more useful of
                // the two and takes the whole remainder (§4.4).
                let date_width = if summary_x.saturating_add(RECIPE_DATE_WIDTH + FIELD_GUTTER + 8)
                    <= row.right()
                {
                    RECIPE_DATE_WIDTH
                } else {
                    0
                };
                let summary_width = row
                    .right()
                    .saturating_sub(date_width)
                    .saturating_sub(if date_width > 0 { FIELD_GUTTER } else { 0 })
                    .saturating_sub(summary_x);
                if summary_width > 0 {
                    frame.render_widget(
                        Paragraph::new(truncated(
                            &recipe_summary(&item.config),
                            usize::from(summary_width),
                        ))
                        .style(styles.description),
                        Rect::new(summary_x, y, summary_width, 1),
                    );
                }
                if date_width > 0 {
                    // §4.4: a date is a fact in a column, right-aligned to the
                    // content edge like every other one.
                    frame.render_widget(
                        Paragraph::new(recipe_date(item.saved_at_unix_nanos, ctx.display_zone))
                            .style(styles.description)
                            .right_aligned(),
                        Rect::new(row.right().saturating_sub(date_width), y, date_width, 1),
                    );
                }
                geometry.rows.push((row, index));
            }
        }
        if let Some(bar) = rects.scrollbar {
            render_scrollbar(
                frame,
                bar,
                first,
                dialog.items.len().saturating_sub(visible),
                theme,
                ascii,
            );
        }

        for (offset, (text, warn)) in note_lines.iter().enumerate() {
            let y = list_area.bottom().saturating_add(offset as u16);
            if y >= inner.bottom() {
                break;
            }
            frame.render_widget(
                Paragraph::new(truncated(
                    text,
                    usize::from(
                        inner
                            .width
                            .saturating_sub(crate::dialog_layout::PANE_INDENT),
                    ),
                ))
                .style(if *warn {
                    styles.error
                } else {
                    styles.description
                }),
                Rect::new(
                    inner.x.saturating_add(crate::dialog_layout::PANE_INDENT),
                    y,
                    inner
                        .width
                        .saturating_sub(crate::dialog_layout::PANE_INDENT),
                    1,
                ),
            );
        }

        // §4.2: the name shares the dialog's one label column.
        let field_y = inner.bottom().saturating_sub(1);
        if field_y >= inner.y && field_rows > 0 {
            let focused = dialog.control == C::Input;
            let (input, caret) = render_form_field(
                frame,
                Rect::new(inner.x, field_y, inner.width, 1),
                RECIPE_LABEL_WIDTH,
                label,
                &dialog.name,
                if matches!(dialog.mode, M::Import | M::Export) {
                    "path to a recipe TOML"
                } else {
                    "a name for this recipe"
                },
                focused,
                focused.then_some(self.cursor.char_index),
                theme,
            );
            geometry.controls.push((input, C::Input));
            caret_cell = caret;
        }

        render_message(frame, regions.message, state, &sentence, theme, ascii);
        render_help_text(frame, regions.help, help, theme);
        let focused_action = actions
            .iter()
            .position(|(_, control)| *control == dialog.control);
        let mut menu_anchor = None;
        for (index, rect) in render_action_row(
            frame,
            regions.actions,
            &action_labels,
            focused_action,
            &[],
            theme,
        ) {
            let control = actions[index].1;
            if control == C::More {
                menu_anchor = Some(rect);
            }
            geometry.controls.push((rect, control));
        }

        // §10: the menu is an anchored popup, drawn last and bounded by the frame.
        if dialog.menu_open
            && let Some(anchor) = menu_anchor
        {
            let items = RECIPE_MORE_ITEMS;
            let box_width = items
                .iter()
                .map(|(label, _)| UnicodeWidthStr::width(*label) as u16)
                .max()
                .unwrap_or(8)
                .saturating_add(4)
                .min(area.width);
            let box_height = (items.len() as u16 + 2).min(area.height);
            let x = anchor.x.min(area.right().saturating_sub(box_width));
            let above = anchor.y.saturating_sub(box_height);
            let y = if anchor.y.saturating_add(1).saturating_add(box_height) <= area.bottom() {
                anchor.y.saturating_add(1)
            } else {
                above.max(area.y)
            };
            let box_area = Rect::new(x, y, box_width, box_height);
            if box_area.width >= 3 && box_area.height >= 3 {
                // §5.2 containment is measured against everything the layer
                // drew; an anchored popup may extend past the dialog (§5.3).
                surface.popup = surface.popup.union(box_area);
                frame.render_widget(Clear, box_area);
                frame.render_widget(
                    List::new(
                        items
                            .iter()
                            .enumerate()
                            .map(|(index, (label, _))| {
                                ListItem::new(*label).style(if index == dialog.menu_selected {
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
                for index in 0..items.len() {
                    geometry.menu.push((
                        Rect::new(
                            box_area.x + 1,
                            box_area.y + 1 + index as u16,
                            box_area.width.saturating_sub(2),
                            1,
                        ),
                        index,
                    ));
                }
            }
        }

        self.record(geometry, caret_cell, surface)
    }
}

impl RecipesDialog {
    fn command(&mut self, id: CommandId, ctx: &mut Ctx<'_>) -> Outcome {
        match id {
            CommandId::RecipeBrowse => return self.switch(RecipeDialogMode::Browse),
            CommandId::RecipeSave => return self.switch(RecipeDialogMode::Save),
            CommandId::RecipeImport => return self.switch(RecipeDialogMode::Import),
            CommandId::RecipeExport => return self.switch(RecipeDialogMode::Export),
            CommandId::RecipeUpdate => return self.switch(RecipeDialogMode::Update),
            CommandId::RecipeHistory => return self.switch(RecipeDialogMode::History),
            CommandId::RecipeApply => return self.submit(ctx),
            CommandId::RecipeRefreshSuggestions => self.refresh_suggestions(),
            CommandId::RecipeAdaptSuggestion => return self.adapt_suggestion(ctx),
            CommandId::RecipeRejectSuggestion => self.reject_suggestion(ctx),
            _ => return Outcome::Ignored,
        }
        Outcome::Consumed
    }
}

/// The shortcut column the palette shows while the layer is on top. It comes
/// from the component rather than from probing a focus-specific key table (§4.3).
fn recipe_command_shortcut(id: CommandId) -> Option<&'static str> {
    Some(match id {
        CommandId::RecipeBrowse => "Alt-B",
        CommandId::RecipeSave => "Alt-S",
        CommandId::RecipeImport => "Alt-I",
        CommandId::RecipeExport => "Alt-E",
        CommandId::RecipeHistory => "Alt-H",
        CommandId::RecipeUpdate => "Alt-U",
        CommandId::RecipeApply => "Enter",
        CommandId::RecipeRefreshSuggestions => "Alt-G",
        CommandId::RecipeAdaptSuggestion => "Alt-A",
        CommandId::RecipeRejectSuggestion => "x",
        _ => return None,
    })
}
