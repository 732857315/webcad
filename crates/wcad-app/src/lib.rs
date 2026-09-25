//! The webcad application, shared by the native binary and the web entry point.
//!
//! Structure (see `docs/ARCHITECTURE.md` §5.8):
//! - [`editor`]: the GPU-free editing core ([`editor::Editor`]): document, active tool, selection,
//!   snapping, command line. Everything a tool touches lives here, so tools are unit-testable
//!   ([`testing`]).
//! - [`tools`]: the [`tools::Tool`] trait and the built-in proof tools; feature modules
//!   (`tools::draw`, `tools::modify`, `tools::annotate`, [`modeling`], [`panels::extra`]) register
//!   their commands into the [`commands::CommandRegistry`].
//! - [`display`]: document → `wcad_render::Batch2D` display lists (incremental).
//! - [`view2d`] / [`view3d`]: the viewports (egui widgets over `wcad_render` offscreen textures).
//! - [`app`]: [`WebCadApp`], the eframe application (menus, ribbon, docks, command line, status bar).

// `!(x > 0.0)` style comparisons are deliberate: they also reject NaN.
#![allow(clippy::neg_cmp_op_on_partial_ord)]

pub mod app;
pub mod cmdline;
pub mod commands;
pub mod demo;
pub mod dimgen;
pub mod display;
pub mod editor;
pub mod files;
pub mod gpu;
pub mod i18n;
pub mod modeling;
pub mod panels;
pub mod platform;
pub mod select;
pub mod settings;
pub mod snap;
pub mod testing;
pub mod tools;
pub mod view2d;
pub mod view3d;
pub mod xform;

pub use app::WebCadApp;
pub use eframe::egui;

/// Application name, used for the native window title and storage keys.
pub const APP_NAME: &str = "webcad";

/// Embedded fonts.
pub mod fonts {
    use eframe::egui;
    use std::sync::{Arc, OnceLock};

    /// Noto Sans SC subset (4515 characters: ASCII, Latin-1, CJK punctuation, fullwidth forms,
    /// CAD symbols, 通用规范汉字表 level 1 ∪ GB2312 level 1), SIL OFL 1.1. Regenerate with
    /// `assets/fonts/make_subsets.sh`. The same bytes feed `wcad-geom2d::text` for CAD text.
    pub static CJK_UI_TTF: &[u8] =
        include_bytes!("../../../assets/fonts/NotoSansSC-Regular-ui.ttf");

    /// Font name used in [`egui::FontDefinitions`].
    pub const CJK_FONT_NAME: &str = "NotoSansSC-ui";

    /// egui's default fonts plus the CJK font: primary proportional face (so Chinese and Latin text
    /// share one design and baseline) and fallback for monospace.
    pub fn definitions() -> egui::FontDefinitions {
        let mut defs = egui::FontDefinitions::default();
        defs.font_data.insert(
            CJK_FONT_NAME.to_owned(),
            Arc::new(egui::FontData::from_static(CJK_UI_TTF)),
        );
        defs.families
            .entry(egui::FontFamily::Proportional)
            .or_default()
            .insert(0, CJK_FONT_NAME.to_owned());
        defs.families
            .entry(egui::FontFamily::Monospace)
            .or_default()
            .push(CJK_FONT_NAME.to_owned());
        defs
    }

    /// Install [`definitions`] into the context (takes effect from the next frame's layout).
    pub fn install(ctx: &egui::Context) {
        ctx.set_fonts(definitions());
    }

    /// The embedded font as a CAD text font (`wcad_geom2d::text`), parsed once.
    pub fn cad_font() -> Option<&'static wcad_geom2d::text::Font> {
        static FONT: OnceLock<Option<wcad_geom2d::text::Font>> = OnceLock::new();
        FONT.get_or_init(|| wcad_geom2d::text::Font::from_bytes(Arc::from(CJK_UI_TTF), 0).ok())
            .as_ref()
    }
}

/// Web entry point (wasm32 only): runs eframe on `<canvas id="webcad_canvas">` of `index.html`.
#[cfg(target_arch = "wasm32")]
pub mod web {
    use std::sync::atomic::{AtomicBool, Ordering};
    use wasm_bindgen::JsCast as _;

    /// Id of the canvas in `web/index.html`.
    pub const CANVAS_ID: &str = "webcad_canvas";

    /// Set once eframe has installed its own panic hook (which already logs to the console).
    static EFRAME_HOOK: AtomicBool = AtomicBool::new(false);

