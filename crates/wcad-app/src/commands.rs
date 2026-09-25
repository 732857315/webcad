//! Command registry: every command (interactive tool or immediate action) with its name, AutoCAD
//! aliases, localized label, icon glyph and ribbon placement, plus registered dock panels and 3D
//! viewport hooks. Feature modules add entries from their `register` functions
//! (see [`CommandRegistry::with_all_modules`]).

use crate::editor::Editor;
use crate::i18n::{Lang, core};
use crate::panels::PanelSpec;
use crate::tools::Tool;
use crate::view3d::View3dHook;

/// Ribbon tab (also decides the menu a command appears in).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RibbonTab {
    /// "Home": drawing commands.
    Draw,
    Modify,
    Annotate,
    /// 3D features.
    Model,
    Sketch,
    View,
}

impl RibbonTab {
    pub const ALL: [RibbonTab; 6] = [
        RibbonTab::Draw,
        RibbonTab::Modify,
        RibbonTab::Annotate,
        RibbonTab::Model,
        RibbonTab::Sketch,
        RibbonTab::View,
    ];

    pub fn label(self, lang: Lang) -> &'static str {
        let s = core(lang);
        match self {
            RibbonTab::Draw => s.tab_draw,
            RibbonTab::Modify => s.tab_modify,
            RibbonTab::Annotate => s.tab_annotate,
            RibbonTab::Model => s.tab_model,
            RibbonTab::Sketch => s.tab_sketch,
            RibbonTab::View => s.tab_view,
        }
    }
}

/// What running a command does.
#[derive(Clone, Copy)]
pub enum CommandKind {
    /// Start an interactive tool (replaces the active one).
    Tool(fn() -> Box<dyn Tool>),
    /// Run immediately (undo, select all, file actions via [`crate::editor::AppRequest`]).
    Action(fn(&mut Editor)),
}

/// One registered command.
#[derive(Clone, Copy)]
pub struct CommandSpec {
    /// Canonical upper-case name typed on the command line ("LINE").
    pub name: &'static str,
    /// Upper-case aliases ("L").
    pub aliases: &'static [&'static str],
    /// Localized button/menu label.
    pub label: fn(Lang) -> &'static str,
    /// Icon glyph for ribbon buttons (must exist in the UI fonts; checked by a test).
    pub icon: &'static str,
    /// Ribbon tab and menu; `None` = command line only.
    pub tab: Option<RibbonTab>,
    /// Buttons of the same group are laid out together; groups are separated.
    pub group: &'static str,
    pub kind: CommandKind,
}

impl CommandSpec {
    pub fn is_tool(&self) -> bool {
        matches!(self.kind, CommandKind::Tool(_))
    }
}

#[derive(Default)]
pub struct CommandRegistry {
    commands: Vec<CommandSpec>,
    panels: Vec<PanelSpec>,
    view3d_hooks: Vec<View3dHook>,
    ui_hooks: Vec<UiHook>,
}

/// Called once per frame after the main layout (for feature dialogs/windows, e.g. page setup or
/// the text editor). Keep per-hook state in `ctx.data_mut` or the document.
pub type UiHook = fn(&egui::Context, &mut Editor);

