use std::{collections::BTreeSet, path::PathBuf, sync::Arc, time::Duration};

use festerm_recording::{
    Completion, ExportDestination, LocalStart, RecordingId, RecordingLease, RecordingManifest,
    RecordingSettings, StreamId,
};
use festerm_recording_export::{
    ExportControl, ExportPreview, ExportTask, LibrarySnapshot, LibraryTask, PrepareTask,
    PreviewTask, PreviewedExport,
};

use super::{
    model::SurfaceSummary,
    recording_draft::{picker_needs_reconnection, selected_sessions_are_live, RecordingDraft},
    recording_library_ui::{LibraryAction, RetryDraft},
    recording_tasks, shell, widgets, MobileApp, RecordingConsent,
};
use eframe::egui;

#[derive(Default)]
pub(super) struct RecordingUi {
    open: bool,
    selected: BTreeSet<u64>,
    pending_consent: Option<RecordingConsent>,
    pending_invalidation: Option<String>,
    include_new: bool,
    pub(super) pending_tunnel_recording: Option<RecordingId>,
    pub(super) draft: RecordingDraft,
    pub(super) picking_for_capture: bool,
    pub(super) picking_for_retry: bool,
    pub(super) preparing: Option<PrepareTask>,
    manifest: Option<RecordingManifest>,
    pub(super) lease: Option<Arc<RecordingLease>>,
    pub(super) pending_export: Option<PendingExport>,
    preview_task: Option<PreviewTask>,
    preview: Option<ExportPreview>,
    pub(super) exporting: Option<ExportTask>,
    start_failed: bool,
    pub(super) status: Option<String>,
    pub(super) library_open: bool,
    pub(super) library_discovered: bool,
    pub(super) library_scanning: bool,
    pub(super) library_task: Option<LibraryTask>,
    pub(super) library: Option<LibrarySnapshot>,
    pub(super) library_action: Option<LibraryAction>,
    pub(super) retry: Option<RetryDraft>,
}

pub(super) struct PendingExport {
    path: PathBuf,
    manifest: RecordingManifest,
    lease: Arc<RecordingLease>,
    settings: RecordingSettings,
}

impl RecordingUi {
    pub(super) fn blocks_terminal_input(&self) -> bool {
        self.open || self.pending_export.is_some() || self.library_open
    }
}

impl MobileApp {
    fn recording_sessions(&self) -> Vec<(u64, String)> {
        let active = self.sink.live.connection.borrow();
        let background = self.sink.live.background.borrow();
        self.tabs
            .iter()
            .filter(|tab| {
                if self.active_tab == Some(tab.id) {
                    active.as_ref().is_some_and(|live| !live.can_reset())
                } else {
                    tab.parked.is_some()
                        && background
                            .iter()
                            .any(|(id, live)| *id == tab.id && !live.can_reset())
                }
            })
            .map(|tab| (tab.id, tab.title.clone()))
            .collect()
    }

    fn recording_has_tab(&self, id: u64, recording: RecordingId) -> bool {
        if self.active_tab == Some(id) {
            self.sink
                .live
                .connection
                .borrow()
                .as_ref()
                .and_then(|live| live.recording_stream.as_ref())
                .is_some_and(|stream| stream.recording_id() == recording)
        } else {
            self.sink
                .live
                .background
                .borrow()
                .iter()
                .find(|(tab, _)| *tab == id)
                .and_then(|(_, live)| live.recording_stream.as_ref())
                .is_some_and(|stream| stream.recording_id() == recording)
        }
    }

    pub(super) fn mark_recorded_surfaces(&self, surfaces: &mut [SurfaceSummary]) {
        let Ok(Some(recording)) = self.sink.live.current_recording() else {
            return;
        };
        if !matches!(recording.is_recording(), Ok(true)) {
            return;
        }
        for surface in surfaces {
            if self.recording_has_tab(surface.id, recording.id()) {
                surface.title.push_str(" [REC]");
                surface.status.push_str(" · Recording");
            }
        }
    }

