use std::sync::Arc;

use festerm_core::{Dimensions, Terminal};
use festerm_pty::{LocalProfile, LocalPtySession};
use festerm_session::Session;
use festerm_ui_egui::TerminalView;

use crate::native_smoke::NativeWindowSmoke;
use crate::session_controller::{seed_session_failure, terminal_size, SessionController};

const APPLICATION_TITLE: &str = "fesTerm";

pub struct FesTermApp {
    terminal: Terminal,
    terminal_view: TerminalView,
    controller: SessionController<LocalPtySession>,
    window_title: String,
    native_smoke: Option<NativeWindowSmoke>,
}

impl FesTermApp {
    pub fn new(context: &eframe::egui::Context) -> Self {
        let mut terminal =
            Terminal::new(Dimensions::new(80, 24).expect("default dimensions are valid"))
                .expect("default terminal allocation should succeed");
        let notifier: Arc<dyn festerm_session::SessionEventNotifier> =
            Arc::new(EguiRepaintNotifier(context.clone()));
        let size = terminal_size(terminal.dimensions()).expect("default dimensions fit PTY limits");
        let native_smoke = NativeWindowSmoke::from_environment();
        let controller = match Self::start_session(native_smoke.as_ref(), size, notifier) {
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
            native_smoke,
        }
    }

    fn start_session(
        native_smoke: Option<&NativeWindowSmoke>,
        size: festerm_session::TerminalSize,
        notifier: Arc<dyn festerm_session::SessionEventNotifier>,
    ) -> Result<LocalPtySession, festerm_pty::LocalPtyError> {
        match native_smoke {
            Some(smoke) => LocalPtySession::start_with_notifier(
                LocalProfile::new(smoke.test_child_path()).with_arguments([
                    "emit:LINE-A",
                    "emit:MARKER",
                    "read-line",
                    "echo:PRE",
                    "read-line",
                    "echo:POST",
                    "report-size",
                    "spin",
                ]),
                size,
                notifier,
            ),
            None => LocalPtySession::start_default_with_notifier(size, notifier),
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
        if let Some(smoke) = &mut self.native_smoke {
            smoke.drive(context, &mut self.terminal, &mut self.controller);
        }
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
        self.controller
            .observe_resize_probe_terminal_state(&self.terminal);
        self.controller.forward_terminal_replies(&mut self.terminal);
        self.controller.flush_pending_writes();
        self.controller.flush_pending_resize();
        if self.controller.pump_events(&mut self.terminal) {
            ui.ctx().request_repaint();
        }
        if self.native_smoke.is_some() {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(10));
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
