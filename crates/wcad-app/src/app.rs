//! [`WebCadApp`]: the eframe application shell (menus, ribbon, docks, command line, status bar,
//! dialogs, file handling, autosave) around the [`Editor`] and the two viewports.

use std::sync::Arc;

use egui::{Align, Color32, Key, KeyboardShortcut, Layout, Modifiers, RichText};

use crate::commands::{CommandRegistry, RibbonTab};
use crate::editor::{AppRequest, Editor, ExportFormat, LogKind, Workspace};
use crate::files;
use crate::gpu::Gpu;
use crate::i18n::{Lang, core, fmt};
use crate::panels::{Dock, PanelCx};
use crate::platform::{self, Inbox, KvStore, OpenPurpose, PlatformEvent};
use crate::settings::{SETTINGS_KEY, Settings, Theme};
use crate::view2d::View2d;
use crate::view3d::View3d;

/// Width below which the layout is compact (docks start collapsed, ribbon icons only).
const NARROW_WIDTH: f32 = 760.0;

/// Actions waiting for the "unsaved changes" confirmation.
enum Pending {
    New,
    Open,
    Demo,
    Bytes {
        name: String,
        bytes: Vec<u8>,
        path: Option<std::path::PathBuf>,
    },
}

pub struct WebCadApp {
    pub ed: Editor,
    pub settings: Settings,
    saved_settings: Settings,
    store: Box<dyn KvStore>,
    gpu: Option<Gpu>,
    gpu_tried: bool,
    pub view2d: View2d,
    pub view3d: View3d,
    pub workspace: Workspace,
    ribbon_tab: RibbonTab,
    left_tab: &'static str,
    cmd_input: String,
    focus_cmd: bool,
    inbox: Inbox,
    /// File name of the document (`None` = untitled) and native path.
    doc_name: Option<String>,
    doc_path: Option<std::path::PathBuf>,
    warnings: Option<(String, Vec<String>)>,
    recovery: Option<(String, String)>,
    pending: Option<Pending>,
    show_about: bool,
    show_shortcuts: bool,
    applied_theme: Option<Theme>,
    first_frame: bool,
    last_autosave: f64,
    autosave_rev: u64,
    registry: Arc<CommandRegistry>,
}