    pub(super) fn recording_menu(&mut self, ui: &mut egui::Ui) {
        match self.sink.live.current_recording() {
            Ok(Some(recording)) => {
                if matches!(recording.is_recording(), Ok(true)) {
                    if let Some(id) = self.active_tab {
                        if !self.recording_has_tab(id, recording.id())
                            && self.recording_sessions().iter().any(|(tab, _)| *tab == id)
                            && ui.button("Include this session in recording").clicked()
                        {
                            if let Err(error) = self.include_terminal_in_recording(id) {
                                self.recording_ui.status = Some(error);
                            }
                            ui.close();
                        }
                    }
                } else {
                    ui.add_enabled(false, egui::Button::new("Finalizing recording"));
                }
            }
            Ok(None) => {
                let ready = self.recording_ui.preparing.is_none()
                    && self.recording_ui.pending_export.is_none()
                    && self.recording_ui.exporting.is_none()
                    && self.recording_ui.library_task.is_none()
                    && !self.shell.recording_picker_pending;
                if ui
                    .add_enabled(ready, egui::Button::new("Record terminal sessions"))
                    .clicked()
                {
                    self.open_recording_sheet();
                    ui.close();
                }
            }
            Err(error) => {
                self.recording_ui.status = Some(error);
            }
        }
    }

    fn open_recording_sheet(&mut self) {
        if let Err(error) = self
            .recording_ui
            .draft
            .open(self.shell.recording_saved.as_ref())
        {
            self.recording_ui.status = Some(error.to_owned());
            return;
        }
        self.recording_ui.selected.clear();
        if let Some(active) = self.active_tab {
            if self
                .recording_sessions()
                .iter()
                .any(|(id, _)| *id == active)
            {
                self.recording_ui.selected.insert(active);
            }
        }
        self.recording_ui.status = None;
        self.recording_ui.open = true;
    }

    pub(super) fn finish_capture_folder(
        &mut self,
        result: Result<Option<ExportDestination>, String>,
    ) {
        let sessions = self.recording_sessions();
        let needs_reconnection = picker_needs_reconnection(&self.recording_ui.selected, &sessions);
        self.recording_ui.status = self
            .recording_ui
            .draft
            .finish_picker(result, needs_reconnection);
    }

