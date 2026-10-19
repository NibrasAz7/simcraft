//! SimCraft Studio — `simcraft-studio [model.simcraft]`.
//!
//! A `model.simcraft` argument opens in the diagram canvas at startup.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
#![cfg_attr(all(target_os = "windows", not(debug_assertions)), windows_subsystem = "windows")]

use simcraft_studio::{StudioApp, StudioOptions};

const USAGE: &str = "SimCraft Studio\n\nusage: simcraft-studio [model.simcraft]\n\n  model.simcraft  open in the diagram canvas\n";

fn main() -> eframe::Result<()> {
    let mut file: Option<std::path::PathBuf> = None;
    for a in std::env::args().skip(1) {
        match a.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                return Ok(());
            }
            s if !s.starts_with('-') && file.is_none() => file = Some(std::path::PathBuf::from(s)),
            s => eprintln!("unknown argument `{s}`"),
        }
    }
    let native = eframe::NativeOptions { viewport: egui::ViewportBuilder::default().with_inner_size([1280.0, 800.0]), ..Default::default() };
    eframe::run_native("SimCraft Studio", native, Box::new(move |cc| Ok(Box::new(StudioApp::new(cc, StudioOptions { startup_model: file.clone() })))))
}
