//! Opt-in real-window smoke driver used only by the scheduled CI workflow.
//!
//! The driver runs inside the production eframe event loop. It verifies that a
//! native viewport is created, observed as focused, resized through the issue
//! #3 sequence, and continues to render a real PTY session. Platform-specific
//! OS input automation remains a later layer.

use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

use festerm_core::Terminal;
use festerm_pty::LocalPtySession;
use festerm_ui_egui::EncodedInputSink;

use crate::session_controller::SessionController;

const SMOKE_ENV: &str = "FESTERM_NATIVE_WINDOW_SMOKE";
const RESULT_PATH_ENV: &str = "FESTERM_NATIVE_SMOKE_RESULT_PATH";
const TIMEOUT: Duration = Duration::from_secs(20);
const RESIZE_SEQUENCE: [(f32, f32); 4] = [
    (420.0, 260.0),
    (860.0, 540.0),
    (560.0, 360.0),
    (860.0, 540.0),
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Phase {
    AwaitMarker,
    AwaitResize(usize),
    AwaitInput,
    AwaitEcho,
    AwaitReportedSize,
    Finished,
}

pub struct NativeWindowSmoke {
    result_path: PathBuf,
    test_child_path: PathBuf,
    started: Instant,
    phase: Phase,
    phase_started: Instant,
    resize_dimensions: Vec<(usize, usize)>,
    focus_observed: bool,
}

impl NativeWindowSmoke {
    pub fn from_environment() -> Option<Self> {
        if std::env::var_os(SMOKE_ENV).is_none() {
            return None;
        }

        let result_path = std::env::var_os(RESULT_PATH_ENV)
            .map(PathBuf::from)
            .expect("FESTERM_NATIVE_SMOKE_RESULT_PATH is required in native smoke mode");
        let test_child_path = test_child_path();
        assert!(
            test_child_path.exists(),
            "native smoke test child is missing at {test_child_path:?}; build the workspace first"
        );
        Self::write_result(&result_path, "running", "");

        Some(Self {
            result_path,
            test_child_path,
            started: Instant::now(),
            phase: Phase::AwaitMarker,
            phase_started: Instant::now(),
            resize_dimensions: Vec::new(),
            focus_observed: false,
        })
    }

    pub fn test_child_path(&self) -> PathBuf {
        self.test_child_path.clone()
    }

    pub fn drive(
        &mut self,
        context: &eframe::egui::Context,
        terminal: &mut Terminal,
        controller: &mut SessionController<LocalPtySession>,
    ) {
        if self.phase == Phase::Finished {
            return;
        }
        if self.started.elapsed() > TIMEOUT {
            self.finish(
                context,
                "fail",
                &format!(
                    "timeout while in {:?}; focus={}",
                    self.phase, self.focus_observed
                ),
            );
            return;
        }

        let viewport = context.input(|input| input.viewport().clone());
        self.focus_observed |= viewport.focused == Some(true);
        if viewport.inner_rect.is_none() || viewport.native_pixels_per_point.is_none() {
            self.finish(context, "fail", "native viewport metadata was unavailable");
            return;
        }

        match self.phase {
            Phase::AwaitMarker if terminal_contains(terminal, "MARKER") => {
                self.request_resize(context, 0);
            }
            Phase::AwaitResize(index) => {
                let requested = RESIZE_SEQUENCE[index];
                let applied = viewport.inner_rect.is_some_and(|rect| {
                    (rect.width() - requested.0).abs() < 8.0
                        && (rect.height() - requested.1).abs() < 8.0
                });
                if applied {
                    let dimensions = terminal.dimensions();
                    let pair = (dimensions.columns(), dimensions.rows());
                    if self.resize_dimensions.last() != Some(&pair) {
                        self.resize_dimensions.push(pair);
                        if index + 1 == RESIZE_SEQUENCE.len() {
                            self.phase = Phase::AwaitInput;
                            self.phase_started = Instant::now();
                        } else {
                            self.request_resize(context, index + 1);
                        }
                    }
                }
            }
            Phase::AwaitInput if self.phase_started.elapsed() >= Duration::from_millis(250) => {
                controller.record_encoded_input(b"hello\r\n");
                self.phase = Phase::AwaitEcho;
            }
            // ConPTY can issue a cursor-position query after resize. The core
            // correctly forwards that reply before the scripted line, so the
            // child may echo the reply bytes before `hello`.
            Phase::AwaitEcho if terminal_contains(terminal, "ECHO:") => {
                self.phase = Phase::AwaitReportedSize;
            }
            Phase::AwaitReportedSize => {
                let dimensions = terminal.dimensions();
                let expected = format!("{} {}", dimensions.rows(), dimensions.columns());
                if terminal_contains(terminal, &expected) {
                    if self.focus_observed && self.resize_dimensions.len() == RESIZE_SEQUENCE.len()
                    {
                        self.finish(
                            context,
                            "pass",
                            "native viewport, focus, resizes, PTY input/output verified",
                        );
                    } else {
                        self.finish(
                            context,
                            "fail",
                            "native focus or resize observations were incomplete",
                        );
                    }
                }
            }
            Phase::Finished | Phase::AwaitMarker | Phase::AwaitInput | Phase::AwaitEcho => {}
        }
    }

    fn request_resize(&mut self, context: &eframe::egui::Context, index: usize) {
        let (width, height) = RESIZE_SEQUENCE[index];
        context.send_viewport_cmd(eframe::egui::ViewportCommand::InnerSize(
            eframe::egui::vec2(width, height),
        ));
        self.phase = Phase::AwaitResize(index);
        self.phase_started = Instant::now();
    }

    fn finish(&mut self, context: &eframe::egui::Context, status: &str, detail: &str) {
        Self::write_result(&self.result_path, status, detail);
        self.phase = Phase::Finished;
        context.send_viewport_cmd(eframe::egui::ViewportCommand::Close);
    }

    fn write_result(path: &std::path::Path, status: &str, detail: &str) {
        let body = format!(
            "status={status}\ndetail={}\n",
            detail.replace(['\r', '\n'], " ")
        );
        std::fs::write(path, body).expect("native smoke result is writable");
    }
}

fn test_child_path() -> PathBuf {
    let mut path = std::env::current_exe().expect("application executable path is known");
    path.pop();
    path.push(if cfg!(windows) {
        "festerm-pty-test-child.exe"
    } else {
        "festerm-pty-test-child"
    });
    path
}

fn terminal_contains(terminal: &Terminal, needle: &str) -> bool {
    (0..terminal.dimensions().rows())
        .filter_map(|row| terminal.row_text(row))
        .any(|row| row.contains(needle))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smoke_mode_requires_an_explicit_environment_variable() {
        assert_eq!(SMOKE_ENV, "FESTERM_NATIVE_WINDOW_SMOKE");
        assert_eq!(RESULT_PATH_ENV, "FESTERM_NATIVE_SMOKE_RESULT_PATH");
    }
}