    pub(super) fn show_recording_sheet(&mut self, ctx: &egui::Context, bounds: egui::Rect) {
        if !self.recording_ui.open {
            return;
        }
        let Some(mut settings) = self.recording_ui.draft.settings.take() else {
            self.recording_ui.open = false;
            self.recording_ui.status = Some("Recording options are unavailable.".to_owned());
            return;
        };
        let sessions = self.recording_sessions();
        let selected_live = selected_sessions_are_live(&self.recording_ui.selected, &sessions);
        let preparing = self.recording_ui.preparing.is_some();
        let busy = preparing || self.shell.recording_picker_pending;
        let ready = !busy
            && settings.destination.is_some()
            && settings.validate().is_ok()
            && !self.recording_ui.selected.is_empty()
            && self.recording_ui.selected.len() <= festerm_recording::MAX_STREAMS
            && selected_live;
        let mut close = false;
        let mut choose_folder = false;
        let mut start = false;
        let mut cancel_preparation = false;
        let outcome = widgets::bottom_sheet_with_footer(
            ctx,
            "recording-consent",
            bounds,
            |ui| {
                close = widgets::sheet_title(ui, "Record terminal sessions", Some("Close"));
                ui.label("The selected sessions start and stop together and export as separate silent MP4 files.");
                ui.label("The visible screen and later terminal output are recorded, including background tabs. Echoed passwords, pasted text, and control sequences can be stored. Local keyboard and clipboard input are not read.");
                ui.label("Recording stops and export cancellation is requested when the app becomes inactive, including for system dialogs.");
                ui.label("Export folders can be synced or copied by other software. Private source files may also be copied by third-party backup software.");
                #[cfg(target_os = "android")]
                ui.label(
                    "On Android, the export folder may be visible before all videos have finished.",
                );
                if let Some(status) = &self.recording_ui.status {
                    ui.colored_label(ui.visuals().error_fg_color, status);
                } else if !selected_live {
                    ui.colored_label(ui.visuals().error_fg_color, "A selected session is no longer live. Close and reopen recording to select current sessions.");
                }
                ui.add_enabled_ui(!busy, |ui| {
                    ui.separator();
                    ui.strong("Sessions to include");
                    if sessions.is_empty() {
                        ui.label("No live terminal sessions are available.");
                    }
                    for (id, title) in &sessions {
                        let mut selected = self.recording_ui.selected.contains(id);
                        if ui.checkbox(&mut selected, title).changed() {
                            if selected {
                                self.recording_ui.selected.insert(*id);
                            } else {
                                self.recording_ui.selected.remove(id);
                            }
                        }
                    }
                    ui.checkbox(
                        &mut self.recording_ui.draft.include_new,
                        "Also include new sessions I open while recording",
                    );
                    if self.recording_ui.draft.include_new {
                        ui.label("If a new session cannot be safely included, the recording stops incomplete.");
                    }
                    ui.label("Reconnecting an existing session still requires explicit inclusion.");
                    ui.separator();
                    ui.strong("Export for this recording");
                    let location = match &settings.destination {
                        Some(ExportDestination::AndroidTree(uri)) => format!("Files folder: {uri}"),
                        Some(ExportDestination::IosBookmark { display_name, .. }) => {
                            format!("Files folder: {display_name}")
                        }
                        Some(ExportDestination::DesktopPath(path)) => {
                            format!("Folder: {}", path.display())
                        }
                        None => "No export folder selected".to_owned(),
                    };
                    ui.label(location);
                    if widgets::button(ui, widgets::ButtonKind::Secondary, "Choose folder", true)
                        .clicked()
                    {
                        choose_folder = true;
                    }
                    widgets::text_field(
                        ui,
                        "Recording folder name",
                        &mut settings.directory_template,
                        "recording-{recording_id}",
                        ui.available_width(),
                    );
                    widgets::text_field(
                        ui,
                        "MP4 filename",
                        &mut settings.session_template,
                        "session-{session_index}.mp4",
                        ui.available_width(),
                    );
                    ui.label("Names may use {recording_id}, {date}, and {time}; MP4 names must also use {session_index}.");
                    match settings.validate() {
                        Ok(validated) => {
                            let id = RecordingId::parse("0123456789abcdef0123456789abcdef")
                                .expect("example recording ID is valid");
                            let local =
                                LocalStart::new(2026, 1, 2, 15, 4, 5).expect("example date is valid");
                            let stream = StreamId::from_index(1).expect("example stream is valid");
                            if let (Ok(directory), Ok(video)) = (
                                validated.templates.directory(id, local),
                                validated.templates.session(id, local, stream),
                            ) {
                                ui.label(format!("Example: {directory}/{video}"));
                            }
                        }
                        Err(error) => {
                            ui.colored_label(
                                ui.visuals().error_fg_color,
                                format!("Recording options need attention: {error}"),
                            );
                        }
                    }
                    ui.separator();
                    ui.strong("Shared playback pacing");
                    shell::recording_export_controls(ui, &mut settings);
                    ui.label("The private source is kept after export until you delete it in the recording library.");
                    ui.label("These choices apply only to this recording; saved Settings are unchanged.");
                });
            },
            |ui| {
                if preparing {
                    ui.label("Preparing private recording storage...");
                    cancel_preparation = widgets::button(
                        ui,
                        widgets::ButtonKind::Secondary,
                        "Cancel preparation",
                        true,
                    )
                    .clicked();
                } else {
                    start =
                        widgets::button(ui, widgets::ButtonKind::Primary, "Start recording", ready)
                            .clicked();
                }
            },
        );
        self.recording_ui.draft.settings = Some(settings);
        if cancel_preparation || close || outcome.dismissed {
            if let Some(task) = &self.recording_ui.preparing {
                task.cancel();
            }
            self.recording_ui.open = false;
        } else if choose_folder {
            self.choose_recording_folder();
            if self.shell.recording_picker_pending {
                self.recording_ui.picking_for_capture = true;
            } else {
                self.recording_ui.status = self.shell.recording_error.take();
            }
        } else if start {
            self.prepare_recording(ctx);
        }
    }

