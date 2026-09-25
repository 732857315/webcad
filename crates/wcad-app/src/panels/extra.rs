//! Extra dock panels. **Owned by a later feature package** (stub created by app-core).
//!
//! Register panels with `r.add_panel(PanelSpec { id, title, dock, ui })`; a panel with the id of a
//! built-in one (`"layers"`, `"model_tree"`, `"properties"`) replaces it.

use crate::commands::CommandRegistry;

/// Register extra panels (currently none).
pub fn register(_r: &mut CommandRegistry) {}
