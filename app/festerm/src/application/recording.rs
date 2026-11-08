use std::{
    path::PathBuf,
    sync::{mpsc, Arc},
    thread,
};

use eframe::egui;
use festerm_recording::{
    Completion, ExportDestination, FinishReason, RecordingFont, RecordingId, RecordingLease,
    RecordingManifest, RecordingSettings, RecordingSourceState, SharedRecording,
};
use festerm_recording_export::{
    DirectoryBackend, ExportBackend, ExportCancellation, ExportControl, ExportPreview,
    ExportResult, ExportTask, LibraryResult, LibrarySnapshot, LibraryTask, PrepareTask,
    PreparedCapture, PreviewTask, PreviewedExport, PublicationRequest, PublishedRecording,
};

use super::{FesTermApplication, WindowId};
use crate::{
    recording_privacy::{PrivacyGuard, PrivacyTask},
    recording_storage::{OpenTask, RecordingStorage},
    tabs::TabId,
};

#[path = "recording_ui.rs"]
mod ui;
use ui::{Action, DeleteKind, Dialog, PickerTarget, SessionChoice};

type Wake = Arc<dyn Fn() + Send + Sync>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Request {
    Open,
    Settings,
    Library,
    Stop,
    CancelExport,
    IncludeActive,
    Dismiss,
}

#[derive(Clone, Default)]
pub(crate) struct WindowView {
    pub active: Option<RecordingId>,
    pub auto_include: Option<SharedRecording>,
    pub label: Option<String>,
    pub exporting: bool,
    pub preparing: bool,
    pub busy: bool,
    pub modal: bool,
}

pub(super) struct DesktopRecording {
    auto_open: bool,
    attempted_open: bool,
    open: Option<OpenTask>,
    storage: Option<RecordingStorage>,
    defaults: RecordingSettings,
    settings_saving: Option<bool>,
    library_task: Option<LibraryTask>,
    library: Option<LibrarySnapshot>,
    prepare: Option<PrepareTask>,
    privacy_task: Option<PrivacyTask>,
    privacy: Option<PrivacyGuard>,
    #[cfg(target_os = "linux")]
    wayland_backend: bool,
    starting: Option<Start>,
    recording: Option<SharedRecording>,
    duration_watch: Option<DurationWatch>,
    quit_waiting_until: Option<std::time::Instant>,
    quit_blocked: bool,
    include_new: Option<RecordingId>,
    manifest: Option<RecordingManifest>,
    lease: Option<Arc<RecordingLease>>,
    pending_export: Option<PendingExport>,
    preview_task: Option<PreviewTask>,
    preview: Option<ExportPreview>,
    export: Option<ExportTask>,
    picker: Option<mpsc::Receiver<Result<Option<PathBuf>, String>>>,
    picker_target: Option<PickerTarget>,
    dialog: Option<Dialog>,
    pub(super) owner: WindowId,
    pending_open: Option<(WindowId, Request)>,
    action: Option<Action>,
    status: Option<String>,
}

struct Start {
    selected: Vec<SessionIdentity>,
    include_new: bool,
    error: Option<String>,
}

struct PendingExport {
    path: PathBuf,
    manifest: RecordingManifest,
    lease: Arc<RecordingLease>,
}

struct SessionIdentity {
    tab: TabId,
    instance: u64,
    generation: u64,
}

struct DurationWatch {
    cancel: mpsc::SyncSender<()>,
}

impl DurationWatch {
    fn start(recording: SharedRecording) -> Result<Self, String> {
        let (cancel, receive) = mpsc::sync_channel(1);
        thread::Builder::new()
            .name("festerm-recording-limit".to_owned())
            .spawn(move || {
                if matches!(
                    receive.recv_timeout(
                        festerm_recording::MAX_DURATION + std::time::Duration::from_millis(1)
                    ),
                    Err(mpsc::RecvTimeoutError::Timeout)
                ) {
                    if let Err(error) = recording.check_duration() {
                        tracing::warn!("Recording duration limit reached: {error}");
                    }
                }
            })
            .map_err(|error| format!("Recording duration monitor could not start: {error}"))?;
        Ok(Self { cancel })
    }
}

impl Drop for DurationWatch {
    fn drop(&mut self) {
        let _ = self.cancel.try_send(());
    }
}

impl SessionIdentity {
    fn capture(app: &FesTermApplication, tab: TabId) -> Option<Self> {
        let session = app.recording_session(tab)?;
        Some(Self {
            tab,
            instance: session.controller.automation_instance()?,
            generation: session.controller.lifecycle_generation(),
        })
    }

    fn matches(&self, app: &FesTermApplication) -> bool {
        app.recording_session(self.tab).is_some_and(|session| {
            session.controller.automation_instance() == Some(self.instance)
                && session.controller.lifecycle_generation() == self.generation
        })
    }
}

impl Default for DesktopRecording {
    fn default() -> Self {
        Self::new(false)
    }
}

impl DesktopRecording {
    #[cfg(target_os = "linux")]
    pub(super) fn set_wayland_backend(&mut self, available: bool) {
        if self.wayland_backend == available {
            return;
        }
        self.wayland_backend = available;
        if available {
            return;
        }
        if let Some(task) = &self.privacy_task {
            task.cancel();
        }
        if let Some(task) = &self.prepare {
            task.cancel();
        }
        if let Some(start) = &mut self.starting {
            start.error = Some("The Wayland display is unavailable".to_owned());
        }
        self.privacy = None;
        if self.recording.is_some()
            || self.starting.is_some()
            || matches!(self.dialog, Some(Dialog::Start { .. }))
        {
            self.status = Some("Recording requires a verified GNOME Wayland display".to_owned());
        }
    }

