//! zune-gui — a desktop client for the Zune 30 / Zune HD.
//!
//! Threading model: the UI thread never touches USB. A worker thread owns the MTP
//! session and reports through a channel; see `worker.rs`.

mod app;
mod cache;
mod core;
mod coverfetch;
mod design;
mod model;
mod t2s;
mod theme;
mod ui;
mod worker;

use eframe::egui;

fn main() -> eframe::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    // Dev affordance: ZUNE_WINDOW=1400x2600 makes one tall window so the whole
    // component board fits in a single screenshot.
    let size = std::env::var("ZUNE_WINDOW")
        .ok()
        .and_then(|s| {
            let (w, h) = s.split_once('x')?;
            Some([w.trim().parse().ok()?, h.trim().parse().ok()?])
        })
        .unwrap_or([1280.0, 860.0]);

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Zune")
            .with_inner_size(size)
            .with_min_inner_size([900.0, 600.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Zune",
        options,
        Box::new(|cc| Ok(Box::new(app::ZuneApp::new(cc)))),
    )
}