impl WebCadApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        crate::fonts::install(&cc.egui_ctx);
        let mut app = Self::with_store(platform::default_store());
        if let Some(rs) = cc.wgpu_render_state.as_ref() {
            app.gpu = Some(Gpu::new(rs));
        }
        app.gpu_tried = true;
        #[cfg(target_arch = "wasm32")]
        app.apply_startup_query(&crate::web::query_string());
        app
    }

    /// Startup options from the page URL (web) or a test: `demo` loads the demo drawing,
    /// `lang=en|zh`, `theme=light|dark`, `ws=3d`, `cmd=L;0,0;10,0;` runs command-line entries.
    pub fn apply_startup_query(&mut self, query: &str) {
        for kv in query
            .trim_start_matches('?')
            .split('&')
            .filter(|s| !s.is_empty())
        {
            let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
            let v = percent_decode(v);
            match k {
                "demo" => {
                    self.load_document(
                        crate::demo::demo_document(),
                        Some("demo.wcad".into()),
                        None,
                    );
                    self.recovery = None;
                }
                "lang" => {
                    self.settings.lang = if v.eq_ignore_ascii_case("en") {
                        Lang::En
                    } else {
                        Lang::Zh
                    }
                }
                "theme" => {
                    self.settings.theme = if v.eq_ignore_ascii_case("light") {
                        Theme::Light
                    } else {
                        Theme::Dark
                    }
                }
                "ws" => {
                    self.workspace = if v.eq_ignore_ascii_case("3d") {
                        Workspace::Modeling
                    } else {
                        Workspace::Drafting
                    };
                }
                "cmd" => {
                    self.ed.lang = self.settings.lang;
                    for entry in v.split(';') {
                        self.ed.submit(entry);
                    }
                }
                _ => {}
            }
        }
    }

    /// App without a window (tests, headless screenshots). GPU rendering is disabled.
    pub fn with_store(store: Box<dyn KvStore>) -> Self {
        let settings = store
            .get(SETTINGS_KEY)
            .map(|s| Settings::from_json(&s))
            .unwrap_or_default();
        let registry = Arc::new(CommandRegistry::with_all_modules());
        let mut ed = Editor::new(registry.clone());
        ed.lang = settings.lang;
        ed.draft = settings.draft.clone();
        let recovery = store
            .get(files::AUTOSAVE_KEY)
            .filter(|s| !s.is_empty())
            .map(|payload| {
                (
                    store.get(files::AUTOSAVE_META_KEY).unwrap_or_default(),
                    payload,
                )
            });
        Self {
            ed,
            saved_settings: settings.clone(),
            settings,
            store,
            gpu: None,
            gpu_tried: false,
            view2d: View2d::default(),
            view3d: View3d::default(),
            workspace: Workspace::Drafting,
            ribbon_tab: RibbonTab::Draw,
            left_tab: "layers",
            cmd_input: String::new(),
            focus_cmd: false,
            inbox: Inbox::default(),
            doc_name: None,
            doc_path: None,
            warnings: None,
            recovery,
            pending: None,
            show_about: false,
            show_shortcuts: false,
            applied_theme: None,
            first_frame: true,
            last_autosave: 0.0,
            autosave_rev: 0,
            registry,
        }
    }

    /// Attach a GPU (headless screenshot tools create their own render state).
    pub fn attach_gpu(&mut self, rs: &eframe::egui_wgpu::RenderState) {
        self.gpu = Some(Gpu::new(rs));
        self.gpu_tried = true;
    }

    fn s(&self) -> &'static crate::i18n::CoreStrings {
        core(self.settings.lang)
    }

    /// Replace the document.
    pub fn load_document(
        &mut self,
        doc: wcad_doc::Document,
        name: Option<String>,
        path: Option<std::path::PathBuf>,
    ) {
        self.ed.set_document(doc);
        self.doc_name = name;
        self.doc_path = path;
        self.view2d.fit_pending = true;
        self.view3d.fit_pending = true;
        self.view3d.mark_part_dirty();
        self.autosave_rev = self.ed.doc.revision();
    }

    fn title_name(&self) -> String {
        self.doc_name
            .clone()
            .unwrap_or_else(|| self.s().untitled.to_owned())
    }

    // -------------------------------------------------------------------------------------------
    // Frame

    /// The whole UI for one frame (window-independent; used by `eframe::App::ui` and tests).
    pub fn frame_ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.ed.lang = self.settings.lang;
        if self.applied_theme != Some(self.settings.theme) {
            ctx.set_visuals(match self.settings.theme {
                Theme::Dark => egui::Visuals::dark(),
                Theme::Light => egui::Visuals::light(),
            });
            self.applied_theme = Some(self.settings.theme);
            self.ed.invalidate_display();
        }
        let width = ctx.content_rect().width();
        let narrow = width < NARROW_WIDTH;
        if self.first_frame {
            self.first_frame = false;
            if narrow {
                self.settings.left_panel = false;
                self.settings.right_panel = false;
            }
        }

        self.handle_platform_events();
        self.handle_dropped_files(&ctx);
        self.handle_shortcuts(&ctx);
        self.handle_requests(&ctx);

        // Distribute document changes to both views.
        let ch = self.ed.take_display_changes();
        if !ch.is_empty() {
            self.view2d.absorb(&ch);
            if ch.part || ch.all {
                self.view3d.mark_part_dirty();
            }
        }

        egui::Panel::top("menu_bar").show(ui, |ui| self.menu_bar(ui, narrow));
        egui::Panel::top("ribbon").show(ui, |ui| self.ribbon(ui, narrow));
        egui::Panel::bottom("status_bar").show(ui, |ui| self.status_bar(ui, narrow));
        egui::Panel::bottom("command_line")
            .resizable(true)
            .default_size(96.0)
            .show(ui, |ui| self.command_line(ui));
        if self.settings.left_panel {
            egui::Panel::left("left_dock")
                .resizable(true)
                .default_size(if narrow { width * 0.6 } else { 250.0 })
                .show(ui, |ui| self.dock(ui, Dock::Left));
        }
        if self.settings.right_panel {
            egui::Panel::right("right_dock")
                .resizable(true)
                .default_size(if narrow { width * 0.6 } else { 260.0 })
                .show(ui, |ui| self.dock(ui, Dock::Right));
        }
        egui::CentralPanel::no_frame().show(ui, |ui| {
            let theme = self.settings.theme;
            match self.workspace {
                Workspace::Drafting => self.view2d.show(ui, &mut self.ed, self.gpu.as_mut(), theme),
                Workspace::Modeling => {
                    let hooks: Vec<_> = self.registry.view3d_hooks().to_vec();
                    self.view3d
                        .show(ui, &mut self.ed, self.gpu.as_mut(), theme, &hooks)
                }
            }
        });
        // Registered dialogs/windows of feature modules.
        let hooks: Vec<_> = self.registry.ui_hooks().to_vec();
        for h in hooks {
            h(&ctx, &mut self.ed);
        }
        // Requests raised by the viewport or hooks (e.g. clicks that finished ZOOM).
        self.handle_requests(&ctx);
        self.dialogs(&ctx);
        self.hovered_files_overlay(&ctx);
        self.autosave(&ctx);
        self.persist_settings();
    }

    // -------------------------------------------------------------------------------------------
    // Input

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        let text_focus = ctx.memory(|m| m.focused()).is_some() && ctx.egui_wants_keyboard_input();
        let cmd_focused = ctx.memory(|m| m.has_focus(egui::Id::new("cmd_input")));
        let sc = |m: Modifiers, k: Key| KeyboardShortcut::new(m, k);
        let consume = |s: KeyboardShortcut| ctx.input_mut(|i| i.consume_shortcut(&s));
        if consume(sc(Modifiers::NONE, Key::Escape)) {
            self.ed.escape();
            self.cmd_input.clear();
        }
        if consume(sc(Modifiers::COMMAND, Key::S)) {
            self.ed.requests.push(AppRequest::Save);
        }
        if consume(sc(Modifiers::COMMAND | Modifiers::SHIFT, Key::S)) {
            self.ed.requests.push(AppRequest::SaveAs);
        }
        if consume(sc(Modifiers::COMMAND, Key::O)) {
            self.ed.requests.push(AppRequest::Open);
        }
        for (k, f) in [
            (Key::F3, 3u8),
            (Key::F7, 7),
            (Key::F8, 8),
            (Key::F9, 9),
            (Key::F10, 10),
        ] {
            if consume(sc(Modifiers::NONE, k)) {
                let d = &mut self.ed.draft;
                match f {
                    3 => d.osnap_on = !d.osnap_on,
                    7 => d.grid_on = !d.grid_on,
                    8 => {
                        d.ortho = !d.ortho;
                        if d.ortho {
                            d.polar = false;
                        }
                    }
                    9 => d.snap_on = !d.snap_on,
                    _ => {
                        d.polar = !d.polar;
                        if d.polar {
                            d.ortho = false;
                        }
                    }
                }
            }
        }
        if text_focus && !cmd_focused {
            return;
        }
        // Keys that must not reach the command-line text field.
        if self.cmd_input.is_empty() {
            if consume(sc(Modifiers::COMMAND, Key::Z)) {
                self.ed.undo();
            }
            if consume(sc(Modifiers::COMMAND, Key::Y))
                || consume(sc(Modifiers::COMMAND | Modifiers::SHIFT, Key::Z))
            {
                self.ed.redo();
            }
            if consume(sc(Modifiers::COMMAND, Key::A)) {
                self.ed.select_all();
            }
            if consume(sc(Modifiers::NONE, Key::Delete))
                && !self.ed.selection.is_empty()
                && !self.ed.has_tool()
            {
                self.ed.run_command("ERASE");
            }
        }
        let text_mode = self.ed.tool_accepts().text;
        // Enter / Space submit the command line (Space inserts a blank only in text prompts).
        let submit = ctx.input_mut(|i| {
            let mut hit = i.consume_key(Modifiers::NONE, Key::Enter);
            if !text_mode && i.consume_key(Modifiers::NONE, Key::Space) {
                i.events
                    .retain(|e| !matches!(e, egui::Event::Text(t) if t == " "));
                hit = true;
            }
            hit
        });
        if submit {
            let line = std::mem::take(&mut self.cmd_input);
            self.ed.submit(&line);
        }
        if !cmd_focused {
            // Typing anywhere goes to the command line (AutoCAD).
            let typed: String = ctx.input(|i| {
                i.events
                    .iter()
                    .filter_map(|e| {
                        if let egui::Event::Text(t) = e {
                            Some(t.as_str())
                        } else {
                            None
                        }
                    })
                    .collect()
            });
            if !typed.is_empty() && !ctx.input(|i| i.modifiers.command) {
                self.cmd_input.push_str(&typed);
                self.focus_cmd = true;
            }
        }
    }

    fn handle_dropped_files(&mut self, ctx: &egui::Context) {
        let dropped = ctx.input(|i| i.raw.dropped_files.clone());
        for f in dropped {
            let name = f
                .path()
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "dropped".into());
            #[cfg(target_arch = "wasm32")]
            {
                let inbox = self.inbox.clone();
                let ctx = ctx.clone();
                platform::spawn(async move {
                    match f.bytes_async().await {
                        Ok(bytes) => inbox.push(PlatformEvent::FileOpened {
                            name,
                            bytes,
                            purpose: OpenPurpose::Open,
                            path: None,
                        }),
                        Err(e) => inbox.push(PlatformEvent::Error(e)),
                    }
                    ctx.request_repaint();
                });
            }
            #[cfg(not(target_arch = "wasm32"))]
            {
                let path = f.path().to_path_buf();
                match f.bytes() {
                    Ok(bytes) => self.inbox.push(PlatformEvent::FileOpened {
                        name,
                        bytes,
                        purpose: OpenPurpose::Open,
                        path: Some(path),
                    }),
                    Err(e) => self.inbox.push(PlatformEvent::Error(e)),
                }
            }
        }
    }

    fn handle_platform_events(&mut self) {
        for ev in self.inbox.drain() {
            match ev {
                PlatformEvent::FileOpened {
                    name,
                    bytes,
                    purpose: _,
                    path,
                } => {
                    if self.ed.doc.is_dirty() {
                        self.pending = Some(Pending::Bytes { name, bytes, path });
                    } else {
                        self.open_bytes(&name, &bytes, path);
                    }
                }
                PlatformEvent::FileSaved { name, path } => {
                    let s = self.s();
                    self.ed.info(fmt(s.file_saved, &[&name]));
                    if matches!(
                        files::kind_of(&name),
                        files::FileKind::Wcad | files::FileKind::Wcadz
                    ) {
                        self.doc_name = Some(name);
                        if path.is_some() {
                            self.doc_path = path;
                        }
                        self.ed.doc.mark_saved();
                        self.store.remove(files::AUTOSAVE_KEY);
                    }
                }
                PlatformEvent::Error(e) => {
                    let s = self.s();
                    self.ed.error(fmt(s.save_failed, &[&e]));
                }
            }
        }
    }

    /// Open a file from bytes (native document, DXF or DWG).
    pub fn open_bytes(&mut self, name: &str, bytes: &[u8], path: Option<std::path::PathBuf>) {
        let s = self.s();
        match files::load_bytes(name, bytes) {
            Ok(l) => {
                let doc_name = if l.native {
                    name.to_owned()
                } else {
                    format!("{}.wcad", files::stem(name))
                };
                let native = l.native;
                let n = l.warnings.len();
                self.load_document(l.doc, Some(doc_name), if native { path } else { None });
                self.ed.info(fmt(s.file_opened, &[&name]));
                if n > 0 {
                    let title = fmt(s.import_warnings, &[&name, &n]);
                    self.ed.info(title.clone());
                    self.warnings = Some((title, l.warnings));
                }
                self.store.remove(files::AUTOSAVE_KEY);
            }
            Err(e) => self.ed.error(fmt(s.open_failed, &[&name, &e])),
        }
    }

    fn handle_requests(&mut self, ctx: &egui::Context) {
        let reqs = std::mem::take(&mut self.ed.requests);
        for r in reqs {
            match r {
                AppRequest::ZoomExtents => match self.workspace {
                    Workspace::Drafting => self.view2d.zoom_extents(&self.ed),
                    Workspace::Modeling => self.view3d.fit(16.0 / 9.0),
                },
                AppRequest::ZoomWindow(b) => self.view2d.zoom_window(&b),
                AppRequest::New => self.guard(Pending::New, ctx),
                AppRequest::Open => self.guard(Pending::Open, ctx),
                AppRequest::LoadDemo => self.guard(Pending::Demo, ctx),
                AppRequest::Save => self.save(ctx, false),
                AppRequest::SaveAs => self.save(ctx, true),
                AppRequest::Import => platform::open_file(
                    self.inbox.clone(),
                    ctx.clone(),
                    OpenPurpose::Import,
                    &[("DXF / DWG", &["dxf", "dwg"])],
                ),
                AppRequest::Export(f) => self.export(ctx, f),
                AppRequest::SetWorkspace(w) => self.workspace = w,
                AppRequest::SaveBytes { name, bytes } => {
                    platform::save_file(self.inbox.clone(), ctx.clone(), name, bytes, None)
                }
                AppRequest::ShowPanel(id) => {
                    let dock = self
                        .registry
                        .panels()
                        .iter()
                        .find(|p| p.id == id)
                        .map(|p| p.dock);
                    match dock {
                        Some(Dock::Right) => self.settings.right_panel = true,
                        _ => {
                            self.settings.left_panel = true;
                            self.left_tab = id;
                        }
                    }
                }
            }
        }
    }

    /// Run `p` now, or ask first when the drawing has unsaved changes.
    fn guard(&mut self, p: Pending, ctx: &egui::Context) {
        if self.ed.doc.is_dirty() {
            self.pending = Some(p);
        } else {
            self.run_pending(p, ctx);
        }
    }

    fn run_pending(&mut self, p: Pending, ctx: &egui::Context) {
        match p {
            Pending::New => {
                self.load_document(wcad_doc::Document::new(), None, None);
                self.store.remove(files::AUTOSAVE_KEY);
            }
            Pending::Open => {
                let filters = files::open_filters();
                platform::open_file(self.inbox.clone(), ctx.clone(), OpenPurpose::Open, &filters);
            }
            Pending::Demo => {
                self.load_document(crate::demo::demo_document(), Some("demo.wcad".into()), None)
            }
            Pending::Bytes { name, bytes, path } => self.open_bytes(&name, &bytes, path),
        }
    }

    fn save(&mut self, ctx: &egui::Context, save_as: bool) {
        let name = match &self.doc_name {
            Some(n)
                if matches!(
                    files::kind_of(n),
                    files::FileKind::Wcad | files::FileKind::Wcadz
                ) =>
            {
                n.clone()
            }
            _ => format!(
                "{}.wcad",
                self.doc_name
                    .as_deref()
                    .map(files::stem)
                    .unwrap_or_else(|| "drawing".into())
            ),
        };
        let compressed = files::kind_of(&name) == files::FileKind::Wcadz;
        match files::save_native(&self.ed.doc, compressed) {
            Ok(bytes) => {
                let path = if save_as { None } else { self.doc_path.clone() };
                platform::save_file(self.inbox.clone(), ctx.clone(), name, bytes, path);
            }
            Err(e) => {
                let s = self.s();
                self.ed.error(fmt(s.save_failed, &[&e]));
            }
        }
    }

    fn export(&mut self, ctx: &egui::Context, f: ExportFormat) {
        let dark = self.settings.theme == Theme::Dark && f == ExportFormat::Svg;
        let s = self.s();
        match files::export_bytes(&self.ed.doc, f, dark) {
            Ok((ext, bytes)) => {
                let stem = self
                    .doc_name
                    .as_deref()
                    .map(files::stem)
                    .unwrap_or_else(|| "drawing".into());
                let name = format!("{stem}.{ext}");
                self.ed.info(fmt(s.file_exported, &[&name]));
                platform::save_file(self.inbox.clone(), ctx.clone(), name, bytes, None);
            }
            Err(e) => self.ed.error(fmt(s.save_failed, &[&e])),
        }
    }

    fn autosave(&mut self, ctx: &egui::Context) {
        if !self.settings.autosave || self.recovery.is_some() {
            return;
        }
        let now = ctx.input(|i| i.time);
        let rev = self.ed.doc.revision();
        if rev == self.autosave_rev || !self.ed.doc.is_dirty() {
            return;
        }
        // The web build cannot save when the tab is closed, so it saves more often.
        let mut interval = self.settings.autosave_seconds.max(5.0);
        if cfg!(target_arch = "wasm32") {
            interval = interval.min(15.0);
        }
        if now - self.last_autosave < interval as f64 {
            ctx.request_repaint_after(std::time::Duration::from_secs_f32(interval));
            return;
        }
        self.last_autosave = now;
        self.autosave_rev = rev;
        if let Some(payload) = files::autosave_payload(&self.ed.doc) {
            let meta = format!(
                "{} · {}",
                self.title_name(),
                self.ed.doc.drawing.entities.len()
            );
            if self.store.set(files::AUTOSAVE_KEY, &payload) {
                self.store.set(files::AUTOSAVE_META_KEY, &meta);
            }
        }
    }

    fn persist_settings(&mut self) {
        self.settings.draft = self.ed.draft.clone();
        if self.settings != self.saved_settings {
            self.store.set(SETTINGS_KEY, &self.settings.to_json());
            self.saved_settings = self.settings.clone();
        }
    }

    /// Called on native exit: keep the autosave only when there are unsaved changes.
    pub fn on_exit_save(&mut self) {
        if self.ed.doc.is_dirty() {
            if let Some(p) = files::autosave_payload(&self.ed.doc) {
                self.store.set(files::AUTOSAVE_KEY, &p);
                let meta = format!(
                    "{} · {}",
                    self.title_name(),
                    self.ed.doc.drawing.entities.len()
                );
                self.store.set(files::AUTOSAVE_META_KEY, &meta);
            }
        } else {
            self.store.remove(files::AUTOSAVE_KEY);
        }
        self.persist_settings();
    }

    // -------------------------------------------------------------------------------------------
    // Menu bar

    fn command_menu(&mut self, ui: &mut egui::Ui, tabs: &[RibbonTab]) {
        let lang = self.settings.lang;
        let mut any = false;
        for tab in tabs {
            let cmds: Vec<_> = self.registry.on_tab(*tab).copied().collect();
            for c in cmds {
                any = true;
                if ui
                    .button(format!("{}  {}", c.icon, (c.label)(lang)))
                    .on_hover_text(c.name)
                    .clicked()
                {
                    self.ed.run_command(c.name);
                    ui.close();
                }
            }
        }
        if !any {
            ui.weak(self.s().ribbon_empty);
        }
    }

    fn menu_bar(&mut self, ui: &mut egui::Ui, narrow: bool) {
        let s = self.s();
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button(s.menu_file, |ui| {
                if ui.button(s.new).clicked() {
                    self.ed.requests.push(AppRequest::New);
                    ui.close();
                }
                if ui.button(s.open).clicked() {
                    self.ed.requests.push(AppRequest::Open);
                    ui.close();
                }
                if ui.button(s.save).clicked() {
                    self.ed.requests.push(AppRequest::Save);
                    ui.close();
                }
                if ui.button(s.save_as).clicked() {
                    self.ed.requests.push(AppRequest::SaveAs);
                    ui.close();
                }
                ui.separator();
                if ui.button(s.import).clicked() {
                    self.ed.requests.push(AppRequest::Import);
                    ui.close();
                }
                ui.menu_button(s.export, |ui| {
                    for (label, f) in [
                        (s.export_dxf, ExportFormat::Dxf),
                        (s.export_dwg, ExportFormat::Dwg),
                        (s.export_svg, ExportFormat::Svg),
                        (s.export_pdf, ExportFormat::Pdf),
                    ] {
                        if ui.button(label).clicked() {
                            self.ed.requests.push(AppRequest::Export(f));
                            ui.close();
                        }
                    }
                });
                ui.separator();
                if ui.button(s.load_demo).clicked() {
                    self.ed.requests.push(AppRequest::LoadDemo);
                    ui.close();
                }
                #[cfg(not(target_arch = "wasm32"))]
                {
                    ui.separator();
                    if ui.button(s.quit).clicked() {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                }
            });
            ui.menu_button(s.menu_edit, |ui| {
                if ui
                    .add_enabled(
                        self.ed.doc.can_undo(),
                        egui::Button::new(s.undo).shortcut_text("Ctrl+Z"),
                    )
                    .clicked()
                {
                    self.ed.undo();
                    ui.close();
                }
                if ui
                    .add_enabled(
                        self.ed.doc.can_redo(),
                        egui::Button::new(s.redo).shortcut_text("Ctrl+Y"),
                    )
                    .clicked()
                {
                    self.ed.redo();
                    ui.close();
                }
                ui.separator();
                if ui
                    .add_enabled(
                        !self.ed.selection.is_empty(),
                        egui::Button::new(s.erase).shortcut_text("Del"),
                    )
                    .clicked()
                {
                    self.ed.run_command("ERASE");
                    ui.close();
                }
                if ui
                    .add(egui::Button::new(s.select_all).shortcut_text("Ctrl+A"))
                    .clicked()
                {
                    self.ed.select_all();
                    ui.close();
                }
                if ui
                    .add(egui::Button::new(s.deselect).shortcut_text("Esc"))
                    .clicked()
                {
                    self.ed.selection.clear();
                    ui.close();
                }
            });
            ui.menu_button(s.menu_view, |ui| {
                if ui.button(s.zoom_extents).clicked() {
                    self.ed.requests.push(AppRequest::ZoomExtents);
                    ui.close();
                }
                if ui.button(s.zoom_window).clicked() {
                    self.ed.run_command("ZOOM");
                    ui.close();
                }
                ui.separator();
                ui.label(s.workspace);
                ui.radio_value(
                    &mut self.workspace,
                    Workspace::Drafting,
                    s.workspace_drafting,
                );
                ui.radio_value(
                    &mut self.workspace,
                    Workspace::Modeling,
                    s.workspace_modeling,
                );
                ui.separator();
                ui.checkbox(&mut self.settings.left_panel, s.show_left);
                ui.checkbox(&mut self.settings.right_panel, s.show_right);
                ui.checkbox(&mut self.settings.ribbon_labels, s.show_ribbon_labels);
                if ui
                    .checkbox(&mut self.ed.draft.show_lineweights, s.lineweight_display)
                    .changed()
                {
                    self.ed.invalidate_display();
                }
                ui.separator();
                ui.label(s.theme);
                ui.radio_value(&mut self.settings.theme, Theme::Dark, s.theme_dark);
                ui.radio_value(&mut self.settings.theme, Theme::Light, s.theme_light);
                ui.separator();
                ui.label(s.language);
                for l in Lang::ALL {
                    ui.radio_value(&mut self.settings.lang, l, l.native_name());
                }
            });
            ui.menu_button(s.menu_draw, |ui| self.command_menu(ui, &[RibbonTab::Draw]));
            ui.menu_button(s.menu_modify, |ui| {
                self.command_menu(ui, &[RibbonTab::Modify])
            });
            ui.menu_button(s.menu_annotate, |ui| {
                self.command_menu(ui, &[RibbonTab::Annotate])
            });
            ui.menu_button(s.menu_3d, |ui| {
                self.command_menu(ui, &[RibbonTab::Model, RibbonTab::Sketch])
            });
            ui.menu_button(s.menu_help, |ui| {
                if ui.button(s.shortcuts).clicked() {
                    self.show_shortcuts = true;
                    ui.close();
                }
                if ui.button(s.about).clicked() {
                    self.show_about = true;
                    ui.close();
                }
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.selectable_value(
                    &mut self.workspace,
                    Workspace::Modeling,
                    if narrow { "3D" } else { s.workspace_modeling },
                );
                ui.selectable_value(
                    &mut self.workspace,
                    Workspace::Drafting,
                    if narrow { "2D" } else { s.workspace_drafting },
                );
                ui.separator();
                if ui
                    .selectable_label(self.settings.right_panel, "▣")
                    .on_hover_text(s.properties)
                    .clicked()
                {
                    self.settings.right_panel = !self.settings.right_panel;
                }
                if ui
                    .selectable_label(self.settings.left_panel, "☰")
                    .on_hover_text(s.layers)
                    .clicked()
                {
                    self.settings.left_panel = !self.settings.left_panel;
                }
                ui.separator();
                if ui
                    .add_enabled(self.ed.doc.can_redo(), egui::Button::new("⟳").frame(false))
                    .on_hover_text(s.redo)
                    .clicked()
                {
                    self.ed.redo();
                }
                if ui
                    .add_enabled(self.ed.doc.can_undo(), egui::Button::new("⟲").frame(false))
                    .on_hover_text(s.undo)
                    .clicked()
                {
                    self.ed.undo();
                }
                if !narrow {
                    let dirty = if self.ed.doc.is_dirty() { " *" } else { "" };
                    ui.weak(format!("{}{dirty}", self.title_name()));
                }
            });
        });
    }

    // -------------------------------------------------------------------------------------------
    // Ribbon

    fn ribbon(&mut self, ui: &mut egui::Ui, narrow: bool) {
        let lang = self.settings.lang;
        ui.horizontal(|ui| {
            for tab in RibbonTab::ALL {
                ui.selectable_value(&mut self.ribbon_tab, tab, tab.label(lang));
            }
        });
        let cmds: Vec<_> = self.registry.on_tab(self.ribbon_tab).copied().collect();
        let labels = self.settings.ribbon_labels && !narrow;
        egui::ScrollArea::horizontal()
            .id_salt("ribbon_scroll")
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.set_min_height(if labels { 52.0 } else { 36.0 });
                    if cmds.is_empty() {
                        ui.weak(self.s().ribbon_empty);
                    }
                    let mut group = None;
                    for c in cmds {
                        if group.is_some_and(|g| g != c.group) {
                            ui.separator();
                        }
                        group = Some(c.group);
                        let active = self.ed.active_tool_name() == Some(c.name);
                        let text = if labels {
                            format!("{}\n{}", c.icon, (c.label)(lang))
                        } else {
                            c.icon.to_owned()
                        };
                        let btn = egui::Button::new(RichText::new(text).size(if labels {
                            15.0
                        } else {
                            20.0
                        }))
                        .min_size(egui::vec2(
                            if labels { 52.0 } else { 36.0 },
                            if labels { 48.0 } else { 34.0 },
                        ))
                        .selected(active);
                        let aliases = if c.aliases.is_empty() {
                            String::new()
                        } else {
                            format!(" ({})", c.aliases.join(", "))
                        };
                        if ui
                            .add(btn)
                            .on_hover_text(format!("{} — {}{}", (c.label)(lang), c.name, aliases))
                            .clicked()
                        {
                            self.ed.run_command(c.name);
                        }
                    }
                });
            });
    }

    // -------------------------------------------------------------------------------------------
    // Docks

    fn dock(&mut self, ui: &mut egui::Ui, dock: Dock) {
        let lang = self.settings.lang;
        let panels: Vec<_> = self
            .registry
            .panels()
            .iter()
            .filter(|p| p.dock == dock)
            .copied()
            .collect();
        if panels.is_empty() {
            return;
        }
        let current = if dock == Dock::Left {
            if !panels.iter().any(|p| p.id == self.left_tab) {
                self.left_tab = panels[0].id;
            }
            ui.horizontal(|ui| {
                for p in &panels {
                    if ui
                        .selectable_label(self.left_tab == p.id, (p.title)(lang))
                        .clicked()
                    {
                        self.left_tab = p.id;
                    }
                }
            });
            ui.separator();
            panels.iter().find(|p| p.id == self.left_tab).copied()
        } else {
            let p = panels[0];
            ui.strong((p.title)(lang));
            ui.separator();
            Some(p)
        };
        if let Some(p) = current {
            self.view3d.regenerate_if_needed(&self.ed);
            let regen = std::mem::take(&mut self.view3d.regen);
            let mut cx = PanelCx {
                editor: &mut self.ed,
                regen: &regen,
                workspace: self.workspace,
            };
            (p.ui)(ui, &mut cx);
            self.view3d.regen = regen;
        }
        // Other right-dock panels below the first one.
        if dock == Dock::Right {
            for p in panels.iter().skip(1) {
                ui.separator();
                egui::CollapsingHeader::new((p.title)(lang))
                    .default_open(true)
                    .show(ui, |ui| {
                        let regen = std::mem::take(&mut self.view3d.regen);
                        let mut cx = PanelCx {
                            editor: &mut self.ed,
                            regen: &regen,
                            workspace: self.workspace,
                        };
                        (p.ui)(ui, &mut cx);
                        self.view3d.regen = regen;
                    });
            }
        }
    }

    // -------------------------------------------------------------------------------------------
    // Command line and status bar

    fn command_line(&mut self, ui: &mut egui::Ui) {
        let s = self.s();
        let hist_h = (ui.available_height() - 30.0).max(18.0);
        egui::ScrollArea::vertical()
            .id_salt("cmd_history")
            .max_height(hist_h)
            .min_scrolled_height(hist_h)
            .stick_to_bottom(true)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let start = self.ed.log.len().saturating_sub(200);
                for l in &self.ed.log[start..] {
                    let color = match l.kind {
                        LogKind::Echo => ui.visuals().weak_text_color(),
                        LogKind::Info => ui.visuals().text_color(),
                        LogKind::Error => Color32::from_rgb(235, 100, 90),
                    };
                    ui.label(RichText::new(&l.text).monospace().size(12.0).color(color));
                }
            });
        ui.horizontal(|ui| {
            let (prompt, keywords) = self.ed.prompt();
            ui.label(RichText::new(format!("{prompt}:")).strong());
            for k in keywords {
                if ui.small_button(format!("{}({})", k.label, k.key)).clicked() {
                    self.ed.echo_keyword(k);
                }
            }
            let hint = s.command_line_hint;
            let te = egui::TextEdit::singleline(&mut self.cmd_input)
                .id(egui::Id::new("cmd_input"))
                .hint_text(hint)
                .desired_width(ui.available_width())
                .font(egui::TextStyle::Monospace);
            let resp = ui.add(te);
            if self.focus_cmd {
                resp.request_focus();
                if let Some(mut state) = egui::TextEdit::load_state(ui.ctx(), resp.id) {
                    let end = egui::text::CCursor::new(self.cmd_input.chars().count());
                    state
                        .cursor
                        .set_char_range(Some(egui::text::CCursorRange::one(end)));
                    state.store(ui.ctx(), resp.id);
                }
                self.focus_cmd = false;
            }
        });
    }

    fn status_bar(&mut self, ui: &mut egui::Ui, narrow: bool) {
        let s = self.s();
        ui.horizontal(|ui| {
            let dec = self.ed.doc.drawing.tables.settings.display_decimals.min(8) as usize;
            let coords = match (self.workspace, self.ed.cursor) {
                (Workspace::Drafting, Some(c)) => {
                    format!("{:.*}, {:.*}", dec, c.point.x, dec, c.point.y)
                }
                _ => "—".into(),
            };
            ui.add_sized(
                [if narrow { 120.0 } else { 190.0 }, 18.0],
                egui::Label::new(RichText::new(coords).monospace()),
            );
            ui.separator();
            let d = &mut self.ed.draft;
            let toggle =
                |ui: &mut egui::Ui, on: &mut bool, label: &str, tip: &str| -> egui::Response {
                    let r = ui.selectable_label(*on, label).on_hover_text(tip);
                    if r.clicked() {
                        *on = !*on;
                    }
                    r
                };
            toggle(ui, &mut d.snap_on, s.st_snap, "F9");
            toggle(ui, &mut d.grid_on, s.st_grid, "F7");
            if toggle(ui, &mut d.ortho, s.st_ortho, "F8").clicked() && d.ortho {
                d.polar = false;
            }
            let polar = toggle(ui, &mut d.polar, s.st_polar, "F10");
            if polar.clicked() && d.polar {
                d.ortho = false;
            }
            polar.context_menu(|ui| {
                ui.label(s.st_polar_increment);
                for inc in [15.0, 22.5, 30.0, 45.0, 90.0] {
                    ui.radio_value(&mut d.polar_increment_deg, inc, format!("{inc}°"));
                }
            });
            let os = toggle(ui, &mut d.osnap_on, s.st_osnap, "F3");
            os.context_menu(|ui| {
                ui.label(s.st_osnap_settings);
                let m = &mut d.osnap;
                ui.checkbox(&mut m.endpoint, s.snap_endpoint);
                ui.checkbox(&mut m.midpoint, s.snap_midpoint);
                ui.checkbox(&mut m.center, s.snap_center);
                ui.checkbox(&mut m.quadrant, s.snap_quadrant);
                ui.checkbox(&mut m.intersection, s.snap_intersection);
                ui.checkbox(&mut m.perpendicular, s.snap_perpendicular);
                ui.checkbox(&mut m.tangent, s.snap_tangent);
                ui.checkbox(&mut m.nearest, s.snap_nearest);
                ui.checkbox(&mut m.node, s.snap_node);
            });
            let lw = d.show_lineweights;
            toggle(ui, &mut d.show_lineweights, s.st_lwt, s.lineweight_display);
            if lw != self.ed.draft.show_lineweights {
                self.ed.invalidate_display();
            }
            if !narrow {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.weak(self.ed.doc.meta.units.suffix());
                    if let Some(g) = &self.gpu {
                        ui.weak(g.backend_info());
                    }
                });
            }
        });
    }

    // -------------------------------------------------------------------------------------------
    // Dialogs

    fn dialogs(&mut self, ctx: &egui::Context) {
        let s = self.s();
        if let Some((meta, payload)) = self.recovery.clone() {
            let mut close = false;
            egui::Window::new(s.recover_title)
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label(fmt(s.recover_text, &[&meta]));
                    ui.horizontal(|ui| {
                        if ui.button(s.recover).clicked() {
                            if let Some(doc) = files::recover(&payload) {
                                self.load_document(doc, None, None);
                            }
                            close = true;
                        }
                        if ui.button(s.discard).clicked() {
                            self.store.remove(files::AUTOSAVE_KEY);
                            close = true;
                        }
                    });
                });
            if close {
                self.recovery = None;
            }
        }
        if self.pending.is_some() {
            let mut choice = None;
            egui::Window::new(s.warnings)
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label(s.unsaved_changes);
                    ui.horizontal(|ui| {
                        if ui.button(s.discard_changes).clicked() {
                            choice = Some(true);
                        }
                        if ui.button(s.cancel).clicked() {
                            choice = Some(false);
                        }
                    });
                });
            match choice {
                Some(true) => {
                    if let Some(p) = self.pending.take() {
                        self.run_pending(p, ctx);
                    }
                }
                Some(false) => self.pending = None,
                None => {}
            }
        }
        if let Some((title, list)) = &self.warnings {
            let mut open = true;
            egui::Window::new(title.as_str())
                .open(&mut open)
                .default_width(420.0)
                .show(ctx, |ui| {
                    egui::ScrollArea::vertical()
                        .max_height(300.0)
                        .show(ui, |ui| {
                            for w in list {
                                ui.label(format!("• {w}"));
                            }
                        });
                });
            if !open {
                self.warnings = None;
            }
        }
        if self.show_about {
            let info = self
                .gpu
                .as_ref()
                .map(|g| g.backend_info())
                .unwrap_or_default();
            egui::Window::new(s.about)
                .open(&mut self.show_about)
                .collapsible(false)
                .show(ctx, |ui| {
                    ui.heading(format!("webcad v{}", env!("CARGO_PKG_VERSION")));
                    ui.label(s.about_text);
                    ui.weak(info);
                });
        }
        if self.show_shortcuts {
            egui::Window::new(s.shortcuts)
                .open(&mut self.show_shortcuts)
                .collapsible(false)
                .show(ctx, |ui| {
                    ui.label(s.shortcuts_text);
                });
        }
    }

    fn hovered_files_overlay(&self, ctx: &egui::Context) {
        if ctx.input(|i| i.raw.hovered_files.is_empty()) {
            return;
        }
        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::new("drop_overlay"),
        ));
        let rect = ctx.content_rect();
        painter.rect_filled(rect, 0.0, Color32::from_black_alpha(160));
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            self.s().drop_hint,
            egui::FontId::proportional(24.0),
            Color32::WHITE,
        );
    }
}