    pub(super) fn new(auto_open: bool) -> Self {
        Self {
            auto_open,
            attempted_open: false,
            open: None,
            storage: None,
            defaults: RecordingSettings::default(),
            settings_saving: None,
            library_task: None,
            library: None,
            prepare: None,
            privacy_task: None,
            privacy: None,
            #[cfg(target_os = "linux")]
            wayland_backend: false,
            starting: None,
            recording: None,
            duration_watch: None,
            quit_waiting_until: None,
            quit_blocked: false,
            include_new: None,
            manifest: None,
            lease: None,
            pending_export: None,
            preview_task: None,
            preview: None,
            export: None,
            picker: None,
            picker_target: None,
            dialog: None,
            owner: WindowId::PRIMARY,
            pending_open: None,
            action: None,
            status: None,
        }
    }

    fn wake(context: &egui::Context) -> Wake {
        let context = context.clone();
        Arc::new(move || context.request_repaint())
    }

    fn open_storage(&mut self, context: &egui::Context) {
        if self.open.is_some() || self.storage.is_some() {
            return;
        }
        self.attempted_open = true;
        let wake = Self::wake(context);
        match OpenTask::start(move || wake()) {
            Ok(task) => self.open = Some(task),
            Err(error) => {
                self.status = Some(error);
                self.pending_open = None;
            }
        }
    }

    fn poll(&mut self, app: &mut FesTermApplication, context: &egui::Context) {
        if self.auto_open && !self.attempted_open {
            self.open_storage(context);
        }
        if let Some(result) = self.open.as_ref().and_then(OpenTask::poll) {
            self.open = None;
            match result {
                Ok((storage, settings)) => {
                    self.defaults = settings;
                    self.storage = Some(storage);
                    self.discover(context);
                    if let Some((owner, request)) = self.pending_open.take() {
                        self.request(app, owner, request, context);
                    }
                }
                Err(error) => {
                    self.status = Some(error);
                    self.pending_open = None;
                }
            }
        }
        if let Some(result) = self.storage.as_mut().and_then(RecordingStorage::poll) {
            let saved = self.settings_saving.take().unwrap_or(false);
            match result {
                Ok(settings) => {
                    self.defaults = settings.clone();
                    if let Some(Dialog::Settings { draft }) = &mut self.dialog {
                        *draft = settings;
                    }
                    self.status = Some(if saved {
                        "Recording settings saved on this device".to_owned()
                    } else {
                        "Recording settings reloaded".to_owned()
                    });
                }
                Err(error) => self.status = Some(error),
            }
        }
        if let Some(result) = self.library_task.as_ref().and_then(LibraryTask::poll) {
            self.library_task = None;
            let mut needs_scan = false;
            match result {
                Ok(LibraryResult::Discovered(true)) => {
                    self.status = Some(
                        "Retained or unfinished recordings are available in the recording library"
                            .to_owned(),
                    );
                    needs_scan = true;
                }
                Ok(LibraryResult::Discovered(false)) => needs_scan = true,
                Ok(LibraryResult::Snapshot(snapshot)) => self.library = Some(snapshot),
                Ok(LibraryResult::Changed(message)) => {
                    self.status = Some(message);
                    needs_scan = true;
                }
                Err(error) => self.status = Some(error),
            }
            if needs_scan && matches!(self.dialog, Some(Dialog::Library)) && self.library.is_none()
            {
                self.scan_library(context);
            }
        }
        if let Some(result) = self
            .picker
            .as_ref()
            .and_then(|receiver| match receiver.try_recv() {
                Ok(result) => Some(result),
                Err(mpsc::TryRecvError::Empty) => None,
                Err(mpsc::TryRecvError::Disconnected) => Some(Err(
                    "The folder picker stopped unexpectedly; check system picker availability"
                        .to_owned(),
                )),
            })
        {
            self.picker = None;
            let target = self.picker_target.take();
            match result {
                Ok(Some(path)) => {
                    if let Some(settings) = self
                        .dialog
                        .as_mut()
                        .and_then(|dialog| dialog.settings_for_picker(target))
                    {
                        settings.destination = Some(ExportDestination::DesktopPath(path));
                    } else {
                        self.status = Some(
                            "The chosen folder was not applied because the recording dialog closed"
                                .to_owned(),
                        );
                    }
                }
                Ok(None) => {
                    self.status = Some(
                        "No recording folder was chosen. If the picker did not open, check the system file dialog or desktop portal."
                            .to_owned(),
                    );
                }
                Err(error) => self.status = Some(error),
            }
        }
        self.poll_start(app, context);
        self.poll_capture(app, context);
        if !context.input(|input| input.pointer.any_down()) {
            if let Some(result) = self.preview_task.as_ref().and_then(PreviewTask::poll) {
                self.preview_task = None;
                match result {
                    Ok(preview) => {
                        self.preview = Some(preview);
                        self.status = None;
                    }
                    Err(error) => self.status = Some(error),
                }
            }
        } else if self.preview_task.is_some() {
            context.request_repaint_after(std::time::Duration::from_millis(50));
        }
        if let Some(result) = self.export.as_ref().and_then(ExportTask::poll) {
            self.export = None;
            self.status = Some(match result {
                Ok(ExportResult {
                    directory,
                    videos,
                    retained_source,
                    recovered_prefix,
                }) => format!(
                    "Exported {}{} video(s) to {directory}{}",
                    if recovered_prefix {
                        "recovered prefix as "
                    } else {
                        ""
                    },
                    videos.len(),
                    if retained_source {
                        "; the private source was retained"
                    } else {
                        ""
                    }
                ),
                Err(error) => error,
            });
        }
        if let Some(action) = self.action.take() {
            self.handle_action(app, action, context);
        }
        if !app.windows.iter().any(|window| window.id == self.owner) {
            self.owner = WindowId::PRIMARY;
        }
        if self.dialog.is_none()
            && self.pending_export.is_some()
            && app
                .windows
                .iter()
                .find(|window| window.id == self.owner)
                .is_some_and(|window| !window.app.document_activation_blocked())
        {
            self.dialog = self.pending_export.as_ref().map(|pending| Dialog::Export {
                draft: pending.manifest.settings().clone(),
            });
        }
    }