    fn prepare_recording(&mut self, ctx: &egui::Context) {
        let generation = match self.recording_privacy_generation() {
            Ok(generation) => generation,
            Err(error) => {
                self.recording_ui.status = Some(error);
                return;
            }
        };
        let sessions = self.recording_sessions();
        let selected: Vec<_> = sessions
            .iter()
            .filter_map(|(id, _)| self.recording_ui.selected.contains(id).then_some(*id))
            .collect();
        if selected.len() != self.recording_ui.selected.len() || selected.is_empty() {
            self.recording_ui.status =
                Some("A selected terminal session is no longer live.".into());
            return;
        }
        let selected = match self.recording_terminals(&selected) {
            Ok((_, selected)) => selected,
            Err(error) => {
                self.recording_ui.status = Some(error);
                return;
            }
        };
        let Some(settings) = self.recording_ui.draft.settings.clone() else {
            self.recording_ui.status = Some("Recording options are unavailable.".into());
            return;
        };
        let Some(storage) = &self.recording_storage else {
            self.recording_ui.status = Some("Private recording storage is unavailable.".into());
            return;
        };
        let context = ctx.clone();
        match PrepareTask::start(
            storage.path().to_owned(),
            settings,
            recording_tasks::preflight,
            Arc::new(move || context.request_repaint()),
        ) {
            Ok(task) => {
                self.recording_ui.pending_consent = Some(RecordingConsent {
                    generation,
                    selected,
                });
                self.recording_ui.pending_invalidation = None;
                self.recording_ui.preparing = Some(task);
                self.recording_ui.status = None;
            }
            Err(error) => self.recording_ui.status = Some(error),
        }
    }

    pub(super) fn poll_recording_ui(&mut self, ctx: &egui::Context) {
        self.poll_recording_library(ctx);
        let invalidation = self
            .recording_ui
            .pending_consent
            .as_ref()
            .and_then(|consent| self.validate_recording_consent(consent).err());
        if let Some(error) = invalidation {
            if let Some(task) = &self.recording_ui.preparing {
                task.cancel();
                self.recording_ui.pending_invalidation.get_or_insert(error);
            }
        }

        match self.sink.live.current_recording() {
            Ok(Some(recording)) => {
                if let Err(error) = recording.check_duration() {
                    self.recording_ui.status = Some(error.to_string());
                }
                match recording.take_failure() {
                    Ok(Some(error)) => self.recording_ui.status = Some(error),
                    Ok(None) => {}
                    Err(error) => self.recording_ui.status = Some(error.to_string()),
                }
                match self.poll_terminal_recording() {
                    Ok(Some(completion)) => self.finish_recording(ctx, completion),
                    Ok(None) => ctx.request_repaint_after(Duration::from_millis(250)),
                    Err(error) => self.recording_ui.status = Some(error),
                }
            }
            Ok(None) => {}
            Err(error) => self.recording_ui.status = Some(error),
        }

        let result = self
            .recording_ui
            .exporting
            .as_ref()
            .and_then(ExportTask::poll);
        if let Some(result) = result {
            self.recording_ui.exporting = None;
            self.recording_ui.status = Some(match result {
                Ok(result) => format!(
                    "Exported {}{}/{}. {}",
                    if result.recovered_prefix {
                        "recovered prefix to "
                    } else {
                        ""
                    },
                    result.directory,
                    result.videos.join(", "),
                    if result.retained_source {
                        "The private source was retained."
                    } else {
                        "The private source was deleted."
                    }
                ),
                Err(error) => error,
            });
        }
    }

