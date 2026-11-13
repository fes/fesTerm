use std::{path::PathBuf, sync::Arc};

use eframe::egui;
use festerm_recording::{ExportDestination, RecordingSettings, RecordingSourceState};
use festerm_recording_export::{ExportControl, ExportTask, LibraryResult, LibraryTask};

use super::{recording_tasks, shell, widgets, MobileApp};

pub(super) enum LibraryAction {
    Source(usize),
    Options(usize),
    Staging(usize),
    #[cfg(target_os = "android")]
    Journal(usize),
    #[cfg(test)]
    GalleryJournal,
}

pub(super) struct RetryDraft {
    source: usize,
    manifest: usize,
    recovered: bool,
    pub settings: RecordingSettings,
}

#[cfg(test)]
impl RetryDraft {
    pub(super) fn gallery(settings: festerm_recording::RecordingSettings, recovered: bool) -> Self {
        Self {
            source: 0,
            manifest: 0,
            recovered,
            settings,
        }
    }
}

fn wake(ctx: &egui::Context) -> Arc<dyn Fn() + Send + Sync> {
    let ctx = ctx.clone();
    Arc::new(move || ctx.request_repaint())
}

#[cfg(any(test, target_os = "android"))]
fn show_export_notice(
    ui: &mut egui::Ui,
    recording: impl std::fmt::Display,
    name: &str,
    published: bool,
) -> bool {
    ui.separator();
    ui.strong(format!("Export notice for recording {recording}"));
    if published {
        ui.label(format!("Videos were verified in {name} before the export was interrupted. Check the destination."));
    } else {
        ui.label(format!("Export was interrupted. A partial folder named {name} may remain in the chosen Files location."));
    }
    ui.button("Acknowledge export notice").clicked()
}

impl MobileApp {
    pub(super) fn poll_recording_library(&mut self, ctx: &egui::Context) {
        if !self.recording_ui.library_discovered {
            self.recording_ui.library_discovered = true;
            if let Some(storage) = &self.recording_storage {
                match LibraryTask::discover(storage.path().to_owned(), wake(ctx)) {
                    Ok(task) => self.recording_ui.library_task = Some(task),
                    Err(error) => self.recording_ui.status = Some(error),
                }
            }
        }
        let result = self
            .recording_ui
            .library_task
            .as_ref()
            .and_then(LibraryTask::poll);
        let Some(result) = result else {
            return;
        };
        self.recording_ui.library_task = None;
        self.recording_ui.library_scanning = false;
        match result {
            Ok(LibraryResult::Discovered(found)) => {
                if self.recording_ui.library_open {
                    self.scan_recording_library(ctx);
                } else if found && self.recording_ui.status.is_none() {
                    self.recording_ui.status = Some(
                        "Private recordings or interrupted exports are available in the recording library."
                            .to_owned(),
                    );
                }
            }
            Ok(LibraryResult::Snapshot(snapshot)) => {
                if self.recording_ui.library_open {
                    self.recording_ui.library = Some(snapshot);
                }
            }
            Ok(LibraryResult::Changed(message)) => {
                self.recording_ui.status = Some(message);
                if self.recording_ui.library_open {
                    self.scan_recording_library(ctx);
                }
            }
            Err(error) => self.recording_ui.status = Some(error),
        }
    }

    pub(super) fn open_recording_library(&mut self, ctx: &egui::Context) {
        match self.sink.live.current_recording() {
            Ok(Some(_)) => {
                self.recording_ui.status =
                    Some("Stop recording before opening the recording library.".to_owned());
                return;
            }
            Err(error) => {
                self.recording_ui.status = Some(error);
                return;
            }
            Ok(None) => {}
        }
        if self.recording_ui.preparing.is_some()
            || self.recording_ui.pending_export.is_some()
            || self.recording_ui.exporting.is_some()
        {
            self.recording_ui.status =
                Some("Wait for recording preparation or export before opening the library.".into());
            return;
        }
        self.recording_ui.library_open = true;
        self.recording_ui.library = None;
        self.recording_ui.library_action = None;
        self.recording_ui.retry = None;
        if self.recording_ui.library_task.is_none() {
            self.scan_recording_library(ctx);
        }
    }

    fn scan_recording_library(&mut self, ctx: &egui::Context) {
        let Some(storage) = &self.recording_storage else {
            self.recording_ui.status = Some("Private recording storage is unavailable.".to_owned());
            return;
        };
        self.recording_ui.library = None;
        match LibraryTask::scan(storage.path().to_owned(), wake(ctx)) {
            Ok(task) => {
                self.recording_ui.library_task = Some(task);
                self.recording_ui.library_scanning = true;
            }
            Err(error) => self.recording_ui.status = Some(error),
        }
    }