    fn discover(&mut self, context: &egui::Context) {
        let Some(storage) = &self.storage else { return };
        if self.library_task.is_some() || self.busy() {
            return;
        }
        match LibraryTask::discover(storage.path().to_owned(), Self::wake(context)) {
            Ok(task) => self.library_task = Some(task),
            Err(error) => self.status = Some(error),
        }
    }

    fn scan_library(&mut self, context: &egui::Context) {
        let Some(storage) = &self.storage else { return };
        if self.library_task.is_some() || self.busy() {
            return;
        }
        self.library = None;
        match LibraryTask::scan(storage.path().to_owned(), Self::wake(context)) {
            Ok(task) => self.library_task = Some(task),
            Err(error) => self.status = Some(error),
        }
    }

    fn busy(&self) -> bool {
        self.starting.is_some()
            || self.recording.is_some()
            || self.pending_export.is_some()
            || self.export.is_some()
    }

    fn request(
        &mut self,
        app: &mut FesTermApplication,
        owner: WindowId,
        request: Request,
        context: &egui::Context,
    ) {
        match request {
            Request::Stop => {
                if let Some(recording) = &self.recording {
                    match recording.stop(FinishReason::User) {
                        Ok(_) => self.status = Some("Finalizing recording".to_owned()),
                        Err(error) => {
                            self.status = Some(format!("Could not stop recording: {error}"))
                        }
                    }
                } else if let Some(task) = &self.prepare {
                    task.cancel();
                    if let Some(task) = &self.privacy_task {
                        task.cancel();
                    }
                    if let Some(start) = &mut self.starting {
                        start.error = Some("Recording preparation was cancelled".to_owned());
                    }
                    self.privacy = None;
                }
            }
            Request::CancelExport => {
                if let Some(task) = &self.export {
                    self.status = Some(if task.cancel() {
                        "Cancelling export; the source will be retained".to_owned()
                    } else {
                        "Export publication is complete; source retention is already being finalized"
                            .to_owned()
                    });
                }
            }
            Request::IncludeActive => {
                if let Some(window) = app.windows.iter().find(|window| window.id == owner) {
                    self.include(app, window.app.recording_active_tab());
                }
            }
            Request::Dismiss => self.status = None,
            Request::Open | Request::Settings | Request::Library => {
                if self.storage.is_none() {
                    self.pending_open = Some((owner, request));
                    self.status = Some("Opening private recording storage".to_owned());
                    self.open_storage(context);
                    return;
                }
                if self.dialog.is_some() {
                    return;
                }
                if app
                    .windows
                    .iter()
                    .find(|window| window.id == owner)
                    .is_none_or(|window| window.app.document_activation_blocked())
                {
                    self.status =
                        Some("Close the current dialog before opening recordings".to_owned());
                    return;
                }
                self.owner = owner;
                match request {
                    Request::Open if self.recording_id().is_some() => {
                        self.dialog = Some(Dialog::include());
                    }
                    Request::Open if !self.busy() => {
                        self.dialog = Some(Dialog::start(
                            self.defaults.clone(),
                            app.recording_choices(self.recording_id(), owner),
                        ));
                        #[cfg(target_os = "linux")]
                        if !self.wayland_backend {
                            self.status =
                                Some("Recording requires a verified GNOME Wayland display".into());
                            return;
                        }
                        match PrivacyTask::start(Self::wake(context)) {
                            Ok(task) => {
                                self.privacy_task = Some(task);
                                self.status = Some("Starting privacy monitoring".to_owned());
                            }
                            Err(error) => self.status = Some(error),
                        }
                    }
                    Request::Settings => {
                        self.dialog = Some(Dialog::Settings {
                            draft: self.defaults.clone(),
                        });
                    }
                    Request::Library if !self.busy() => {
                        self.dialog = Some(Dialog::Library);
                        self.scan_library(context);
                    }
                    _ => {
                        self.status =
                            Some("Wait for the current recording or export to finish".to_owned())
                    }
                }
            }
        }
    }

    pub(super) fn recording_id(&self) -> Option<RecordingId> {
        self.recording
            .as_ref()
            .filter(|recording| matches!(recording.is_recording(), Ok(true)))
            .map(SharedRecording::id)
    }

