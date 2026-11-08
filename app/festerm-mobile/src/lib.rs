//! iOS feasibility host: offline by default, with opt-in transient live SSH.

mod accessibility;
mod app_lock;
mod dictation;
mod document_preview;
mod documents;
#[cfg_attr(not(feature = "live-ssh"), allow(dead_code))] // SFTP needs the SSH transport
mod files;
#[cfg(test)]
mod files_fixture;
#[cfg_attr(not(feature = "live-ssh"), allow(dead_code))] // SFTP needs the SSH transport
mod files_shell;
#[cfg_attr(not(feature = "live-ssh"), allow(dead_code))] // SFTP needs the SSH transport
mod files_ui;
mod forms;
mod host_setup;
mod key_press;
#[cfg(feature = "live-ssh")]
mod live_ssh;
mod local_input;
#[allow(dead_code)] // design-system surface used by the gallery before production wiring
mod model;
mod password_buffer;
mod profiles;
#[cfg(all(
    feature = "live-ssh",
    any(target_os = "android", target_os = "ios", test)
))]
mod recording_draft;
#[cfg(all(feature = "live-ssh", any(target_os = "android", target_os = "ios")))]
mod recording_library_ui;
#[cfg(all(feature = "live-ssh", unix))]
mod recording_storage;
#[cfg(all(feature = "live-ssh", any(target_os = "android", target_os = "ios")))]
mod recording_tasks;
#[cfg(all(feature = "live-ssh", any(target_os = "android", target_os = "ios")))]
mod recording_ui;
mod runtime;
pub use runtime::run_native;
#[cfg(test)]
mod session_close_tests;
#[cfg(feature = "live-ssh")]
mod sftp_link;
mod shell;
mod tabs;
#[allow(dead_code)] // design-system surface used by the gallery before production wiring
mod terminal_chrome;
mod terminal_gesture;
mod terminal_keyboard;
#[allow(dead_code)] // design-system surface used by the gallery before production wiring
mod theme;
#[cfg(feature = "live-ssh")]
mod tunnel_link;
#[cfg(feature = "live-ssh")]
mod tunnel_shell;
#[cfg(all(test, feature = "live-ssh"))]
mod tunnel_tests;
#[allow(dead_code)] // design-system surface used by the gallery before production wiring
mod widgets;
mod workspaces;

#[cfg(all(test, feature = "live-ssh"))]
mod ui_gallery;

use eframe::egui;
use festerm_core::{Dimensions, InputEvent, InputEventOutcome, Key, Modifiers, Terminal};
use festerm_ui_egui::{
    route_input, EncodedInputSink, InputRoute, InputSinkDiagnostics, TerminalView,
    TerminalViewOptions,
};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

/// Writes `text` to the pasteboard and returns the confirmation to show; the
/// non-iOS desktop harness has no pasteboard, so it reports honestly.
fn clipboard_notice(text: &str, done: &'static str) -> Option<&'static str> {
    Some(if festerm_ios_window::set_clipboard_text(text) {
        done
    } else {
        "Copy is unavailable here"
    })
}

/// The egui mesh renderer does not need the full WebGPU limits. In particular,
/// iOS Simulator Metal exposes 15 inter-stage variables, below the default 16.
/// Keep device validation enabled and request the portable downlevel baseline,
/// retaining adapter texture dimensions for full-resolution phone/iPad surfaces.
fn mobile_device_descriptor(
    supported: eframe::wgpu::Limits,
) -> eframe::wgpu::DeviceDescriptor<'static> {
    eframe::wgpu::DeviceDescriptor {
        label: Some("fesTerm mobile renderer"),
        required_limits: eframe::wgpu::Limits::downlevel_defaults().using_resolution(supported),
        ..Default::default()
    }
}

pub fn mobile_wgpu_configuration() -> eframe::egui_wgpu::WgpuConfiguration {
    let mut setup = eframe::egui_wgpu::WgpuSetupCreateNew::without_display_handle();
    #[cfg(target_os = "android")]
    {
        setup.instance_descriptor.backends = eframe::wgpu::Backends::GL;
    }
    setup.device_descriptor =
        std::sync::Arc::new(|adapter| mobile_device_descriptor(adapter.limits()));
    eframe::egui_wgpu::WgpuConfiguration {
        wgpu_setup: setup.into(),
        ..Default::default()
    }
}

fn platform_text(ios: &'static str, android: &'static str) -> &'static str {
    if cfg!(target_os = "android") {
        android
    } else {
        ios
    }
}

fn mobile_style(style: &egui::Style) -> egui::Style {
    theme::apply(style)
}

pub use profiles::Profiles;

pub const SCROLLBACK_LIMIT: usize = 256 * 1024;

/// winit on iOS reports Return and Tab (software or hardware keyboard) as
/// `Key::Character("\n" / "\t")`, which egui-winit drops as non-printable. The
/// event loop queues them here and the app replays them as named keys.
#[derive(Clone, Default)]
pub struct HardwareKeys(Rc<RefCell<Vec<egui::Key>>>);

impl HardwareKeys {
    /// Returns whether `text` was a key that egui would otherwise lose.
    pub fn push_text(&self, text: &str) -> bool {
        let key = match text {
            "\n" | "\r" => egui::Key::Enter,
            "\t" => egui::Key::Tab,
            _ => return false,
        };
        self.0.borrow_mut().push(key);
        true
    }

    fn drain(&self) -> Vec<egui::Key> {
        std::mem::take(&mut *self.0.borrow_mut())
    }
}

/// Only content-free lifecycle observations cross the event-loop/UI seam.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Lifecycle {
    pub active: bool,
    pub resumes: u64,
    pub suspensions: u64,
    pub memory_warnings: u64,
}

impl Lifecycle {
    pub fn resume(&mut self) {
        if !self.active {
            self.resumes = self.resumes.saturating_add(1);
            self.active = true;
        }
    }

    pub fn suspend(&mut self) {
        if self.active {
            self.suspensions = self.suspensions.saturating_add(1);
            self.active = false;
        }
    }

    pub fn memory_warning(&mut self) {
        self.memory_warnings = self.memory_warnings.saturating_add(1);
    }
}

/// Clears sensitive SSH prompts; retains established tunnels with input gated until checked.
#[cfg(feature = "live-ssh")]
#[derive(Clone, Default)]
pub struct SuspensionHandle {
    connection: Rc<RefCell<Option<live_ssh::LiveSsh>>>,
    /// Sessions of parked tabs, keyed by tab id.
    background: Rc<RefCell<Vec<(u64, live_ssh::LiveSsh)>>>,
    recording: std::sync::Arc<std::sync::Mutex<Option<RecordingOwner>>>,
    export: std::sync::Arc<
        std::sync::Mutex<Option<std::sync::Weak<festerm_recording_export::ExportCancellation>>>,
    >,
}

#[cfg(feature = "live-ssh")]
struct RecordingOwner {
    loss: std::sync::Arc<std::sync::atomic::AtomicBool>,
    recording: Option<festerm_recording::SharedRecording>,
}

#[cfg(feature = "live-ssh")]
struct RecordingStartGuard<'a> {
    holder: &'a std::sync::Mutex<Option<RecordingOwner>>,
    loss: std::sync::Arc<std::sync::atomic::AtomicBool>,
    armed: bool,
}

#[cfg(feature = "live-ssh")]
impl Drop for RecordingStartGuard<'_> {
    fn drop(&mut self) {
        use std::sync::atomic::Ordering;

        if !self.armed {
            return;
        }
        self.loss.store(true, Ordering::Release);
        let recording = {
            let mut slot = match self.holder.lock() {
                Ok(slot) => slot,
                Err(poisoned) => {
                    eprintln!("fesTerm: recording lifecycle state was poisoned");
                    poisoned.into_inner()
                }
            };
            match slot.as_ref() {
                Some(owner) if std::sync::Arc::ptr_eq(&owner.loss, &self.loss) => {
                    if let Some(recording) = &owner.recording {
                        Some(recording.clone())
                    } else {
                        *slot = None;
                        None
                    }
                }
                _ => None,
            }
        };
        if let Some(recording) = recording {
            if let Err(error) = recording.stop(festerm_recording::FinishReason::Lifecycle) {
                eprintln!("fesTerm: recording could not be stopped after start failed: {error}");
            }
        }
    }
}

#[cfg(feature = "live-ssh")]
const SUSPENDED: &str =
    "Suspended. Reconnect to continue; a plain shell does not survive, tmux/Screen sessions do.";

#[cfg(feature = "live-ssh")]
impl SuspensionHandle {
    fn stop_recording(&self) {
        Self::stop_shared_recording(&self.recording);
        Self::cancel_shared_export(&self.export);
    }

    fn stop_shared_recording(holder: &std::sync::Mutex<Option<RecordingOwner>>) {
        let recording = match holder.lock() {
            Ok(recording) => recording.as_ref().and_then(|owner| {
                owner.loss.store(true, std::sync::atomic::Ordering::Release);
                owner.recording.clone()
            }),
            Err(poisoned) => {
                eprintln!("fesTerm: recording lifecycle state was poisoned");
                poisoned.into_inner().as_ref().and_then(|owner| {
                    owner.loss.store(true, std::sync::atomic::Ordering::Release);
                    owner.recording.clone()
                })
            }
        };
        if let Some(recording) = recording {
            if let Err(error) = recording.stop(festerm_recording::FinishReason::Lifecycle) {
                eprintln!("fesTerm: recording could not be stopped on suspension: {error}");
            }
        }
    }

