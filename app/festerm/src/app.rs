use std::sync::Arc;

use festerm_core::{Dimensions, Terminal};
use festerm_pty::LocalPtySession;
use festerm_session::Session;
use festerm_ui_egui::TerminalView;

use crate::session_controller::{seed_session_failure, terminal_size, SessionController};

const APPLICATION_TITLE: &str = "fesTerm";

pub struct FesTermApp {
    terminal: Terminal,
    terminal_view: TerminalView,
    controller: SessionController<LocalPtySession>,
    window_title: String,
}

impl FesTermApp {
    pub fn new(context: &eframe::egui::Context) -> Self {
        let mut terminal =
            Terminal::new(Dimensions::new(80, 24).expect("default dimensions are valid"))
                .expect("default terminal allocation should succeed");
        let notifier: Arc<dyn festerm_session::SessionEventNotifier> =
            Arc::new(EguiRepaintNotifier(context.clone()));
        let size = terminal_size(terminal.dimensions()).expect("default dimensions fit PTY limits");
        let controller = match LocalPtySession::start_default_with_notifier(size, notifier) {
            Ok(session) => {
                tracing::info!(
                    target: "festerm::session",
                    session = %session.id(),
                    "started default local shell session"
                );
                SessionController::with_session(session)
            }
            Err(error) => {
                let msg = error.to_string();
                tracing::error!(
                    target: "festerm::session",
                    %error,
                    "could not start default local shell"
                );
                seed_session_failure(&mut terminal, &msg);
                SessionController::with_startup_error(msg)
            }
        };
        Self {
            terminal,
            terminal_view: TerminalView::default(),
            controller,
            window_title: APPLICATION_TITLE.to_owned(),
        }
    }

    fn update_window_title(&mut self, context: &eframe::egui::Context) {
        let title = Self::window_title(self.terminal.title());
        if self.window_title != title {
            context.send_viewport_cmd(eframe::egui::ViewportCommand::Title(title.clone()));
            self.window_title = title;
        }
    }

    fn window_title(terminal_title: &str) -> String {
        match terminal_title {
            "" => APPLICATION_TITLE.to_owned(),
            terminal_title => format!("{terminal_title} - {APPLICATION_TITLE}"),
        }
    }
}

/// Uses egui's thread-safe wake mechanism instead of polling for PTY output.
struct EguiRepaintNotifier(eframe::egui::Context);

impl festerm_session::SessionEventNotifier for EguiRepaintNotifier {
    fn notify(&self) {
        self.0.request_repaint();
    }
}

impl eframe::App for FesTermApp {
    fn logic(&mut self, context: &eframe::egui::Context, _frame: &mut eframe::Frame) {
        if self.controller.pump_events(&mut self.terminal) {
            context.request_repaint();
        }
        self.controller.forward_terminal_replies(&mut self.terminal);
        self.controller.flush_pending_writes();
        self.controller.flush_pending_resize();
        self.update_window_title(context);
    }

    fn ui(&mut self, ui: &mut eframe::egui::Ui, _frame: &mut eframe::Frame) {
        let session_status = self.controller.status_line();
        let session_diagnostics = self.controller.diagnostics_line();
        self.terminal_view.show_with_status(
            ui,
            &mut self.terminal,
            &mut self.controller,
            &session_status,
            &session_diagnostics,
        );
        self.controller.forward_terminal_replies(&mut self.terminal);
        self.controller.flush_pending_writes();
        self.controller.flush_pending_resize();
        if self.controller.pump_events(&mut self.terminal) {
            ui.ctx().request_repaint();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_title_is_scoped_to_the_application_window() {
        assert_eq!(FesTermApp::window_title(""), APPLICATION_TITLE);
        assert_eq!(FesTermApp::window_title("editor"), "editor - fesTerm");
    }
}
