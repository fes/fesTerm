mod diagnostics;

use eframe::egui;
use festerm_core::{Dimensions, Terminal};

fn main() -> eframe::Result<()> {
    diagnostics::init();
    tracing::info!(target: "festerm::app", "starting fesTerm");

    let options = eframe::NativeOptions::default();
    eframe::run_native(
        "fesTerm",
        options,
        Box::new(|_cc| Ok(Box::new(FesTermApp::default()))),
    )
}

struct FesTermApp {
    input: String,
    terminal: Terminal,
}

impl Default for FesTermApp {
    fn default() -> Self {
        Self {
            input: String::new(),
            terminal: Terminal::new(Dimensions::new(80, 24).expect("default dimensions are valid")),
        }
    }
}

impl eframe::App for FesTermApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("fesTerm");
            ui.label("Workspace and terminal-core foundation");
            ui.separator();
            ui.label(format!(
                "Core: {} columns x {} rows; cursor {}, {}",
                self.terminal.dimensions().columns(),
                self.terminal.dimensions().rows(),
                self.terminal.cursor().column(),
                self.terminal.cursor().row()
            ));
            ui.text_edit_singleline(&mut self.input);
        });
    }
}