impl CommandRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// The registry with the core commands and every feature module registered.
    pub fn with_all_modules() -> Self {
        let mut r = Self::new();
        crate::tools::core::register(&mut r);
        crate::tools::draw::register(&mut r);
        crate::tools::modify::register(&mut r);
        crate::tools::annotate::register(&mut r);
        crate::modeling::register(&mut r);
        crate::panels::register_builtin(&mut r);
        crate::panels::extra::register(&mut r);
        r
    }

    /// Register a command. A later registration with the same name replaces the earlier one
    /// (feature modules may override core commands).
    pub fn add(&mut self, spec: CommandSpec) {
        if let Some(old) = self
            .commands
            .iter_mut()
            .find(|c| c.name.eq_ignore_ascii_case(spec.name))
        {
            *old = spec;
        } else {
            self.commands.push(spec);
        }
    }

    /// Register a dock panel. Same `id` replaces (e.g. the modeling module's model tree).
    pub fn add_panel(&mut self, panel: PanelSpec) {
        if let Some(old) = self.panels.iter_mut().find(|p| p.id == panel.id) {
            *old = panel;
        } else {
            self.panels.push(panel);
        }
    }

    /// Register a 3D viewport hook (called every frame the modeling viewport is shown).
    pub fn add_view3d_hook(&mut self, hook: View3dHook) {
        self.view3d_hooks.push(hook);
    }

    /// Register a per-frame UI hook.
    pub fn add_ui_hook(&mut self, hook: UiHook) {
        self.ui_hooks.push(hook);
    }

    pub fn ui_hooks(&self) -> &[UiHook] {
        &self.ui_hooks
    }

    pub fn commands(&self) -> &[CommandSpec] {
        &self.commands
    }

    pub fn panels(&self) -> &[PanelSpec] {
        &self.panels
    }

    pub fn view3d_hooks(&self) -> &[View3dHook] {
        &self.view3d_hooks
    }

    /// Find by name or alias (case-insensitive).
    pub fn find(&self, name: &str) -> Option<&CommandSpec> {
        let n = name.trim();
        if n.is_empty() {
            return None;
        }
        self.commands
            .iter()
            .find(|c| c.name.eq_ignore_ascii_case(n))
            .or_else(|| {
                self.commands
                    .iter()
                    .find(|c| c.aliases.iter().any(|a| a.eq_ignore_ascii_case(n)))
            })
    }

    /// Commands shown on `tab`, in registration order.
    pub fn on_tab(&self, tab: RibbonTab) -> impl Iterator<Item = &CommandSpec> {
        self.commands.iter().filter(move |c| c.tab == Some(tab))
    }

    /// Command names starting with `prefix` (for command-line completion), sorted.
    pub fn complete(&self, prefix: &str) -> Vec<&'static str> {
        let p = prefix.trim().to_ascii_uppercase();
        if p.is_empty() {
            return Vec::new();
        }
        let mut v: Vec<&'static str> = self
            .commands
            .iter()
            .filter(|c| c.name.starts_with(&p))
            .map(|c| c.name)
            .collect();
        v.sort_unstable();
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_by_name_and_alias() {
        let r = CommandRegistry::with_all_modules();
        assert_eq!(r.find("line").map(|c| c.name), Some("LINE"));
        assert_eq!(r.find("L").map(|c| c.name), Some("LINE"));
        assert_eq!(r.find("c").map(|c| c.name), Some("CIRCLE"));
        assert_eq!(r.find("e").map(|c| c.name), Some("ERASE"));
        assert_eq!(r.find("M").map(|c| c.name), Some("MOVE"));
        assert_eq!(r.find("z").map(|c| c.name), Some("ZOOM"));
        assert!(r.find("nope").is_none());
        assert!(r.find("").is_none());
        assert!(r.complete("LI").contains(&"LINE"));
    }

    #[test]
    fn names_are_unique_and_upper_case() {
        let r = CommandRegistry::with_all_modules();
        let mut seen = std::collections::HashSet::new();
        for c in r.commands() {
            assert_eq!(c.name, c.name.to_ascii_uppercase());
            assert!(seen.insert(c.name), "duplicate {}", c.name);
            for a in c.aliases {
                assert_eq!(*a, a.to_ascii_uppercase());
            }
        }
    }

    #[test]
    fn icons_and_labels_render_with_ui_fonts() {
        use egui::epaint::text::{Fonts, TextOptions};
        let mut fonts = Fonts::new(TextOptions::default(), crate::fonts::definitions());
        let id = egui::FontId::proportional(16.0);
        let r = CommandRegistry::with_all_modules();
        for c in r.commands() {
            assert!(
                fonts.has_glyphs(&id, c.icon),
                "icon of {} ({:?}) missing in fonts",
                c.name,
                c.icon
            );
            for lang in Lang::ALL {
                assert!(
                    fonts.has_glyphs(&id, (c.label)(lang)),
                    "label of {} missing glyphs",
                    c.name
                );
            }
        }
    }
}
