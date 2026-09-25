//! Modify commands. **Owned by the modify feature package** (stub created by app-core).
//!
//! Add tools here and register them in [`register`] with
//! `r.add(CommandSpec { name, aliases, label, icon, tab: Some(RibbonTab::…), group, kind: CommandKind::Tool(|| Box::new(MyTool::default())) })`.
//! Object-pick prompts (TRIM/EXTEND/FILLET) should return `Accept::PICK` from `Tool::accepts` and
//! resolve the clicked entity with `ToolCx::pick`.
//! Put this module's UI strings in its own struct (see `crate::i18n` for the pattern).

use crate::commands::CommandRegistry;

/// Register this module's commands (currently none).
pub fn register(_r: &mut CommandRegistry) {}