    fn view(&self, owner: WindowId) -> WindowView {
        let active = self.recording_id();
        let label = if active.is_some() {
            Some(match &self.status {
                Some(status) => format!("Recording · {status}"),
                None => "Recording terminal output".to_owned(),
            })
        } else if self.recording.is_some() {
            Some(match &self.status {
                Some(status) => format!("Finalizing recording · {status}"),
                None => "Finalizing recording".to_owned(),
            })
        } else if self.starting.is_some() {
            Some("Preparing recording".to_owned())
        } else if let Some(export) = &self.export {
            let (stream, done, total) = export.progress();
            Some(format!(
                "Exporting recording stream {stream}: {done}/{total} frames"
            ))
        } else {
            self.status.clone()
        };
        WindowView {
            active,
            auto_include: self
                .recording
                .as_ref()
                .filter(|recording| active == Some(recording.id()) && self.include_new == active)
                .cloned(),
            label,
            exporting: self.export.is_some(),
            preparing: self.starting.is_some(),
            busy: self.busy(),
            modal: self.dialog.is_some() && self.owner == owner,
        }
    }

    fn begin(
        &mut self,
        app: &FesTermApplication,
        selected: Vec<TabId>,
        settings: RecordingSettings,
        include_new: bool,
        context: &egui::Context,
    ) {
        if self.busy() || self.library_task.is_some() {
            self.status =
                Some("Wait for the current recording library operation to finish".to_owned());
            return;
        }
        if selected.len() > festerm_recording::MAX_STREAMS {
            self.status = Some(format!(
                "Choose at most {} sessions for one recording",
                festerm_recording::MAX_STREAMS
            ));
            return;
        }
        let identities: Option<Vec<_>> = selected
            .iter()
            .map(|id| SessionIdentity::capture(app, *id))
            .collect();
        let Some(identities) = identities.filter(|identities| !identities.is_empty()) else {
            self.status = Some("Select at least one currently running session".to_owned());
            return;
        };
        if let Some(error) = identities.iter().find_map(|identity| {
            app.recording_session(identity.tab).and_then(|session| {
                session
                    .view
                    .validate_recording_view(&session.terminal)
                    .err()
            })
        }) {
            self.status = Some(error.to_owned());
            return;
        }
        let Some(storage) = &self.storage else {
            self.status = Some("Private recording storage is unavailable".to_owned());
            return;
        };
        match settings.validate() {
            Ok(validated)
                if matches!(
                    validated.destination,
                    Some(ExportDestination::DesktopPath(_))
                ) => {}
            Ok(_) => {
                self.status = Some("Choose an export folder in recording options first".to_owned());
                return;
            }
            Err(error) => {
                self.status = Some(error);
                return;
            }
        }
        if self.privacy.as_ref().is_none_or(PrivacyGuard::is_lost) {
            self.status = Some(
                "Privacy monitoring is unavailable; close and reopen recording consent".into(),
            );
            return;
        }
        let wake = Self::wake(context);
        let prepare = match PrepareTask::start(
            storage.path().to_owned(),
            settings,
            |destination| DirectoryBackend.preflight(destination),
            wake,
        ) {
            Ok(task) => task,
            Err(error) => {
                self.status = Some(error);
                return;
            }
        };
        self.starting = Some(Start {
            selected: identities,
            include_new,
            error: None,
        });
        self.prepare = Some(prepare);
        self.status = None;
        self.dialog = None;
    }

    fn poll_start(&mut self, app: &mut FesTermApplication, context: &egui::Context) {
        if let Some(result) = self.privacy_task.as_ref().and_then(PrivacyTask::poll) {
            self.privacy_task = None;
            match result {
                Ok(guard)
                    if self
                        .starting
                        .as_ref()
                        .is_some_and(|start| start.error.is_none())
                        || (self.starting.is_none()
                            && matches!(self.dialog, Some(Dialog::Start { .. }))) =>
                {
                    self.privacy = Some(guard);
                    if self.starting.is_none() {
                        self.status = None;
                    }
                }
                Ok(guard) => drop(guard),
                Err(error) => {
                    if let Some(start) = &mut self.starting {
                        if start.error.is_none() {
                            start.error = Some(error);
                        }
                    } else {
                        self.status = Some(error);
                    }
                    if let Some(task) = &self.prepare {
                        task.cancel();
                    }
                }
            }
        }
        if self.privacy.as_ref().is_some_and(PrivacyGuard::is_lost) {
            let error = "Recording stopped because device privacy changed".to_owned();
            if let Some(start) = &mut self.starting {
                start.error.get_or_insert_with(|| error.clone());
                if let Some(task) = &self.prepare {
                    task.cancel();
                }
            } else if matches!(self.dialog, Some(Dialog::Start { .. })) {
                self.status = Some(format!(
                    "{error}; close and reopen recording consent to try again"
                ));
            }
            self.privacy = None;
        }
        if self.starting.is_none() || self.privacy_task.is_some() {
            return;
        }
        let Some(result) = self.prepare.as_mut().and_then(PrepareTask::poll) else {
            return;
        };
        self.prepare = None;
        let Some(start) = self.starting.take() else {
            return;
        };
        match result {
            Ok(prepared) if start.error.is_none() && self.privacy.is_some() => {
                self.start_capture(app, start.selected, start.include_new, prepared, context);
            }
            Ok(_) => {
                self.privacy = None;
                self.status = Some(format!(
                    "{}; check the recording library for unfinished private files",
                    start
                        .error
                        .unwrap_or_else(|| "Recording preparation failed".to_owned())
                ));
            }
            Err(error) => {
                self.privacy = None;
                self.status = Some(match start.error {
                    Some(reason) if error != "Recording preparation was cancelled" => {
                        format!("{reason}; {error}")
                    }
                    Some(reason) => reason,
                    None => error,
                });
            }
        }
    }

