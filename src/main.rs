#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod api;
mod app;
mod config;
mod covers;
mod library;
mod theme;
mod util;
mod widgets;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("RomM Explorer")
            .with_inner_size([1400.0, 880.0])
            .with_min_inner_size([980.0, 600.0]),
        ..Default::default()
    };
    eframe::run_native("RomM Explorer", options, Box::new(|cc| Ok(Box::new(app::App::new(cc)))))
}
