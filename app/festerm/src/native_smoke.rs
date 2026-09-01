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
use festerm_session::Session;
use festerm_ui_egui::EncodedInputSink;

use crate::session_controller::SessionController;

const SMOKE_ENV: &str = "FESTERM_NATIVE_WINDOW_SMOKE";
const OS_INPUT_SMOKE_ENV: &str = "FESTERM_NATIVE_OS_INPUT_SMOKE";
const LIVE_RESIZE_SMOKE_ENV: &str = "FESTERM_NATIVE_LIVE_RESIZE_SMOKE";
const EMOJI_SMOKE_ENV: &str = "FESTERM_NATIVE_EMOJI_SMOKE";
const RESULT_PATH_ENV: &str = "FESTERM_NATIVE_SMOKE_RESULT_PATH";
const LIVE_RESIZE_DRIVER_RESULT_PATH_ENV: &str = "FESTERM_NATIVE_LIVE_RESIZE_DRIVER_RESULT_PATH";
const ALLOW_UNFOCUSED_ENV: &str = "FESTERM_NATIVE_SMOKE_ALLOW_UNFOCUSED";
const TIMEOUT: Duration = Duration::from_secs(20);
const LIVE_RESIZE_FRAME_COUNT: usize = 120;
const EMOJI_FIXTURE_COMMAND: &str =
    "emit:🤖 bot  🗑️ clean  ⚠️ warn  ℹ️ info  👩‍🔬 lab  1️⃣ key  🇺🇸 flag  aligned";
