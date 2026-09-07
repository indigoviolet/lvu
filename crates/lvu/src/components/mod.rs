//! Converted dialogs. Each is a `Component` (`docs/component-model.md` §1) that
//! owns its state, keymap, geometry and outbox.

pub mod fields;
pub mod help;
pub mod recipes;
pub mod settings;
pub mod storage;
pub mod time;
pub mod view;

use crate::component::LayerId;
use fields::FieldsDialog;
use help::HelpDialog;
use recipes::RecipesDialog;
use settings::SettingsDialog;
use storage::StorageDialog;
use time::TimeDialog;
use view::ViewDialog;

/// One permanent slot per component plus the layer stack (§2.5). Kept as a
/// separate field of `App` so a `Ctx` built from the shell's state and a
/// `&mut` component are disjoint borrows.
#[derive(Debug, Default)]
pub struct Layers {
    pub storage: StorageDialog,
    pub time: TimeDialog,
    pub help: HelpDialog,
    pub settings: SettingsDialog,
    pub fields: FieldsDialog,
    pub view: ViewDialog,
    /// Serves both `LayerId::Recipes` and `LayerId::RecipeHistory`: they are
    /// two surfaces of one dialog, and every transition between them is a
    /// `Replace` that carries its state across (§6.5).
    pub recipes: RecipesDialog,
    /// Bottom → top.
    pub stack: Vec<LayerId>,
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
