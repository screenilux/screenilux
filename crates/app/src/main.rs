//! Screenilux: a screen recorder for Windows.

// A GUI app: no console window on release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod icon;
mod overlay;
mod player_view;
mod sizing;
mod state;
mod theme;

use eframe::egui;

fn main() -> eframe::Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_target(false)
        .init();

    // Before any window exists, so captures are in physical pixels.
    screenilux_win::set_dpi_awareness();

    // Whichever copy runs keeps .screenilux files and "Open with" pointing
    // at itself, so they work from the first launch, wherever the program
    // was put, and follow it if it moves. Only changed values are written,
    // so a launch with nothing to change costs a few registry reads. Off the
    // UI thread, since Explorer is notified when anything did change.
    if let Ok(exe) = std::env::current_exe() {
        std::thread::spawn(move || {
            if let Err(e) = screenilux_win::register_file_types(&exe) {
                tracing::warn!(error = %e, "could not register file types");
            }
        });
    }

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Screenilux")
            .with_icon(egui::IconData {
                rgba: icon::rgba(64),
                width: 64,
                height: 64,
            })
            // Reshaped on the first frame, once the screen it opened on is
            // known; see `sizing`.
            .with_inner_size([sizing::RECORD_WIDTH, 780.0])
            .with_min_inner_size(sizing::MIN_SIZE),
        ..Default::default()
    };

    eframe::run_native(
        "Screenilux",
        options,
        Box::new(|cc| {
            theme::install(&cc.egui_ctx);
            // The window's own handle, for keeping it out of recordings and
            // for the recording bar's look.
            let hwnd = {
                use raw_window_handle::{HasWindowHandle, RawWindowHandle};
                cc.window_handle().ok().and_then(|h| match h.as_raw() {
                    RawWindowHandle::Win32(w) => Some(w.hwnd.get()),
                    _ => None,
                })
            };
            let mut app = app::App::new(hwnd, &cc.egui_ctx);
            // `screenilux path\to\recording.screenilux` opens it for playing,
            // which is what double-clicking a recording passes.
            if let Some(path) = std::env::args_os().nth(1) {
                app.play(std::path::PathBuf::from(path), &cc.egui_ctx);
            }
            Ok(Box::new(app) as Box<dyn eframe::App>)
        }),
    )
}