    pub(super) fn poll_recording_preparation(&mut self, ctx: &egui::Context) {
        if ctx.input(|input| input.pointer.any_down()) {
            ctx.request_repaint_after(Duration::from_millis(50));
            return;
        }
        let prepared = self
            .recording_ui
            .preparing
            .as_mut()
            .and_then(PrepareTask::poll);
        if let Some(result) = prepared {
            self.recording_ui.preparing = None;
            match result {
                Ok(prepared) => {
                    let Some(consent) = self.recording_ui.pending_consent.take() else {
                        self.recording_ui.status = Some(
                            "Recording consent was lost; the prepared source was retained in private storage".to_owned(),
                        );
                        return;
                    };
                    self.recording_ui.manifest = Some(prepared.manifest);
                    self.recording_ui.lease = Some(prepared.lease);
                    match self.start_prepared_terminal_recording(prepared.source, &consent, ctx) {
                        Ok(()) => {
                            self.recording_ui.include_new = self.recording_ui.draft.include_new;
                            self.recording_ui.open = false;
                            self.recording_ui.start_failed = false;
                            self.recording_ui.status = None;
                        }
                        Err(error) => {
                            self.recording_ui.start_failed = true;
                            self.recording_ui.status = Some(format!(
                                "{error}; any prepared source was retained in private storage"
                            ));
                            if matches!(self.sink.live.current_recording(), Ok(None)) {
                                self.recording_ui.manifest = None;
                                self.recording_ui.lease = None;
                                self.recording_ui.start_failed = false;
                            }
                        }
                    }
                }
                Err(error) => {
                    self.recording_ui.pending_consent = None;
                    self.recording_ui.status =
                        Some(match self.recording_ui.pending_invalidation.take() {
                            Some(reason) if error == "Recording preparation was cancelled" => {
                                reason
                            }
                            Some(reason) => format!("{reason}; {error}"),
                            None => error,
                        });
                }
            }
        }
    }

    pub(super) fn recording_for_new_session(&self) -> Option<RecordingId> {
        if !self.recording_ui.include_new {
            return None;
        }
        let recording = self.sink.live.current_recording().ok().flatten()?;
        matches!(recording.is_recording(), Ok(true)).then_some(recording.id())
    }

    pub(super) fn include_new_session(&mut self, id: u64, intended: Option<RecordingId>) {
        if intended.is_none() || self.recording_for_new_session() != intended {
            return;
        }
        if let Err(error) = self.include_terminal_in_recording(id) {
            let mut status = format!("The new session was not added to the recording: {error}");
            match self.sink.live.current_recording() {
                Ok(Some(recording)) if Some(recording.id()) == intended => {
                    if let Err(stop_error) = recording.fail_integrity(
                        "Recording stopped because a new session could not be included",
                    ) {
                        status.push_str(&format!("; could not stop recording: {stop_error}"));
                    }
                }
                Ok(_) => {}
                Err(state_error) => {
                    status.push_str(&format!("; recording state failed: {state_error}"));
                }
            }
            self.recording_ui.status = Some(status);
        }
    }

    fn finish_recording(&mut self, ctx: &egui::Context, completion: Completion) {
        self.recording_ui.include_new = false;
        self.recording_ui.pending_tunnel_recording = None;
        let manifest = self.recording_ui.manifest.take();
        let lease = self.recording_ui.lease.take();
        if let Some(id) = manifest.as_ref().map(RecordingManifest::id) {
            if let Some(live) = self.sink.live.connection.borrow_mut().as_mut() {
                if live
                    .recording_stream
                    .as_ref()
                    .is_some_and(|stream| stream.recording_id() == id)
                {
                    live.recording_stream = None;
                }
            }
            for (_, live) in self.sink.live.background.borrow_mut().iter_mut() {
                if live
                    .recording_stream
                    .as_ref()
                    .is_some_and(|stream| stream.recording_id() == id)
                {
                    live.recording_stream = None;
                }
            }
        }
        let failed_start = std::mem::take(&mut self.recording_ui.start_failed);
        match completion {
            Completion::Complete { path, .. } if !failed_start => {
                let Some(manifest) = manifest else {
                    self.recording_ui.status = Some(
                        "Recording finished without saved export options; the source was retained."
                            .to_owned(),
                    );
                    return;
                };
                if self.recording_storage.is_none() {
                    self.recording_ui.status = Some(
                        "Private recording storage is unavailable; the source was retained.".into(),
                    );
                    return;
                }
                let Some(lease) = lease else {
                    self.recording_ui.status = Some(
                        "Recording storage ownership was lost; the source was retained.".into(),
                    );
                    return;
                };
                let settings =
                    crate::recording_storage::retain_mobile_source(manifest.settings().clone());
                widgets::reset_bottom_sheet_scroll(ctx, "recording-export");
                self.recording_ui.pending_export = Some(PendingExport {
                    path,
                    manifest,
                    lease,
                    settings,
                });
                self.recording_ui.status = None;
            }
            Completion::Complete { .. } => {
                let detail = self.recording_ui.status.take().unwrap_or_default();
                self.recording_ui.status = Some(format!(
                    "{detail}; recording was not exported and its source was retained"
                ));
            }
            Completion::Incomplete { reason, detail, .. } => {
                let error = detail
                    .map(|detail| format!(": {detail}"))
                    .unwrap_or_default();
                let status = self.recording_ui.status.take().unwrap_or_default();
                self.recording_ui.status = Some(format!(
                    "Recording is incomplete ({reason:?}{error}); the source was retained. {status}"
                ));
            }
            Completion::Failed { message, .. } => {
                self.recording_ui.status = Some(format!(
                    "Recording could not be finalized; the source was retained: {message}"
                ));
            }
        }
    }

