//! Converted dialogs. Each is a `Component` (`docs/component-model.md` §1) that
//! owns its state, keymap, geometry and outbox.

pub mod ask;
pub mod bookmarks;
pub mod color_rules;
pub mod correlation;
pub mod editors;
pub mod enrichment;
pub mod enrichment_step;
pub mod external_command;
pub mod fields;
pub mod folding;
pub mod help;
pub mod investigation;
pub mod recipes;
pub mod settings;
pub mod source;
pub mod storage;
pub mod time;
pub mod view;

use crate::app::QueryPurpose;
use crate::component::LayerId;
use ask::AskDialog;
use bookmarks::BookmarksDialog;
use color_rules::ColorRulesDialog;
use correlation::CorrelationDialog;
use editors::EditorDialog;
use enrichment::EnrichmentDialog;
use enrichment_step::EnrichmentStepLayer;
use external_command::ExternalCommandDialog;
use fields::FieldsDialog;
use folding::FoldingDialog;
use help::HelpDialog;
use investigation::InvestigationDialog;
use recipes::RecipesDialog;
use settings::SettingsDialog;
use source::SourceDialog;
use storage::StorageDialog;
use time::TimeDialog;
use view::ViewDialog;

/// One permanent slot per component plus the layer stack (§2.5). Kept as a
/// separate field of `App` so a `Ctx` built from the shell's state and a
/// `&mut` component are disjoint borrows.
#[derive(Debug)]
pub struct Layers {
    pub storage: StorageDialog,
    pub ask: AskDialog,
    pub investigation: InvestigationDialog,
    pub time: TimeDialog,
    pub help: HelpDialog,
    pub settings: SettingsDialog,
    pub fields: FieldsDialog,
    /// Reached from Fields by `Replace`: the lookup and the mapping are one
    /// session, and the record Fields froze is the only thing it carries in.
    pub correlation: CorrelationDialog,
    pub bookmarks: BookmarksDialog,
    pub view: ViewDialog,
    /// The per-view folding policy (`components/folding.rs`).
    pub folding: FoldingDialog,
    pub source: SourceDialog,
    /// Serves both `LayerId::Recipes` and `LayerId::RecipeHistory`: they are
    /// two surfaces of one dialog, and every transition between them is a
    /// `Replace` that carries its state across (§6.5).
    pub recipes: RecipesDialog,
    /// The Filter dialog (§12.1): Search and Advanced as two tabs of one
    /// slot, whose `purpose` is the active tab.
    pub filter: EditorDialog,
    pub grouping: EditorDialog,
    pub color_rules: ColorRulesDialog,
    pub enrichment: EnrichmentDialog,
    /// The only true child in the model (§5.3): it is opened by `Enrichment`
    /// with `OpenChild`, draws over the list it came from, and `Close` returns
    /// the user to it.
    pub enrichment_step: EnrichmentStepLayer,
    /// Reached from Enrichment by `Replace`, not `OpenChild`: it draws no
    /// parent and does not return to the list (§6.5).
    pub external_command: ExternalCommandDialog,
    /// Bottom → top.
    pub stack: Vec<LayerId>,
}

impl Default for Layers {
    fn default() -> Self {
        Self {
            storage: StorageDialog::default(),
            ask: AskDialog::default(),
            investigation: InvestigationDialog::default(),
            time: TimeDialog::default(),
            help: HelpDialog::default(),
            settings: SettingsDialog::default(),
            fields: FieldsDialog::default(),
            correlation: CorrelationDialog::default(),
            bookmarks: BookmarksDialog::default(),
            view: ViewDialog::default(),
            folding: FoldingDialog::default(),
            source: SourceDialog::default(),
            recipes: RecipesDialog::default(),
            // One type, two slots: the editors differ only by the purpose
            // they submit under, so the slot carries it (§6.5); Filter's is
            // whichever tab is active.
            filter: EditorDialog::filter(),
            grouping: EditorDialog::new(QueryPurpose::Grouping),
            color_rules: ColorRulesDialog::default(),
            enrichment: EnrichmentDialog::default(),
            enrichment_step: EnrichmentStepLayer::default(),
            external_command: ExternalCommandDialog::default(),
            stack: Vec::new(),
        }
    }
}

impl Layers {
    pub fn top(&self) -> Option<LayerId> {
        self.stack.last().copied()
    }

    /// The stack bottom → top. Read-only: pushing and popping is the shell's.
    pub fn stack_ids(&self) -> Vec<LayerId> {
        self.stack.clone()
    }
}
