//! Screenilux: a screen recorder for Windows.

// A GUI app: no console window on release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod overlay;
mod player_view;
mod state;

use eframe::egui;

fn main() -> eframe::Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_target(false)
        .init();

    // Before any window exists, so captures are in physical pixels.
    screenilux_win::set_dpi_awareness();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Screenilux")
            .with_inner_size([560.0, 720.0])
            .with_min_inner_size([480.0, 520.0]),
        ..Default::default()
    };

    eframe::run_native(
        "Screenilux",
        options,
        Box::new(|cc| {
            let mut app = app::App::new();
            // `screenilux path\to\recording.screenilux` opens it for playing,
            // which is what double-clicking a recording passes.
            if let Some(path) = std::env::args_os().nth(1) {
                app.play(std::path::PathBuf::from(path), &cc.egui_ctx);
            }
            Ok(Box::new(app) as Box<dyn eframe::App>)
        }),
    )
}