    fn start_capture(
        &mut self,
        app: &mut FesTermApplication,
        selected: Vec<SessionIdentity>,
        include_new: bool,
        prepared: PreparedCapture,
        context: &egui::Context,
    ) {
        let PreparedCapture {
            source,
            manifest,
            lease,
        } = prepared;
        let Some(privacy) = self.privacy.take() else {
            return;
        };
        if selected.iter().any(|identity| !identity.matches(app)) {
            self.status = Some(
                "A selected session changed before recording started; check the recording library for unfinished private files".to_owned(),
            );
            return;
        }
        if let Some(error) = selected.iter().find_map(|identity| {
            app.recording_session(identity.tab).and_then(|session| {
                session
                    .view
                    .validate_recording_view(&session.terminal)
                    .err()
            })
        }) {
            self.status = Some(format!(
                "{error}. Check the recording library for unfinished private files"
            ));
            return;
        }
        let terminals: Option<Vec<_>> = selected
            .iter()
            .map(|identity| {
                app.recording_session(identity.tab)
                    .map(|session| &session.terminal)
            })
            .collect();
        let Some(terminals) = terminals else {
            self.status = Some(
                "A selected session changed before recording started; check the recording library for unfinished private files".to_owned(),
            );
            return;
        };
        let writer_lease = lease.clone();
        let repaint = Self::wake(context);
        let wake: Wake = Arc::new(move || {
            let _keep_lease = &writer_lease;
            repaint();
        });
        match SharedRecording::start_prepared_with_privacy_loss_flag(
            source,
            &terminals,
            privacy.loss_flag(),
            move || wake(),
        ) {
            Ok((recording, streams)) => {
                self.manifest = Some(manifest);
                self.lease = Some(lease);
                self.recording = Some(recording.clone());
                if let Err(error) = privacy.bind(&recording) {
                    if let Err(stop_error) =
                        recording.fail_integrity("Recording privacy monitor failed")
                    {
                        tracing::error!(
                            "Could not stop recording after privacy failure: {stop_error}"
                        );
                    }
                    self.status = Some(error);
                } else {
                    for (identity, stream) in selected.into_iter().zip(streams) {
                        if !app.attach_recording(identity.tab, stream) {
                            if let Err(error) = recording.fail_integrity("Selected session changed")
                            {
                                tracing::error!(
                                    "Could not stop recording after session change: {error}"
                                );
                            }
                            self.status = Some(
                                "A selected session changed while recording started".to_owned(),
                            );
                            break;
                        }
                    }
                    if include_new && matches!(recording.is_recording(), Ok(true)) {
                        self.include_new = Some(recording.id());
                    }
                    if matches!(recording.is_recording(), Ok(true)) {
                        match DurationWatch::start(recording.clone()) {
                            Ok(watch) => self.duration_watch = Some(watch),
                            Err(error) => {
                                if let Err(stop_error) = recording
                                    .fail_integrity("Recording duration monitor could not start")
                                {
                                    tracing::error!("Could not stop recording after monitor failure: {stop_error}");
                                }
                                self.status = Some(error);
                            }
                        }
                    }
                }
                self.privacy = Some(privacy);
            }
            Err(error) => {
                self.status = Some(format!(
                    "Could not start recording: {error}; check the recording library for private files"
                ));
            }
        }
    }

    fn include(&mut self, app: &mut FesTermApplication, id: TabId) {
        let Some(recording) = self.recording.as_ref() else {
            return;
        };
        let Some(session) = app.recording_session(id) else {
            self.status = Some("The session is no longer running".to_owned());
            return;
        };
        if session.controller.recording_id() == Some(recording.id()) {
            return;
        }
        if let Err(error) = session.view.validate_recording_view(&session.terminal) {
            self.status = Some(error.to_owned());
            return;
        }
        match recording.enroll(&session.terminal) {
            Ok(stream) => {
                if !app.attach_recording(id, stream) {
                    if let Err(error) = recording.fail_integrity("Selected session changed") {
                        tracing::error!("Could not stop recording after session change: {error}");
                    }
                    self.status =
                        Some("The session changed while it was being included".to_owned());
                } else {
                    self.status = None;
                }
            }
            Err(error) => self.status = Some(format!("Could not include session: {error}")),
        }
    }

    pub(super) fn defer_quit(&mut self, context: &egui::Context) {
        let Some(recording) = &self.recording else {
            self.quit_blocked = false;
            return;
        };
        self.quit_blocked = true;
        context.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        if self.quit_waiting_until.is_some() {
            return;
        }
        match recording.stop(FinishReason::Lifecycle) {
            Ok(_) => {
                self.quit_waiting_until =
                    Some(std::time::Instant::now() + std::time::Duration::from_secs(10));
                self.status = Some("Finalizing the recording before quitting".to_owned());
                context.request_repaint();
            }
            Err(error) => {
                self.status = Some(format!(
                    "Could not finalize the recording before quitting: {error}; inspect the recording library"
                ));
            }
        }
    }