    pub(super) fn show_recording_export(&mut self, ctx: &egui::Context, bounds: egui::Rect) {
        let Some(mut pending) = self.recording_ui.pending_export.take() else {
            return;
        };
        let busy = self.recording_ui.preview_task.is_some();
        let preview = self.recording_ui.preview.as_ref();
        let mut close = false;
        let mut calculate = false;
        let mut export = false;
        let can_export = std::cell::Cell::new(false);
        let outcome = widgets::bottom_sheet_with_footer(
            ctx,
            "recording-export",
            bounds,
            |ui| {
                close = widgets::sheet_title(ui, "Export recording", Some("Keep"));
                ui.label("The completed private source is kept after export until you delete it in the recording library. Pacing applies to every session on the shared timeline.");
                if let Some(status) = &self.recording_ui.status {
                    ui.colored_label(ui.visuals().error_fg_color, status);
                }
                ui.add_enabled_ui(!busy, |ui| {
                    shell::recording_export_controls(ui, &mut pending.settings);
                });
                match preview.filter(|preview| preview.matches(&pending.settings)) {
                    Some(preview) => {
                        ui.label(format!(
                            "Estimated duration: {:.1} seconds",
                            preview.duration().as_secs_f64()
                        ));
                    }
                    None if busy => {
                        ui.spinner();
                        ui.label("Calculating the synchronized duration...");
                    }
                    None => {
                        ui.label("Calculate the synchronized duration before exporting.");
                    }
                }
                calculate = widgets::button(
                    ui,
                    widgets::ButtonKind::Secondary,
                    "Calculate duration",
                    !busy && pending.settings.validate().is_ok(),
                )
                .clicked();
                can_export.set(
                    !busy && preview.is_some_and(|preview| preview.matches(&pending.settings)),
                );
            },
            |ui| {
                export = widgets::button(
                    ui,
                    widgets::ButtonKind::Primary,
                    "Export recording",
                    can_export.get(),
                )
                .clicked();
            },
        );
        if close || outcome.dismissed {
            self.recording_ui.preview_task = None;
            self.recording_ui.preview = None;
            self.recording_ui.status =
                Some("The private recording was retained for export later.".to_owned());
            return;
        }
        self.recording_ui.pending_export = Some(pending);
        if calculate {
            self.start_recording_preview(ctx);
        } else if export {
            self.start_confirmed_recording_export(ctx);
        }
    }

    fn start_recording_preview(&mut self, ctx: &egui::Context) {
        let cancellation = match self.foreground_export_cancellation() {
            Ok(cancellation) => cancellation,
            Err(error) => {
                self.recording_ui.status = Some(error);
                return;
            }
        };
        let (Some(storage), Some(pending)) = (
            self.recording_storage.as_ref(),
            self.recording_ui.pending_export.as_ref(),
        ) else {
            self.recording_ui.status = Some("The completed recording is unavailable.".to_owned());
            return;
        };
        let context = ctx.clone();
        self.recording_ui.preview = None;
        match PreviewTask::start(
            storage.path().to_owned(),
            pending.path.clone(),
            pending.manifest.id(),
            pending.lease.clone(),
            pending.settings.clone(),
            ExportControl::new(cancellation, Arc::new(move || context.request_repaint())),
        ) {
            Ok(task) => {
                self.recording_ui.preview_task = Some(task);
                self.recording_ui.status = None;
            }
            Err(error) => self.recording_ui.status = Some(error),
        }
    }