const RESIZE_SEQUENCE: [(f32, f32); 4] = [
    (420.0, 260.0),
    (860.0, 540.0),
    (560.0, 360.0),
    (860.0, 540.0),
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Phase {
    AwaitInitialOutput,
    AwaitPreOutput,
    AwaitResize(usize),
    AwaitInput,
    AwaitPostOutput,
    Finished,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SmokeKind {
    NativeWindow,
    OsInput,
    LiveResize,
    Emoji,
}

pub struct NativeWindowSmoke {
    result_path: PathBuf,
    test_child_path: PathBuf,
    started: Instant,
    phase: Phase,
    phase_started: Instant,
    resize_dimensions: Vec<(usize, usize)>,
    focus_observed: bool,
    initial_output_bytes: Option<u64>,
    post_resize_output_bytes: Option<u64>,
    first_resize_generation: Option<u64>,
    allow_unfocused: bool,
    kind: SmokeKind,
    live_resize_driver_result_path: Option<PathBuf>,
}

impl NativeWindowSmoke {
    pub fn from_environment() -> Option<Self> {
        let kind = match (
            std::env::var_os(SMOKE_ENV).is_some(),
            std::env::var_os(OS_INPUT_SMOKE_ENV).is_some(),
            std::env::var_os(LIVE_RESIZE_SMOKE_ENV).is_some(),
            std::env::var_os(EMOJI_SMOKE_ENV).is_some(),
        ) {
            (false, false, false, false) => return None,
            (true, false, false, false) => SmokeKind::NativeWindow,
            (false, true, false, false) => SmokeKind::OsInput,
            (false, false, true, false) => SmokeKind::LiveResize,
            (false, false, false, true) => SmokeKind::Emoji,
            _ => {
                panic!(
                    "only one of {SMOKE_ENV}, {OS_INPUT_SMOKE_ENV}, {LIVE_RESIZE_SMOKE_ENV}, and {EMOJI_SMOKE_ENV} may be enabled"
                )
            }
        };

        let result_path = std::env::var_os(RESULT_PATH_ENV)
            .map(PathBuf::from)
            .expect("FESTERM_NATIVE_SMOKE_RESULT_PATH is required in native smoke mode");
        let test_child_path = test_child_path();
        assert!(
            test_child_path.exists(),
            "native smoke test child is missing at {test_child_path:?}; build the workspace first"
        );
        Self::write_result(&result_path, "running", "");
        let live_resize_driver_result_path = match kind {
            SmokeKind::LiveResize => Some(
                std::env::var_os(LIVE_RESIZE_DRIVER_RESULT_PATH_ENV)
                    .map(PathBuf::from)
                    .expect(
                        "FESTERM_NATIVE_LIVE_RESIZE_DRIVER_RESULT_PATH is required in live resize smoke mode",
                    ),
            ),
            SmokeKind::NativeWindow | SmokeKind::OsInput | SmokeKind::Emoji => None,
        };

        Some(Self {
            result_path,
            test_child_path,
            started: Instant::now(),
            phase: Phase::AwaitInitialOutput,
            phase_started: Instant::now(),
            resize_dimensions: Vec::new(),
            focus_observed: false,
            initial_output_bytes: None,
            post_resize_output_bytes: None,
            first_resize_generation: None,
            allow_unfocused: cfg!(target_os = "linux")
                && std::env::var_os(ALLOW_UNFOCUSED_ENV).is_some(),
            kind,
            live_resize_driver_result_path,
        })
    }

    pub fn test_child_path(&self) -> PathBuf {
        self.test_child_path.clone()
    }

    pub const fn test_child_arguments(&self) -> &'static [&'static str] {
        match self.kind {
            SmokeKind::NativeWindow => &[
                "emit:LINE-A",
                "emit:MARKER",
                "read-line",
                "echo:PRE",
                "read-line",
                "echo:POST",
                "report-size",
                "spin",
            ],
            // The OS driver sends Tab, Up, a fixed token, and Enter. The
            // child cannot emit its post-read line until those real window
            // events make it through the UI and PTY input path.
            SmokeKind::OsInput => &["emit:READY", "read-line", "echo:OS-INPUT", "spin"],
            // The macOS driver performs a real, rapid corner drag while this
            // child emits for three seconds. Keeping the source independent
            // from the UI driver makes any lost frame observable in terminal
            // history instead of relying on a timing-sensitive visual check.
            SmokeKind::LiveResize => &["emit-frames:120:25", "spin"],
            SmokeKind::Emoji => &[EMOJI_FIXTURE_COMMAND, "spin"],
        }
    }

    pub fn drive<S: Session>(
        &mut self,
        context: &eframe::egui::Context,
        terminal: &mut Terminal,
        controller: &mut SessionController<S>,
        color_emoji_paints: usize,
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
        if viewport.native_pixels_per_point.is_none() {
            // WSLg and some compositors populate viewport metadata after their
            // first frame. Keep the native window alive until the bounded
            // smoke timeout rather than mistaking that startup frame for a
            // missing native viewport.
            //
            // `inner_rect`/`outer_rect` are deliberately not part of this
            // readiness gate: per `egui::ViewportInfo`, both are always
            // `None` on native Wayland (and Android) because the platform
            // does not expose absolute window position to clients. Window
            // size (unlike position) is still delivered on every platform,
            // so size-dependent checks below use `Context::viewport_rect`
            // instead of `inner_rect`.
            context.request_repaint_after(Duration::from_millis(10));
            return;
        }

        match (self.kind, self.phase) {
            (SmokeKind::LiveResize, Phase::AwaitInitialOutput)
                if controller.resize_probe().observed_output_bytes() > 0 =>
            {
                self.initial_output_bytes = Some(controller.resize_probe().observed_output_bytes());
                self.phase = Phase::AwaitPostOutput;
            }
            (SmokeKind::LiveResize, Phase::AwaitPostOutput) => {
                let driver_completed =
                    self.live_resize_driver_result_path
                        .as_deref()
                        .is_some_and(|path| {
                            std::fs::read_to_string(path).ok().as_deref() == Some("status=pass\n")
                        });
                let terminal_text = terminal_text_including_scrollback(terminal);
                let all_frames_survived = (0..LIVE_RESIZE_FRAME_COUNT)
                    .all(|index| terminal_text.contains(&format!("FRAME:{index:02}")));
                let applied_resize = controller
                    .resize_probe()
                    .generations()
                    .iter()
                    .any(|generation| generation.applied);
                if driver_completed && all_frames_survived && applied_resize {
                    self.finish(
                        context,
                        "pass",
                        &format!(
                            "physical corner drag completed; all {LIVE_RESIZE_FRAME_COUNT} PTY frames survived; \
                             resize generations {}",
                            controller.resize_probe().generations().len()
                        ),
                    );
                }
            }
            (SmokeKind::OsInput, Phase::AwaitInitialOutput)
                if controller.resize_probe().observed_output_bytes() > 0 =>
            {
                self.initial_output_bytes = Some(controller.resize_probe().observed_output_bytes());
                self.phase = Phase::AwaitInput;
            }
            (SmokeKind::OsInput, Phase::AwaitInput)
                if controller.resize_probe().observed_output_bytes()
                    > self.initial_output_bytes.unwrap_or_default() =>
            {
                let generations = controller.resize_probe().generations();
                let resize_applied = generations
                    .iter()
                    .any(|generation| generation.applied && generation.visible_nonblank_cells > 0);
                if self.focus_observed && resize_applied {
                    self.finish(
                        context,
                        "pass",
                        &format!(
                            "OS input reached PTY; resize generations {}; output {}B->{}B",
                            generations.len(),
                            self.initial_output_bytes.unwrap_or_default(),
                            controller.resize_probe().observed_output_bytes(),
                        ),
                    );
                }
            }
            (SmokeKind::Emoji, Phase::AwaitInitialOutput)
                if controller.resize_probe().observed_output_bytes() > 0 =>
            {
                self.phase = Phase::AwaitPostOutput;
                context.request_repaint_after(Duration::from_millis(10));
            }
            (SmokeKind::Emoji, Phase::AwaitPostOutput) => {
                if (self.focus_observed || self.allow_unfocused)
                    && emoji_smoke_matches(terminal, color_emoji_paints)
                {
                    self.finish(
                        context,
                        "pass",
                        "native emoji fixture retained seven double-width clusters, used seven color textures, and preserved trailing ASCII",
                    );
                } else {
                    context.request_repaint_after(Duration::from_millis(50));
                }
            }
            (SmokeKind::NativeWindow, Phase::AwaitInitialOutput)
                if controller.resize_probe().observed_output_bytes() > 0 =>
            {
                self.initial_output_bytes = Some(controller.resize_probe().observed_output_bytes());
                controller.record_encoded_input(b"pre\r\n");
                self.phase = Phase::AwaitPreOutput;
            }
            (SmokeKind::NativeWindow, Phase::AwaitPreOutput)
                if controller.resize_probe().observed_output_bytes()
                    > self.initial_output_bytes.unwrap_or_default() =>
            {
                self.request_resize(context, controller, 0);
            }
            (SmokeKind::NativeWindow, Phase::AwaitResize(index)) => {
                let requested = RESIZE_SEQUENCE[index];
                // `viewport_rect` is the window's content size in points,
                // derived from resize events rather than absolute window
                // position, so (unlike `inner_rect`) it is populated on
                // native Wayland.
                let content_rect = context.viewport_rect();
                let applied = (content_rect.width() - requested.0).abs() < 8.0
                    && (content_rect.height() - requested.1).abs() < 8.0;
                // The viewport event reaches `logic` before the following
                // `ui` call applies its measured grid size to the terminal.
                // Wait one settled frame so the recorded dimensions belong to
                // this requested native size, rather than the previous one.
                let first_generation = self.first_resize_generation.unwrap_or_default();
                let generations = controller
                    .resize_probe()
                    .generations()
                    .into_iter()
                    .filter(|generation| generation.generation >= first_generation)
                    .collect::<Vec<_>>();
                let resize_applied = generations
                    .get(index)
                    .is_some_and(|generation| generation.applied);
                if applied
                    && resize_applied
                    && self.phase_started.elapsed() >= Duration::from_millis(100)
                {
                    let dimensions = terminal.dimensions();
                    let pair = (dimensions.columns(), dimensions.rows());
                    if self.resize_dimensions.last() != Some(&pair) {
                        self.resize_dimensions.push(pair);
                        if index + 1 == RESIZE_SEQUENCE.len() {
                            self.post_resize_output_bytes =
                                Some(controller.resize_probe().observed_output_bytes());
                            self.phase = Phase::AwaitInput;
                            self.phase_started = Instant::now();
                        } else {
                            self.request_resize(context, controller, index + 1);
                        }
                    }
                }
            }
            (SmokeKind::NativeWindow, Phase::AwaitInput)
                if self.phase_started.elapsed() >= Duration::from_millis(250) =>
            {
                controller.record_encoded_input(b"post\r\n");
                self.phase = Phase::AwaitPostOutput;
            }
            (SmokeKind::NativeWindow, Phase::AwaitPostOutput)
                if controller.resize_probe().observed_output_bytes()
                    > self.post_resize_output_bytes.unwrap_or_default() =>
            {
                let first_generation = self.first_resize_generation.unwrap_or_default();
                let generations = controller
                    .resize_probe()
                    .generations()
                    .into_iter()
                    .filter(|generation| generation.generation >= first_generation)
                    .collect::<Vec<_>>();
                // A compositor may apply one startup resize after the smoke
                // begins but before the first requested viewport size. The
                // final four generations correspond to the requested sequence.
                let requested_generations = generations
                    .iter()
                    .rev()
                    .take(RESIZE_SEQUENCE.len())
                    .copied()
                    .collect::<Vec<_>>();
                let all_resizes_applied = requested_generations.len() == RESIZE_SEQUENCE.len()
                    && requested_generations
                        .iter()
                        .all(|generation| generation.applied);
                let visible_cells_remained = requested_generations
                    .iter()
                    .all(|generation| generation.visible_nonblank_cells > 0);
                let post_resize_output = controller.resize_probe().observed_output_bytes();
                if (self.focus_observed || self.allow_unfocused)
                    && self.resize_dimensions.len() == RESIZE_SEQUENCE.len()
                    && all_resizes_applied
                    && visible_cells_remained
                    && post_resize_output > self.post_resize_output_bytes.unwrap_or_default()
                {
                    self.finish(
                        context,
                        "pass",
                        &format!(
                            "native viewport{}; resize generations {}; output {}B->{}B; \
                             recognized CSI 6n {}; visible cells {:?}",
                            if self.focus_observed {
                                "/focus"
                            } else {
                                " (focus unavailable in explicit headless mode)"
                            },
                            generations.len(),
                            self.post_resize_output_bytes.unwrap_or_default(),
                            post_resize_output,
                            controller.resize_probe().cursor_position_queries(),
                            generations
                                .iter()
                                .map(|generation| generation.visible_nonblank_cells)
                                .collect::<Vec<_>>(),
                        ),
                    );
                } else {
                    self.finish(
                        context,
                        "fail",
                        &format!(
                            "content-free resize probe incomplete: focus={}, resize_count={}, \
                             generated={}, applied={}, visible={}, output={}B->{}B, csi_6n={}",
                            self.focus_observed,
                            self.resize_dimensions.len(),
                            generations.len(),
                            generations
                                .iter()
                                .filter(|generation| generation.applied)
                                .count(),
                            visible_cells_remained,
                            self.post_resize_output_bytes.unwrap_or_default(),
                            post_resize_output,
                            controller.resize_probe().cursor_position_queries(),
                        ),
                    );
                }
            }
            (_, Phase::Finished)
            | (_, Phase::AwaitInitialOutput)
            | (_, Phase::AwaitPreOutput)
            | (_, Phase::AwaitInput)
            | (_, Phase::AwaitPostOutput)
            | (SmokeKind::OsInput, Phase::AwaitResize(_))
            | (SmokeKind::LiveResize, Phase::AwaitResize(_))
            | (SmokeKind::Emoji, Phase::AwaitResize(_)) => {}
        }
    }

    fn request_resize<S: Session>(
        &mut self,
        context: &eframe::egui::Context,
        controller: &SessionController<S>,
        index: usize,
    ) {
        let (width, height) = RESIZE_SEQUENCE[index];
        if index == 0 {
            self.first_resize_generation = Some(
                controller
                    .resize_probe()
                    .requested_generations()
                    .saturating_add(1),
            );
        }
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

fn terminal_text_including_scrollback(terminal: &Terminal) -> String {
    let history = terminal
        .scrollback_lines()
        .flat_map(|line| line.cells())
        .map(|cell| cell.character())
        .collect::<String>();
    let visible = (0..terminal.dimensions().rows())
        .filter_map(|row| terminal.row_text(row))
        .collect::<String>();
    format!("{history}{visible}")
}

fn emoji_fixture_matches(terminal: &Terminal) -> bool {
    let clusters_match = ["🤖", "🗑️", "⚠️", "ℹ️", "👩‍🔬", "1️⃣", "🇺🇸"]
        .into_iter()
        .all(|expected| {
            (0..terminal.dimensions().rows()).any(|row| {
                (0..terminal.dimensions().columns().saturating_sub(1)).any(|column| {
                    terminal.cell(column, row).is_some_and(|cell| {
                        cell.text() == expected
                            && cell.width() == festerm_core::CellWidth::Double
                            && terminal
                                .cell(column + 1, row)
                                .is_some_and(|cell| cell.is_continuation())
                    })
                })
            })
        });
    clusters_match && terminal_text_including_scrollback(terminal).contains("aligned")
}

fn emoji_smoke_matches(terminal: &Terminal, color_emoji_paints: usize) -> bool {
    emoji_fixture_matches(terminal) && color_emoji_paints >= 7
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smoke_mode_requires_an_explicit_environment_variable() {
        assert_eq!(SMOKE_ENV, "FESTERM_NATIVE_WINDOW_SMOKE");
        assert_eq!(OS_INPUT_SMOKE_ENV, "FESTERM_NATIVE_OS_INPUT_SMOKE");
        assert_eq!(LIVE_RESIZE_SMOKE_ENV, "FESTERM_NATIVE_LIVE_RESIZE_SMOKE");
        assert_eq!(EMOJI_SMOKE_ENV, "FESTERM_NATIVE_EMOJI_SMOKE");
        assert_eq!(RESULT_PATH_ENV, "FESTERM_NATIVE_SMOKE_RESULT_PATH");
    }

    #[test]
    fn native_emoji_fixture_requires_double_width_clusters_and_trailing_ascii() {
        let mut terminal = Terminal::new(festerm_core::Dimensions::new(100, 2).unwrap()).unwrap();
        terminal.ingest(
            EMOJI_FIXTURE_COMMAND
                .strip_prefix("emit:")
                .unwrap()
                .as_bytes(),
        );
        assert!(emoji_fixture_matches(&terminal));
        assert!(!emoji_smoke_matches(&terminal, 6));
        assert!(emoji_smoke_matches(&terminal, 7));
    }
}