    pub(super) fn prepare_accepted_quit(&mut self, context: &egui::Context) {
        if self.recording.is_some() && !self.quit_blocked {
            self.defer_quit(context);
        }
    }

    pub(super) fn blocks_quit(&self) -> bool {
        self.quit_blocked
    }

    fn poll_capture(&mut self, app: &mut FesTermApplication, context: &egui::Context) {
        let Some(recording) = self.recording.as_ref() else {
            return;
        };
        if let Err(error) = recording.check_duration() {
            self.status = Some(format!("Recording stopped: {error}"));
        }
        context.request_repaint_after(std::time::Duration::from_secs(1));
        match recording.take_failure() {
            Ok(Some(error)) => self.status = Some(format!("Recording stopped: {error}")),
            Ok(None) => {}
            Err(error) => {
                self.quit_waiting_until = None;
                self.status = Some(format!("Recording state failed: {error}; restart the app and inspect the recording library"));
                return;
            }
        }
        let completion = match recording.poll_completion() {
            Ok(Some(completion)) => completion,
            Ok(None) => {
                if self
                    .quit_waiting_until
                    .is_some_and(|deadline| std::time::Instant::now() >= deadline)
                {
                    self.quit_waiting_until = None;
                    self.status = Some(
                        "Recording is still finalizing; wait or try quitting again".to_owned(),
                    );
                }
                return;
            }
            Err(error) => {
                self.quit_waiting_until = None;
                self.status = Some(format!("Recording state failed: {error}; restart the app and inspect the recording library"));
                return;
            }
        };
        let id = recording.id();
        self.recording = None;
        self.duration_watch = None;
        self.include_new = None;
        self.privacy = None;
        app.detach_recording(id);
        let manifest = self.manifest.take();
        let lease = self.lease.take();
        self.quit_waiting_until = None;
        if self.quit_blocked {
            match completion {
                Completion::Complete { .. } => {
                    self.quit_blocked = false;
                    self.status = Some(
                        "Recording was saved privately; export it from the library after restarting"
                            .to_owned(),
                    );
                }
                Completion::Incomplete { reason, detail, .. } => {
                    let detail = detail
                        .map(|detail| format!(": {detail}"))
                        .unwrap_or_default();
                    self.status = Some(format!(
                        "Recording ended incompletely ({reason:?}{detail}); inspect its source in the recording library before quitting"
                    ));
                }
                Completion::Failed { message, .. } => {
                    self.status = Some(format!(
                        "Recording could not be finalized: {message}; inspect the recording library before quitting"
                    ));
                }
            }
            return;
        }
        match (completion, manifest, lease) {
            (Completion::Complete { path, .. }, Some(manifest), Some(lease)) => {
                self.pending_export = Some(PendingExport {
                    path,
                    manifest,
                    lease,
                });
                self.status = Some("Recording is ready for export".to_owned());
            }
            (Completion::Incomplete { reason, detail, .. }, _, _) => {
                let detail = detail
                    .map(|detail| format!(": {detail}"))
                    .unwrap_or_default();
                self.status = Some(format!(
                    "Recording ended incompletely ({reason:?}{detail}); inspect its source in the recording library"
                ))
            }
            (Completion::Failed { message, .. }, _, _) => {
                self.status = Some(format!(
                    "Recording could not be finalized: {message}; inspect the recording library"
                ))
            }
            _ => self.status = Some(
                "Recording is complete but export state was lost; inspect the recording library"
                    .to_owned(),
            ),
        }
    }

