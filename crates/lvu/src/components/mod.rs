//! Converted dialogs. Each is a `Component` (`docs/component-model.md` §1) that
//! owns its state, keymap, geometry and outbox.

pub mod fields;
pub mod help;
pub mod settings;
pub mod storage;
pub mod time;

use crate::component::LayerId;
use fields::FieldsDialog;
use help::HelpDialog;
use settings::SettingsDialog;
use storage::StorageDialog;
use time::TimeDialog;

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
    /// Bottom → top.
    pub stack: Vec<LayerId>,
}

impl Layers {
    pub fn top(&self) -> Option<LayerId> {
        self.stack.last().copied()
    }
}
