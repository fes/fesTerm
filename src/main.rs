use eframe::egui;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions::default();
    eframe::run_native(
        "fesTerm",
        options,
        Box::new(|_cc| Ok(Box::new(FesTermApp::default()))),
    )
}

struct FesTermApp {
    input: String,
}

impl Default for FesTermApp {
    fn default() -> Self {
        Self {
            input: String::new(),
        }
    }
}

impl eframe::App for FesTermApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("fesTerm");
            ui.label("Scratch multi-platform SSH/terminal client");
            ui.separator();
            ui.text_edit_singleline(&mut self.input);
        });
    }
}