    fn start_picker(&mut self, target: PickerTarget, context: &egui::Context) {
        if self.picker.is_some() {
            return;
        }
        let future = rfd::AsyncFileDialog::new()
            .set_title("Choose a recording export folder")
            .pick_folder();
        let wake = Self::wake(context);
        let (send, receive) = mpsc::sync_channel(1);
        match thread::Builder::new()
            .name("festerm-recording-folder".to_owned())
            .spawn(move || {
                let result = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|error| format!("Could not start folder picker: {error}"))
                    .map(|runtime| {
                        runtime
                            .block_on(future)
                            .map(|folder| folder.path().to_owned())
                    });
                let _ = send.send(result);
                wake();
            }) {
            Ok(_) => {
                self.picker = Some(receive);
                self.picker_target = Some(target);
            }
            Err(error) => self.status = Some(format!("Could not open folder picker: {error}")),
        }
    }

    fn handle_action(
        &mut self,
        app: &mut FesTermApplication,
        action: Action,
        context: &egui::Context,
    ) {
        match action {
            Action::Close => {
                if matches!(self.dialog, Some(Dialog::Start { .. })) {
                    self.privacy_task = None;
                    self.privacy = None;
                }
                if matches!(self.dialog, Some(Dialog::Export { .. })) {
                    self.preview_task = None;
                    self.preview = None;
                    self.pending_export = None;
                    self.status =
                        Some("The private recording was retained for export later".to_owned());
                }
                self.dialog = None;
            }
            Action::ChooseFolder(target) => self.start_picker(target, context),
            Action::Save(settings) => {
                if let Err(error) = settings.validate() {
                    self.status = Some(error);
                    return;
                }
                let Some(storage) = &mut self.storage else {
                    return;
                };
                let wake = Self::wake(context);
                match storage.save(settings, move || wake()) {
                    Ok(()) => {
                        self.settings_saving = Some(true);
                        self.status = Some("Saving recording settings".to_owned());
                    }
                    Err(error) => self.status = Some(error),
                }
            }
            Action::Reload => {
                let Some(storage) = &mut self.storage else {
                    return;
                };
                let wake = Self::wake(context);
                match storage.reload(move || wake()) {
                    Ok(()) => {
                        self.settings_saving = Some(false);
                        self.status = Some("Reloading recording settings".to_owned());
                    }
                    Err(error) => self.status = Some(error),
                }
            }
            Action::Begin {
                selected,
                settings,
                include_new,
            } => {
                self.begin(app, selected, settings, include_new, context);
            }
            Action::Include(selected) => {
                self.dialog = None;
                for id in selected {
                    self.include(app, id);
                    if self.recording_id().is_none() {
                        break;
                    }
                }
            }
            Action::PreviewExport(settings) => {
                let (Some(storage), Some(pending)) = (&self.storage, &self.pending_export) else {
                    self.status = Some("The completed recording is unavailable".to_owned());
                    return;
                };
                self.preview = None;
                let control = ExportControl::new(
                    Arc::new(ExportCancellation::default()),
                    Self::wake(context),
                );
                match PreviewTask::start(
                    storage.path().to_owned(),
                    pending.path.clone(),
                    pending.manifest.id(),
                    pending.lease.clone(),
                    settings,
                    control,
                ) {
                    Ok(task) => {
                        self.preview_task = Some(task);
                        self.status = None;
                    }
                    Err(error) => self.status = Some(error),
                }
            }
            Action::ConfirmExport(settings) => {
                let Some(preview) = self
                    .preview
                    .take()
                    .filter(|preview| preview.matches(&settings))
                else {
                    self.status = Some("Calculate the current duration before exporting".into());
                    return;
                };
                let (Some(storage), Some(pending)) = (&self.storage, self.pending_export.take())
                else {
                    self.status = Some("The completed recording is unavailable".to_owned());
                    return;
                };
                let control = ExportControl::new(
                    Arc::new(ExportCancellation::default()),
                    Self::wake(context),
                );
                match ExportTask::start_previewed(
                    PreviewedExport {
                        root: storage.path().to_owned(),
                        source: pending.path,
                        manifest: pending.manifest,
                        lease: pending.lease,
                        preview,
                    },
                    encoder_factory(),
                    DesktopExportBackend,
                    control,
                ) {
                    Ok(task) => {
                        self.export = Some(task);
                        self.dialog = None;
                        self.status = None;
                    }
                    Err(error) => {
                        self.dialog = None;
                        self.status = Some(error);
                    }
                }
            }
            Action::Refresh => self.scan_library(context),
            Action::OpenRetry(source) => {
                let Some(library) = &self.library else { return };
                let Some(source_entry) = library.sources.get(source) else {
                    return;
                };
                let Some(manifest) = library
                    .manifests
                    .iter()
                    .find(|manifest| manifest.id() == source_entry.id())
                else {
                    self.status =
                        Some("Saved export options are unavailable for this source".to_owned());
                    return;
                };
                self.dialog = Some(Dialog::Retry {
                    source,
                    draft: manifest.settings().clone(),
                    recovered: matches!(
                        source_entry.state(),
                        RecordingSourceState::Incomplete {
                            prefix: Some(_),
                            ..
                        }
                    ),
                });
            }
            Action::Retry { source, settings } => {
                if self.busy() || self.library_task.is_some() {
                    self.status =
                        Some("Wait for the current recording operation to finish".to_owned());
                    return;
                }
                if let Err(error) = settings.validate() {
                    self.status = Some(error);
                    return;
                }
                let Some(storage) = &self.storage else { return };
                let Some(library) = &mut self.library else {
                    return;
                };
                let Some(source_entry) = library.sources.get(source) else {
                    return;
                };
                let Some(manifest) = library
                    .manifests
                    .iter()
                    .position(|entry| entry.id() == source_entry.id())
                else {
                    self.status =
                        Some("Saved export options are unavailable for this source".to_owned());
                    return;
                };
                let source = library.sources.remove(source);
                let manifest = library.manifests.remove(manifest);
                match ExportTask::start_recovery(
                    storage.path().to_owned(),
                    source,
                    manifest,
                    settings,
                    encoder_factory(),
                    DesktopExportBackend,
                    Self::wake(context),
                ) {
                    Ok(task) => {
                        self.export = Some(task);
                        self.dialog = None;
                        self.library = None;
                        self.status = None;
                    }
                    Err(error) => {
                        self.status = Some(error);
                        self.scan_library(context);
                        self.dialog = Some(Dialog::Library);
                    }
                }
            }
            Action::Confirm(kind) => self.dialog = Some(Dialog::Confirm { kind }),
            Action::BackToLibrary => {
                self.dialog = Some(Dialog::Library);
                if self.library.is_none() {
                    self.scan_library(context);
                }
            }
            Action::Delete(kind) => self.delete_library_item(kind, context),
        }
    }

    fn delete_library_item(&mut self, kind: DeleteKind, context: &egui::Context) {
        if self.busy() || self.library_task.is_some() {
            self.status = Some("Wait for the current recording operation to finish".to_owned());
            return;
        }
        let Some(storage) = &self.storage else { return };
        let Some(library) = &mut self.library else {
            return;
        };
        let root = storage.path().to_owned();
        let wake = Self::wake(context);
        let result = match kind {
            DeleteKind::Source(index) if index < library.sources.len() => {
                let source = library.sources.remove(index);
                let manifest = library
                    .manifests
                    .iter()
                    .position(|manifest| manifest.id() == source.id())
                    .map(|index| library.manifests.remove(index));
                LibraryTask::delete_source(root, source, manifest, wake)
            }
            DeleteKind::Manifest(index) if index < library.manifests.len() => {
                LibraryTask::delete_manifest(root, library.manifests.remove(index), wake)
            }
            DeleteKind::Staging(index) if index < library.staging.len() => {
                LibraryTask::delete_staging(root, library.staging.remove(index), wake)
            }
            _ => {
                self.status =
                    Some("The selected recording entry is no longer available".to_owned());
                return;
            }
        };
        self.dialog = Some(Dialog::Library);
        self.library = None;
        match result {
            Ok(task) => self.library_task = Some(task),
            Err(error) => {
                self.status = Some(error);
                self.scan_library(context);
            }
        }
    }
}