    fn content_lock_callback(&self) -> impl Fn() + Send + Sync + 'static {
        let recording = self.recording.clone();
        let export = self.export.clone();
        move || {
            Self::stop_shared_recording(&recording);
            Self::cancel_shared_export(&export);
        }
    }

    fn cancel_shared_export(
        holder: &std::sync::Mutex<
            Option<std::sync::Weak<festerm_recording_export::ExportCancellation>>,
        >,
    ) {
        let cancellation = match holder.lock() {
            Ok(slot) => slot.as_ref().and_then(std::sync::Weak::upgrade),
            Err(poisoned) => {
                eprintln!("fesTerm: recording export lifecycle state was poisoned");
                poisoned
                    .into_inner()
                    .as_ref()
                    .and_then(std::sync::Weak::upgrade)
            }
        };
        if let Some(cancellation) = cancellation {
            if !cancellation.cancel() {
                eprintln!("fesTerm: recording export retention had already been committed");
            }
        }
    }

    #[cfg(any(test, target_os = "android", target_os = "ios"))]
    fn foreground_export_cancellation(
        &self,
        privacy_generation: impl Fn() -> Result<u64, String>,
    ) -> Result<std::sync::Arc<festerm_recording_export::ExportCancellation>, String> {
        let generation = privacy_generation()?;
        let cancellation =
            std::sync::Arc::new(festerm_recording_export::ExportCancellation::default());
        {
            let mut slot = self
                .export
                .lock()
                .map_err(|_| "Recording export lifecycle state is unavailable".to_owned())?;
            if slot.as_ref().and_then(std::sync::Weak::upgrade).is_some() {
                return Err("A recording export is already active".to_owned());
            }
            *slot = Some(std::sync::Arc::downgrade(&cancellation));
        }
        match privacy_generation() {
            Ok(current)
                if current == generation
                    && !cancellation
                        .flag()
                        .load(std::sync::atomic::Ordering::Acquire) =>
            {
                Ok(cancellation)
            }
            Ok(_) => {
                cancellation.cancel();
                Err("Recording export requires an active, unlocked app".to_owned())
            }
            Err(error) => {
                cancellation.cancel();
                Err(error)
            }
        }
    }

    fn start_recording(
        &self,
        directory: &std::path::Path,
        terminals: &[&Terminal],
        privacy_generation: impl Fn() -> Result<u64, String>,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Result<
        (
            festerm_recording::SharedRecording,
            Vec<festerm_recording::RecordingStream>,
        ),
        String,
    > {
        self.start_recording_using(None, privacy_generation, |loss| {
            festerm_recording::SharedRecording::start_with_privacy_loss_flag(
                directory, terminals, loss, wake,
            )
        })
    }

    #[cfg(any(target_os = "android", target_os = "ios"))]
    fn start_prepared_recording(
        &self,
        prepared: festerm_recording::PreparedRecording,
        terminals: &[&Terminal],
        expected_generation: u64,
        privacy_generation: impl Fn() -> Result<u64, String>,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Result<
        (
            festerm_recording::SharedRecording,
            Vec<festerm_recording::RecordingStream>,
        ),
        String,
    > {
        self.start_recording_using(Some(expected_generation), privacy_generation, |loss| {
            festerm_recording::SharedRecording::start_prepared_with_privacy_loss_flag(
                prepared, terminals, loss, wake,
            )
        })
    }

    fn start_recording_using(
        &self,
        expected_generation: Option<u64>,
        privacy_generation: impl Fn() -> Result<u64, String>,
        start: impl FnOnce(
            std::sync::Arc<std::sync::atomic::AtomicBool>,
        ) -> Result<
            (
                festerm_recording::SharedRecording,
                Vec<festerm_recording::RecordingStream>,
            ),
            festerm_recording::CaptureError,
        >,
    ) -> Result<
        (
            festerm_recording::SharedRecording,
            Vec<festerm_recording::RecordingStream>,
        ),
        String,
    > {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        let generation = privacy_generation()?;
        if expected_generation.is_some_and(|expected| expected != generation) {
            return Err("Recording stopped because privacy or app activity changed".to_owned());
        }
        let loss = Arc::new(AtomicBool::new(false));
        {
            let mut slot = self
                .recording
                .lock()
                .map_err(|_| "Recording lifecycle state is unavailable".to_owned())?;
            if slot.is_some() {
                return Err("A recording is already starting or finishing".to_owned());
            }
            *slot = Some(RecordingOwner {
                loss: loss.clone(),
                recording: None,
            });
        }
        let mut start_guard = RecordingStartGuard {
            holder: &self.recording,
            loss: loss.clone(),
            armed: true,
        };
        let check_privacy = || {
            let current = privacy_generation()?;
            if current != generation || loss.load(Ordering::Acquire) {
                Err("Recording stopped because privacy or app activity changed".to_owned())
            } else {
                Ok(())
            }
        };
        if let Err(error) = check_privacy() {
            self.clear_pending_recording(&loss)?;
            return Err(error);
        }
        let (recording, streams) = match start(loss.clone()) {
            Ok(started) => started,
            Err(error) => {
                self.clear_pending_recording(&loss)?;
                return Err(error.to_string());
            }
        };
        {
            let mut slot = match self.recording.lock() {
                Ok(slot) => slot,
                Err(poisoned) => {
                    let mut slot = poisoned.into_inner();
                    if let Some(owner) = slot.as_mut() {
                        owner.loss.store(true, Ordering::Release);
                        owner.recording = Some(recording.clone());
                    }
                    drop(slot);
                    recording
                        .stop(festerm_recording::FinishReason::Lifecycle)
                        .map_err(|error| error.to_string())?;
                    return Err("Recording lifecycle state was poisoned".to_owned());
                }
            };
            let Some(owner) = slot
                .as_mut()
                .filter(|owner| Arc::ptr_eq(&owner.loss, &loss))
            else {
                loss.store(true, Ordering::Release);
                drop(slot);
                recording
                    .stop(festerm_recording::FinishReason::Lifecycle)
                    .map_err(|error| error.to_string())?;
                return Err("Recording lifecycle state changed unexpectedly".to_owned());
            };
            owner.recording = Some(recording.clone());
        }
        if let Err(error) = check_privacy() {
            recording
                .stop(festerm_recording::FinishReason::Lifecycle)
                .map_err(|stop| format!("{error}; could not stop recording: {stop}"))?;
            return Err(error);
        }
        if !recording
            .is_recording()
            .map_err(|error| error.to_string())?
        {
            return Err("Recording stopped while it was starting".to_owned());
        }
        start_guard.armed = false;
        Ok((recording, streams))
    }

    fn clear_pending_recording(
        &self,
        loss: &std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<(), String> {
        let mut slot = self
            .recording
            .lock()
            .map_err(|_| "Recording lifecycle state is unavailable".to_owned())?;
        if slot.as_ref().is_some_and(|owner| {
            std::sync::Arc::ptr_eq(&owner.loss, loss) && owner.recording.is_none()
        }) {
            *slot = None;
            Ok(())
        } else {
            Err("Recording lifecycle state changed unexpectedly".to_owned())
        }
    }

    fn current_recording(&self) -> Result<Option<festerm_recording::SharedRecording>, String> {
        Ok(self
            .recording
            .lock()
            .map_err(|_| "Recording lifecycle state is unavailable".to_owned())?
            .as_ref()
            .and_then(|owner| owner.recording.clone()))
    }

    fn poll_recording_completion(&self) -> Result<Option<festerm_recording::Completion>, String> {
        let Some(recording) = self.current_recording()? else {
            return Ok(None);
        };
        let Some(completion) = recording
            .poll_completion()
            .map_err(|error| error.to_string())?
        else {
            return Ok(None);
        };
        let mut slot = self
            .recording
            .lock()
            .map_err(|_| "Recording lifecycle state is unavailable".to_owned())?;
        if slot
            .as_ref()
            .and_then(|owner| owner.recording.as_ref())
            .is_none_or(|current| current.id() != recording.id())
        {
            return Err("Recording lifecycle state changed unexpectedly".to_owned());
        }
        *slot = None;
        Ok(Some(completion))
    }

    #[cfg(test)]
    fn install_test_recording(&self, recording: festerm_recording::SharedRecording) {
        *self.recording.lock().unwrap() = Some(RecordingOwner {
            loss: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            recording: Some(recording),
        });
    }

    pub fn suspend(&self) {
        self.stop_recording();
        if let Some(live) = self.connection.borrow_mut().as_mut() {
            live.suspend();
        }
        for (_, live) in self.background.borrow_mut().iter_mut() {
            live.suspend();
        }
    }
}

/// Applies native lifecycle callbacks to the shared [`Lifecycle`] and live sessions.
///
/// winit reports `applicationWillResignActive` as `Suspended`. A dictation
/// permission or owner-authentication alert only resigns active, so that
/// suspension is deferred. If the app then really enters the
/// background, the deferred suspension is applied as before.
pub struct NativeLifecycle {
    lifecycle: Rc<Cell<Lifecycle>>,
    #[cfg(feature = "live-ssh")]
    suspension: SuspensionHandle,
    deferred: bool,
    app_lock: Option<festerm_ios_lock::AppLock>,
}

impl NativeLifecycle {
    pub fn new(
        lifecycle: Rc<Cell<Lifecycle>>,
        #[cfg(feature = "live-ssh")] suspension: SuspensionHandle,
    ) -> Self {
        Self {
            lifecycle,
            #[cfg(feature = "live-ssh")]
            suspension,
            deferred: false,
            app_lock: None,
        }
    }

    pub fn with_app_lock(mut self, lock: festerm_ios_lock::AppLock) -> Self {
        self.app_lock = Some(lock);
        self
    }

    pub fn get(&self) -> Lifecycle {
        self.lifecycle.get()
    }

    fn update(&self, update: impl FnOnce(&mut Lifecycle)) {
        let mut state = self.lifecycle.get();
        update(&mut state);
        self.lifecycle.set(state);
    }

    /// Returns whether the renderer should also be resumed.
    pub fn resumed(&mut self) -> bool {
        if std::mem::take(&mut self.deferred) {
            return false;
        }
        self.update(Lifecycle::resume);
        true
    }

    /// Returns whether the app was suspended (false when deferred for a
    /// system permission/authentication alert).
    pub fn suspended(&mut self, permission_prompt: bool) -> bool {
        if permission_prompt
            || self
                .app_lock
                .as_ref()
                .is_some_and(festerm_ios_lock::AppLock::authentication_prompt_in_flight)
        {
            self.deferred = true;
            return false;
        }
        self.deferred = false;
        self.suspend();
        true
    }

    /// The app entered the background. Returns whether a deferred suspension
    /// was applied now.
    pub fn entered_background(&mut self) -> bool {
        if !std::mem::take(&mut self.deferred) {
            return false;
        }
        self.suspend();
        true
    }

    pub fn memory_warning(&self) {
        self.update(Lifecycle::memory_warning);
    }

    fn suspend(&self) {
        #[cfg(feature = "live-ssh")]
        self.suspension.suspend();
        self.update(Lifecycle::suspend);
    }
}

/// Deliberately discards bytes: the spike must not pretend to be a shell or
/// save what a tester types. Uses the renderer's existing input contract.
#[derive(Default)]
pub struct ProbeSink {
    diagnostics: InputSinkDiagnostics,
    #[cfg(feature = "live-ssh")]
    live: SuspensionHandle,
}

impl EncodedInputSink for ProbeSink {
    fn record_encoded_input(&mut self, bytes: &[u8]) {
        #[cfg(feature = "live-ssh")]
        if let Some(live) = self.live.connection.borrow_mut().as_mut() {
            live.send(bytes);
        }
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

    fn record_terminal_resize(&mut self, dimensions: Dimensions) {
        #[cfg(feature = "live-ssh")]
        if let Some(live) = self.live.connection.borrow_mut().as_mut() {
            live.resize(dimensions);
        }
        #[cfg(not(feature = "live-ssh"))]
        let _ = dimensions;
    }
}

pub fn fixture_terminal() -> Terminal {
    let mut terminal = Terminal::with_scrollback_limit(
        Dimensions::new(80, 24).expect("fixed valid dimensions"),
        SCROLLBACK_LIMIT,
    )
    .expect("bounded fixture grid");
    terminal.ingest(b"\x1b[1;36mfesTerm iOS rendering spike\x1b[0m\r\n");
    terminal.ingest(b"Offline fixture. No SSH connection or local shell.\r\n");
    for row in 1..=80 {
        terminal.ingest(
            format!(
                "\x1b[32m{row:02}\x1b[0m  ANSI  \u{00e9}  \u{03bb}  \u{4e2d}\u{6587}  \u{1f680}\r\n"
            )
            .as_bytes(),
        );
    }
    terminal.ingest(b"Input is counted and discarded, never executed.\r\n");
    terminal
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum Surface {
    #[default]
    Tabs,
    Terminal,
    Documents,
}

/// Content-free view of the live SSH worker for chrome decisions.
#[cfg(feature = "live-ssh")]
struct LiveView {
    can_reset: bool,
    input_enabled: bool,
    status: &'static str,
}

#[cfg(feature = "live-ssh")]
enum RecordingStart<'a> {
    Directory(&'a std::path::Path),
    #[cfg(any(target_os = "android", target_os = "ios"))]
    Prepared {
        source: festerm_recording::PreparedRecording,
        generation: u64,
    },
}

#[cfg(feature = "live-ssh")]
struct RecordingSessionSelection {
    tab: u64,
    session: std::sync::Weak<dyn festerm_session::Session>,
}

#[cfg(all(feature = "live-ssh", any(target_os = "android", target_os = "ios")))]
struct RecordingConsent {
    generation: u64,
    selected: Vec<RecordingSessionSelection>,
}

pub struct MobileApp {
    #[cfg(all(
        target_os = "ios",
        target_abi = "sim",
        feature = "native-document-smoke"
    ))]
    document_smoke: festerm_ios_window::DocumentSmoke,
    reported_first_ui: bool,
    terminal: Terminal,
    view: TerminalView,
    sink: ProbeSink,
    lifecycle: Rc<Cell<Lifecycle>>,
    observed_memory_warnings: u64,
    observed_suspensions: u64,
    keyboard: Option<festerm_ios_window::KeyboardBridge>,
    keys: HardwareKeys,
    control: bool,
    alt: bool,
    shift: bool,
    input_notice: Option<&'static str>,
    toolbar_rect: Option<egui::Rect>,
    #[cfg(test)]
    control_rect: Option<egui::Rect>,
    menu_open: bool,
    /// Tab whose chip was long-pressed, awaiting close confirmation.
    confirm_close: Option<u64>,
    tab_menu: Option<u64>,
    gestures: terminal_gesture::TerminalGesture,
    workspace: workspaces::Workspace,
    files: Option<Box<files::FilesSession>>,
    files_ctx: files_shell::FilesContext,
    markdown: Option<workspaces::MarkdownWorkspace>,
    documents: documents::DocumentHost,
    surface: Surface,
    shell: shell::ShellState,
    profiles: Option<profiles::Profiles>,
    #[cfg(all(feature = "live-ssh", unix))]
    recording_storage: Option<recording_storage::RecordingStorage>,
    #[cfg(all(feature = "live-ssh", any(target_os = "android", target_os = "ios")))]
    recording_ui: recording_ui::RecordingUi,
    #[cfg(all(feature = "live-ssh", target_os = "ios"))]
    recording_folder_picker: Option<festerm_ios_window::RecordingFolderPicker>,
    lock: Option<app_lock::LockHost>,
    text_size: shell::TextSize,
    default_keyboard: terminal_keyboard::KeyboardKind,
    keyboard_kind: terminal_keyboard::KeyboardKind,
    /// Where the switch key goes from the built-in keyboard.
    last_custom_keyboard: terminal_keyboard::KeyboardKind,
    terminal_keyboard: terminal_keyboard::TerminalKeyboard,
    terminal_keyboard_input: bool,
    dictation: Option<festerm_ios_window::Dictation>,
    dictation_send_pending: bool,
    dictation_tab: Option<u64>,
    dictation_target: Option<dictation::Destination>,
    dictation_owns_frame: bool,
    dictation_frame: Option<u64>,
    dictation_context: Option<egui::Context>,
    pending_local_dictation: Option<egui::Id>,
    dictation_source_handoff: bool,
    #[cfg(feature = "live-ssh")]
    tunnel: tunnel_link::TunnelUi,
    caps: model::Capabilities,
    tabs: Vec<tabs::SessionTab>,
    active_tab: Option<u64>,
    next_tab: u64,
    /// One-line outcome of the last action, shown on the Profiles screen.
    notice: Option<String>,
    editor: forms::EditorState,
    host_setup: host_setup::HostSetupState,
    keys_panel: terminal_chrome::KeysPanel,
    keyboard_visible: bool,
    /// The focused form field's last IME request, kept while the field is
    /// clipped (e.g. under the keyboard) so UIKit does not dismiss it.
    field_ime: Option<(egui::Id, egui::output::IMEOutput)>,
    /// Gallery/test fixtures; production derives these from live state.
    sessions_override: Option<model::StaticSessions>,
    chrome_override: Option<model::ChromeModel>,
    #[cfg(test)]
    rows_override: Option<Vec<model::Profile>>,
    #[cfg(feature = "live-ssh")]
    polled_frame: Option<u64>,
}

fn mobile_terminal_view() -> TerminalView {
    let mut view = TerminalView::default();
    // Phone-density default (about 46 columns on a 390pt screen); the Settings
    // text size and pinch-zoom move it within the view's bounds.
    view.zoom_by_factor(shell::TextSize::Normal.points() / view.font_size_points());
    view
}

impl MobileApp {
    pub fn new(lifecycle: Rc<Cell<Lifecycle>>) -> Self {
        Self {
            #[cfg(all(
                target_os = "ios",
                target_abi = "sim",
                feature = "native-document-smoke"
            ))]
            document_smoke: festerm_ios_window::DocumentSmoke::default(),
            reported_first_ui: false,
            terminal: fixture_terminal(),
            view: mobile_terminal_view(),
            sink: ProbeSink::default(),
            lifecycle,
            observed_memory_warnings: 0,
            observed_suspensions: 0,
            keyboard: None,
            keys: HardwareKeys::default(),
            control: false,
            alt: false,
            shift: false,
            input_notice: None,
            toolbar_rect: None,
            #[cfg(test)]
            control_rect: None,
            menu_open: false,
            confirm_close: None,
            tab_menu: None,
            gestures: terminal_gesture::TerminalGesture::default(),
            workspace: workspaces::Workspace::Terminal,
            files: None,
            files_ctx: files_shell::FilesContext::default(),
            markdown: None,
            documents: documents::DocumentHost::default(),
            #[cfg(feature = "live-ssh")]
            tunnel: tunnel_link::TunnelUi::default(),
            surface: Surface::Tabs,
            shell: shell::ShellState::default(),
            profiles: None,
            #[cfg(all(feature = "live-ssh", unix))]
            recording_storage: None,
            #[cfg(all(feature = "live-ssh", any(target_os = "android", target_os = "ios")))]
            recording_ui: recording_ui::RecordingUi::default(),
            #[cfg(all(feature = "live-ssh", target_os = "ios"))]
            recording_folder_picker: None,
            lock: None,
            text_size: shell::TextSize::default(),
            default_keyboard: terminal_keyboard::KeyboardKind::Native,
            keyboard_kind: terminal_keyboard::KeyboardKind::Native,
            last_custom_keyboard: terminal_keyboard::KeyboardKind::HoldSymbols,
            terminal_keyboard: terminal_keyboard::TerminalKeyboard::default(),
            terminal_keyboard_input: false,
            dictation: None,
            dictation_send_pending: false,
            dictation_tab: None,
            dictation_target: None,
            dictation_owns_frame: false,
            dictation_frame: None,
            dictation_context: None,
            pending_local_dictation: None,
            dictation_source_handoff: false,
            caps: model::Capabilities::production(),
            tabs: Vec::new(),
            active_tab: None,
            next_tab: 1,
            notice: None,
            editor: forms::EditorState::default(),
            host_setup: host_setup::HostSetupState::default(),
            keys_panel: terminal_chrome::KeysPanel::default(),
            keyboard_visible: true,
            field_ime: None,
            sessions_override: None,
            chrome_override: None,
            #[cfg(test)]
            rows_override: None,
            #[cfg(feature = "live-ssh")]
            polled_frame: None,
        }
    }

    /// Starts on the terminal surface, as the pre-shell spike did.
    #[cfg(test)]
    fn new_terminal(lifecycle: Rc<Cell<Lifecycle>>) -> Self {
        let mut app = Self::new(lifecycle);
        app.surface = Surface::Terminal;
        app.open_local_tab();
        app
    }

    /// Plugs in the saved-profile store (Keychain-backed in production).
    pub fn with_profiles(mut self, profiles: profiles::Profiles) -> Self {
        let text_size = profiles.text_size();
        match profiles.default_keyboard() {
            Ok(kind) => {
                self.default_keyboard = kind;
                self.keyboard_kind = kind;
                if kind != terminal_keyboard::KeyboardKind::Native {
                    self.last_custom_keyboard = kind;
                }
            }
            Err(error) => self.notice = Some(error),
        }
        self.terminal_keyboard.split = profiles.split_keyboard();
        self.profiles = Some(profiles);
        self.apply_text_size(text_size);
        self
    }

    #[cfg(all(feature = "live-ssh", any(target_os = "ios", target_os = "android")))]
    fn with_recording_storage(mut self) -> Self {
        match recording_storage::RecordingStorage::open_default() {
            Ok((storage, settings)) => {
                self.shell.recording_settings = Some(settings.clone());
                self.shell.recording_saved = Some(settings);
                self.recording_storage = Some(storage);
            }
            Err(error) => self.shell.recording_error = Some(error),
        }
        self
    }

    /// Sets every terminal, open or parked, to the preset size.
    fn apply_text_size(&mut self, size: shell::TextSize) {
        self.text_size = size;
        let points = size.points();
        self.view
            .zoom_by_factor(points / self.view.font_size_points());
        for view in self.parked_views() {
            view.zoom_by_factor(points / view.font_size_points());
        }
    }

    /// Plugs in the app lock; content is drawn only while it is unlocked.
    pub fn with_lock(
        mut self,
        lock: festerm_ios_lock::AppLock,
        bridge: Option<festerm_ios_lock::LockBridge>,
    ) -> Self {
        self.lock = Some(app_lock::LockHost::new(lock, bridge));
        self
    }

    pub fn show(&mut self, ui: &mut egui::Ui) {
        let occlusion = if self.surface == Surface::Terminal
            && self.workspace != workspaces::Workspace::Terminal
            && !self.editor.open
        {
            0.0
        } else {
            self.keyboard
                .as_ref()
                .map_or(0.0, |keyboard| keyboard.occluded_height_fraction())
        };
        self.show_with_occlusion(ui, occlusion);
    }

    /// Queue shared with the native event loop (see [`HardwareKeys`]).
    pub fn with_hardware_keys(mut self, keys: HardwareKeys) -> Self {
        self.keys = keys;
        self
    }

    pub fn with_keyboard(mut self, keyboard: Option<festerm_ios_window::KeyboardBridge>) -> Self {
        self.documents.bind_picker(
            keyboard
                .as_ref()
                .and_then(|bridge| bridge.document_picker()),
        );
        #[cfg(all(feature = "live-ssh", target_os = "ios"))]
        {
            self.recording_folder_picker = keyboard
                .as_ref()
                .and_then(|bridge| bridge.recording_folder_picker());
        }
        self.keyboard = keyboard;
        self
    }

    #[cfg(feature = "live-ssh")]
    pub fn with_suspension(mut self, suspension: SuspensionHandle) -> Self {
        self.sink.live = suspension;
        self
    }

    /// Starts one shared recording for the explicitly selected live terminal
    /// tabs. The caller must establish private, backup-excluded storage first.
    #[cfg(feature = "live-ssh")]
    pub fn start_terminal_recording(
        &mut self,
        directory: &std::path::Path,
        selected: &[u64],
        context: &egui::Context,
    ) -> Result<(), String> {
        let (_, selected) = self.recording_terminals(selected)?;
        self.start_terminal_recording_from(RecordingStart::Directory(directory), &selected, context)
    }

    #[cfg(all(feature = "live-ssh", any(target_os = "android", target_os = "ios")))]
    fn start_prepared_terminal_recording(
        &mut self,
        prepared: festerm_recording::PreparedRecording,
        consent: &RecordingConsent,
        context: &egui::Context,
    ) -> Result<(), String> {
        self.start_terminal_recording_from(
            RecordingStart::Prepared {
                source: prepared,
                generation: consent.generation,
            },
            &consent.selected,
            context,
        )
    }

    #[cfg(feature = "live-ssh")]
    fn recording_terminals(
        &self,
        selected: &[u64],
    ) -> Result<(Vec<&Terminal>, Vec<RecordingSessionSelection>), String> {
        use std::collections::HashSet;

        if selected.is_empty() || selected.len() > festerm_recording::MAX_STREAMS {
            return Err("Select between one and sixteen terminal sessions".to_owned());
        }
        let mut unique = HashSet::new();
        let active = self.sink.live.connection.borrow();
        let parked = self.sink.live.background.borrow();
        let mut terminals = Vec::with_capacity(selected.len());
        let mut identities = Vec::with_capacity(selected.len());
        for id in selected {
            if !unique.insert(*id) {
                return Err("A terminal session was selected more than once".to_owned());
            }
            let tab = self
                .tabs
                .iter()
                .find(|tab| tab.id == *id)
                .ok_or_else(|| "A selected terminal session is no longer open".to_owned())?;
            let (terminal, view, live) = if self.active_tab == Some(*id) {
                (&self.terminal, &self.view, active.as_ref())
            } else {
                let (terminal, view) = tab
                    .parked
                    .as_ref()
                    .ok_or_else(|| "A selected terminal session is unavailable".to_owned())?;
                let live = parked
                    .iter()
                    .find(|(parked_id, _)| parked_id == id)
                    .map(|(_, live)| live);
                (terminal, view, live)
            };
            let session = live
                .and_then(live_ssh::LiveSsh::input_destination)
                .ok_or_else(|| "A selected terminal session is not live".to_owned())?;
            view.validate_recording_view(terminal)
                .map_err(str::to_owned)?;
            terminals.push(terminal);
            identities.push(RecordingSessionSelection { tab: *id, session });
        }
        Ok((terminals, identities))
    }

    #[cfg(feature = "live-ssh")]
    fn validate_recording_selection(
        &self,
        selected: &[RecordingSessionSelection],
    ) -> Result<Vec<&Terminal>, String> {
        let ids: Vec<_> = selected.iter().map(|selection| selection.tab).collect();
        let (terminals, current) = self.recording_terminals(&ids)?;
        if selected.iter().zip(&current).any(|(expected, current)| {
            !std::sync::Weak::ptr_eq(&expected.session, &current.session)
        }) {
            return Err("A selected terminal session changed; select it again".to_owned());
        }
        Ok(terminals)
    }

    #[cfg(all(feature = "live-ssh", any(target_os = "android", target_os = "ios")))]
    fn validate_recording_consent(&self, consent: &RecordingConsent) -> Result<(), String> {
        if self.recording_privacy_generation()? != consent.generation {
            return Err("Recording stopped because privacy or app activity changed".to_owned());
        }
        self.validate_recording_selection(&consent.selected)?;
        Ok(())
    }

    #[cfg(feature = "live-ssh")]
    fn start_terminal_recording_from(
        &mut self,
        source: RecordingStart<'_>,
        selected: &[RecordingSessionSelection],
        context: &egui::Context,
    ) -> Result<(), String> {
        let terminals = self.validate_recording_selection(selected)?;
        let context = context.clone();
        #[cfg(any(target_os = "android", target_os = "ios"))]
        let recording_lease = self.recording_ui.lease.clone();
        let wake = move || {
            #[cfg(any(target_os = "android", target_os = "ios"))]
            let _lease = &recording_lease;
            context.request_repaint();
        };
        let (recording, streams) = match source {
            RecordingStart::Directory(directory) => self.sink.live.start_recording(
                directory,
                &terminals,
                || self.recording_privacy_generation(),
                wake,
            ),
            #[cfg(any(target_os = "android", target_os = "ios"))]
            RecordingStart::Prepared {
                source: prepared,
                generation,
            } => self.sink.live.start_prepared_recording(
                prepared,
                &terminals,
                generation,
                || self.recording_privacy_generation(),
                wake,
            ),
        }?;
        drop(terminals);
        for (selection, stream) in selected.iter().zip(streams) {
            let attached = if self.active_tab == Some(selection.tab) {
                if let Some(live) = self.sink.live.connection.borrow_mut().as_mut() {
                    live.recording_stream = Some(stream);
                    true
                } else {
                    false
                }
            } else if let Some((_, live)) = self
                .sink
                .live
                .background
                .borrow_mut()
                .iter_mut()
                .find(|(parked_id, _)| *parked_id == selection.tab)
            {
                live.recording_stream = Some(stream);
                true
            } else {
                false
            };
            if !attached {
                recording
                    .fail_integrity("A selected terminal session changed during recording start")
                    .map_err(|error| error.to_string())?;
                return Err("A selected terminal session changed during recording start".to_owned());
            }
        }
        Ok(())
    }

    /// Explicitly includes an additional live terminal in the shared timeline.
    #[cfg(feature = "live-ssh")]
    pub fn include_terminal_in_recording(&mut self, id: u64) -> Result<(), String> {
        let recording = self
            .sink
            .live
            .current_recording()?
            .ok_or_else(|| "No recording is active".to_owned())?;
        let generation = match self.recording_privacy_generation() {
            Ok(generation) => generation,
            Err(error) => {
                recording
                    .stop(festerm_recording::FinishReason::Lifecycle)
                    .map_err(|stop| format!("{error}; could not stop recording: {stop}"))?;
                return Err(error);
            }
        };
        let stream = {
            let active = self.sink.live.connection.borrow();
            let parked = self.sink.live.background.borrow();
            let tab = self
                .tabs
                .iter()
                .find(|tab| tab.id == id)
                .ok_or_else(|| "The selected terminal session is no longer open".to_owned())?;
            let (terminal, view, live) = if self.active_tab == Some(id) {
                (
                    &self.terminal,
                    &self.view,
                    active
                        .as_ref()
                        .ok_or_else(|| "The selected terminal session is not live".to_owned())?,
                )
            } else {
                (
                    &tab.parked
                        .as_ref()
                        .ok_or_else(|| "The selected terminal session is unavailable".to_owned())?
                        .0,
                    &tab.parked
                        .as_ref()
                        .ok_or_else(|| "The selected terminal session is unavailable".to_owned())?
                        .1,
                    &parked
                        .iter()
                        .find(|(parked_id, _)| *parked_id == id)
                        .ok_or_else(|| "The selected terminal session is not live".to_owned())?
                        .1,
                )
            };
            if live.can_reset() {
                return Err("The selected terminal session is not live".to_owned());
            }
            view.validate_recording_view(terminal)
                .map_err(str::to_owned)?;
            if live
                .recording_stream
                .as_ref()
                .is_some_and(|stream| stream.recording_id() == recording.id())
            {
                return Err("The terminal session is already included".to_owned());
            }
            recording
                .enroll(terminal)
                .map_err(|error| error.to_string())?
        };
        let privacy_error = match self.recording_privacy_generation() {
            Ok(current) if current == generation => None,
            Ok(_) => Some("Recording stopped because privacy or app activity changed".to_owned()),
            Err(error) => Some(error),
        };
        if let Some(error) = privacy_error {
            recording
                .stop(festerm_recording::FinishReason::Lifecycle)
                .map_err(|stop| format!("{error}; could not stop recording: {stop}"))?;
            return Err(error);
        }
        if self.active_tab == Some(id) {
            if let Some(live) = self.sink.live.connection.borrow_mut().as_mut() {
                live.recording_stream = Some(stream);
                return Ok(());
            }
        } else if let Some((_, live)) = self
            .sink
            .live
            .background
            .borrow_mut()
            .iter_mut()
            .find(|(parked_id, _)| *parked_id == id)
        {
            live.recording_stream = Some(stream);
            return Ok(());
        }
        recording
            .fail_integrity("A terminal session changed during recording enrollment")
            .map_err(|error| error.to_string())?;
        Err("A terminal session changed during recording enrollment".to_owned())
    }

    #[cfg(feature = "live-ssh")]
    fn recording_privacy_generation(&self) -> Result<u64, String> {
        if !self.lifecycle.get().active {
            return Err("Recording requires an active, unlocked app".to_owned());
        }
        self.lock
            .as_ref()
            .ok_or_else(|| "Recording requires the native privacy monitor".to_owned())?
            .recording_privacy_generation()
    }

    #[cfg(all(feature = "live-ssh", any(target_os = "android", target_os = "ios")))]
    fn foreground_export_cancellation(
        &self,
    ) -> Result<std::sync::Arc<festerm_recording_export::ExportCancellation>, String> {
        self.sink
            .live
            .foreground_export_cancellation(|| self.recording_privacy_generation())
    }

    #[cfg(feature = "live-ssh")]
    pub fn stop_terminal_recording(&self) -> Result<bool, String> {
        let Some(recording) = self.sink.live.current_recording()? else {
            return Ok(false);
        };
        recording
            .stop(festerm_recording::FinishReason::User)
            .map_err(|error| error.to_string())
    }

    /// The owner must collect completion after every wake, including when a
    /// privacy change interrupts startup before it can return success.
    #[cfg(feature = "live-ssh")]
    pub fn poll_terminal_recording(&self) -> Result<Option<festerm_recording::Completion>, String> {
        self.sink.live.poll_recording_completion()
    }

    fn terminal_surface(&self) -> bool {
        self.surface == Surface::Terminal
            && self.workspace == workspaces::Workspace::Terminal
            && !self.editor.open
    }

    fn show_with_occlusion(&mut self, ui: &mut egui::Ui, occlusion: f32) {
        #[cfg(all(feature = "live-ssh", unix))]
        self.poll_recording_settings();
        #[cfg(all(feature = "live-ssh", any(target_os = "android", target_os = "ios")))]
        self.poll_recording_folder();
        #[cfg(all(feature = "live-ssh", any(target_os = "android", target_os = "ios")))]
        self.poll_recording_ui(ui.ctx());
        #[cfg(all(
            target_os = "ios",
            target_abi = "sim",
            feature = "native-document-smoke"
        ))]
        {
            let context = ui.ctx().clone();
            self.document_smoke
                .poll(std::sync::Arc::new(move || context.request_repaint()));
        }
        self.terminal_keyboard_input = false;
        let frame = ui.ctx().cumulative_frame_nr();
        if self.dictation_frame != Some(frame) {
            self.dictation_frame = Some(frame);
            self.dictation_owns_frame = self.dictation_open();
            self.dictation_source_handoff = false;
        } else {
            self.dictation_owns_frame |= self.dictation_open();
        }
        // Accessory taps must not blur the terminal and restart UIKit's keyboard.
        let terminal_surface = self.terminal_surface();
        ui.ctx().options_mut(|options| {
            options.input_options.surrender_focus_on =
                if terminal_surface || self.source_input() || self.dictation_open() {
                    egui::SurrenderFocusOn::Never
                } else {
                    egui::SurrenderFocusOn::Clicks
                };
        });
        let mut safe_rect = ui.ctx().content_rect().intersect(ui.max_rect());
        let viewport = ui.ctx().viewport_rect();
        safe_rect.max.y = safe_rect
            .max
            .y
            .min(viewport.bottom() - viewport.height() * occlusion.clamp(0.0, 1.0))
            .max(safe_rect.min.y);
        ui.scope_builder(egui::UiBuilder::new().max_rect(safe_rect), |ui| {
            ui.set_clip_rect(safe_rect);
            ui.set_style(mobile_style(ui.style()));
            ui.painter()
                .rect_filled(safe_rect, 0.0, ui.visuals().panel_fill);
            if let Some(keyboard) = &self.keyboard {
                let inputs = keyboard.take_field_input();
                for input in &inputs {
                    if let festerm_ios_window::FieldInput::Unavailable(error) = input {
                        self.input_notice = Some(error);
                        #[cfg(feature = "live-ssh")]
                        self.sink.live.suspend();
                    }
                }
                if !self.dictation_open()
                    || self.dictation.as_ref().is_some_and(|speech| {
                        speech.phase() == festerm_ios_window::DictationPhase::Review
                    })
                {
                    apply_field_input(ui.ctx(), inputs);
                }
            }
            apply_field_edit(ui.ctx());
            self.show_content(ui);
        });
        let field = widgets::take_field_document(ui.ctx());
        self.keep_field_keyboard(ui.ctx());
        if !self.lifecycle.get().active || !ui.input(|input| input.focused) {
            ui.ctx().output_mut(|output| output.ime = None);
        }
        if let Some(keyboard) = &self.keyboard {
            let mut field = field;
            let mode = if self.terminal_keyboard_input {
                ui.ctx().output_mut(|output| output.ime = None);
                field = None;
                festerm_ios_window::KeyboardMode::Terminal
            } else if ui.ctx().output(|output| output.ime.is_some()) {
                if field.is_some() {
                    if (self.source_input() || self.dictation_open())
                        && self.keyboard_kind != terminal_keyboard::KeyboardKind::Native
                    {
                        festerm_ios_window::KeyboardMode::FieldCustom
                    } else if (self.source_input() || self.dictation_open())
                        && !self.keyboard_visible
                    {
                        field = None;
                        festerm_ios_window::KeyboardMode::Hidden
                    } else {
                        festerm_ios_window::KeyboardMode::Field
                    }
                } else {
                    festerm_ios_window::KeyboardMode::System
                }
            } else {
                field = None;
                festerm_ios_window::KeyboardMode::Hidden
            };
            if mode != festerm_ios_window::KeyboardMode::System {
                // winit's iOS IME request makes its own view first responder,
                // replacing the bridge's chosen responder with the system keyboard.
                ui.ctx().output_mut(|output| output.ime = None);
            }
            // Publish before switching, so the keyboard reads the new field.
            keyboard.set_field_document(field);
            if let Err(error) = keyboard.set_keyboard_mode(mode) {
                log::error!("{error}");
                self.input_notice = Some(error);
            }
        }
    }

    /// egui only requests the keyboard for a text field it painted. When the
    /// keyboard's own occlusion clips the focused field, that request would
    /// vanish, UIKit would hide the keyboard, the field would reappear, and
    /// the keyboard would flicker. Reuse the field's request while it keeps
    /// focus; the field scrolls itself back into view meanwhile.
    fn keep_field_keyboard(&mut self, ctx: &egui::Context) {
        if self.terminal_surface() {
            self.field_ime = None;
            return;
        }
        let focused = ctx.memory(|memory| memory.focused());
        ctx.output_mut(|output| match (focused, output.ime) {
            (Some(id), Some(ime)) => self.field_ime = Some((id, ime)),
            (Some(id), None) => match self.field_ime {
                Some((kept, ime)) if kept == id => {
                    output.ime = Some(egui::output::IMEOutput {
                        should_interrupt_composition: false,
                        ..ime
                    });
                }
                _ => self.field_ime = None,
            },
            (None, _) => self.field_ime = None,
        });
    }

    fn send_key(&mut self, key: Key) {
        self.send_key_with(key, Modifiers::NONE);
    }

    fn clear_sticky_modifiers(&mut self) {
        self.control = false;
        self.alt = false;
        self.shift = false;
        self.input_notice = None;
    }

    fn select_keyboard(&mut self, kind: terminal_keyboard::KeyboardKind) {
        if kind != terminal_keyboard::KeyboardKind::Native {
            self.last_custom_keyboard = kind;
        }
        self.keyboard_kind = kind;
        self.keyboard_visible = true;
        self.keys_panel.open = false;
        self.terminal_keyboard.reset();
        self.gestures.cancel();
        self.clear_sticky_modifiers();
    }

    /// A live pick (More list or globe key) is sticky: it also becomes the
    /// saved startup keyboard shown in Settings.
    fn choose_keyboard(&mut self, kind: terminal_keyboard::KeyboardKind) {
        self.select_keyboard(kind);
        if self.default_keyboard == kind {
            return;
        }
        match self
            .profiles
            .as_ref()
            .map(|p| p.save_default_keyboard(kind))
        {
            Some(Err(error)) => self.notice = Some(error),
            _ => self.default_keyboard = kind,
        }
    }

    /// Sticky modifiers apply to the next key whichever surface produced it.
    fn send_key_with(&mut self, key: Key, extra: Modifiers) {
        let mut modifiers = extra;
        if self.control {
            modifiers = modifiers.with(Modifiers::CONTROL);
        }
        if self.alt {
            modifiers = modifiers.with(Modifiers::ALT);
        }
        if self.shift {
            modifiers = modifiers.with(Modifiers::SHIFT);
        }
        let key = match key {
            Key::Character(character) if modifiers.contains(Modifiers::SHIFT) => {
                Key::Character(character.to_ascii_uppercase())
            }
            _ => key,
        };
        self.input_notice = None;
        let route = route_input(
            &mut self.terminal,
            InputEvent::ModifiedKey { key, modifiers },
            &mut self.sink,
        );
        // A latched modifier the key cannot carry (e.g. Shift+Return) must not
        // swallow the key: send it with only the explicit modifiers.
        if route.outcome == InputEventOutcome::Rejected && modifiers != extra {
            route_input(
                &mut self.terminal,
                InputEvent::ModifiedKey {
                    key,
                    modifiers: extra,
                },
                &mut self.sink,
            );
        }
        self.clear_sticky_modifiers();
    }

    fn consume_sticky_text(
        &mut self,
        events: &mut Vec<egui::Event>,
        mut clipboard_text: impl FnMut() -> Option<String>,
    ) {
        if !self.lifecycle.get().active {
            return;
        }
        let mut processed = Vec::with_capacity(events.len());
        for mut event in std::mem::take(events) {
            let hardware_shortcut = match &event {
                egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } if modifiers.ctrl
                    && (modifiers.shift || self.shift)
                    && !modifiers.alt
                    && !modifiers.mac_cmd
                    && !self.alt =>
                {
                    match key {
                        egui::Key::C => Some('c'),
                        egui::Key::V => Some('v'),
                        _ => None,
                    }
                }
                _ => None,
            };
            if let Some(character) = hardware_shortcut {
                self.queue_clipboard_shortcut(character, &mut processed, &mut clipboard_text);
                continue;
            }
            if self.control || self.alt || self.shift {
                if let egui::Event::Text(text) | egui::Event::Ime(egui::ImeEvent::Commit(text)) =
                    &mut event
                {
                    let committed = std::mem::take(text);
                    let mut characters = committed.chars();
                    if let Some(character) = characters.next() {
                        if self.control
                            && self.shift
                            && !self.alt
                            && matches!(character.to_ascii_lowercase(), 'c' | 'v')
                        {
                            // Finish IME composition before the local action, then
                            // let trailing committed text follow it in the same batch.
                            processed.push(event);
                            self.queue_clipboard_shortcut(
                                character,
                                &mut processed,
                                &mut clipboard_text,
                            );
                            let rest: String = characters.collect();
                            if !rest.is_empty() {
                                processed.push(egui::Event::Text(rest));
                            }
                            continue;
                        }
                    }
                    // One-shot latch on the first character; preserve the rest
                    // of a multi-character commit in order through the encoder.
                    for character in committed.chars() {
                        self.send_key(Key::Character(character));
                    }
                    // Preserve the empty commit event so the shared adapter
                    // finishes any preedit composition without duplicate text.
                }
            }
            processed.push(event);
        }
        *events = processed;
    }

    fn queue_clipboard_shortcut(
        &mut self,
        character: char,
        events: &mut Vec<egui::Event>,
        clipboard_text: &mut impl FnMut() -> Option<String>,
    ) {
        self.clear_sticky_modifiers();
        self.input_notice = None;
        if character.eq_ignore_ascii_case(&'c') {
            events.push(egui::Event::Copy);
        } else if let Some(text) = clipboard_text().filter(|text| !text.is_empty()) {
            events.push(egui::Event::Paste(text));
        } else {
            self.input_notice = Some("The clipboard has no text to paste");
        }
    }

    #[cfg(feature = "live-ssh")]
    fn live_view(&self) -> Option<LiveView> {
        self.sink
            .live
            .connection
            .borrow()
            .as_ref()
            .map(|live| LiveView {
                can_reset: live.can_reset(),
                input_enabled: live.input_enabled(),
                status: live.status(),
            })
    }

    fn chrome(&self) -> model::ChromeModel {
        use model::{ChromeModel, ConnectionLine, SurfaceState};
        if let Some(chrome) = &self.chrome_override {
            return chrome.clone();
        }
        if let Some(spec) = self.active_spec() {
            #[cfg_attr(not(feature = "live-ssh"), allow(unused_mut))]
            let mut state = SurfaceState::Disconnected;
            #[cfg_attr(not(feature = "live-ssh"), allow(unused_mut))]
            let mut label = "Not connected".to_owned();
            #[cfg(feature = "live-ssh")]
            if let Some(live) = self.live_view() {
                state = if live.can_reset {
                    SurfaceState::Disconnected
                } else if live.input_enabled {
                    SurfaceState::Connected
                } else {
                    SurfaceState::Connecting
                };
                label = live.status.to_owned();
            }
            return ChromeModel {
                title: spec.name.clone(),
                subtitle: spec.detail(),
                line: ConnectionLine {
                    state,
                    label,
                    context: spec.persistent_label(),
                },
            };
        }
        if let Some(spec) = self.active_tunnel() {
            #[cfg_attr(not(feature = "live-ssh"), allow(unused_mut))]
            let mut state = SurfaceState::Disconnected;
            #[cfg_attr(not(feature = "live-ssh"), allow(unused_mut))]
            let mut label = "Not connected".to_owned();
            #[cfg(feature = "live-ssh")]
            if let Some(live) = self.live_view() {
                state = if live.can_reset {
                    SurfaceState::Disconnected
                } else if live.input_enabled {
                    SurfaceState::Connected
                } else {
                    SurfaceState::Connecting
                };
                label = live.status.to_owned();
            }
            return ChromeModel {
                title: spec.name.clone(),
                subtitle: format!("Dev Tunnel · {}", spec.persistent_label()),
                line: ConnectionLine {
                    state,
                    label,
                    context: Some(spec.persistent_label()),
                },
            };
        }
        ChromeModel {
            title: "Offline demo".to_owned(),
            subtitle: "Local fixture · no shell".to_owned(),
            line: ConnectionLine {
                state: SurfaceState::Disconnected,
                label: "Offline demo · input is discarded".to_owned(),
                context: None,
            },
        }
    }

    fn surfaces(&self) -> Vec<model::SurfaceSummary> {
        if let Some(sessions) = &self.sessions_override {
            return model::SessionSource::surfaces(sessions);
        }
        let mut surfaces = self.tab_surfaces();
        #[cfg(all(feature = "live-ssh", any(target_os = "android", target_os = "ios")))]
        self.mark_recorded_surfaces(&mut surfaces);
        surfaces.extend(self.documents.surfaces(self.surface == Surface::Documents));
        surfaces
    }

    fn reattachable(&self) -> Vec<model::Reattachable> {
        if let Some(sessions) = &self.sessions_override {
            return model::SessionSource::reattachable(sessions);
        }
        self.reattach_targets()
            .into_iter()
            .map(|target| target.row)
            .collect()
    }

    fn diagnostics(&self) -> Vec<(String, String)> {
        let lifecycle = self.lifecycle.get();
        vec![
            ("Resumes".to_owned(), lifecycle.resumes.to_string()),
            ("Suspensions".to_owned(), lifecycle.suspensions.to_string()),
            (
                "Memory warnings".to_owned(),
                lifecycle.memory_warnings.to_string(),
            ),
            (
                "Input bytes".to_owned(),
                self.sink.diagnostics.byte_count.to_string(),
            ),
        ]
    }

    /// Retains tunnel sessions for a foreground host check. Plain SSH still stops.
    /// Lost tabs may recover once when next active.
    #[cfg(feature = "live-ssh")]
    fn suspend_sessions(&mut self) {
        self.sink.live.suspend();
        for tab in &mut self.tabs {
            tab.reconnect_armed = true;
        }
    }

    fn show_content(&mut self, ui: &mut egui::Ui) {
        let lifecycle = self.lifecycle.get();
        // Permission alerts never count as suspensions (see NativeLifecycle).
        if (self.dictation_open() && !self.dictation_destination_valid())
            || lifecycle.suspensions != self.observed_suspensions
        {
            self.cancel_dictation();
        }
        if (!self.terminal_surface() && !self.source_input() && !self.dictation_open())
            || !lifecycle.active
            || !ui.input(|input| input.focused)
        {
            self.terminal_keyboard.cancel();
        }
        if lifecycle.suspensions != self.observed_suspensions {
            self.documents.invalidate_sources();
            #[cfg(feature = "live-ssh")]
            self.suspend_sessions();
            self.gestures.cancel();
            self.terminal_keyboard.cancel();
            self.clear_sticky_modifiers();
            self.observed_suspensions = lifecycle.suspensions;
        }
        if lifecycle.memory_warnings != self.observed_memory_warnings {
            self.gestures.cancel();
            // Preserve grid/history and discard only reconstructible view caches.
            let font_size = self.view.font_size_points();
            self.view = mobile_terminal_view();
            self.view
                .zoom_by_factor(font_size / self.view.font_size_points());
            self.observed_memory_warnings = lifecycle.memory_warnings;
        }
        #[cfg(feature = "live-ssh")]
        if self
            .lock
            .as_ref()
            .is_some_and(|lock| !lock.content_available())
        {
            self.sink.live.stop_recording();
        }
        #[cfg(feature = "live-ssh")]
        if lifecycle.active && self.polled_frame != Some(ui.ctx().cumulative_frame_nr()) {
            self.polled_frame = Some(ui.ctx().cumulative_frame_nr());
            if let Some(live) = self.sink.live.connection.borrow_mut().as_mut() {
                live.poll(&mut self.terminal, ui.ctx());
            }
            self.persist_new_trust();
            self.poll_background(ui.ctx());
            self.poll_tunnels(ui.ctx());
            self.auto_reconnect_tunnel(ui.ctx());
        }
        #[cfg(feature = "live-ssh")]
        if lifecycle.active {
            self.poll_files(ui.ctx());
        }
        let bounds = ui.max_rect();
        if let Some(lock) = self.lock.as_mut() {
            let hidden = lock.gate(ui, lifecycle.active);
            lock.frame_presented(ui.ctx());
            if hidden {
                self.field_ime = None;
                ui.output_mut(|output| output.ime = None);
                ui.ctx().memory_mut(|memory| {
                    if let Some(id) = memory.focused() {
                        memory.surrender_focus(id);
                    }
                });
                self.cancel_dictation();
                self.gestures.cancel();
                self.terminal_keyboard.cancel();
                self.clear_sticky_modifiers();
                return;
            }
        }
        #[cfg(all(feature = "live-ssh", any(target_os = "android", target_os = "ios")))]
        self.show_recording_status(ui);
        if self.editor.open {
            self.show_editor(ui);
            #[cfg(feature = "live-ssh")]
            self.show_tunnel_sheets(ui.ctx(), bounds);
            self.show_host_setup(ui.ctx(), bounds);
            return;
        }
        if lifecycle.active && self.documents.poll(&mut self.next_tab) {
            self.park_active();
            self.surface = Surface::Documents;
        }
        self.show_local_input(ui);
        let content = ui.available_rect_before_wrap();
        ui.scope_builder(egui::UiBuilder::new().max_rect(content), |ui| {
            ui.set_clip_rect(content);
            match self.surface {
                Surface::Tabs => self.show_tabs(ui),
                Surface::Terminal => self.show_surface(ui, lifecycle),
                Surface::Documents => self.show_documents(ui),
            }
        });
        self.show_sheets(ui.ctx(), bounds);
        self.show_files_sheets(ui.ctx(), bounds);
        self.show_host_setup(ui.ctx(), bounds);
        self.documents
            .show_sheets(ui.ctx(), bounds, self.surface == Surface::Documents);
        self.finish_local_dictation_action(ui.ctx());
    }

    fn show_host_setup(&mut self, ctx: &egui::Context, bounds: egui::Rect) {
        use host_setup::HostSetupAction;
        match host_setup::show(ctx, bounds, &mut self.host_setup) {
            HostSetupAction::None => {}
            HostSetupAction::Close => self.host_setup.close(),
            HostSetupAction::ViewScript => {
                self.host_setup.viewing = true;
                self.host_setup.notice = None;
            }
            HostSetupAction::BackToCommand => {
                self.host_setup.viewing = false;
                self.host_setup.notice = None;
            }
            HostSetupAction::CopyCommand => {
                self.host_setup.notice =
                    clipboard_notice(&self.host_setup.command(), "Command copied");
            }
            HostSetupAction::CopyScript => {
                self.host_setup.notice =
                    clipboard_notice(self.host_setup.script(), "Script copied");
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn shell_data<'a>(
        &self,
        profiles: &'a [model::Profile],
        _recents: &'a [model::Profile],
        surfaces: &'a [model::SurfaceSummary],
        reattachable: &'a [model::Reattachable],
        diagnostics: &'a [(String, String)],
        trusted_hosts: &'a [profiles::TrustedHostRow],
        notice: Option<&'a str>,
    ) -> shell::ShellData<'a> {
        shell::ShellData {
            profiles,
            surfaces,
            reattachable,
            profiles_persistent: self
                .profiles
                .as_ref()
                .is_some_and(profiles::Profiles::durable_credentials),
            trusted_hosts,
            diagnostics,
            notice,
            lock: self.lock.as_ref().map(app_lock::LockHost::view),
            text_size: self.text_size,
            default_keyboard: self.default_keyboard,
            split_keyboard: self.terminal_keyboard.split,
        }
    }

    fn show_tabs(&mut self, ui: &mut egui::Ui) {
        let (rows, recents, trusted) = match &self.profiles {
            Some(profiles) => (
                profiles.rows(self.caps),
                profiles.recents(self.caps, 2),
                profiles.trusted_hosts(),
            ),
            None => Default::default(),
        };
        #[cfg(test)]
        let (rows, recents) = match &self.rows_override {
            Some(rows) => (rows.clone(), rows.iter().take(2).cloned().collect()),
            None => (rows, recents),
        };
        let surfaces = self.surfaces();
        let reattachable = self.reattachable();
        let diagnostics = self.diagnostics();
        let notice = self.notice.clone();
        let data = self.shell_data(
            &rows,
            &recents,
            &surfaces,
            &reattachable,
            &diagnostics,
            &trusted,
            notice.as_deref(),
        );
        let actions = shell::show_tabs(ui, &mut self.shell, &data);
        self.apply_shell_actions(ui.ctx(), actions);
    }

    fn apply_shell_actions(&mut self, ctx: &egui::Context, actions: Vec<shell::ShellAction>) {
        use shell::ShellAction;
        for action in actions {
            match action {
                ShellAction::NewProfile => {
                    self.notice = None;
                    self.editor.open_new();
                    self.editor.durable_credentials = self.credentials_durable();
                }
                ShellAction::EditProfile(id) => self.edit_profile(&id),
                ShellAction::OpenHostSetup => self.host_setup.open(),
                ShellAction::SetTextSize(size) => {
                    self.apply_text_size(size);
                    if let Some(Err(error)) = self.profiles.as_ref().map(|p| p.save_text_size(size))
                    {
                        self.notice = Some(error);
                    }
                }
                ShellAction::SetDefaultKeyboard(kind) => {
                    if let Some(Err(error)) = self
                        .profiles
                        .as_ref()
                        .map(|p| p.save_default_keyboard(kind))
                    {
                        self.notice = Some(error);
                    } else {
                        self.default_keyboard = kind;
                        self.select_keyboard(kind);
                        self.notice = self.profiles.is_none().then(|| {
                            "Default keyboard changed for this run; saved settings are unavailable".to_owned()
                        });
                    }
                }
                ShellAction::SetSplitKeyboard(mode) => {
                    self.terminal_keyboard.split = mode;
                    if let Some(Err(error)) =
                        self.profiles.as_ref().map(|p| p.save_split_keyboard(mode))
                    {
                        self.notice = Some(error);
                    }
                }
                #[cfg(all(feature = "live-ssh", unix))]
                ShellAction::ChooseRecordingFolder => self.choose_recording_folder(),
                #[cfg(all(feature = "live-ssh", unix))]
                ShellAction::SaveRecordingSettings => self.save_recording_settings(ctx),
                #[cfg(all(feature = "live-ssh", unix))]
                ShellAction::ReloadRecordingSettings => self.reload_recording_settings(ctx),
                #[cfg(all(feature = "live-ssh", any(target_os = "android", target_os = "ios")))]
                ShellAction::OpenRecordingLibrary => self.open_recording_library(ctx),
                ShellAction::Lock(action) => {
                    if let Some(lock) = self.lock.as_mut() {
                        lock.apply(action);
                        #[cfg(feature = "live-ssh")]
                        if !lock.content_available() {
                            self.sink.live.stop_recording();
                        }
                    }
                }
                ShellAction::OpenProfile(id) => {
                    self.shell.tab = shell::Tab::Sessions;
                    self.connect_profile(ctx, &id);
                }
                ShellAction::ForgetHost(index) => {
                    if let Some(profiles) = self.profiles.as_mut() {
                        let hosts = profiles.trusted_hosts();
                        if let Some(host) = hosts.get(index) {
                            if let Err(error) = profiles.forget_host(&host.host, host.port) {
                                self.notice = Some(error);
                            }
                        }
                    }
                }
                ShellAction::MoveProfile { id, before } => {
                    if let Some(profiles) = self.profiles.as_mut() {
                        if let Err(error) = profiles.move_before(&id, before.as_deref()) {
                            self.notice = Some(error);
                        }
                    }
                }
                ShellAction::CloseSurface(id) => self.request_surface_close(id),
                ShellAction::OpenSurface(id) => {
                    self.shell.sessions_sheet = false;
                    if self.documents.contains(id) {
                        self.park_active();
                        self.documents.activate(id);
                        self.surface = Surface::Documents;
                        continue;
                    }
                    self.activate_tab(id);
                    if self.active_tab == Some(id) {
                        self.surface = Surface::Terminal;
                    }
                }
                ShellAction::Reattach(index) => {
                    self.shell.sessions_sheet = false;
                    if let Some(target) = self.reattach_targets().into_iter().nth(index) {
                        self.connect_profile(ctx, &target.profile);
                    }
                }
                ShellAction::OpenDemo => {
                    self.open_local_tab();
                    self.shell.sessions_sheet = false;
                    self.surface = Surface::Terminal;
                }
                ShellAction::HideSessions => self.shell.sessions_sheet = false,
                ShellAction::OpenProfilesTab => {
                    self.shell.sessions_sheet = false;
                    self.shell.tab = shell::Tab::Profiles;
                    self.leave_terminal();
                }
                ShellAction::OpenDocuments => {
                    self.shell.sessions_sheet = false;
                    self.shell.tab = shell::Tab::Sessions;
                    self.leave_terminal();
                    self.documents.browse();
                    self.notice = self.documents.notice.take();
                }
            }
        }
    }

    #[cfg(all(feature = "live-ssh", unix))]
    fn save_recording_settings(&mut self, ctx: &egui::Context) {
        let Some(settings) = self.shell.recording_settings.clone() else {
            self.shell.recording_error = Some("Recording settings are unavailable".to_owned());
            return;
        };
        let Some(storage) = self.recording_storage.as_mut() else {
            self.shell.recording_error = Some("Recording storage is unavailable".to_owned());
            return;
        };
        let context = ctx.clone();
        match storage.save(settings, move || context.request_repaint()) {
            Ok(()) => {
                self.shell.recording_busy = true;
                self.shell.recording_error = None;
                self.shell.recording_message = None;
            }
            Err(error) => self.shell.recording_error = Some(error),
        }
    }

    #[cfg(all(feature = "live-ssh", unix))]
    fn reload_recording_settings(&mut self, ctx: &egui::Context) {
        let Some(storage) = self.recording_storage.as_mut() else {
            self.shell.recording_error = Some("Recording storage is unavailable".to_owned());
            return;
        };
        let context = ctx.clone();
        match storage.reload(move || context.request_repaint()) {
            Ok(()) => {
                self.shell.recording_busy = true;
                self.shell.recording_error = None;
                self.shell.recording_message = None;
            }
            Err(error) => self.shell.recording_error = Some(error),
        }
    }

    #[cfg(all(feature = "live-ssh", unix))]
    fn poll_recording_settings(&mut self) {
        let Some(result) = self
            .recording_storage
            .as_mut()
            .and_then(recording_storage::RecordingStorage::poll)
        else {
            return;
        };
        self.shell.recording_busy = false;
        match result {
            Ok(settings) => {
                self.shell.recording_settings = Some(settings.clone());
                self.shell.recording_saved = Some(settings);
                self.shell.recording_message = Some("Recording settings updated.".to_owned());
                self.shell.recording_error = None;
            }
            Err(error) => self.shell.recording_error = Some(error),
        }
    }

    #[cfg(all(feature = "live-ssh", unix))]
    fn choose_recording_folder(&mut self) {
        #[cfg(target_os = "android")]
        {
            match festerm_android_platform::pick_recording_folder() {
                Ok(()) => {
                    self.shell.recording_picker_pending = true;
                    self.shell.recording_error = None;
                    self.shell.recording_message = None;
                }
                Err(error) => self.shell.recording_error = Some(error.to_owned()),
            }
        }
        #[cfg(target_os = "ios")]
        {
            let result = self
                .recording_folder_picker
                .as_ref()
                .ok_or("The native recording folder picker is unavailable")
                .and_then(|picker| picker.open().map_err(|error| error.message()));
            match result {
                Ok(()) => {
                    self.shell.recording_picker_pending = true;
                    self.shell.recording_error = None;
                    self.shell.recording_message = None;
                }
                Err(error) => self.shell.recording_error = Some(error.to_owned()),
            }
        }
        #[cfg(not(any(target_os = "android", target_os = "ios")))]
        {
            self.shell.recording_error =
                Some("The native recording folder picker is unavailable".to_owned());
        }
    }

    #[cfg(all(feature = "live-ssh", any(target_os = "android", target_os = "ios")))]
    fn finish_recording_folder(
        &mut self,
        result: Result<Option<festerm_recording::ExportDestination>, String>,
    ) {
        self.shell.recording_picker_pending = false;
        if std::mem::take(&mut self.recording_ui.picking_for_retry) {
            match result {
                Ok(Some(destination)) => {
                    if let Some(retry) = self.recording_ui.retry.as_mut() {
                        retry.settings.destination = Some(destination);
                        self.recording_ui.status = None;
                    } else {
                        self.recording_ui.status =
                            Some("The export options are no longer available.".to_owned());
                    }
                }
                Ok(None) => {}
                Err(error) => self.recording_ui.status = Some(error),
            }
        } else if std::mem::take(&mut self.recording_ui.picking_for_capture) {
            self.finish_capture_folder(result);
        } else {
            match result {
                Ok(Some(destination)) => {
                    if let Some(settings) = self.shell.recording_settings.as_mut() {
                        settings.destination = Some(destination);
                        self.shell.recording_message =
                            Some("Folder selected. Save recording settings to keep it.".to_owned());
                    } else {
                        self.shell.recording_error =
                            Some("Recording settings are unavailable".into());
                    }
                }
                Ok(None) => {
                    self.shell.recording_message = Some("Folder selection cancelled.".to_owned());
                }
                Err(error) => self.shell.recording_error = Some(error),
            }
        }
    }

    #[cfg(all(feature = "live-ssh", target_os = "ios"))]
    fn poll_recording_folder(&mut self) {
        if !self.shell.recording_picker_pending {
            return;
        }
        let Some(result) = self
            .recording_folder_picker
            .as_ref()
            .and_then(festerm_ios_window::RecordingFolderPicker::take_result)
        else {
            return;
        };
        self.finish_recording_folder(match result {
            Ok(selection) => Ok(Some(festerm_recording::ExportDestination::IosBookmark {
                bookmark: selection.bookmark,
                display_name: selection.display_name,
            })),
            Err(festerm_ios_window::RecordingFolderError::Cancelled) => Ok(None),
            Err(error) => Err(error.message().to_owned()),
        });
    }

    #[cfg(all(feature = "live-ssh", target_os = "android"))]
    fn poll_recording_folder(&mut self) {
        if !self.shell.recording_picker_pending {
            return;
        }
        match festerm_android_platform::take_picked_recording_folder() {
            Ok(None) => {}
            Ok(Some(Ok(uri))) => {
                self.finish_recording_folder(Ok(Some(
                    festerm_recording::ExportDestination::AndroidTree(uri),
                )));
            }
            Ok(Some(Err(festerm_android_platform::PickError::Cancelled))) => {
                self.finish_recording_folder(Ok(None));
            }
            Ok(Some(Err(festerm_android_platform::PickError::Unavailable))) | Err(_) => {
                self.finish_recording_folder(Err(
                    "The Files folder could not be selected. Try again.".to_owned(),
                ));
            }
        }
    }

    fn credentials_durable(&self) -> bool {
        self.profiles
            .as_ref()
            .is_some_and(profiles::Profiles::durable_credentials)
    }

    fn edit_profile(&mut self, id: &str) {
        if let Some(spec) = self
            .profiles
            .as_ref()
            .and_then(|profiles| profiles.sftp_spec(id))
        {
            self.notice = None;
            self.editor.open_edit_sftp(id, &spec, self.caps);
            self.editor.durable_credentials = self.credentials_durable();
            return;
        }
        if let Some(spec) = self
            .profiles
            .as_ref()
            .and_then(|profiles| profiles.tunnel_spec(id))
        {
            self.notice = None;
            self.editor.open_edit_tunnel(id, &spec, self.caps);
            return;
        }
        let Some((name, ssh)) = self
            .profiles
            .as_ref()
            .and_then(|profiles| profiles.ssh_profile(id))
        else {
            return;
        };
        self.notice = None;
        self.editor.open_edit(id, &name, &ssh);
        self.editor.durable_credentials = self.credentials_durable();
    }

    fn connect_profile(&mut self, ctx: &egui::Context, id: &str) {
        if let Some(spec) = self
            .profiles
            .as_ref()
            .and_then(|profiles| profiles.sftp_spec(id))
        {
            self.notice = None;
            self.open_files(spec, true, ctx);
            return;
        }
        #[cfg(feature = "live-ssh")]
        if self
            .profiles
            .as_ref()
            .is_some_and(|profiles| profiles.is_tunnel(id))
        {
            self.connect_tunnel_profile(ctx, id);
            return;
        }
        #[cfg(feature = "live-ssh")]
        {
            self.notice = None;
            let open = self.tabs.iter().find(|tab| {
                tab.spec
                    .as_ref()
                    .and_then(|spec| spec.profile.as_ref())
                    .is_some_and(|profile| profile.as_str() == id)
            });
            let open = open.map(|tab| tab.id).filter(|open| {
                self.tab_surfaces().iter().any(|surface| {
                    surface.id == *open && surface.state != model::SurfaceState::Disconnected
                })
            });
            if let Some(open) = open {
                self.activate_tab(open);
                self.surface = Surface::Terminal;
                return;
            }
            match self.open_profile(id, ctx) {
                Ok(()) => self.surface = Surface::Terminal,
                Err(error) => self.notice = Some(error),
            }
        }
        #[cfg(not(feature = "live-ssh"))]
        {
            let _ = (ctx, id);
            self.notice = Some("SSH is not available in this build".to_owned());
        }
    }

    /// Persists a host key the user just trusted so later connections pin it.
    #[cfg(feature = "live-ssh")]
    fn persist_new_trust(&mut self) {
        let trusted = self
            .sink
            .live
            .connection
            .borrow_mut()
            .as_mut()
            .and_then(live_ssh::LiveSsh::take_new_trust);
        if let (Some((identity, fingerprint)), Some(profiles)) = (trusted, self.profiles.as_mut()) {
            if let Err(error) = profiles.trust_host(&identity, &fingerprint) {
                self.notice = Some(format!("Could not save the host key: {error}"));
            }
        }
    }

    fn leave_terminal(&mut self) {
        if self.surface == Surface::Terminal {
            self.gestures.cancel();
            self.terminal_keyboard.cancel();
            self.clear_sticky_modifiers();
            self.keys_panel.open = false;
            if self.workspace == workspaces::Workspace::Terminal {
                if let Some(focus) = self.view.relinquish_transient_input() {
                    route_input(&mut self.terminal, InputEvent::Focus(focus), &mut self.sink);
                }
            }
        }
        self.surface = Surface::Tabs;
    }

    fn show_sheets(&mut self, ctx: &egui::Context, bounds: egui::Rect) {
        if self.shell.sessions_sheet {
            let surfaces = self.surfaces();
            let reattachable = self.reattachable();
            let (profiles, diagnostics, trusted) = (Vec::new(), Vec::new(), Vec::new());
            let data = self.shell_data(
                &profiles,
                &profiles,
                &surfaces,
                &reattachable,
                &diagnostics,
                &trusted,
                None,
            );
            let actions = shell::sessions_sheet(ctx, bounds, &data);
            self.apply_shell_actions(ctx, actions);
        }
        self.show_tab_menu(ctx, bounds);
        #[cfg(all(feature = "live-ssh", any(target_os = "android", target_os = "ios")))]
        self.show_recording_sheet(ctx, bounds);
        #[cfg(all(feature = "live-ssh", any(target_os = "android", target_os = "ios")))]
        self.poll_recording_preparation(ctx);
        #[cfg(all(feature = "live-ssh", any(target_os = "android", target_os = "ios")))]
        self.show_recording_export(ctx, bounds);
        #[cfg(all(feature = "live-ssh", any(target_os = "android", target_os = "ios")))]
        self.poll_recording_export_preview(ctx);
        #[cfg(all(feature = "live-ssh", any(target_os = "android", target_os = "ios")))]
        self.show_recording_library(ctx, bounds);
        if let Some(id) = self.confirm_close {
            match self.tabs.iter().find(|tab| tab.id == id) {
                Some(tab) => {
                    let title = tab.title.clone();
                    let durable = tab.tunnel.is_some()
                        || tab
                            .spec
                            .as_ref()
                            .and_then(profiles::SessionSpec::persistent_label)
                            .is_some();
                    let detail = if durable {
                        "Close this tab and detach. The durable remote session keeps running."
                    } else if tab.spec.is_some() {
                        "Close this tab and disconnect. A plain SSH shell may end; a file browser has no shell to end."
                    } else {
                        "Close this offline tab. No remote process is running."
                    };
                    match terminal_chrome::close_session_sheet(ctx, bounds, &title, detail) {
                        Some(true) => {
                            self.confirm_close = None;
                            self.close_tab(id);
                        }
                        Some(false) => self.confirm_close = None,
                        None => {}
                    }
                }
                None => self.confirm_close = None,
            }
        }
        #[cfg(feature = "live-ssh")]
        self.show_tunnel_sheets(ctx, bounds);
        #[cfg(feature = "live-ssh")]
        if let Some(live) = self.sink.live.connection.borrow_mut().as_mut() {
            if let Some(model) = live.host_verification() {
                if let Some(decision) = forms::host_verification_sheet(ctx, bounds, &model) {
                    live.resolve_host(decision);
                }
            } else if let Some((target, retry)) = live.password_request() {
                match forms::password_sheet(ctx, bounds, &target, retry, live.password_buffer()) {
                    forms::PasswordDecision::Submit => live.submit_password(),
                    forms::PasswordDecision::Cancel => {
                        live.stop("Disconnected. Connect again for a fresh shell.")
                    }
                    forms::PasswordDecision::None => {}
                }
            }
        }
    }

    fn show_editor(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let can_connect = cfg!(feature = "live-ssh") && self.lifecycle.get().active;
        #[cfg(feature = "live-ssh")]
        self.sync_editor_tunnel(&ctx);
        let action = forms::show_editor(ui, &mut self.editor, self.caps, can_connect);
        match action {
            forms::EditorAction::None => {}
            forms::EditorAction::Cancel => self.close_editor(),
            forms::EditorAction::OpenAuthentication => self.editor.auth_sheet = true,
            forms::EditorAction::OpenHostSetup => self.host_setup.open(),
            forms::EditorAction::SaveOnly => {
                if self.save_editor_profile().is_some() {
                    self.editor.close();
                }
            }
            forms::EditorAction::SaveAndConnect => {
                if let Some(id) = self.save_editor_profile() {
                    self.editor.close();
                    self.connect_profile(&ctx, &id);
                }
            }
            forms::EditorAction::Delete => self.delete_editor_profile(),
            forms::EditorAction::SignIn
            | forms::EditorAction::SelectTunnelAccount(_)
            | forms::EditorAction::RefreshTunnels
            | forms::EditorAction::SignOut
            | forms::EditorAction::PickTunnel(_)
            | forms::EditorAction::DeleteTunnel(_)
            | forms::EditorAction::RenameTunnel(_) => {
                #[cfg(feature = "live-ssh")]
                self.apply_tunnel_editor_action(&ctx, action);
            }
        }
        if self.editor.open && self.editor.auth_sheet {
            let bounds = ui.max_rect();
            match forms::authentication_sheet(&ctx, bounds, &mut self.editor, true) {
                forms::AuthAction::None => {}
                forms::AuthAction::Done => self.finish_authentication(),
                forms::AuthAction::PasteKey => self.import_key(None),
                forms::AuthAction::ImportPending => {
                    let passphrase = std::mem::take(&mut self.editor.key_passphrase);
                    self.import_key(Some(passphrase));
                }
                forms::AuthAction::GenerateKey => self.generate_key(),
                forms::AuthAction::CopyPublicKey => {
                    if let Some(festerm_mobile_profiles::SshAuth::Key { key, .. }) =
                        &self.editor.auth
                    {
                        festerm_ios_window::set_clipboard_text(&key.public_key);
                        self.editor.auth_notice = Some("Public key copied".to_owned());
                    }
                }
            }
        }
    }

    fn close_editor(&mut self) {
        self.discard_unsaved_credential();
        self.editor.close();
    }

    fn discard_unsaved_credential(&mut self) {
        if let (Some(profiles), Some(auth)) = (&self.profiles, self.editor.unsaved_auth()) {
            profiles.discard_credential(auth);
        }
    }

    /// Replaces the credential chosen in the editor, dropping a stored but
    /// never-saved predecessor.
    fn replace_editor_credential(&mut self, auth: festerm_mobile_profiles::SshAuth) {
        self.discard_unsaved_credential();
        self.editor.auth = Some(auth);
    }

    fn import_key(&mut self, passphrase: Option<String>) {
        use profiles::KeyImportFailure;
        let Some(store) = self.profiles.as_ref() else {
            self.editor.auth_notice = Some("Saved profiles are unavailable".to_owned());
            return;
        };
        let text = match passphrase {
            Some(_) => self.editor.pending_key.clone(),
            None => festerm_ios_window::clipboard_text().unwrap_or_default(),
        };
        if text.trim().is_empty() {
            self.editor.auth_notice = Some("The clipboard has no private key to import".to_owned());
            return;
        }
        match store.import_key(&text, passphrase.as_deref().filter(|p| !p.is_empty())) {
            Ok(auth) => {
                self.editor.pending_key.clear();
                self.editor.needs_passphrase = false;
                self.editor.auth_notice = Some("Key imported".to_owned());
                self.replace_editor_credential(auth);
            }
            Err(KeyImportFailure::PassphraseRequired) => {
                self.editor.pending_key = text;
                self.editor.needs_passphrase = true;
                self.editor.auth_notice = Some(
                    if passphrase.is_some() {
                        "That passphrase did not unlock the key"
                    } else {
                        "This key is encrypted; enter its passphrase"
                    }
                    .to_owned(),
                );
            }
            Err(KeyImportFailure::Message(message)) => {
                self.editor.needs_passphrase = false;
                self.editor.auth_notice = Some(message);
            }
        }
    }

    fn generate_key(&mut self) {
        let Some(store) = self.profiles.as_ref() else {
            self.editor.auth_notice = Some("Saved profiles are unavailable".to_owned());
            return;
        };
        match store.generate_key(platform_text("fesTerm iPhone", "fesTerm Android")) {
            Ok(auth) => {
                self.editor.auth_notice = Some(
                    "Key generated. Copy the public key into the server's authorized_keys."
                        .to_owned(),
                );
                self.replace_editor_credential(auth);
            }
            Err(error) => self.editor.auth_notice = Some(error),
        }
    }

    /// Closes the Authentication sheet, storing a typed password first.
    fn finish_authentication(&mut self) {
        if self.editor.auth_mode == forms::AuthMode::Password && !self.editor.password.is_empty() {
            let password = std::mem::take(&mut self.editor.password);
            match self
                .profiles
                .as_ref()
                .map(|store| store.store_password(password))
            {
                Some(Ok(auth)) => self.replace_editor_credential(auth),
                Some(Err(error)) => {
                    self.editor.auth_notice = Some(error);
                    return;
                }
                None => {
                    self.editor.auth_notice = Some("Saved profiles are unavailable".to_owned());
                    return;
                }
            }
        }
        self.editor.wipe_secrets();
        self.editor.auth_notice = None;
        self.editor.auth_sheet = false;
    }

    /// Validates the editor and saves it; returns the profile id.
    fn save_editor_profile(&mut self) -> Option<String> {
        use festerm_mobile_profiles::{Recovery, SshAuth, SshProfile};
        #[cfg(feature = "live-ssh")]
        if self.editor.kind(self.caps) == model::ProfileKind::DevTunnel {
            return self.save_tunnel_editor();
        }
        if self.editor.auth_mode == forms::AuthMode::Password && !self.editor.password.is_empty() {
            self.finish_authentication();
            if self.editor.auth_sheet {
                return None;
            }
        }
        let host = self.editor.host.trim().to_owned();
        if host.is_empty() {
            self.editor.error = Some("Enter a host to save this profile".to_owned());
            return None;
        }
        let Ok(port) = self.editor.port.trim().parse::<u16>() else {
            self.editor.error = Some("Enter a valid port (1-65535)".to_owned());
            return None;
        };
        let username = self.editor.username.trim().to_owned();
        if username.is_empty() {
            self.editor.error = Some("Enter a username".to_owned());
            return None;
        }
        let auth = match (self.editor.auth_mode, self.editor.auth.clone()) {
            (forms::AuthMode::Ask, _) => SshAuth::Ask,
            (forms::AuthMode::Password, Some(auth @ SshAuth::Password { .. })) => auth,
            (forms::AuthMode::Password, _) => {
                self.editor.error = Some("Set a password, or choose Ask each time".to_owned());
                return None;
            }
            (forms::AuthMode::Key, Some(auth @ SshAuth::Key { .. })) => auth,
            (forms::AuthMode::Key, _) => {
                self.editor.error = Some("Import or generate an SSH key first".to_owned());
                return None;
            }
        };
        let name = self.editor.name.trim().to_owned();
        let Some(profiles) = self.profiles.as_mut() else {
            self.editor.error = Some("Saved profiles are unavailable".to_owned());
            return None;
        };
        let name = if name.is_empty() {
            profiles.suggest_name(&host)
        } else {
            name
        };
        if self.editor.kind(self.caps) == model::ProfileKind::Sftp {
            let start_dir = self.editor.start_dir.trim();
            let sftp = festerm_mobile_profiles::SftpProfile {
                host,
                port,
                username,
                auth: auth.clone(),
                start_dir: (!start_dir.is_empty()).then(|| start_dir.to_owned()),
            };
            let result = profiles.save_sftp(self.editor.editing.as_deref(), &name, sftp);
            return self.finish_profile_save(result, &auth);
        }
        let ssh = SshProfile {
            host,
            port,
            username,
            auth: auth.clone(),
            remote_session: self.editor.remote_session(),
            recovery: Recovery::default(),
        };
        let result = profiles.save_ssh(self.editor.editing.as_deref(), &name, ssh);
        self.finish_profile_save(result, &auth)
    }

    fn finish_profile_save(
        &mut self,
        result: Result<String, String>,
        auth: &festerm_mobile_profiles::SshAuth,
    ) -> Option<String> {
        match result {
            Ok(id) => {
                if let (Some(original), Some(profiles)) =
                    (&self.editor.original_auth, self.profiles.as_ref())
                {
                    if original != auth {
                        profiles.discard_credential(original);
                    }
                }
                self.editor.error = None;
                Some(id)
            }
            Err(error) => {
                self.editor.error = Some(error);
                None
            }
        }
    }

    fn delete_editor_profile(&mut self) {
        let (Some(id), Some(profiles)) = (self.editor.editing.clone(), self.profiles.as_mut())
        else {
            return;
        };
        match profiles.delete(&id) {
            Ok(()) => self.editor.close(),
            Err(error) => self.editor.error = Some(error),
        }
    }

    fn switch_workspace(&mut self, workspace: workspaces::Workspace) {
        self.cancel_dictation();
        if self.workspace == workspace {
            return;
        }
        self.gestures.cancel();
        self.clear_sticky_modifiers();
        if self.workspace == workspaces::Workspace::Terminal {
            if let Some(focus) = self.view.relinquish_transient_input() {
                route_input(&mut self.terminal, InputEvent::Focus(focus), &mut self.sink);
            }
        }
        self.workspace = workspace;
    }

    fn reset_fixture(&mut self) {
        self.cancel_dictation();
        self.switch_workspace(workspaces::Workspace::Terminal);
        self.gestures.cancel();
        if self.active_is_local() {
            self.terminal = fixture_terminal();
            self.view = self.fresh_view();
        } else {
            self.open_local_tab();
        }
        self.sink.diagnostics = InputSinkDiagnostics::default();
        self.clear_sticky_modifiers();
        self.editor.close();
    }

    fn show_surface(&mut self, ui: &mut egui::Ui, lifecycle: Lifecycle) {
        #[cfg(feature = "live-ssh")]
        let live = self.live_view();
        #[cfg(feature = "live-ssh")]
        let session_active = live.as_ref().is_some_and(|live| !live.can_reset);
        #[cfg(not(feature = "live-ssh"))]
        let session_active = false;
        let in_terminal = self.workspace == workspaces::Workspace::Terminal;
        let chrome = self.chrome();
        let surfaces = if in_terminal {
            self.surfaces()
        } else {
            Vec::new()
        };
        let heading = (!in_terminal).then(|| self.files_heading());
        let can_open_files = in_terminal && self.active_spec().is_some();
        let mut back = false;
        let mut files = false;
        let mut chip_action = terminal_chrome::ChipAction::None;
        let mut close_tab = None;
        let mut keyboard_action = terminal_chrome::KeyRowAction::None;
        let input_ready = self.terminal_input_ready(lifecycle);
        egui::Panel::top("mobile-header")
            .frame(egui::Frame::new().fill(if in_terminal {
                theme::color::TERMINAL_CHROME
            } else {
                theme::color::BAR
            }))
            .show_separator_line(false)
            .show(ui, |ui| {
                let center = match &heading {
                    Some((title, subtitle)) => {
                        terminal_chrome::HeaderCenter::Title { title, subtitle }
                    }
                    None => terminal_chrome::HeaderCenter::Chips(&surfaces),
                };
                let header = terminal_chrome::header(ui, center);
                back = header.back;
                chip_action = header.chip;
                let menu = egui::Popup::menu(&header.more).show(|ui| {
                    if ui.button("Documents").clicked() {
                        self.park_active();
                        self.documents.active = None;
                        self.surface = Surface::Documents;
                        ui.close();
                    }
                    if in_terminal && self.active_tab.is_some() {
                        ui.weak(&chrome.line.label);
                        ui.separator();
                    }
                    if can_open_files && ui.button("Files").clicked() {
                        files = true;
                        ui.close();
                    }
                    #[cfg(all(
                        feature = "live-ssh",
                        any(target_os = "android", target_os = "ios")
                    ))]
                    if in_terminal {
                        self.recording_menu(ui);
                    }
                    if in_terminal {
                        keyboard_action = terminal_chrome::keyboard_menu(ui, self.keyboard_kind);
                        if self.caps.paste
                            && ui
                                .add_enabled(input_ready, egui::Button::new("Paste"))
                                .clicked()
                        {
                            keyboard_action = terminal_chrome::KeyRowAction::Paste;
                            ui.close();
                        }
                        if ui
                            .add_enabled(input_ready, egui::Button::new("Extended keys"))
                            .clicked()
                        {
                            keyboard_action = terminal_chrome::KeyRowAction::TogglePanel;
                            ui.close();
                        }
                    }
                    if in_terminal
                        && ui
                            .add_enabled(lifecycle.active, egui::Button::new("Force redraw"))
                            .clicked()
                    {
                        self.view.request_full_redraw();
                        ui.ctx().request_repaint();
                        ui.close();
                    }
                    if session_active && ui.button("Disconnect").clicked() {
                        #[cfg(feature = "live-ssh")]
                        if let Some(live) = self.sink.live.connection.borrow_mut().as_mut() {
                            live.stop("Disconnected. Reconnect to continue.");
                        }
                        ui.close();
                    }
                    if self.active_is_local()
                        && ui
                            .add_enabled(lifecycle.active, egui::Button::new("Reset demo"))
                            .clicked()
                    {
                        self.reset_fixture();
                        ui.close();
                    }
                    if let Some(id) = self.active_tab {
                        if ui.button("Close session").clicked() {
                            close_tab = Some(id);
                            ui.close();
                        }
                    }
                    ui.separator();
                    for (name, value) in self.diagnostics() {
                        ui.weak(format!("{name}: {value}"));
                    }
                });
                self.menu_open = menu.is_some();
            });
        if self.surface != Surface::Terminal {
            return;
        }
        if let Some(id) = close_tab {
            self.close_tab(id);
            return;
        }
        if back {
            match self.workspace {
                workspaces::Workspace::Terminal => self.leave_terminal(),
                workspaces::Workspace::Markdown => {
                    self.markdown = None;
                    self.switch_workspace(workspaces::Workspace::Files);
                }
                workspaces::Workspace::Files if self.files_ctx.only => {
                    self.close_files();
                    self.switch_workspace(workspaces::Workspace::Terminal);
                    self.surface = Surface::Tabs;
                }
                workspaces::Workspace::Files => {
                    self.switch_workspace(workspaces::Workspace::Terminal)
                }
            }
            return;
        }
        if files && can_open_files {
            let ctx = ui.ctx().clone();
            self.open_files_for_active(&ctx);
            return;
        }
        match chip_action {
            terminal_chrome::ChipAction::None => {}
            terminal_chrome::ChipAction::Select(id) => {
                self.activate_tab(id);
                return;
            }
            terminal_chrome::ChipAction::Menu(id) => self.request_tab_menu(id),
            terminal_chrome::ChipAction::All => self.shell.sessions_sheet = true,
            terminal_chrome::ChipAction::New => {
                self.shell.tab = shell::Tab::Profiles;
                self.leave_terminal();
                return;
            }
        }
        if !in_terminal {
            egui::Panel::top("mobile-status")
                .frame(egui::Frame::new().fill(theme::color::BG))
                .show_separator_line(false)
                .show(ui, |_| {});
            match (self.workspace, self.markdown.as_mut()) {
                (workspaces::Workspace::Markdown, Some(markdown)) => {
                    egui::Frame::new()
                        .inner_margin(egui::Margin::symmetric(theme::space::LG as i8, 0))
                        .show(ui, |ui| markdown.show(ui));
                }
                _ => self.show_files_body(ui),
            }
            return;
        }
        self.apply_key_row(ui.ctx(), keyboard_action);
        self.show_terminal(ui, lifecycle);
    }

    fn terminal_input_ready(&self, lifecycle: Lifecycle) -> bool {
        let ready = lifecycle.active
            && !self.shell.sessions_sheet
            && self.tab_menu.is_none()
            && self.confirm_close.is_none()
            && !self.editor.open
            && !self.documents.modal_open();
        #[cfg(all(feature = "live-ssh", any(target_os = "android", target_os = "ios")))]
        let ready = ready && !self.recording_ui.blocks_terminal_input();
        #[cfg(feature = "live-ssh")]
        let ready = ready
            && self
                .sink
                .live
                .connection
                .borrow()
                .as_ref()
                .is_none_or(|live| live.input_enabled());
        ready
    }

    fn show_terminal(&mut self, ui: &mut egui::Ui, lifecycle: Lifecycle) {
        let input_ready = self.terminal_input_ready(lifecycle) && !self.dictation_owns_frame;
        #[cfg(feature = "live-ssh")]
        let read_only = self
            .sink
            .live
            .connection
            .borrow()
            .as_ref()
            .is_some_and(|live| live.can_reset());
        #[cfg(not(feature = "live-ssh"))]
        let read_only = false;
        if !input_ready && !self.dictation_owns_frame {
            self.clear_sticky_modifiers();
            self.terminal_keyboard.cancel();
        }
        let panel_open = self.keys_panel.open && input_ready && !read_only;
        self.terminal_keyboard
            .keys
            .latch(self.control || self.alt || self.shift);
        if panel_open {
            let output = egui::Panel::bottom("terminal-keys-panel")
                .frame(egui::Frame::new().fill(theme::color::TERMINAL_CHROME))
                .show_separator_line(false)
                .show(ui, |ui| {
                    terminal_chrome::keys_panel(
                        ui,
                        &mut self.keys_panel,
                        &mut self.terminal_keyboard.keys,
                    )
                })
                .inner;
            if let Some((key, modifiers)) = output.key {
                self.send_key_with(key, modifiers);
            }
            if output.close {
                self.keys_panel.open = false;
                self.keyboard_visible = true;
            }
        }
        if self.dictation_owns_frame {
            self.toolbar_rect = None;
        } else if !read_only {
            let mut row_action = terminal_chrome::KeyRowAction::None;
            let keyboard_kind = self.keyboard_kind;
            let custom_visible = keyboard_kind != terminal_keyboard::KeyboardKind::Native
                && self.keyboard_visible
                && !panel_open;
            let keyboard_space = ui.available_height();
            let toolbar = egui::Panel::bottom("terminal-keys")
                .frame(egui::Frame::new().fill(theme::color::TERMINAL_CHROME))
                .show_separator_line(false)
                .show(ui, |ui| {
                    ui.add_enabled_ui(input_ready && !self.menu_open, |ui| {
                        let state = terminal_chrome::KeyRowState {
                            control: self.control,
                            alt: self.alt,
                            shift: self.shift,
                            panel_open,
                            keyboard_visible: self.keyboard_visible,
                            paste: self.caps.paste,
                        };
                        let output = if custom_visible {
                            self.terminal_keyboard
                                .show(ui, keyboard_kind, &state, keyboard_space)
                        } else {
                            terminal_chrome::key_row(ui, &state, &mut self.terminal_keyboard.keys)
                        };
                        if custom_visible && output.click {
                            if let Some(keyboard) = &self.keyboard {
                                keyboard.play_input_click();
                            }
                        }
                        #[cfg(test)]
                        {
                            self.control_rect = Some(output.control_rect);
                        }
                        row_action = output.action;
                    });
                });
            self.toolbar_rect = Some(toolbar.response.rect);
            self.apply_key_row(ui.ctx(), row_action);
        } else {
            self.toolbar_rect = None;
            egui::Panel::bottom("terminal-keys")
                .frame(egui::Frame::new().fill(theme::color::TERMINAL_CHROME))
                .show_separator_line(false)
                .show(ui, terminal_chrome::read_only_note);
        }
        #[cfg(feature = "live-ssh")]
        if read_only {
            let banner = self.reconnect_banner();
            let compact = ui.available_height() < 260.0;
            let mut reconnect = false;
            egui::Panel::bottom("mobile-recovery")
                .frame(egui::Frame::new().fill(theme::color::BAR))
                .show_separator_line(false)
                .show(ui, |ui| {
                    reconnect = terminal_chrome::reconnect_banner(ui, &banner, compact);
                });
            if reconnect {
                if let Some(id) = self
                    .active_tunnel()
                    .map(|spec| spec.profile.as_str().to_owned())
                {
                    self.start_tunnel_connect(ui.ctx(), &id, self.active_tab, false);
                } else if let Err(error) = self.reconnect_active(ui.ctx()) {
                    self.notice = Some(error);
                }
                return;
            }
        }
        let input_ready = input_ready && !self.dictation_open();
        let history_ready = lifecycle.active
            && !self.editor.open
            && !self.dictation_open()
            && (input_ready || read_only);
        if self.menu_open || !ui.input(|input| input.focused) {
            self.terminal_keyboard.cancel();
        }
        if input_ready && !self.menu_open {
            ui.input_mut(|input| {
                self.consume_sticky_text(&mut input.events, festerm_ios_window::clipboard_text)
            });
        }
        if let Some(notice) = self.input_notice {
            ui.label(egui::RichText::new(notice).color(theme::color::DANGER));
        }
        let terminal_rect = ui.available_rect_before_wrap().shrink(theme::space::SM);
        let gesture = ui.input_mut(|input| {
            let any_touches = input.any_touches();
            self.gestures.update(
                &mut input.events,
                input.time,
                terminal_rect,
                any_touches,
                input_ready && !self.menu_open && input.focused,
            )
        });
        if let Some(key) = gesture.key {
            self.send_key(key);
        }
        if let Some(factor) = gesture.zoom_factor {
            self.view.zoom_by_factor(factor);
        }
        if let Some(delay) = gesture.repaint_after {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_secs_f64(delay));
        }
        let mut terminal_ui = ui.new_child(egui::UiBuilder::new().max_rect(terminal_rect));
        terminal_ui.set_clip_rect(terminal_rect);
        #[cfg(feature = "live-ssh")]
        let dimensions_before = self.terminal.dimensions();
        self.view.show_with_options(
            &mut terminal_ui,
            &mut self.terminal,
            &mut self.sink,
            TerminalViewOptions {
                paste_available: false,
                terminal_input_enabled: history_ready && !self.menu_open && !gesture.block_pointer,
                keyboard_input_enabled: input_ready && !self.menu_open,
                persistent_keyboard_focus: input_ready && !self.menu_open,
                expose_text_to_accessibility: !self
                    .lock
                    .as_ref()
                    .is_some_and(app_lock::LockHost::hides_terminal_text),
                ..Default::default()
            },
        );
        #[cfg(feature = "live-ssh")]
        if self.terminal.dimensions() != dimensions_before {
            if let Some(stream) = self
                .sink
                .live
                .connection
                .borrow()
                .as_ref()
                .and_then(|live| live.recording_stream.as_ref())
            {
                let _ = stream.observe_resize(&self.terminal);
            }
        }
        self.gestures.paint_helper(&terminal_ui, terminal_rect);
        self.terminal_keyboard_input = input_ready
            && ui.input(|input| input.focused)
            && !self.menu_open
            && !panel_open
            && self.keyboard_kind != terminal_keyboard::KeyboardKind::Native;
        if input_ready
            && self.keyboard_visible
            && !panel_open
            && !self.menu_open
            && self.keyboard_kind == terminal_keyboard::KeyboardKind::Native
        {
            // Persistent request for the system keyboard, independent of egui
            // focus on the accessory buttons. UIKit owns keyboard presentation.
            let rect = self.view.diagnostics().grid_rect.unwrap_or(terminal_rect);
            ui.output_mut(|output| {
                output.ime = Some(egui::output::IMEOutput {
                    purpose: egui::IMEPurpose::Normal,
                    rect,
                    cursor_rect: egui::Rect::from_min_size(
                        rect.left_bottom(),
                        egui::vec2(1.0, 1.0),
                    ),
                    should_interrupt_composition: false,
                })
            });
        }
        // Explicitly reject unsupported application intents; keep queues bounded.
        self.view.take_paste_requests();
        self.view.take_clipboard_read_request();
        self.view.take_link_requests();
        self.view.take_find_request();
    }

    fn apply_key_row(&mut self, ctx: &egui::Context, action: terminal_chrome::KeyRowAction) {
        use terminal_chrome::KeyRowAction;
        if self.dictation_open() {
            return;
        }
        match action {
            KeyRowAction::None => {}
            KeyRowAction::Undo
            | KeyRowAction::Redo
            | KeyRowAction::Outdent
            | KeyRowAction::SelectAll
            | KeyRowAction::Copy => {
                self.input_notice = Some("This shortcut requires a local text field");
            }
            KeyRowAction::Key(Key::Character(character)) => {
                // Reuse sticky-modifier and local clipboard-shortcut arbitration.
                ctx.input_mut(|input| input.events.push(egui::Event::Text(character.to_string())));
            }
            KeyRowAction::Key(key) => self.send_key(key),
            KeyRowAction::SelectKeyboard(kind) => self.choose_keyboard(kind),
            KeyRowAction::SwitchKeyboard => self.choose_keyboard(
                if self.keyboard_kind == terminal_keyboard::KeyboardKind::Native {
                    self.last_custom_keyboard
                } else {
                    terminal_keyboard::KeyboardKind::Native
                },
            ),
            KeyRowAction::Paste => {
                self.clear_sticky_modifiers();
                if let Some(text) =
                    festerm_ios_window::clipboard_text().filter(|text| !text.is_empty())
                {
                    route_input(&mut self.terminal, InputEvent::Paste(text), &mut self.sink);
                } else {
                    self.input_notice = Some("The clipboard has no text to paste");
                }
            }
            KeyRowAction::Dictate => self.begin_dictation(ctx),
            KeyRowAction::Escape => self.send_key(Key::Escape),
            KeyRowAction::Tab => self.send_key(Key::Tab),
            KeyRowAction::ToggleControl => self.control = !self.control,
            KeyRowAction::ToggleAlt => self.alt = !self.alt,
            KeyRowAction::ToggleShift => self.shift = !self.shift,
            KeyRowAction::TogglePanel => {
                self.keys_panel.open = !self.keys_panel.open;
                self.terminal_keyboard.cancel();
            }
            KeyRowAction::ToggleKeyboard => {
                self.keyboard_visible = !self.keyboard_visible;
                self.terminal_keyboard.cancel();
                self.clear_sticky_modifiers();
                if self.keyboard_visible {
                    self.keys_panel.open = false;
                }
            }
        }
    }

    #[cfg(feature = "live-ssh")]
    fn reconnect_banner(&self) -> model::ReconnectBanner {
        let status = self
            .live_view()
            .map_or("Connection ended", |live| live.status);
        let custom = self
            .active_tunnel()
            .map(|_| self.tunnel_banner_text(status));
        model::ReconnectBanner {
            custom,
            detail: status
                .starts_with("Input rejected")
                .then(|| "Earlier accepted input may have executed.".to_owned()),
            status: status.to_owned(),
            reattach: self
                .active_spec()
                .and_then(profiles::SessionSpec::persistent_label),
        }
    }
}