    pub(super) fn show_recording_library(&mut self, ctx: &egui::Context, bounds: egui::Rect) {
        if !self.recording_ui.library_open {
            return;
        }
        let mut close = false;
        let mut refresh = false;
        let mut action = None;
        let mut retry = None;
        let busy = self.recording_ui.library_task.is_some();
        #[cfg(test)]
        let busy = busy
            || self
                .recording_ui
                .gallery_display
                .as_ref()
                .is_some_and(|display| display.library_busy);
        let outcome = widgets::bottom_sheet_with_footer(
            ctx,
            "recording-library",
            bounds,
            |ui| {
                close = widgets::sheet_title(ui, "Recording library", Some("Close"));
                ui.label("Sources and temporary videos can contain terminal output, including secrets and control-sequence payloads. They are private to fesTerm, but third-party backups may copy them.");
                if let Some(status) = &self.recording_ui.status {
                    ui.label(status);
                }
                if busy {
                    ui.label("Inspecting private recording storage...");
                    return;
                }
                let Some(library) = &self.recording_ui.library else {
                    ui.label("The library could not be loaded. Refresh to try again.");
                    return;
                };
                let empty = library.sources.is_empty()
                    && library.manifests.is_empty()
                    && library.staging.is_empty()
                    && {
                        #[cfg(target_os = "android")]
                        {
                            library.journals.is_empty()
                        }
                        #[cfg(not(target_os = "android"))]
                        {
                            true
                        }
                    };
                #[cfg(test)]
                let empty = empty
                    && self
                        .recording_ui
                        .gallery_display
                        .as_ref()
                        .is_none_or(|display| display.journal.is_none());
                if empty {
                    ui.label("No private recordings or interrupted exports.");
                }
                for (index, source) in library.sources.iter().enumerate() {
                    ui.push_id(("recording-source", index), |ui| {
                        ui.separator();
                        ui.strong(format!("Recording {}", source.id()));
                        let description = match source.state() {
                            RecordingSourceState::Empty => {
                                "Empty source left by interrupted preparation".to_owned()
                            }
                            RecordingSourceState::Complete { duration, .. } => format!(
                                "{} source, {} seconds, {} bytes",
                                if source.is_partial() { "Recoverable" } else { "Complete" },
                                duration.as_secs(),
                                source.length()
                            ),
                            RecordingSourceState::Incomplete { detail, prefix } => match prefix {
                                Some(prefix) => format!(
                                    "Incomplete: {detail}. Validated prefix: {:.1} seconds, {} session(s); later output may be missing.",
                                    prefix.duration().as_secs_f64(),
                                    prefix.stream_count()
                                ),
                                None => format!("Incomplete or invalid source; no prefix can be exported: {detail}"),
                            },
                        };
                        ui.label(description);
                        let manifest = library
                            .manifests
                            .iter()
                            .position(|manifest| manifest.id() == source.id());
                        if let Some(manifest) = manifest {
                            let recovered = matches!(source.state(), RecordingSourceState::Incomplete { prefix: Some(_), .. });
                            if (matches!(source.state(), RecordingSourceState::Complete { .. }) || recovered)
                                && ui
                                    .button(if recovered {
                                        "Export recovered prefix"
                                    } else if source.is_partial() {
                                        "Recover and export"
                                    } else {
                                        "Export recording"
                                    })
                                    .clicked()
                            {
                                retry = Some((index, manifest));
                            }
                        } else {
                            ui.label("Saved export options are missing; this source cannot be exported here.");
                        }
                        if ui.button("Delete source").clicked() {
                            action = Some(LibraryAction::Source(index));
                        }
                    });
                }
                for (index, manifest) in library.manifests.iter().enumerate() {
                    if library
                        .sources
                        .iter()
                        .any(|source| source.id() == manifest.id())
                    {
                        continue;
                    }
                    ui.push_id(("recording-options", index), |ui| {
                        ui.separator();
                        ui.strong(format!("Recording {}", manifest.id()));
                        ui.label("Saved options remain, but no source was found.");
                        if ui.button("Delete unused options").clicked() {
                            action = Some(LibraryAction::Options(index));
                        }
                    });
                }
                for (index, staging) in library.staging.iter().enumerate() {
                    ui.push_id(("recording-staging", index), |ui| {
                        ui.separator();
                        let path = staging.path();
                        let name = path.file_name().unwrap_or_default().to_string_lossy();
                        ui.strong("Interrupted private video staging");
                        ui.label(name);
                        if ui.button("Delete private staging").clicked() {
                            action = Some(LibraryAction::Staging(index));
                        }
                    });
                }
                #[cfg(target_os = "android")]
                for (index, journal) in library.journals.iter().enumerate() {
                    ui.push_id(("recording-journal", index), |ui| {
                        let record = journal.record();
                        let name = record
                            .directory()
                            .map_or(record.requested_directory(), |entry| {
                                entry.display_name.as_str()
                            });
                        if show_export_notice(
                            ui,
                            record.recording_id(),
                            name,
                            record.published_videos().is_some(),
                        ) {
                            action = Some(LibraryAction::Journal(index));
                        }
                    });
                }
                #[cfg(test)]
                if let Some(published) = self
                    .recording_ui
                    .gallery_display
                    .as_ref()
                    .and_then(|display| display.journal)
                {
                    show_export_notice(
                        ui,
                        "0123456789abcdef0123456789abcdef",
                        "Synthetic export folder",
                        published,
                    );
                }
            },
            |ui| {
                refresh = widgets::button(
                    ui,
                    widgets::ButtonKind::Secondary,
                    "Refresh recording library",
                    !busy,
                )
                .clicked();
            },
        );
        #[cfg(test)]
        if self.recording_ui.gallery_display.is_some() {
            self.show_recording_library_confirmation(ctx, bounds);
            self.show_recording_retry(ctx, bounds);
            return;
        }
        if close || outcome.dismissed {
            self.recording_ui.library_open = false;
            self.recording_ui.library = None;
            self.recording_ui.library_action = None;
            self.recording_ui.retry = None;
            if self.recording_ui.library_scanning {
                if let Some(task) = &self.recording_ui.library_task {
                    task.cancel();
                }
            }
            return;
        }
        if refresh {
            self.scan_recording_library(ctx);
        } else if let Some(action) = action {
            self.recording_ui.library_action = Some(action);
        } else if let Some((source, manifest)) = retry {
            if let Some(library) = &self.recording_ui.library {
                self.recording_ui.retry = Some(RetryDraft {
                    source,
                    manifest,
                    recovered: matches!(
                        library.sources[source].state(),
                        RecordingSourceState::Incomplete {
                            prefix: Some(_),
                            ..
                        }
                    ),
                    settings: crate::recording_storage::retain_mobile_source(
                        library.manifests[manifest].settings().clone(),
                    ),
                });
            }
        }
        self.show_recording_library_confirmation(ctx, bounds);
        self.show_recording_retry(ctx, bounds);
    }