struct DesktopExportBackend;

impl ExportBackend for DesktopExportBackend {
    type Publication = PublishedRecording;

    fn allows_automatic_source_deletion(&self) -> bool {
        true
    }

    fn recording_font_faces(
        &self,
        font: RecordingFont,
        cancelled: &std::sync::atomic::AtomicBool,
    ) -> Result<Option<[Vec<u8>; 4]>, String> {
        crate::font_downloads::recording_faces(font, cancelled)
    }

    fn preflight(&self, destination: &ExportDestination) -> Result<(), String> {
        DirectoryBackend.preflight(destination)
    }

    fn publish(&self, request: PublicationRequest<'_>) -> Result<Self::Publication, String> {
        DirectoryBackend.publish(request)
    }
}

#[cfg(target_os = "macos")]
fn encoder_factory() -> festerm_recording_export::AppleEncoderFactory {
    festerm_recording_export::AppleEncoderFactory
}

#[cfg(target_os = "linux")]
fn encoder_factory() -> festerm_recording_export::LinuxEncoderFactory {
    festerm_recording_export::LinuxEncoderFactory
}

#[cfg(windows)]
fn encoder_factory() -> festerm_recording_export::WindowsEncoderFactory {
    festerm_recording_export::WindowsEncoderFactory
}

impl Drop for DesktopRecording {
    fn drop(&mut self) {
        if let Some(task) = &self.prepare {
            task.cancel();
        }
        if let Some(task) = &self.privacy_task {
            task.cancel();
        }
        if let Some(recording) = &self.recording {
            if let Err(error) = recording.stop(FinishReason::Lifecycle) {
                tracing::error!("Could not stop recording on application shutdown: {error}");
            }
        }
    }
}

impl FesTermApplication {
    pub(super) fn recording_choices(
        &self,
        recording: Option<RecordingId>,
        owner: WindowId,
    ) -> Vec<SessionChoice> {
        self.windows
            .iter()
            .flat_map(|window| {
                window
                    .app
                    .recording_choices()
                    .into_iter()
                    .map(move |(id, label, active)| SessionChoice {
                        id,
                        label: format!("Window {} · {label}", window.id.0 + 1),
                        active: active && window.id == owner,
                        included: recording.is_some_and(|recording| {
                            window.app.recording_session(id).is_some_and(|session| {
                                session.controller.recording_id() == Some(recording)
                            })
                        }),
                    })
            })
            .collect()
    }

    fn recording_session(&self, id: TabId) -> Option<&crate::tabs::SessionTab> {
        self.windows
            .iter()
            .find_map(|window| window.app.recording_session(id))
    }

    fn attach_recording(&mut self, id: TabId, stream: festerm_recording::RecordingStream) -> bool {
        let Some(window) = self
            .windows
            .iter_mut()
            .find(|window| window.app.recording_session(id).is_some())
        else {
            return false;
        };
        window.app.attach_recording(id, stream)
    }

    fn detach_recording(&mut self, id: RecordingId) {
        for window in &mut self.windows {
            window.app.detach_recording(id);
        }
    }

    pub(super) fn poll_recording(&mut self, context: &egui::Context) {
        let mut recording = std::mem::take(&mut self.recording);
        recording.poll(self, context);
        for window in &mut self.windows {
            window.app.set_recording_view(recording.view(window.id));
        }
        self.recording = recording;
    }

    pub(super) fn settle_recording(&mut self, context: &egui::Context) {
        let mut recording = std::mem::take(&mut self.recording);
        let requests: Vec<_> = self
            .windows
            .iter_mut()
            .filter_map(|window| {
                window
                    .app
                    .take_recording_request()
                    .map(|request| (window.id, request))
            })
            .collect();
        for (window, request) in requests {
            recording.request(self, window, request, context);
            context.request_repaint();
        }
        let had_action = recording.action.is_some();
        recording.poll(self, context);
        if had_action {
            context.request_repaint();
        }
        self.recording = recording;
    }
}

#[cfg(test)]
#[path = "recording_tests.rs"]
mod tests;