    pub(super) fn poll_recording_export_preview(&mut self, ctx: &egui::Context) {
        if ctx.input(|input| input.pointer.any_down()) {
            if self.recording_ui.preview_task.is_some() {
                ctx.request_repaint_after(Duration::from_millis(50));
            }
            return;
        }
        if let Some(result) = self
            .recording_ui
            .preview_task
            .as_ref()
            .and_then(PreviewTask::poll)
        {
            self.recording_ui.preview_task = None;
            match result {
                Ok(preview) => {
                    self.recording_ui.preview = Some(preview);
                    self.recording_ui.status = None;
                }
                Err(error) => self.recording_ui.status = Some(error),
            }
        }
    }

    fn start_confirmed_recording_export(&mut self, ctx: &egui::Context) {
        let cancellation = match self.foreground_export_cancellation() {
            Ok(cancellation) => cancellation,
            Err(error) => {
                self.recording_ui.status = Some(error);
                return;
            }
        };
        let Some(pending) = self.recording_ui.pending_export.take() else {
            self.recording_ui.status = Some("The completed recording is unavailable.".to_owned());
            return;
        };
        let Some(preview) = self
            .recording_ui
            .preview
            .take()
            .filter(|preview| preview.matches(&pending.settings))
        else {
            self.recording_ui.pending_export = Some(pending);
            self.recording_ui.status =
                Some("Calculate the current duration before exporting.".to_owned());
            return;
        };
        let Some(storage) = &self.recording_storage else {
            self.recording_ui.status =
                Some("Private recording storage is unavailable; the source was retained.".into());
            return;
        };
        let context = ctx.clone();
        match ExportTask::start_previewed(
            PreviewedExport {
                root: storage.path().to_owned(),
                source: pending.path,
                manifest: pending.manifest,
                lease: pending.lease,
                preview,
            },
            recording_tasks::encoder_factory(),
            recording_tasks::MobileBackend,
            ExportControl::new(cancellation, Arc::new(move || context.request_repaint())),
        ) {
            Ok(task) => {
                self.recording_ui.exporting = Some(task);
                self.recording_ui.status = None;
            }
            Err(error) => self.recording_ui.status = Some(error),
        }
    }

    pub(super) fn show_recording_status(&mut self, ui: &mut egui::Ui) {
        let recording = self.sink.live.current_recording().ok().flatten();
        let active = recording
            .as_ref()
            .is_some_and(|recording| matches!(recording.is_recording(), Ok(true)));
        let preparing = self.recording_ui.preparing.is_some();
        let progress = self
            .recording_ui
            .exporting
            .as_ref()
            .map(ExportTask::progress);
        if recording.is_none()
            && !preparing
            && progress.is_none()
            && self.recording_ui.status.is_none()
        {
            return;
        }
        let mut stop = false;
        let mut cancel = false;
        let mut dismiss = false;
        egui::Panel::top("mobile-recording-status").show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                if active {
                    ui.colored_label(ui.visuals().error_fg_color, "Recording");
                    stop = ui.button("Stop recording").clicked();
                } else if recording.is_some() {
                    ui.label("Finalizing recording...");
                } else if preparing {
                    ui.label("Preparing recording...");
                    cancel = ui.button("Cancel preparation").clicked();
                } else if let Some((stream, done, total)) = progress {
                    if total == 0 {
                        ui.label("Preparing recording export...");
                    } else {
                        ui.label(format!("Exporting session {stream}: {done}/{total} frames"));
                    }
                    cancel = ui.button("Cancel export").clicked();
                }
                if let Some(status) = &self.recording_ui.status {
                    ui.label(status);
                    dismiss = ui.button("Dismiss recording message").clicked();
                }
            });
        });
        if stop {
            if let Err(error) = self.stop_terminal_recording() {
                self.recording_ui.status = Some(error);
            }
        }
        if cancel {
            if let Some(task) = &self.recording_ui.preparing {
                task.cancel();
            }
            if let Some(task) = &self.recording_ui.exporting {
                if !task.cancel() {
                    self.recording_ui.status = Some(
                        "Export is finalizing; cancellation came too late to stop source retention."
                            .to_owned(),
                    );
                }
            }
        }
        if dismiss {
            self.recording_ui.status = None;
        }
    }
}
