//! `webcad` binary. Native: opens the desktop window. wasm32: wasm-bindgen turns `main` into the
//! module's start function, which boots the app on the page's canvas (see `wcad_app::web`).

// No console window for release builds on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(not(target_arch = "wasm32"))]
fn main() -> eframe::Result {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title(wcad_app::APP_NAME)
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([640.0, 400.0]),
        ..Default::default()
    };
    eframe::run_native(
        wcad_app::APP_NAME,
        options,
        Box::new(|cc| Ok(Box::new(wcad_app::WebCadApp::new(cc)))),
    )
}

#[cfg(target_arch = "wasm32")]
fn main() {
    wcad_app::web::start();
}
