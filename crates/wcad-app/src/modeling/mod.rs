//! 3D modeling UI (features, sketches). **Owned by the modeling package** (stub created by app-core).
//!
//! Hooks available to this module:
//! - commands: `r.add(CommandSpec { tab: Some(RibbonTab::Model | RibbonTab::Sketch), .. })`;
//! - dock panels: `r.add_panel(PanelSpec { id: "model_tree", .. })` replaces the core model tree;
//! - 3D viewport: `r.add_view3d_hook(fn(&mut View3dCx))` runs every frame the modeling viewport is
//!   shown (input handling, overlays, picking via `View3dCx::ray`).
//!
//! The regenerated bodies are in `View3dCx::regen` (`wcad_solid::RegenResult`).

use crate::commands::CommandRegistry;

/// Register the modeling commands, panels and viewport hooks (currently none).
pub fn register(_r: &mut CommandRegistry) {}