    /// Called from `main` when the wasm module is instantiated.
    pub fn start() {
        std::panic::set_hook(Box::new(|info| {
            if !EFRAME_HOOK.load(Ordering::Relaxed) {
                console_error_panic_hook::hook(info);
            }
            show_error(&info.to_string());
        }));
        eframe::WebLogger::init(log::LevelFilter::Info).ok();

        // Wraps (and calls) the hook above.
        let runner = eframe::WebRunner::new();
        EFRAME_HOOK.store(true, Ordering::Relaxed);

        wasm_bindgen_futures::spawn_local(async move {
            let Some(canvas) = find_canvas() else {
                show_error(&format!(
                    "<canvas id=\"{CANVAS_ID}\"> not found in the page"
                ));
                return;
            };
            let result = runner
                .start(
                    canvas,
                    eframe::WebOptions::default(),
                    Box::new(|cc| Ok(Box::new(super::WebCadApp::new(cc)))),
                )
                .await;
            match result {
                Ok(()) => {
                    remove_element("loading");
                    // Debug builds: `index.html#webcad-panic-test` exercises the crash box.
                    if cfg!(debug_assertions) && location_hash() == "#webcad-panic-test" {
                        panic!("panic test requested via #webcad-panic-test");
                    }
                }
                Err(e) => {
                    log::error!("failed to start eframe: {e:?}");
                    show_error(&format!("failed to start: {}", js_error_text(&e)));
                }
            }
        });
    }

    fn document() -> Option<web_sys::Document> {
        web_sys::window()?.document()
    }

    fn find_canvas() -> Option<web_sys::HtmlCanvasElement> {
        document()?
            .get_element_by_id(CANVAS_ID)?
            .dyn_into::<web_sys::HtmlCanvasElement>()
            .ok()
    }

    fn location_hash() -> String {
        web_sys::window()
            .and_then(|w| w.location().hash().ok())
            .unwrap_or_default()
    }

    /// The page's query string without the leading `?` (empty when absent).
    pub fn query_string() -> String {
        web_sys::window()
            .and_then(|w| w.location().search().ok())
            .map(|s| s.trim_start_matches('?').to_owned())
            .unwrap_or_default()
    }

    fn remove_element(id: &str) {
        if let Some(el) = document().and_then(|d| d.get_element_by_id(id)) {
            el.remove();
        }
    }

    /// Reveal the page's `#error` box with `msg` (also used by the panic hook, so it must not
    /// panic itself).
    fn show_error(msg: &str) {
        remove_element("loading");
        let Some(doc) = document() else { return };
        if let Some(text) = doc.get_element_by_id("error_text") {
            let old = text.text_content().unwrap_or_default();
            let new = if old.is_empty() {
                msg.to_owned()
            } else {
                format!("{old}\n\n{msg}")
            };
            text.set_text_content(Some(&new));
        }
        if let Some(bx) = doc
            .get_element_by_id("error")
            .and_then(|e| e.dyn_into::<web_sys::HtmlElement>().ok())
        {
            bx.set_hidden(false);
        }
    }

    fn js_error_text(v: &wasm_bindgen::JsValue) -> String {
        v.as_string()
            .or_else(|| {
                v.dyn_ref::<js_sys::Error>()
                    .map(|e| String::from(e.message()))
            })
            .unwrap_or_else(|| format!("{v:?}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cjk_font_is_primary_proportional_and_monospace_fallback() {
        let defs = fonts::definitions();
        let prop = &defs.families[&egui::FontFamily::Proportional];
        assert_eq!(prop.first().map(String::as_str), Some(fonts::CJK_FONT_NAME));
        let mono = &defs.families[&egui::FontFamily::Monospace];
        assert_eq!(mono.last().map(String::as_str), Some(fonts::CJK_FONT_NAME));
    }

    #[test]
    fn cjk_font_covers_ui_text() {
        use egui::epaint::text::{Fonts, TextOptions};
        let id = egui::FontId::proportional(14.0);
        let sample = "你好，webcad！二维绘图参数化草图三维实体建模图层尺寸标注Ø±°";
        let mut stock = Fonts::new(TextOptions::default(), egui::FontDefinitions::default());
        assert!(
            !stock.has_glyphs(&id, "你好"),
            "stock egui fonts have no CJK"
        );
        let mut fonts = Fonts::new(TextOptions::default(), fonts::definitions());
        assert!(fonts.has_glyphs(&id, sample));
        assert!(fonts.has_glyphs(&egui::FontId::monospace(12.0), "图层"));
    }

    #[test]
    fn cad_font_parses() {
        let f = fonts::cad_font().expect("embedded font parses");
        assert!(f.has_glyph('图'));
    }
}
