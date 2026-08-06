mod diagnostics;

use festerm_core::{Dimensions, Terminal};
use festerm_ui_egui::{EncodedInputSink, InputRoute, InputSinkDiagnostics, TerminalView};

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
    terminal: Terminal,
    terminal_view: TerminalView,
    no_session_input: NoSessionInputSink,
}

impl Default for FesTermApp {
    fn default() -> Self {
        let mut terminal =
            Terminal::new(Dimensions::new(80, 24).expect("default dimensions are valid"))
                .expect("default terminal allocation should succeed");
        seed_no_session_demo(&mut terminal);
        Self {
            terminal,
            terminal_view: TerminalView::default(),
            no_session_input: NoSessionInputSink::default(),
        }
    }
}

impl eframe::App for FesTermApp {
    fn update(&mut self, context: &eframe::egui::Context, _frame: &mut eframe::Frame) {
        self.terminal_view
            .show(context, &mut self.terminal, &mut self.no_session_input);
    }
}

/// Temporary application-owned sink until Milestone 5 supplies session I/O.
#[derive(Default)]
struct NoSessionInputSink {
    diagnostics: InputSinkDiagnostics,
}

impl EncodedInputSink for NoSessionInputSink {
    fn record_encoded_input(&mut self, bytes: &[u8]) {
        self.diagnostics.byte_count = self
            .diagnostics
            .byte_count
            .saturating_add(bytes.len() as u64);
    }

    fn observe_input_route(&mut self, route: InputRoute) {
        self.diagnostics.event_count = self.diagnostics.event_count.saturating_add(1);
        self.diagnostics.last_outcome = Some(route.outcome);
        self.diagnostics.last_queue_depth = route.queue_depth;
    }

    fn input_diagnostics(&self) -> Option<InputSinkDiagnostics> {
        Some(self.diagnostics)
    }
}

fn seed_no_session_demo(terminal: &mut Terminal) {
    terminal.ingest(
        "\x1b[2J\x1b[H\
\x1b[1;36mfesTerm M4 graphical terminal view\x1b[0m\r\n\
\r\n\
This is a recorded no-session display stream.\r\n\
It is not a shell and it does not run commands.\r\n\
\r\n\
\x1b[1;33mInput is encoded by festerm-core; the app retains counts only.\x1b[0m\r\n\
Local selection and Copy use the native egui clipboard path.\r\n\
\r\n\
\x1b[32m✓\x1b[0m colors, attributes, wide cells: 界 😀\r\n\
\x1b[35mResize this window to recalculate terminal rows and columns.\x1b[0m"
            .as_bytes(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_session_sink_retains_only_content_free_bounded_metadata() {
        let mut sink = NoSessionInputSink::default();
        sink.record_encoded_input(b"private paste contents");
        sink.observe_input_route(InputRoute {
            outcome: festerm_core::InputEventOutcome::Encoded { bytes: 22 },
            queue_depth: 22,
            delivered_bytes: 22,
        });

        assert_eq!(
            sink.input_diagnostics(),
            Some(InputSinkDiagnostics {
                event_count: 1,
                byte_count: 22,
                last_outcome: Some(festerm_core::InputEventOutcome::Encoded { bytes: 22 }),
                last_queue_depth: 22,
            })
        );
    }

    #[test]
    fn demo_stream_is_terminal_content_not_a_prompt() {
        let mut terminal =
            Terminal::new(Dimensions::new(80, 24).unwrap()).expect("terminal allocation");
        seed_no_session_demo(&mut terminal);
        assert!(terminal
            .row_text(2)
            .is_some_and(|row| row.starts_with("This is a recorded no-session display stream.")));
    }
}