    fn show_recording_library_confirmation(&mut self, ctx: &egui::Context, bounds: egui::Rect) {
        let Some(action) = &self.recording_ui.library_action else {
            return;
        };
        let message = match action {
            LibraryAction::Source(_) => "Permanently delete this private source? It cannot be recovered or re-exported. Published MP4s and provider folders are not removed.",
            LibraryAction::Options(_) => "Delete the unused saved options for this unfinished recording?",
            LibraryAction::Staging(_) => "Delete this interrupted private video staging directory? Unexpected or changed files are retained and reported.",
            #[cfg(target_os = "android")]
            LibraryAction::Journal(_) => "Acknowledge this export notice? No destination files or folders will be removed; check the selected Files folder yourself.",
            #[cfg(test)]
            LibraryAction::GalleryJournal => "Acknowledge this export notice? No destination files or folders will be removed; check the selected Files folder yourself.",
        };
        let mut confirm = false;
        let mut close = false;
        let outcome = widgets::bottom_sheet_with_footer(
            ctx,
            "recording-library-confirm",
            bounds,
            |ui| {
                close = widgets::sheet_title(ui, "Confirm recording action", Some("Cancel"));
                ui.label(message);
            },
            |ui| {
                confirm = widgets::button(ui, widgets::ButtonKind::Destructive, "Confirm", true)
                    .clicked();
            },
        );
        #[cfg(test)]
        if self.recording_ui.gallery_display.is_some() {
            return;
        }
        if close || outcome.dismissed {
            self.recording_ui.library_action = None;
        } else if confirm {
            self.perform_recording_library_action(ctx);
        }
    }

    fn perform_recording_library_action(&mut self, ctx: &egui::Context) {
        let Some(action) = self.recording_ui.library_action.take() else {
            return;
        };
        let Some(mut library) = self.recording_ui.library.take() else {
            self.recording_ui.status = Some("Recording library is unavailable.".to_owned());
            return;
        };
        let Some(root) = self.recording_root() else {
            return;
        };
        let result = match action {
            LibraryAction::Source(index) => {
                let source = library.sources.remove(index);
                let manifest = library
                    .manifests
                    .iter()
                    .position(|manifest| manifest.id() == source.id())
                    .map(|index| library.manifests.remove(index));
                LibraryTask::delete_source(root, source, manifest, wake(ctx))
            }
            LibraryAction::Options(index) => {
                let manifest = library.manifests.remove(index);
                LibraryTask::delete_manifest(root, manifest, wake(ctx))
            }
            LibraryAction::Staging(index) => {
                let staging = library.staging.remove(index);
                LibraryTask::delete_staging(root, staging, wake(ctx))
            }
            #[cfg(target_os = "android")]
            LibraryAction::Journal(index) => {
                let journal = library.journals.remove(index);
                LibraryTask::acknowledge_journal(root, journal, wake(ctx))
            }
            #[cfg(test)]
            LibraryAction::GalleryJournal => {
                return;
            }
        };
        match result {
            Ok(task) => self.recording_ui.library_task = Some(task),
            Err(error) => self.recording_ui.status = Some(error),
        }
    }