impl eframe::App for MobileApp {
    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        #[cfg(target_os = "android")]
        if let Some(rect) = raw_input.screen_rect {
            let insets = festerm_android_platform::insets();
            raw_input.safe_area_insets = Some(egui::SafeAreaInsets(egui::epaint::MarginF32 {
                top: rect.height() * insets.top,
                left: rect.width() * insets.left,
                bottom: rect.height() * insets.bottom,
                right: rect.width() * insets.right,
            }));
        }
        #[cfg(target_os = "android")]
        {
            self.terminal_keyboard.hinge = festerm_android_platform::hinge()
                .map(|(left, right)| terminal_keyboard::Hinge { left, right });
        }
        for key in self.keys.drain() {
            for pressed in [true, false] {
                raw_input.events.push(egui::Event::Key {
                    key,
                    physical_key: Some(key),
                    pressed,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                });
            }
        }
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        // Safe-area strips (status bar, home indicator) match the app surface.
        theme::color::BG.to_normalized_gamma_f32()
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.show(ui);
        #[cfg(any(target_os = "ios", target_os = "android"))]
        ui.ctx().output_mut(|output| {
            output.commands.retain(|command| {
                if let egui::OutputCommand::CopyText(text) = command {
                    if !festerm_ios_window::set_clipboard_text(text) {
                        log::error!("Native clipboard copy failed");
                        self.input_notice = Some("Copy is unavailable here");
                    }
                    false
                } else {
                    true
                }
            });
        });
        if !self.reported_first_ui {
            eprintln!("festerm-mobile: first UI built");
            self.reported_first_ui = true;
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn return_and_tab_reported_as_characters_are_replayed_as_named_keys() {
        let keys = HardwareKeys::default();
        assert!(keys.push_text("\n"));
        assert!(keys.push_text("\t"));
        assert!(!keys.push_text("a"));
        assert_eq!(keys.drain(), vec![egui::Key::Enter, egui::Key::Tab]);
        assert!(keys.drain().is_empty());
    }

    use super::*;

    #[test]
    fn latched_modifiers_never_swallow_return_backspace_or_escape() {
        let ctx = egui::Context::default();
        let mut app = MobileApp::new_terminal(Rc::new(Cell::new(Lifecycle {
            active: true,
            ..Default::default()
        })));
        for toggle in [
            terminal_chrome::KeyRowAction::ToggleShift,
            terminal_chrome::KeyRowAction::ToggleControl,
        ] {
            for key in [Key::Enter, Key::Backspace, Key::Escape] {
                app.apply_key_row(&ctx, toggle);
                let before = app.sink.diagnostics.byte_count;
                app.apply_key_row(&ctx, terminal_chrome::KeyRowAction::Key(key));
                assert!(
                    app.sink.diagnostics.byte_count > before,
                    "{toggle:?} then {key:?} must still send the key"
                );
                assert!(!app.shift && !app.control);
            }
        }
    }

    #[test]
    fn keyboard_live_picks_persist_as_the_default_but_hiding_does_not() {
        use egui_kittest::{kittest::Queryable, Harness};
        use terminal_chrome::KeyRowAction;
        use terminal_keyboard::KeyboardKind;

        let ctx = egui::Context::default();
        let mut app = MobileApp::new_terminal(Rc::new(Cell::new(Lifecycle {
            active: true,
            ..Default::default()
        })))
        .with_profiles(profiles::testing::profiles());
        for default in KeyboardKind::ALL {
            app.apply_shell_actions(&ctx, vec![shell::ShellAction::SetDefaultKeyboard(default)]);
            assert_eq!(app.default_keyboard, default);
            assert_eq!(app.keyboard_kind, default);
            for kind in KeyboardKind::ALL {
                app.apply_key_row(&ctx, KeyRowAction::SelectKeyboard(kind));
                assert_eq!(app.keyboard_kind, kind);
                app.apply_key_row(&ctx, KeyRowAction::ToggleKeyboard);
                assert!(!app.keyboard_visible);
                app.apply_key_row(&ctx, KeyRowAction::ToggleKeyboard);
                assert!(app.keyboard_visible);
                assert_eq!(app.default_keyboard, kind);
                assert_eq!(
                    app.profiles.as_ref().unwrap().default_keyboard().unwrap(),
                    kind
                );
            }
            app.apply_key_row(&ctx, KeyRowAction::SelectKeyboard(default));
            let profiles = app.profiles.take().unwrap();
            app = MobileApp::new_terminal(app.lifecycle.clone()).with_profiles(profiles);
            assert_eq!(app.keyboard_kind, default);
        }
        let mut harness = Harness::builder()
            .with_size(egui::vec2(390.0, 844.0))
            .build_ui_state(|ui, app: &mut MobileApp| app.show(ui), app);
        harness.run();
        harness.get_by_label("Switch keyboard").click();
        harness.run();
        assert_eq!(harness.state().keyboard_kind, KeyboardKind::Native);
        assert!(harness.output().platform_output.ime.is_some());
        harness.get_by_label("Switch keyboard").click();
        harness.run();
        assert_eq!(harness.state().keyboard_kind, KeyboardKind::ShellRow);
        harness.get_by_label("More").click();
        harness.run();
        assert_eq!(harness.state().default_keyboard, KeyboardKind::ShellRow);
        harness.get_by_label("Built-in keyboard").click();
        harness.run();
        assert_eq!(harness.state().keyboard_kind, KeyboardKind::Native);
        assert_eq!(harness.state().default_keyboard, KeyboardKind::Native);
        assert!(harness.output().platform_output.ime.is_some());
        harness.get_by_label("More").click();
        harness.run();
        harness.get_by_label("Terminal: Hold symbols").click();
        harness.run();
        assert!(harness.query_by_label("q").is_some());
        assert!(harness.output().platform_output.ime.is_none());
        assert!(harness.state().terminal_keyboard_input);
        harness.get_by_label("Extended keys").click();
        harness.run();
        assert!(harness.query_by_label("q").is_none());
        assert!(harness.query_by_label("F5").is_some() || harness.query_by_label("Home").is_some());
        harness.get_by_label("Show keyboard").click();
        harness.run();
        assert!(harness.query_by_label("q").is_some());
        harness.get_by_label("Hide keyboard").click();
        harness.run();
        assert!(harness.query_by_label("q").is_none());
        harness.get_by_label("Show keyboard").click();
        harness.run();
        assert!(harness.query_by_label("q").is_some());
        harness.get_by_label("More").click();
        harness.run();
        assert!(!harness.state().terminal_keyboard_input);
        harness
            .get_all_by_label("Extended keys")
            .min_by(|a, b| a.rect().top().total_cmp(&b.rect().top()))
            .unwrap()
            .click();
        harness.run();
        assert!(harness.query_by_label("q").is_none());
        assert!(harness.output().platform_output.ime.is_none());
        assert!(!harness.state().terminal_keyboard_input);
        harness.get_by_label("Show keyboard").click();
        harness.run();
        assert!(harness.query_by_label("q").is_some());
        assert!(harness.state().terminal_keyboard_input);
        harness.state_mut().select_keyboard(KeyboardKind::Native);
        harness.run();
        assert!(harness.output().platform_output.ime.is_some());
        harness.input_mut().focused = false;
        harness.run();
        assert!(harness.output().platform_output.ime.is_none());
        assert!(!harness.state().terminal_keyboard_input);
        harness.input_mut().focused = true;
        harness.run();
        assert!(harness.output().platform_output.ime.is_some());
    }

    #[test]
    fn terminal_keyboards_fit_measured_space_and_yield_to_forms_and_lifecycle() {
        use egui_kittest::{kittest::Queryable, Harness};
        use terminal_keyboard::KeyboardKind;

        for kind in [KeyboardKind::HoldSymbols, KeyboardKind::ShellRow] {
            for size in [
                egui::vec2(320.0, 568.0),
                egui::vec2(390.0, 844.0),
                egui::vec2(834.0, 1194.0),
                egui::vec2(844.0, 390.0),
                egui::vec2(390.0, 260.0),
            ] {
                let lifecycle = Rc::new(Cell::new(Lifecycle {
                    active: true,
                    ..Default::default()
                }));
                let mut app = MobileApp::new_terminal(lifecycle.clone());
                app.select_keyboard(kind);
                let mut harness = Harness::builder()
                    .with_size(size)
                    .build_ui_state(|ui, app: &mut MobileApp| app.show(ui), app);
                harness.run();
                let grid = harness.state().view.diagnostics().grid_rect.unwrap();
                let toolbar = harness.state().toolbar_rect.unwrap();
                assert!(
                    grid.height() >= 40.0 && grid.bottom() <= toolbar.top(),
                    "{kind:?} {size:?}: {grid:?} {toolbar:?}"
                );
                let bounds = egui::Rect::from_min_size(egui::Pos2::ZERO, size);
                for label in [
                    "Hide keyboard",
                    "Switch keyboard",
                    "Extended keys",
                    "Return",
                    "Ctrl",
                    "Alt",
                ] {
                    let rect = harness.get_by_label(label).rect();
                    assert!(
                        bounds.contains_rect(rect),
                        "{kind:?} {size:?}: {label} {rect:?}"
                    );
                }
                let space = harness.get_by_label("Space").rect();
                assert!((space.center().x - toolbar.center().x).abs() < 1.0);
                assert!(space.width() > (toolbar.width() - 14.0).min(720.0) * 0.47);
                assert!(harness.output().platform_output.ime.is_none());
                lifecycle.set(Lifecycle {
                    active: false,
                    suspensions: 1,
                    ..Default::default()
                });
                harness.run();
                assert!(!harness.state().terminal_keyboard_input);
                let before = harness.state().sink.diagnostics.byte_count;
                harness.get_by_label("Return").click();
                harness.run();
                assert_eq!(harness.state().sink.diagnostics.byte_count, before);
                lifecycle.set(Lifecycle {
                    active: true,
                    suspensions: 1,
                    ..Default::default()
                });
                harness.run();
                harness.state_mut().editor.open_new();
                harness.run();
                assert!(
                    harness.query_by_label("q").is_none(),
                    "forms must use the native keyboard"
                );
            }
        }
    }

    #[test]
    fn text_size_setting_resizes_open_and_parked_terminals_and_is_saved() {
        let mut app = MobileApp::new_terminal(Rc::new(Cell::new(Lifecycle::default())))
            .with_profiles(profiles::testing::profiles());
        app.park_active();
        app.apply_shell_actions(
            &egui::Context::default(),
            vec![shell::ShellAction::SetTextSize(shell::TextSize::Large)],
        );
        let large = |size: f32| (size - 14.0).abs() < 1e-3;
        assert!(large(app.view.font_size_points()));
        let parked: Vec<f32> = app.parked_views().map(|v| v.font_size_points()).collect();
        assert!(parked.len() == 1 && large(parked[0]));
        let profiles = app.profiles.take().unwrap();
        assert_eq!(profiles.text_size(), shell::TextSize::Large);
        let reopened =
            MobileApp::new(Rc::new(Cell::new(Lifecycle::default()))).with_profiles(profiles);
        assert!(large(reopened.view.font_size_points()));
    }

    #[test]
    fn split_keyboard_setting_applies_now_and_is_restored_at_startup() {
        use terminal_keyboard::SplitMode;
        let mut app = MobileApp::new_terminal(Rc::new(Cell::new(Lifecycle::default())))
            .with_profiles(profiles::testing::profiles());
        assert_eq!(app.terminal_keyboard.split, SplitMode::Auto);
        app.apply_shell_actions(
            &egui::Context::default(),
            vec![shell::ShellAction::SetSplitKeyboard(SplitMode::Never)],
        );
        assert_eq!(app.terminal_keyboard.split, SplitMode::Never);
        let profiles = app.profiles.take().unwrap();
        let reopened =
            MobileApp::new(Rc::new(Cell::new(Lifecycle::default()))).with_profiles(profiles);
        assert_eq!(reopened.terminal_keyboard.split, SplitMode::Never);
    }

    #[test]
    fn settings_split_control_changes_only_the_split_mode() {
        use egui_kittest::{kittest::Queryable, Harness};
        use terminal_keyboard::SplitMode;

        let mut app = MobileApp::new(Rc::new(Cell::new(Lifecycle {
            active: true,
            ..Default::default()
        })))
        .with_profiles(profiles::testing::profiles());
        app.surface = Surface::Tabs;
        app.shell.tab = shell::Tab::Settings;
        let text_size = app.text_size;
        let mut harness = Harness::builder()
            .with_size(egui::vec2(1194.0, 834.0))
            .build_ui_state(
                |ui, app: &mut MobileApp| app.show_with_occlusion(ui, 0.0),
                app,
            );
        harness.run();
        harness.get_by_label("Split").click();
        harness.run();
        assert_eq!(harness.state().terminal_keyboard.split, SplitMode::Always);
        harness.get_by_label("Full").click();
        harness.run();
        assert_eq!(harness.state().terminal_keyboard.split, SplitMode::Never);
        assert_eq!(harness.state().text_size, text_size);
    }

    #[cfg(all(feature = "live-ssh", unix))]
    #[test]
    fn recording_settings_ui_saves_a_valid_device_local_draft() {
        use egui_kittest::{kittest::Queryable, Harness};
        use std::{fs::File, time::Duration};

        let root = tempfile::tempdir().unwrap();
        festerm_unix_security::make_private_directory(&File::open(root.path()).unwrap()).unwrap();
        let (storage, settings) = recording_storage::RecordingStorage::open(root.path()).unwrap();
        let mut app = MobileApp::new(Rc::new(Cell::new(Lifecycle {
            active: true,
            ..Default::default()
        })));
        app.recording_storage = Some(storage);
        app.shell.recording_settings = Some(settings.clone());
        app.shell.recording_saved = Some(settings);
        app.shell.tab = shell::Tab::Settings;
        let mut harness = Harness::builder()
            .with_size(egui::vec2(850.0, 2400.0))
            .build_ui_state(
                |ui, app: &mut MobileApp| app.show_with_occlusion(ui, 0.0),
                app,
            );
        harness.run();
        harness.get_by_label("Normalize echoed typing").click();
        harness.run();
        assert!(
            harness
                .state()
                .shell
                .recording_settings
                .as_ref()
                .unwrap()
                .normalize_typing
        );
        harness.get_by_label("Save recording settings").click();
        harness.run();
        for _ in 0..100 {
            if !harness.state().shell.recording_busy {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
            harness.run();
        }
        assert!(!harness.state().shell.recording_busy);
        assert!(harness.state().shell.recording_error.is_none());
        let (_, persisted) = festerm_recording::RecordingSettingsStore::open(root.path()).unwrap();
        assert!(persisted.normalize_typing);
        assert!(persisted.destination.is_none());
        assert!(!persisted.delete_source_after_export);
    }

    #[test]
    fn touch_and_hold_drags_a_saved_profile_into_a_new_order() {
        use egui_kittest::{kittest::Queryable, Harness};

        let mut store = profiles::testing::profiles();
        for name in ["alpha", "bravo", "charlie"] {
            store
                .save_ssh(
                    None,
                    name,
                    festerm_mobile_profiles::SshProfile::new("web.example.net", "deploy"),
                )
                .unwrap();
        }
        let app = Rc::new(RefCell::new(
            MobileApp::new(Rc::new(Cell::new(Lifecycle {
                active: true,
                ..Default::default()
            })))
            .with_profiles(store),
        ));
        let displayed = app.clone();
        let mut harness = Harness::builder()
            .with_size(egui::vec2(390.0, 844.0))
            .build_ui_state(
                move |ui, _: &mut ()| displayed.borrow_mut().show_with_occlusion(ui, 0.0),
                (),
            );
        harness.run();
        let order = |app: &Rc<RefCell<MobileApp>>| -> Vec<String> {
            let app = app.borrow();
            app.profiles
                .as_ref()
                .unwrap()
                .rows(app.caps)
                .into_iter()
                .map(|row| row.name)
                .collect()
        };
        let row = |harness: &Harness<'_, ()>, name: &str| harness.get_by_label(name).rect();
        let pointer = |pressed: bool, pos: egui::Pos2| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        let alpha = row(&harness, "alpha");
        let grip = egui::pos2(alpha.left() + 60.0, alpha.center().y);
        let below = egui::pos2(grip.x, row(&harness, "charlie").bottom() - 4.0);

        // A press that moves at once scrolls; it never lifts the row.
        let now = harness.ctx.input(|input| input.time);
        let mut at = 0.0;
        let mut play = |harness: &mut Harness<'_, ()>, steps: Vec<(f64, Vec<egui::Event>)>| {
            for (elapsed, events) in steps {
                at += elapsed;
                harness.input_mut().time = Some(now + at);
                for event in events {
                    harness.event(event);
                }
                harness.step();
            }
        };
        play(
            &mut harness,
            vec![
                (
                    0.01,
                    vec![egui::Event::PointerMoved(grip), pointer(true, grip)],
                ),
                (0.05, vec![egui::Event::PointerMoved(below)]),
                (0.6, vec![]),
                (0.05, vec![pointer(false, below)]),
            ],
        );
        harness.run();
        assert_eq!(order(&app), ["alpha", "bravo", "charlie"]);

        play(
            &mut harness,
            vec![
                (
                    0.5,
                    vec![egui::Event::PointerMoved(grip), pointer(true, grip)],
                ),
                (0.3, vec![]),
                (0.3, vec![]),
                (0.05, vec![egui::Event::PointerMoved(below)]),
                (0.05, vec![]),
                (0.05, vec![pointer(false, below), egui::Event::PointerGone]),
            ],
        );
        harness.run();
        assert_eq!(order(&app), ["bravo", "charlie", "alpha"]);
        assert_eq!(
            app.borrow().surface,
            Surface::Tabs,
            "the release that drops a profile does not open it"
        );
        assert!(app.borrow().shell.reorder.is_none());

        // A touch cancelled without a release abandons the drag in place.
        let charlie = row(&harness, "charlie");
        let grip = egui::pos2(charlie.left() + 60.0, charlie.center().y);
        let top = egui::pos2(grip.x, row(&harness, "bravo").top() + 4.0);
        play(
            &mut harness,
            vec![
                (
                    0.5,
                    vec![egui::Event::PointerMoved(grip), pointer(true, grip)],
                ),
                (0.3, vec![]),
                (0.3, vec![]),
                (0.05, vec![egui::Event::PointerMoved(top)]),
                (0.05, vec![egui::Event::PointerGone]),
            ],
        );
        harness.run();
        assert_eq!(order(&app), ["bravo", "charlie", "alpha"]);
        assert!(app.borrow().shell.reorder.is_none());
    }

    #[test]
    fn mobile_editor_saves_edits_and_deletes_profiles_with_credentials() {
        let mut app = MobileApp::new_terminal(Rc::new(Cell::new(Lifecycle::default())))
            .with_profiles(profiles::testing::profiles());
        app.editor.open_new();
        app.editor.host = "web.example.net".to_owned();
        app.editor.username = "deploy".to_owned();
        app.editor.session_mode = 1;
        app.editor.session_name = "work".to_owned();
        app.editor.auth_mode = forms::AuthMode::Key;
        assert!(
            app.save_editor_profile().is_none(),
            "a key must be chosen first"
        );
        app.generate_key();
        assert!(app.editor.auth_notice.is_some());
        let id = app.save_editor_profile().expect("saved");
        let spec = app.profiles.as_ref().unwrap().spec(&id).unwrap();
        assert_eq!(spec.name, "web.example.net");
        assert_eq!(spec.persistent_label().as_deref(), Some("tmux · work"));
        let first_key = spec.ssh.auth.clone();

        // Replacing the key discards the old secret once the edit is saved.
        app.edit_profile(&id);
        assert_eq!(app.editor.auth_mode, forms::AuthMode::Key);
        app.generate_key();
        assert!(app.editor.unsaved_auth().is_some());
        app.save_editor_profile().expect("updated");
        let store = app.profiles.as_ref().unwrap();
        assert_ne!(
            store.credential_status(&first_key),
            festerm_mobile_profiles::CredentialStatus::Present
        );

        app.edit_profile(&id);
        app.delete_editor_profile();
        assert!(app.profiles.as_ref().unwrap().rows(app.caps).is_empty());
    }

    #[test]
    fn mobile_sftp_profiles_save_edit_open_and_keep_their_start_folder() {
        let mut app = MobileApp::new_terminal(Rc::new(Cell::new(Lifecycle::default())))
            .with_profiles(profiles::testing::profiles());
        app.editor.open_new();
        app.editor.kind_index = forms::EditorState::kinds(app.caps)
            .iter()
            .position(|kind| *kind == model::ProfileKind::Sftp)
            .expect("SFTP is offered in production");
        app.editor.host = "files.example.net".to_owned();
        app.editor.username = "ops".to_owned();
        app.editor.start_dir = " /srv/app ".to_owned();
        let id = app.save_editor_profile().expect("saved");
        let store = app.profiles.as_ref().unwrap();
        assert!(store.is_sftp(&id) && store.spec(&id).is_none());
        let spec = store.sftp_spec(&id).unwrap();
        assert_eq!(spec.start_dir.as_deref(), Some("/srv/app"));
        assert_eq!(spec.detail(), "SFTP · ops@files.example.net");

        app.editor.close();
        app.edit_profile(&id);
        assert_eq!(app.editor.kind(app.caps), model::ProfileKind::Sftp);
        assert_eq!(app.editor.start_dir, "/srv/app");
        app.editor.start_dir.clear();
        app.save_editor_profile().expect("updated");
        let spec = app.profiles.as_ref().unwrap().sftp_spec(&id).unwrap();
        assert_eq!(spec.start_dir, None);
    }

    #[cfg(feature = "live-ssh")]
    #[test]
    fn mobile_files_only_tab_returns_to_profiles_and_drops_its_session() {
        use egui_kittest::{kittest::Queryable, Harness};
        use files_fixture::{fixture_entry, FixtureFs, FixtureTransfers};
        use std::sync::Arc;

        let app = Rc::new(RefCell::new(MobileApp::new(Rc::new(Cell::new(
            Lifecycle {
                active: true,
                ..Default::default()
            },
        )))));
        let ctx = egui::Context::default();
        let fs = |kind, home| {
            Arc::new(FixtureFs {
                kind,
                home,
                entries: vec![fixture_entry("a.txt", false, 5, 1)],
            })
        };
        let session = files::FilesSession::ready(
            "release-files".to_owned(),
            "SFTP · ops@files.example.net".to_owned(),
            files::Ready {
                remote: fs(festerm_sftp::PaneKind::Remote, "/srv"),
                local: fs(festerm_sftp::PaneKind::Local, ""),
                local_root: std::path::PathBuf::from("/nonexistent-festerm-test"),
                transfers: Arc::new(FixtureTransfers::new(Vec::new())),
                start_dir: None,
            },
            &ctx,
        );
        {
            let mut app = app.borrow_mut();
            app.files = Some(Box::new(session));
            app.files_ctx.only = true;
            app.surface = Surface::Terminal;
            app.workspace = workspaces::Workspace::Files;
        }
        let displayed = app.clone();
        let mut harness = Harness::builder()
            .with_size(egui::vec2(390.0, 844.0))
            .build_ui_state(
                move |ui, _: &mut ()| displayed.borrow_mut().show_with_occlusion(ui, 0.0),
                (),
            );
        harness.run_steps(3);
        assert!(
            harness.query_by_label("Files").is_none(),
            "no terminal Files button"
        );
        harness.get_by_label("Back").click();
        harness.run_steps(3);
        let app = app.borrow();
        assert!(app.files.is_none());
        assert_eq!(app.surface, Surface::Tabs);
        assert_eq!(app.workspace, workspaces::Workspace::Terminal);
    }

    #[cfg(feature = "live-ssh")]
    #[test]
    fn mobile_header_menu_resets_demo_without_stale_input_and_has_no_preview_entries() {
        use egui_kittest::{kittest::Queryable, Harness};

        let app = Rc::new(RefCell::new(MobileApp::new_terminal(Rc::new(Cell::new(
            Lifecycle {
                active: true,
                ..Default::default()
            },
        )))));
        let displayed = app.clone();
        let mut harness = Harness::builder()
            .with_size(egui::vec2(390.0, 844.0))
            .build_ui_state(
                move |ui, _: &mut ()| displayed.borrow_mut().show_with_occlusion(ui, 0.35),
                (),
            );
        harness.run();
        app.borrow_mut().control = true;
        harness.get_by_label("More").click();
        harness.run();
        assert!(app.borrow().menu_open);
        assert!(harness.query_by_label("Files (preview)").is_none());
        assert!(harness.query_by_label("README (preview)").is_none());
        harness.get_by_label("Reset demo").click();
        harness.run();
        assert!(!app.borrow().control, "resetting releases sticky modifiers");
        app.borrow_mut().control = true;
        app.borrow_mut()
            .switch_workspace(workspaces::Workspace::Files);
        assert!(!app.borrow().control);
        app.borrow_mut()
            .switch_workspace(workspaces::Workspace::Terminal);
        assert_eq!(app.borrow().sink.diagnostics.byte_count, 0);
        app.borrow_mut().send_key(Key::Tab);
        assert!(app.borrow().sink.diagnostics.byte_count > 0);
        app.borrow_mut().editor.open_new();
        harness.run();
        assert!(app.borrow().editor.open);
        app.borrow_mut().reset_fixture();
        harness.run();
        assert!(!app.borrow().editor.open);
        assert_eq!(app.borrow().sink.diagnostics.byte_count, 0);
    }

    #[cfg(feature = "live-ssh")]
    #[test]
    fn mobile_editor_fits_measured_phone_tablet_space_and_releases_it() {
        use egui_kittest::{kittest::Queryable, Harness};
        for (width, height, occlusion) in [
            (390.0, 844.0, 0.35),
            (834.0, 1194.0, 0.30),
            (1194.0, 834.0, 0.35),
            (375.0, 1024.0, 0.35),
            (844.0, 390.0, 0.35),
            (390.0, 844.0, 0.0),
        ] {
            let app = Rc::new(RefCell::new(MobileApp::new_terminal(Rc::new(Cell::new(
                Lifecycle {
                    active: true,
                    ..Default::default()
                },
            )))));
            let displayed = app.clone();
            let mut harness = Harness::builder()
                .with_size(egui::vec2(width, height))
                .build_ui_state(
                    move |ui, _: &mut ()| displayed.borrow_mut().show_with_occlusion(ui, occlusion),
                    (),
                );
            harness.run();
            let grid_height = app.borrow().view.diagnostics().grid_rect.unwrap().height();
            // Open/close/reopen after cached panel geometry has settled.
            for _ in 0..2 {
                app.borrow_mut().editor.open_new();
                harness.run();
                let limit = height * (1.0 - occlusion) + 1.0;
                for label in ["Save & Connect", "Save only", "Cancel"] {
                    let rect = harness.get_by_label(label).rect();
                    assert!(
                        rect.bottom() <= limit,
                        "{label} {rect:?} in {width}x{height}"
                    );
                    assert!(rect.height() >= 40.0, "{label} {rect:?}");
                }
                assert_eq!(app.borrow().sink.diagnostics.byte_count, 0);
                app.borrow_mut().editor.close();
                harness.run();
                harness.run();
                let released = app.borrow().view.diagnostics().grid_rect.unwrap().height();
                assert!(
                    (released - grid_height).abs() < 2.0,
                    "{released} vs {grid_height}"
                );
            }
        }
    }

    #[test]
    fn mobile_gpu_limits_accept_simulator_downlevel_capabilities() {
        let simulator = eframe::wgpu::Limits {
            max_texture_dimension_2d: 8192,
            ..eframe::wgpu::Limits::downlevel_defaults()
        };
        // Reproduce the rejected default request, then check the descriptor
        // used by the mobile host against the same constrained adapter.
        assert!(!eframe::wgpu::Limits::default().check_limits(&simulator));
        let device = mobile_device_descriptor(simulator.clone());
        assert!(device.required_limits.check_limits(&simulator));
        assert!(device.required_features.is_empty());
        assert!(device.required_limits.max_texture_dimension_2d >= 4096);
    }

    #[test]
    fn mobile_pinch_resizes_only_terminal_and_preserves_zoom_on_memory_warning() {
        for (width, height) in [
            (390.0, 844.0),
            (834.0, 1194.0),
            (1194.0, 834.0),
            (375.0, 1024.0),
        ] {
            let lifecycle = Rc::new(Cell::new(Lifecycle {
                active: true,
                ..Default::default()
            }));
            let mut app = MobileApp::new_terminal(lifecycle.clone());
            app.terminal.ingest(b"\x1b[?1003h\x1b[?1006h");
            let ctx = egui::Context::default();
            let render = |app: &mut MobileApp, time, events| {
                let output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(width, height),
                        )),
                        time: Some(time),
                        events,
                        ..Default::default()
                    },
                    |ui| app.show_with_occlusion(ui, 0.35),
                );
                let ime = output.platform_output.ime.is_some();
                output.drop_without_applying_deltas();
                assert!(ime);
            };
            render(&mut app, 0.0, vec![]);
            render(&mut app, 0.01, vec![]);
            let toolbar = app.toolbar_rect.unwrap();
            let center = app.view.diagnostics().grid_rect.unwrap().center();
            let columns = app.terminal.dimensions().columns();
            let touch = |id, phase, dx| egui::Event::Touch {
                device_id: egui::TouchDeviceId(1),
                id: egui::TouchId(id),
                phase,
                pos: center + egui::vec2(dx, 0.0),
                force: None,
            };
            render(
                &mut app,
                0.1,
                vec![
                    touch(1, egui::TouchPhase::Start, -50.0),
                    touch(2, egui::TouchPhase::Start, 50.0),
                    egui::Event::PointerButton {
                        pos: center - egui::vec2(50.0, 0.0),
                        pressed: true,
                        button: egui::PointerButton::Primary,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
            render(
                &mut app,
                0.2,
                vec![
                    touch(1, egui::TouchPhase::Move, -75.0),
                    touch(2, egui::TouchPhase::Move, 75.0),
                    egui::Event::PointerMoved(center - egui::vec2(75.0, 0.0)),
                ],
            );
            render(&mut app, 0.3, vec![]);
            assert_eq!(app.view.font_size_points(), 17.25);
            assert!(app.terminal.dimensions().columns() < columns);
            assert_eq!(app.toolbar_rect, Some(toolbar));
            assert!(app.view.diagnostics().grid_rect.unwrap().bottom() <= toolbar.top());
            assert!(app.gestures.helper().is_none());
            assert_eq!(ctx.zoom_factor(), 1.0); // App chrome must not zoom.
            let mut state = lifecycle.get();
            state.memory_warning();
            lifecycle.set(state);
            render(&mut app, 0.4, vec![touch(2, egui::TouchPhase::Move, 100.0)]);
            render(
                &mut app,
                0.5,
                vec![
                    touch(1, egui::TouchPhase::End, -75.0),
                    touch(2, egui::TouchPhase::End, 100.0),
                    egui::Event::PointerButton {
                        pos: center - egui::vec2(75.0, 0.0),
                        pressed: false,
                        button: egui::PointerButton::Primary,
                        modifiers: egui::Modifiers::NONE,
                    },
                    egui::Event::PointerGone,
                ],
            );
            render(&mut app, 1.0, vec![]);
            assert_eq!(app.view.font_size_points(), 17.25);
            assert_eq!(app.sink.diagnostics.byte_count, 0);
            assert!(app.terminal.queued_input().is_empty());
        }
    }

    #[test]
    fn mobile_arrow_gesture_keeps_keyboard_and_does_not_leak_mouse_reports() {
        for (width, height) in [(390.0, 844.0), (834.0, 1194.0), (1194.0, 834.0)] {
            let lifecycle = Rc::new(Cell::new(Lifecycle {
                active: true,
                ..Default::default()
            }));
            let mut app = MobileApp::new_terminal(lifecycle);
            app.terminal.ingest(b"\x1b[?1000h\x1b[?1006h");
            let ctx = egui::Context::default();
            let render = |app: &mut MobileApp, time, events| {
                let output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(width, height),
                        )),
                        time: Some(time),
                        events,
                        ..Default::default()
                    },
                    |ui| app.show_with_occlusion(ui, 0.35),
                );
                let ime = output.platform_output.ime.is_some();
                output.drop_without_applying_deltas();
                assert!(ime);
            };
            render(&mut app, 0.0, vec![]);
            render(&mut app, 0.01, vec![]);
            let origin = app.view.diagnostics().grid_rect.unwrap().center();
            let moved = origin + egui::vec2(30.0, 0.0);
            let touch = |phase, pos| egui::Event::Touch {
                device_id: egui::TouchDeviceId(1),
                id: egui::TouchId(1),
                phase,
                pos,
                force: None,
            };
            let mouse = |pressed, pos| egui::Event::PointerButton {
                pos,
                pressed,
                button: egui::PointerButton::Primary,
                modifiers: egui::Modifiers::NONE,
            };
            render(
                &mut app,
                0.1,
                vec![
                    touch(egui::TouchPhase::Start, origin),
                    egui::Event::PointerMoved(origin),
                    mouse(true, origin),
                ],
            );
            render(&mut app, 0.56, vec![]);
            assert_eq!(app.gestures.helper(), Some(None));
            assert_eq!(app.sink.diagnostics.byte_count, 0);
            render(
                &mut app,
                0.6,
                vec![
                    touch(egui::TouchPhase::Move, moved),
                    egui::Event::PointerMoved(moved),
                ],
            );
            assert_eq!(app.sink.diagnostics.byte_count, 3);
            assert_eq!(
                app.gestures.helper(),
                Some(Some(terminal_gesture::Direction::Right))
            );
            render(
                &mut app,
                0.61,
                vec![
                    touch(egui::TouchPhase::End, moved),
                    mouse(false, moved),
                    egui::Event::PointerGone,
                ],
            );
            render(&mut app, 2.0, vec![]);
            assert_eq!(app.sink.diagnostics.byte_count, 3);
            assert!(app.gestures.helper().is_none());
            assert!(app.terminal.queued_input().is_empty());
        }
    }

    #[test]
    fn mobile_persistent_keyboard_does_not_restart_ime_between_frames() {
        let lifecycle = Rc::new(Cell::new(Lifecycle {
            active: true,
            ..Default::default()
        }));
        let mut app = MobileApp::new_terminal(lifecycle.clone());
        let ctx = egui::Context::default();
        let render = |app: &mut MobileApp, occlusion, events| {
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(390.0, 844.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ui| app.show_with_occlusion(ui, occlusion),
            )
        };
        for _ in 0..2 {
            render(&mut app, 0.0, vec![]).drop_without_applying_deltas();
        }
        assert!(ctx.memory(|memory| memory.focused().is_some()));
        let mut checked_frames = 0;
        let mut assert_stable = |output: egui::FullOutput| {
            checked_frames += 1;
            let interrupt = output
                .platform_output
                .ime
                .as_ref()
                .map(|ime| ime.should_interrupt_composition);
            output.drop_without_applying_deltas();
            assert_eq!(
                interrupt,
                Some(false),
                "final platform output must not hide/reopen the native keyboard (frame {checked_frames})"
            );
        };
        for occlusion in [0.0, 0.2, 0.36, 0.36, 0.0, 0.36] {
            assert_stable(render(&mut app, occlusion, vec![]));
        }
        let toolbar = app.toolbar_rect.unwrap();
        let grid = app.view.diagnostics().grid_rect.unwrap();
        for point in [
            toolbar.min + egui::vec2(22.0, 22.0),
            toolbar.min + egui::vec2(57.0, 22.0),
            toolbar.min + egui::vec2(92.0, 22.0),
            toolbar.min + egui::vec2(128.0, 22.0),
            grid.center(),
        ] {
            for pressed in [true, false] {
                assert_stable(render(
                    &mut app,
                    0.36,
                    vec![
                        egui::Event::PointerMoved(point),
                        egui::Event::PointerButton {
                            pos: point,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ],
                ));
            }
            assert_stable(render(&mut app, 0.36, vec![]));
        }
        assert!(app.sink.diagnostics.byte_count > 0);
        for event in [
            egui::Event::Text("x".into()),
            egui::Event::Ime(egui::ImeEvent::Preedit {
                text: "x".into(),
                active_range_chars: None,
            }),
            egui::Event::Ime(egui::ImeEvent::Commit("x".into())),
        ] {
            assert_stable(render(&mut app, 0.36, vec![event]));
        }
        for key in [
            egui::Key::Enter,
            egui::Key::Backspace,
            egui::Key::Tab,
            egui::Key::ArrowUp,
        ] {
            assert_stable(render(
                &mut app,
                0.36,
                vec![egui::Event::Key {
                    key,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }],
            ));
        }
        let mut state = lifecycle.get();
        state.suspend();
        lifecycle.set(state);
        let output = render(&mut app, 0.0, vec![]);
        assert!(output.platform_output.ime.is_none());
        output.drop_without_applying_deltas();
        state.resume();
        lifecycle.set(state);
        render(&mut app, 0.36, vec![]).drop_without_applying_deltas();
        assert_stable(render(&mut app, 0.36, vec![]));
    }

    #[test]
    fn mobile_phone_ipad_and_split_view_keep_terminal_above_persistent_keyboard() {
        for (width, height, keyboard_height) in [
            (390.0, 844.0, 300.0),
            (834.0, 1194.0, 360.0),
            (1194.0, 834.0, 300.0),
            (375.0, 1024.0, 340.0),
            (844.0, 390.0, 160.0),
        ] {
            let lifecycle = Rc::new(Cell::new(Lifecycle {
                active: true,
                ..Default::default()
            }));
            let mut app = MobileApp::new_terminal(lifecycle.clone());
            let ctx = egui::Context::default();
            let render = |app: &mut MobileApp, occlusion, events| {
                ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(width, height),
                        )),
                        events,
                        ..Default::default()
                    },
                    |ui| app.show_with_occlusion(ui, occlusion),
                )
            };
            for _ in 0..2 {
                let output = render(&mut app, keyboard_height / height, vec![]);
                assert!(output.platform_output.ime.is_some());
                output.drop_without_applying_deltas();
            }
            let grid = app.view.diagnostics().grid_rect.unwrap();
            let toolbar = app.toolbar_rect.unwrap();
            assert!(
                grid.bottom() <= toolbar.top(),
                "{width}x{height} {grid:?} {toolbar:?}"
            );
            assert!(toolbar.bottom() <= height - keyboard_height + 0.5);
            assert!(grid.height() > 0.0);
            // Tap Ctrl through the real UI: it must not dismiss the keyboard.
            let point = app.control_rect.expect("rendered Ctrl key").center();
            for pressed in [true, false] {
                let output = render(
                    &mut app,
                    keyboard_height / height,
                    vec![
                        egui::Event::PointerMoved(point),
                        egui::Event::PointerButton {
                            pos: point,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ],
                );
                assert!(output.platform_output.ime.is_some());
                output.drop_without_applying_deltas();
            }
            assert!(app.control);
            render(
                &mut app,
                keyboard_height / height,
                vec![egui::Event::Text("c".into())],
            )
            .drop_without_applying_deltas();
            assert_eq!(app.sink.diagnostics.byte_count, 1);
            assert!(!app.control);
            // A hardware keyboard reclaims the system keyboard's region but
            // leaves the same terminal-key row available on iPhone and iPad.
            render(&mut app, 0.0, vec![]).drop_without_applying_deltas();
            assert!(app.view.diagnostics().grid_rect.unwrap().height() > grid.height());
            let mut state = lifecycle.get();
            state.suspend();
            lifecycle.set(state);
            let output = render(&mut app, 0.0, vec![egui::Event::Text("ignored".into())]);
            assert!(output.platform_output.ime.is_none());
            output.drop_without_applying_deltas();
            assert_eq!(app.sink.diagnostics.byte_count, 1);
        }
    }

    #[test]
    fn mobile_sticky_modifiers_are_one_shot_and_preserve_ime_commit_boundaries() {
        let lifecycle = Rc::new(Cell::new(Lifecycle {
            active: true,
            ..Default::default()
        }));
        let mut app = MobileApp::new_terminal(lifecycle);
        app.control = true;
        app.alt = true;
        app.shift = true;
        let mut events = vec![egui::Event::Ime(egui::ImeEvent::Commit("cx".into()))];
        app.consume_sticky_text(&mut events, || panic!("not a clipboard shortcut"));
        assert_eq!(app.sink.diagnostics.byte_count, 3); // ESC, Ctrl-C, literal x.
        assert!(!app.control && !app.alt && !app.shift);
        assert!(
            matches!(&events[0], egui::Event::Ime(egui::ImeEvent::Commit(text)) if text.is_empty())
        );
        assert!(app.terminal.queued_input().is_empty());
    }

    #[test]
    fn mobile_key_row_and_panel_apply_modifiers_once_through_core_encoding() {
        let lifecycle = Rc::new(Cell::new(Lifecycle {
            active: true,
            ..Default::default()
        }));
        let mut app = MobileApp::new_terminal(lifecycle);
        app.apply_key_row(
            &egui::Context::default(),
            terminal_chrome::KeyRowAction::ToggleControl,
        );
        app.apply_key_row(
            &egui::Context::default(),
            terminal_chrome::KeyRowAction::ToggleShift,
        );
        assert!(app.control);
        app.send_key_with(Key::Function(5), Modifiers::NONE);
        assert!(
            !app.control && !app.shift,
            "modifiers are consumed by the next key"
        );
        app.send_key_with(Key::Tab, Modifiers::SHIFT);
        assert_eq!(app.sink.diagnostics.byte_count, 10); // CSI 15;5 ~ then CSI Z.
    }

    #[test]
    fn mobile_clipboard_chords_do_not_leak_key_text_or_control_bytes() {
        let mut app = MobileApp::new_terminal(Rc::new(Cell::new(Lifecycle {
            active: true,
            ..Default::default()
        })));
        app.control = true;
        app.shift = true;
        let mut events = vec![
            egui::Event::Key {
                key: egui::Key::C,
                physical_key: Some(egui::Key::C),
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            },
            egui::Event::Ime(egui::ImeEvent::Commit("cx".into())),
        ];
        app.consume_sticky_text(&mut events, || panic!("Copy must not read the clipboard"));
        assert!(matches!(
            &events[0],
            egui::Event::Key {
                key: egui::Key::C,
                ..
            }
        ));
        assert!(
            matches!(&events[1], egui::Event::Ime(egui::ImeEvent::Commit(text)) if text.is_empty())
        );
        assert!(matches!(&events[2], egui::Event::Copy));
        assert!(matches!(&events[3], egui::Event::Text(text) if text == "x"));
        assert_eq!(app.sink.diagnostics.byte_count, 0);
        assert!(!app.control && !app.shift);

        app.control = true;
        app.shift = true;
        let mut events = vec![egui::Event::Text("v".into())];
        let mut reads = 0;
        app.consume_sticky_text(&mut events, || {
            reads += 1;
            Some("clipboard text".into())
        });
        assert_eq!(reads, 1);
        assert!(
            matches!(&events[..], [egui::Event::Text(empty), egui::Event::Paste(text)] if empty.is_empty() && text == "clipboard text")
        );
        assert_eq!(app.sink.diagnostics.byte_count, 0);
        assert!(!app.control && !app.shift);

        let mut events = vec![egui::Event::Key {
            key: egui::Key::V,
            physical_key: Some(egui::Key::V),
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::CTRL | egui::Modifiers::SHIFT,
        }];
        app.consume_sticky_text(&mut events, || None);
        assert!(events.is_empty());
        assert_eq!(app.input_notice, Some("The clipboard has no text to paste"));
        assert_eq!(app.sink.diagnostics.byte_count, 0);

        // Ctrl+Shift+P is no longer a paste shortcut; it stays ordinary input.
        app.control = true;
        app.shift = true;
        let mut events = vec![egui::Event::Key {
            key: egui::Key::P,
            physical_key: Some(egui::Key::P),
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::CTRL | egui::Modifiers::SHIFT,
        }];
        app.consume_sticky_text(&mut events, || panic!("P must not read the clipboard"));
        assert!(!events
            .iter()
            .any(|event| matches!(event, egui::Event::Paste(_))));
    }

    #[test]
    fn mobile_clipboard_shortcut_paste_reaches_the_encoder_before_trailing_text() {
        #[derive(Default)]
        struct Sink(Vec<u8>);
        impl EncodedInputSink for Sink {
            fn record_encoded_input(&mut self, bytes: &[u8]) {
                self.0.extend_from_slice(bytes);
            }
        }
        let mut app = MobileApp::new_terminal(Rc::new(Cell::new(Lifecycle {
            active: true,
            ..Default::default()
        })));
        app.control = true;
        app.shift = true;
        app.terminal.ingest(b"\x1b[?2004h");
        let mut events = vec![egui::Event::Text("vx".into())];
        app.consume_sticky_text(&mut events, || Some("pasted".into()));
        let mut sink = Sink::default();
        let ctx = egui::Context::default();
        ctx.run_ui(egui::RawInput::default(), |ui| {
            app.view.show(ui, &mut app.terminal, &mut sink);
        })
        .drop_without_applying_deltas();
        ctx.run_ui(
            egui::RawInput {
                events,
                ..Default::default()
            },
            |ui| {
                app.view.show(ui, &mut app.terminal, &mut sink);
            },
        )
        .drop_without_applying_deltas();
        assert_eq!(sink.0, b"\x1b[200~pasted\x1b[201~x");
    }

    #[test]
    fn privacy_mode_keeps_terminal_text_out_of_the_accessibility_tree() {
        use egui_kittest::{kittest::Queryable, Harness};
        use festerm_ios_lock::{
            AppLock, Availability, BiometryKind, FakeLocker, LockSettings, ManualClock,
        };

        for (lock_enabled, hide_preview, exposed) in [
            (None, false, true),
            (Some(true), false, true),
            (Some(true), true, false),
        ] {
            let mut app = MobileApp::new_terminal(Rc::new(Cell::new(Lifecycle {
                active: true,
                ..Default::default()
            })));
            if let Some(enabled) = lock_enabled {
                let locker = FakeLocker::new(Availability::Available(BiometryKind::FaceId));
                let clock = std::sync::Arc::new(ManualClock::new());
                let lock = AppLock::new(None, locker, clock, || {});
                lock.apply_settings(LockSettings {
                    enabled,
                    hide_app_switcher_preview: hide_preview,
                    ..LockSettings::default()
                })
                .unwrap();
                app = app.with_lock(lock, None);
            }
            app.terminal.ingest(b"\x1b[2J\x1b[Hsecret output");
            let mut harness = Harness::builder()
                .with_size(egui::vec2(390.0, 844.0))
                .build_ui_state(|ui, app: &mut MobileApp| app.show(ui), app);
            harness.run();
            let visible = harness
                .query_by_role(egui::accesskit::Role::Terminal)
                .is_some()
                && harness
                    .query_all_by_role(egui::accesskit::Role::Label)
                    .any(|node| node.value().as_deref() == Some("secret output"));
            assert_eq!(
                visible, exposed,
                "lock {lock_enabled:?}, hide {hide_preview}"
            );
        }
    }

    #[test]
    fn mobile_ctrl_shift_copy_uses_selected_text_without_remote_input() {
        use egui_kittest::{kittest::Queryable, Harness};

        let mut app = MobileApp::new_terminal(Rc::new(Cell::new(Lifecycle {
            active: true,
            ..Default::default()
        })));
        app.terminal.ingest(b"\x1b[2J\x1b[Hcopy this text");
        let mut harness = Harness::builder()
            .with_size(egui::vec2(390.0, 844.0))
            .build_ui_state(
                |ui, state: &mut (MobileApp, Vec<String>)| {
                    state.0.show_with_occlusion(ui, 0.35);
                    ui.ctx().output(|output| {
                        for command in &output.commands {
                            if let egui::OutputCommand::CopyText(text) = command {
                                state.1.push(text.clone());
                            }
                        }
                    });
                },
                (app, Vec::new()),
            );
        harness.run();
        let grid = harness.state().0.view.diagnostics().grid_rect.unwrap();
        let dimensions = harness.state().0.terminal.dimensions();
        let cell = egui::vec2(
            grid.width() / dimensions.columns() as f32,
            grid.height() / dimensions.rows() as f32,
        );
        let start = grid.min + egui::vec2(cell.x * 0.2, cell.y * 0.5);
        let end = start + egui::vec2(cell.x * 3.0, 0.0);
        for (point, pressed) in [(start, true), (end, false)] {
            harness.event(egui::Event::PointerMoved(point));
            harness.event(egui::Event::PointerButton {
                pos: point,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            });
            harness.run();
        }
        let selected = harness
            .state()
            .0
            .view
            .selected_text(&harness.state().0.terminal)
            .expect("selected fixture text");
        let bytes = harness.state().0.sink.diagnostics.byte_count;
        harness.get_by_label("Ctrl").click();
        harness.run();
        harness.get_by_label("Shift").click();
        harness.run();
        harness.event(egui::Event::Key {
            key: egui::Key::C,
            physical_key: Some(egui::Key::C),
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
        harness.event(egui::Event::Text("c".into()));
        harness.run();
        assert_eq!(harness.state().1, [selected]);
        assert_eq!(harness.state().0.sink.diagnostics.byte_count, bytes);
        assert!(!harness.state().0.control && !harness.state().0.shift);
    }

    #[test]
    fn mobile_force_redraw_menu_rebuilds_without_resetting_or_sending_input() {
        use egui_kittest::{kittest::Queryable, Harness};

        let app = Rc::new(RefCell::new(MobileApp::new_terminal(Rc::new(Cell::new(
            Lifecycle {
                active: true,
                ..Default::default()
            },
        )))));
        app.borrow_mut().view.zoom_by_factor(1.2);
        let displayed = app.clone();
        let mut harness = Harness::builder()
            .with_size(egui::vec2(390.0, 844.0))
            .build_ui_state(
                move |ui, _: &mut ()| displayed.borrow_mut().show_with_occlusion(ui, 0.35),
                (),
            );
        harness.run();
        let before = app.borrow().terminal.clone();
        let font_size = app.borrow().view.font_size_points();
        let bytes = app.borrow().sink.diagnostics.byte_count;
        let redrawn = std::sync::Arc::new(std::sync::Mutex::new(0));
        let observed = redrawn.clone();
        festerm_ui_egui::install_root_terminal_painter(&harness.ctx, move |_, frame| {
            if frame.full_redraw {
                *observed.lock().unwrap() += 1;
            }
            None
        });
        harness.get_by_label("More").click();
        harness.run();
        harness.get_by_label("Force redraw").click();
        harness.run();
        assert_eq!(*redrawn.lock().unwrap(), 1);
        let app = app.borrow();
        assert_eq!(app.terminal, before);
        assert_eq!(app.view.font_size_points(), font_size);
        assert_eq!(app.sink.diagnostics.byte_count, bytes);
    }

    #[test]
    fn mobile_workspaces_limit_native_keyboard_and_terminal_gestures_to_terminal() {
        let lifecycle = Rc::new(Cell::new(Lifecycle {
            active: true,
            ..Default::default()
        }));
        let mut app = MobileApp::new_terminal(lifecycle);
        let ctx = egui::Context::default();
        let render = |app: &mut MobileApp, events| {
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(390.0, 844.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ui| app.show_with_occlusion(ui, 0.0),
            )
        };
        render(&mut app, vec![]).drop_without_applying_deltas();
        let output = render(&mut app, vec![]);
        assert!(output.platform_output.ime.is_some());
        output.drop_without_applying_deltas();
        render(
            &mut app,
            vec![egui::Event::Ime(egui::ImeEvent::Preedit {
                text: "composing".into(),
                active_range_chars: None,
            })],
        )
        .drop_without_applying_deltas();

        app.control = true;
        app.switch_workspace(workspaces::Workspace::Files);
        assert!(!app.control);
        let bytes_after_focus_out = app.sink.diagnostics.byte_count;
        let output = render(&mut app, vec![]);
        assert!(output.platform_output.ime.is_none());
        assert!(!output.shapes.is_empty());
        output.drop_without_applying_deltas();

        app.switch_workspace(workspaces::Workspace::Markdown);
        let output = render(&mut app, vec![]);
        assert!(output.platform_output.ime.is_none());
        assert!(!output.shapes.is_empty());
        output.drop_without_applying_deltas();

        app.switch_workspace(workspaces::Workspace::Terminal);
        render(&mut app, vec![]).drop_without_applying_deltas();
        let output = render(
            &mut app,
            vec![egui::Event::Key {
                key: egui::Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        assert!(output.platform_output.ime.is_some());
        output.drop_without_applying_deltas();
        assert_eq!(
            app.sink.diagnostics.byte_count,
            bytes_after_focus_out + 1,
            "abandoning preedit must let terminal keys route after returning"
        );
    }

    #[test]
    fn mobile_lifecycle_is_idempotent_and_counts_resume_after_suspend() {
        let mut state = Lifecycle::default();
        state.suspend();
        state.resume();
        state.resume();
        state.suspend();
        state.suspend();
        state.memory_warning();
        state.resume();
        assert_eq!(
            state,
            Lifecycle {
                active: true,
                resumes: 2,
                suspensions: 1,
                memory_warnings: 1
            }
        );
    }

    #[test]
    fn mobile_probe_uses_core_encoding_without_echoing_or_retaining_input() {
        let mut terminal = fixture_terminal();
        let before = terminal.row_text(terminal.dimensions().rows() - 2);
        let mut sink = ProbeSink::default();
        let route = route_input(&mut terminal, InputEvent::Key(Key::ArrowUp), &mut sink);
        assert_eq!(route.delivered_bytes, 3);
        assert_eq!(sink.diagnostics.byte_count, 3);
        assert_eq!(sink.diagnostics.event_count, 1);
        assert!(terminal.queued_input().is_empty());
        assert_eq!(terminal.row_text(terminal.dimensions().rows() - 2), before);
    }

    #[test]
    fn mobile_fixture_renders_at_phone_width_and_preserves_grid_on_memory_warning() {
        let lifecycle = Rc::new(Cell::new(Lifecycle {
            active: true,
            ..Default::default()
        }));
        let mut app = MobileApp::new_terminal(lifecycle.clone());
        let ctx = egui::Context::default();
        let render = |app: &mut MobileApp| {
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(390.0, 844.0),
                    )),
                    safe_area_insets: Some(egui::SafeAreaInsets(egui::epaint::MarginF32 {
                        top: 59.0,
                        bottom: 34.0,
                        ..Default::default()
                    })),
                    ..Default::default()
                },
                |ui| app.show(ui),
            )
        };
        render(&mut app).drop_without_applying_deltas();
        let output = render(&mut app);
        assert!(!output.shapes.is_empty());
        output.drop_without_applying_deltas();
        assert!(app.terminal.dimensions().columns() < 80);
        let grid = app.view.diagnostics().grid_rect.unwrap();
        assert!(grid.top() >= 59.0);
        assert!(grid.bottom() <= 844.0 - 34.0);
        let before = app.terminal.row_text(0);
        let mut state = lifecycle.get();
        state.memory_warning();
        lifecycle.set(state);
        render(&mut app).drop_without_applying_deltas();
        assert_eq!(app.terminal.row_text(0), before);
        assert_eq!(app.observed_memory_warnings, 1);
    }
}

