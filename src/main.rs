#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod extractor;
mod gui;
mod palette;
mod models;
mod search;
mod utils;

use eframe::egui;
use gui::TraceTextGui;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1200.0, 600.0])
            .with_min_inner_size([750.0, 500.0]),
        ..Default::default()
    };
    
    eframe::run_native(
        "TraceText",
        options,
        Box::new(|cc| Ok(Box::new(TraceTextGui::new(cc)))),
    )
}