    fn show_recording_retry(&mut self, ctx: &egui::Context, bounds: egui::Rect) {
        let Some(mut retry) = self.recording_ui.retry.take() else {
            return;
        };
        let mut close = false;
        let mut choose = false;
        let mut start = false;
        let busy = self.shell.recording_picker_pending;
        let ready =
            !busy && retry.settings.destination.is_some() && retry.settings.validate().is_ok();
        let outcome = widgets::bottom_sheet_with_footer(
            ctx,
            "recording-retry",
            bounds,
            |ui| {
                close = widgets::sheet_title(
                    ui,
                    if retry.recovered {
                        "Export recovered prefix"
                    } else {
                        "Export saved recording"
                    },
                    Some("Cancel"),
                );
                if retry.recovered {
                    ui.label("Only the validated prefix will be exported. Later output may be missing; the original incomplete source is retained. The export folder is marked recovered.");
                }
                ui.label("Replaying this private source exports its captured terminal output. Choose a destination you trust; cloud folders may sync outside fesTerm.");
                #[cfg(target_os = "android")]
                ui.label("The export folder may be visible before every video has finished.");
                if let Some(status) = &self.recording_ui.status {
                    ui.label(status);
                }
                ui.add_enabled_ui(!busy, |ui| {
                    let location = match &retry.settings.destination {
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
                    choose = widgets::button(
                        ui,
                        widgets::ButtonKind::Secondary,
                        "Choose export folder",
                        true,
                    )
                    .clicked();
                    widgets::text_field(
                        ui,
                        "Recording folder name",
                        &mut retry.settings.directory_template,
                        "recording-{recording_id}",
                        ui.available_width(),
                    );
                    widgets::text_field(
                        ui,
                        "MP4 filename",
                        &mut retry.settings.session_template,
                        "session-{session_index}.mp4",
                        ui.available_width(),
                    );
                    if let Err(error) = retry.settings.validate() {
                        ui.colored_label(ui.visuals().error_fg_color, error);
                    }
                    shell::recording_export_controls(ui, &mut retry.settings);
                    ui.label("The private source is kept after export until you delete it in the recording library.");
                });
            },
            |ui| {
                start = widgets::button(
                    ui,
                    widgets::ButtonKind::Primary,
                    if retry.recovered {
                        "Export recovered prefix"
                    } else {
                        "Export recording"
                    },
                    ready,
                )
                .clicked();
            },
        );
        #[cfg(test)]
        if self.recording_ui.gallery_display.is_some() {
            self.recording_ui.retry = Some(retry);
            return;
        }
        if close || outcome.dismissed {
            return;
        }
        self.recording_ui.retry = Some(retry);
        if choose {
            self.choose_recording_folder();
            if self.shell.recording_picker_pending {
                self.recording_ui.picking_for_retry = true;
            } else {
                self.recording_ui.status = self.shell.recording_error.take();
            }
        } else if start {
            self.retry_recording_export(ctx);
        }
    }

    fn retry_recording_export(&mut self, ctx: &egui::Context) {
        let cancellation = match self.foreground_export_cancellation() {
            Ok(cancellation) => cancellation,
            Err(error) => {
                self.recording_ui.status = Some(error);
                return;
            }
        };
        let Some(retry) = self.recording_ui.retry.take() else {
            return;
        };
        let Some(mut library) = self.recording_ui.library.take() else {
            self.recording_ui.status = Some("Recording library is unavailable.".to_owned());
            return;
        };
        let Some(root) = self.recording_root() else {
            return;
        };
        let source = library.sources.remove(retry.source);
        let manifest = library.manifests.remove(retry.manifest);
        match ExportTask::start_recovery_controlled(
            root,
            source,
            manifest,
            retry.settings,
            recording_tasks::encoder_factory(),
            recording_tasks::MobileBackend,
            ExportControl::new(cancellation, wake(ctx)),
        ) {
            Ok(task) => {
                self.recording_ui.exporting = Some(task);
                self.recording_ui.library_open = false;
                self.recording_ui.status = None;
            }
            Err(error) => self.recording_ui.status = Some(error),
        }
    }

    fn recording_root(&mut self) -> Option<PathBuf> {
        match &self.recording_storage {
            Some(storage) => Some(storage.path().to_owned()),
            None => {
                self.recording_ui.status = Some("Private recording storage is unavailable.".into());
                None
            }
        }
    }
}