#[cfg(test)]
thread_local! {
    static TEST_CLIPBOARD: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

#[cfg(all(test, feature = "live-ssh"))]
fn set_test_clipboard(text: Option<&str>) {
    TEST_CLIPBOARD.with(|clipboard| *clipboard.borrow_mut() = text.map(str::to_owned));
}

fn clipboard_text() -> Option<String> {
    #[cfg(test)]
    if let Some(text) = TEST_CLIPBOARD.with(|clipboard| clipboard.borrow().clone()) {
        return Some(text);
    }
    festerm_ios_window::clipboard_text()
}

/// Replays an edit chosen from a form field's menu as the clipboard or
/// keyboard event egui's `TextEdit` already understands.
/// Replays native keyboard edits into the focused form field as the events
/// egui's `TextEdit` handles. A selection change leads its batch and is set
/// on the field's state directly, before the field runs this frame.
fn apply_field_input(ctx: &egui::Context, inputs: Vec<festerm_ios_window::FieldInput>) {
    use festerm_ios_window::FieldInput;
    let Some(field) = ctx.memory(|memory| memory.focused()) else {
        return;
    };
    let key = |key| {
        [true, false].map(|pressed| egui::Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        })
    };
    let mut events = Vec::new();
    for input in inputs {
        match input {
            FieldInput::Unavailable(_) => {}
            FieldInput::Key(native) => {
                use festerm_ios_window::FieldKey;
                events.extend(key(match native {
                    FieldKey::Tab => egui::Key::Tab,
                    FieldKey::Escape => egui::Key::Escape,
                    FieldKey::Left => egui::Key::ArrowLeft,
                    FieldKey::Right => egui::Key::ArrowRight,
                    FieldKey::Up => egui::Key::ArrowUp,
                    FieldKey::Down => egui::Key::ArrowDown,
                    FieldKey::Home => egui::Key::Home,
                    FieldKey::End => egui::Key::End,
                    FieldKey::Delete => egui::Key::Delete,
                }));
            }
            FieldInput::Select(range) => {
                let mut state = egui::TextEdit::load_state(ctx, field).unwrap_or_default();
                state
                    .cursor
                    .set_char_range(Some(egui::text::CCursorRange::two(
                        egui::text::CCursor::new(range.start),
                        egui::text::CCursor::new(range.end),
                    )));
                state.store(ctx, field);
            }
            FieldInput::Insert(text) if text == "\n" => events.extend(key(egui::Key::Enter)),
            FieldInput::Insert(text) => events.push(egui::Event::Text(text)),
            FieldInput::DeleteBackward => events.extend(key(egui::Key::Backspace)),
            FieldInput::Preedit { text, selected } => {
                events.push(egui::Event::Ime(egui::ImeEvent::Preedit {
                    text,
                    active_range_chars: Some(selected),
                }));
            }
            FieldInput::Commit(text) => events.push(egui::Event::Ime(egui::ImeEvent::Commit(text))),
        }
    }
    ctx.input_mut(|input| input.events.extend(events));
}

fn apply_field_edit(ctx: &egui::Context) {
    let Some(edit) = widgets::take_field_edit(ctx) else {
        return;
    };
    let event = match edit {
        widgets::FieldEdit::Paste => match clipboard_text() {
            Some(text) if !text.is_empty() => egui::Event::Paste(text),
            _ => return,
        },
        widgets::FieldEdit::Copy => egui::Event::Copy,
        widgets::FieldEdit::Cut => egui::Event::Cut,
        widgets::FieldEdit::SelectAll => egui::Event::Key {
            key: egui::Key::A,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::COMMAND,
        },
    };
    ctx.input_mut(|input| input.events.push(event));
}