/// Decode `%XX` escapes and `+` (URL query values).
pub fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => {
                let hex = std::str::from_utf8(&b[i + 1..i + 3])
                    .ok()
                    .and_then(|h| u8::from_str_radix(h, 16).ok());
                match hex {
                    Some(v) => {
                        out.push(v);
                        i += 2;
                    }
                    None => out.push(b'%'),
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

impl eframe::App for WebCadApp {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        if self.gpu.is_none()
            && !self.gpu_tried
            && let Some(rs) = frame.wgpu_render_state()
        {
            self.gpu = Some(Gpu::new(rs));
        }
        self.gpu_tried = true;
        self.frame_ui(ui);
    }

    fn on_exit(&mut self) {
        self.on_exit_save();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::MemStore;
    use crate::testing::{run_app_frame_with, run_app_frames};

    fn app() -> (WebCadApp, egui::Context) {
        let ctx = egui::Context::default();
        crate::fonts::install(&ctx);
        (WebCadApp::with_store(Box::new(MemStore::default())), ctx)
    }

    #[test]
    fn ui_runs_headless_in_both_workspaces_and_sizes() {
        let (mut app, ctx) = app();
        app.load_document(crate::demo::demo_document(), Some("demo.wcad".into()), None);
        for size in [egui::vec2(1280.0, 800.0), egui::vec2(390.0, 800.0)] {
            run_app_frames(&mut app, &ctx, 3, size);
            app.workspace = Workspace::Modeling;
            run_app_frames(&mut app, &ctx, 2, size);
            app.workspace = Workspace::Drafting;
            app.settings.lang = Lang::En;
            app.settings.theme = Theme::Light;
            run_app_frames(&mut app, &ctx, 2, size);
        }
        assert_eq!(app.view3d.regen.bodies.len(), 1, "demo box regenerated");
    }

    #[test]
    fn typing_goes_to_command_line() {
        let (mut app, ctx) = app();
        let size = egui::vec2(1000.0, 700.0);
        run_app_frames(&mut app, &ctx, 2, size);
        let ev = |t: &str| egui::Event::Text(t.into());
        let key = |k: Key| egui::Event::Key {
            key: k,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        };
        run_app_frame_with(&mut app, &ctx, size, vec![ev("L")]);
        run_app_frame_with(&mut app, &ctx, size, vec![key(Key::Enter)]);
        assert_eq!(app.ed.active_tool_name(), Some("LINE"));
        for p in ["0,0", "10,0", "10,10"] {
            run_app_frame_with(&mut app, &ctx, size, vec![ev(p)]);
            run_app_frame_with(&mut app, &ctx, size, vec![key(Key::Space)]);
        }
        run_app_frame_with(&mut app, &ctx, size, vec![key(Key::Escape)]);
        assert!(!app.ed.has_tool());
        assert_eq!(app.ed.doc.drawing.entities.len(), 2);
        // Ctrl+Z undoes a segment.
        let undo = egui::Event::Key {
            key: Key::Z,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::COMMAND,
        };
        run_app_frame_with(&mut app, &ctx, size, vec![undo]);
        assert_eq!(app.ed.doc.drawing.entities.len(), 1);
    }

    #[test]
    fn settings_persist_and_autosave_recovery() {
        let (mut app, ctx) = app();
        app.settings.lang = Lang::En;
        app.settings.autosave_seconds = 0.0;
        app.last_autosave = -100.0;
        app.ed.submit("LINE");
        app.ed.submit("0,0");
        app.ed.submit("5,5");
        app.ed.submit("");
        run_app_frames(&mut app, &ctx, 1, egui::vec2(800.0, 600.0));
        // Simulate a crash: reopen from the same store.
        let store = std::mem::replace(&mut app.store, Box::new(MemStore::default()));
        let mut app2 = WebCadApp::with_store(store);
        assert_eq!(app2.settings.lang, Lang::En);
        assert!(app2.recovery.is_some(), "autosave offered for recovery");
        let (_, payload) = app2.recovery.take().unwrap();
        let doc = files::recover(&payload).unwrap();
        assert_eq!(doc.drawing.entities.len(), 1);
    }

    #[test]
    fn startup_query() {
        assert_eq!(percent_decode("a%2Cb+c%3B%zz%"), "a,b c;%zz%");
        let (mut app, _ctx) = app();
        app.apply_startup_query("?demo&lang=en&ws=3d&cmd=SELECTALL");
        assert_eq!(app.settings.lang, Lang::En);
        assert_eq!(app.workspace, Workspace::Modeling);
        assert!(app.ed.selection.len() > 10);
        let lines = |a: &WebCadApp| {
            a.ed.doc
                .drawing
                .entities
                .values()
                .filter(|e| e.kind.type_name() == "LINE")
                .count()
        };
        let before = lines(&app);
        app.ed.selection.clear();
        app.apply_startup_query("cmd=L%3B0%2C0%3B10%2C0%3B");
        assert_eq!(lines(&app), before + 1);
    }

    #[test]
    fn open_bytes_reports_errors_and_loads_dxf() {
        let (mut app, _ctx) = app();
        app.open_bytes("bad.dxf", b"nonsense", None);
        assert_eq!(app.ed.log.last().map(|l| l.kind), Some(LogKind::Error));
        let (_, dxf) =
            files::export_bytes(&crate::demo::demo_document(), ExportFormat::Dxf, true).unwrap();
        app.open_bytes("plate.dxf", &dxf, None);
        assert!(app.ed.doc.drawing.entities.len() > 10);
        assert_eq!(app.doc_name.as_deref(), Some("plate.wcad"));
    }
}
