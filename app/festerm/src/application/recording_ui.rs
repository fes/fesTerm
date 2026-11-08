use std::collections::BTreeSet;

use eframe::egui;
use festerm_recording::{
    ExportDestination, LocalStart, PlaybackSpeed, RecordingFont, RecordingId, RecordingSettings,
    RecordingSourceState, RecordingTheme, StreamId,
};
use festerm_recording_export::LibrarySnapshot;

use super::{DesktopRecording, TabId, WindowId};

pub(in crate::application) struct SessionChoice {
    pub id: TabId,
    pub label: String,
    pub active: bool,
    pub included: bool,
}

#[derive(Clone, Copy)]
pub(super) enum PickerTarget {
    Settings,
    Start,
    Retry,
}

#[derive(Clone, Copy)]
pub(super) enum DeleteKind {
    Source(usize),
    Manifest(usize),
    Staging(usize),
}

pub(super) enum Dialog {
    Settings {
        draft: RecordingSettings,
    },
    Start {
        draft: RecordingSettings,
        selected: BTreeSet<TabId>,
        include_new: bool,
    },
    Export {
        draft: RecordingSettings,
    },
    Include {
        selected: BTreeSet<TabId>,
    },
    Library,
    Retry {
        source: usize,
        draft: RecordingSettings,
        recovered: bool,
    },
    Confirm {
        kind: DeleteKind,
    },
}

impl Dialog {
    pub fn start(draft: RecordingSettings, choices: Vec<SessionChoice>) -> Self {
        let selected = choices
            .iter()
            .find(|choice| choice.active)
            .map(|choice| BTreeSet::from([choice.id]))
            .unwrap_or_default();
        Self::Start {
            draft,
            selected,
            include_new: false,
        }
    }

    pub fn include() -> Self {
        Self::Include {
            selected: BTreeSet::new(),
        }
    }

    pub fn settings_for_picker(
        &mut self,
        target: Option<PickerTarget>,
    ) -> Option<&mut RecordingSettings> {
        match (self, target?) {
            (Self::Settings { draft }, PickerTarget::Settings)
            | (Self::Start { draft, .. }, PickerTarget::Start)
            | (Self::Export { draft }, PickerTarget::Start)
            | (Self::Retry { draft, .. }, PickerTarget::Retry) => Some(draft),
            _ => None,
        }
    }
}

pub(super) enum Action {
    Close,
    ChooseFolder(PickerTarget),
    Save(RecordingSettings),
    Reload,
    Begin {
        selected: Vec<TabId>,
        settings: RecordingSettings,
        include_new: bool,
    },
    Include(Vec<TabId>),
    PreviewExport(RecordingSettings),
    ConfirmExport(RecordingSettings),
    Refresh,
    OpenRetry(usize),
    Retry {
        source: usize,
        settings: RecordingSettings,
    },
    Confirm(DeleteKind),
    Delete(DeleteKind),
    BackToLibrary,
}

impl DesktopRecording {
    pub(in crate::application) fn show(
        &mut self,
        context: &egui::Context,
        window: WindowId,
        choices: &[SessionChoice],
    ) {
        if self.owner != window {
            return;
        }
        let Some(dialog) = &mut self.dialog else {
            return;
        };
        let mut action = None;
        let busy = self.library_task.is_some()
            || self.storage.as_ref().is_some_and(|storage| storage.busy());
        let picker_busy = self.picker.is_some();
        let preview_busy = self.preview_task.is_some();
        let preview = self.preview.as_ref();
        let privacy_ready = self
            .privacy
            .as_ref()
            .is_some_and(|privacy| !privacy.is_lost());
        let status = self.status.as_deref();
        egui::Modal::new(egui::Id::new("desktop-recording-dialog"))
            .backdrop_color(egui::Color32::from_black_alpha(128))
            .show(context, |ui| {
                ui.set_width(560.0_f32.min(context.content_rect().width() - 40.0).max(240.0));
                if let Some(status) = status {
                    ui.label(egui::RichText::new(status).color(egui::Color32::YELLOW));
                    ui.separator();
                }
                egui::ScrollArea::vertical()
                    .max_height((context.content_rect().height() - 100.0).max(150.0))
                    .show(ui, |ui| match dialog {
                        Dialog::Settings { draft } => {
                            ui.heading("Recording settings");
                            ui.label("These defaults stay on this device and are not part of profiles or workspaces.");
                            settings(ui, draft, PickerTarget::Settings, picker_busy, true, &mut action);
                            ui.horizontal(|ui| {
                                if ui.add_enabled(!busy && !picker_busy, egui::Button::new("Save defaults")).clicked() {
                                    action = Some(Action::Save(draft.clone()));
                                }
                                if ui.add_enabled(!busy && !picker_busy, egui::Button::new("Reload")).clicked() {
                                    action = Some(Action::Reload);
                                }
                                if ui.button("Close").clicked() { action = Some(Action::Close); }
                            });
                        }
                        Dialog::Start { draft, selected, include_new } => {
                            ui.heading("Record terminal sessions");
                            ui.label("Selected sessions start and stop together and export as separate silent MP4 files on one shared timeline.");
                            ui.label("Only visible terminal state and terminal output are recorded; local keyboard and clipboard input are not captured. Output can still contain echoed secrets, titles, paths, links or clipboard sequences.");
                            ui.label("Selected background tabs continue to be recorded. Reconnected sessions require explicit inclusion.");
                            ui.label("Export folders may be cloud-backed and copied outside fesTerm.");
                            ui.label("Private source and staging files are excluded from system backups where supported; third-party backup software may still copy them.");
                            #[cfg(target_os = "linux")]
                            ui.label("Linux recording requires GNOME Shell's built-in lock. Third-party lockers are not monitored; do not use them while recording.");
                            ui.separator();
                            ui.strong("Sessions to include");
                            if choices.is_empty() {
                                ui.label("No running terminal sessions are available.");
                            }
                            for choice in choices {
                                let mut checked = selected.contains(&choice.id);
                                if ui.checkbox(&mut checked, &choice.label).changed() {
                                    if checked { selected.insert(choice.id); } else { selected.remove(&choice.id); }
                                }
                            }
                            ui.checkbox(include_new, "Also include new sessions I open while recording");
                            if *include_new {
                                ui.label("If a new session cannot be safely included, the recording stops incomplete.");
                            }
                            ui.checkbox(&mut draft.delete_source_after_export, "Delete the private source after every MP4 is published");
                            ui.separator();
                            ui.collapsing("One-recording export options", |ui| {
                                settings(ui, draft, PickerTarget::Start, picker_busy, true, &mut action);
                            });
                            destination_summary(ui, draft);
                            ui.horizontal(|ui| {
                                if ui.add_enabled(!selected.is_empty() && !picker_busy && privacy_ready, egui::Button::new("Start recording")).clicked() {
                                    action = Some(Action::Begin {
                                        selected: selected.iter().copied().collect(),
                                        settings: draft.clone(),
                                        include_new: *include_new,
                                    });
                                }
                                if ui.button("Cancel").clicked() { action = Some(Action::Close); }
                            });
                        }
                        Dialog::Include { selected } => {
                            ui.heading("Include more sessions");
                            ui.label("Newly included sessions begin at their current visible screen on the existing shared timeline.");
                            let mut count = 0;
                            for choice in choices.iter().filter(|choice| !choice.included) {
                                count += 1;
                                let mut checked = selected.contains(&choice.id);
                                if ui.checkbox(&mut checked, &choice.label).changed() {
                                    if checked { selected.insert(choice.id); } else { selected.remove(&choice.id); }
                                }
                            }
                            if count == 0 { ui.label("No additional running sessions are available."); }
                            ui.horizontal(|ui| {
                                if ui.add_enabled(!selected.is_empty(), egui::Button::new("Include selected")).clicked() {
                                    action = Some(Action::Include(selected.iter().copied().collect()));
                                }
                                if ui.button("Cancel").clicked() { action = Some(Action::Close); }
                            });
                        }
                        Dialog::Export { draft } => {
                            ui.heading("Export recording");
                            ui.label("The completed private source is retained until you export or delete it. Pacing applies to every session on the shared timeline.");
                            settings(ui, draft, PickerTarget::Start, picker_busy, true, &mut action);
                            ui.separator();
                            match preview.filter(|preview| preview.matches(draft)) {
                                Some(preview) => {
                                    ui.label(format!("Estimated duration: {:.1} seconds", preview.duration().as_secs_f64()));
                                }
                                None if preview_busy => {
                                    ui.spinner();
                                    ui.label("Calculating the synchronized duration...");
                                }
                                None => {
                                    ui.label("Calculate the synchronized duration before exporting.");
                                }
                            }
                            ui.horizontal(|ui| {
                                if ui.add_enabled(!preview_busy && !picker_busy && draft.validate().is_ok(), egui::Button::new("Calculate duration")).clicked() {
                                    action = Some(Action::PreviewExport(draft.clone()));
                                }
                                if ui.add_enabled(!preview_busy && !picker_busy && preview.is_some_and(|preview| preview.matches(draft)), egui::Button::new("Export recording")).clicked() {
                                    action = Some(Action::ConfirmExport(draft.clone()));
                                }
                                if ui.button("Keep source for later").clicked() {
                                    action = Some(Action::Close);
                                }
                            });
                        }
                        Dialog::Library => {
                            ui.heading("Recording library");
                            ui.label("Private sources can contain sensitive terminal output. Only validated prefixes of incomplete sources can be recovered.");
                            if busy { ui.spinner(); ui.label("Inspecting private recording storage…"); }
                            if let Some(library) = &self.library {
                                library_contents(ui, library, &mut action);
                            } else if !busy {
                                ui.label("Recording storage has not been inspected.");
                            }
                            ui.horizontal(|ui| {
                                if ui.add_enabled(!busy, egui::Button::new("Refresh")).clicked() { action = Some(Action::Refresh); }
                                if ui.button("Close").clicked() { action = Some(Action::Close); }
                            });
                        }
                        Dialog::Retry { source, draft, recovered } => {
                            ui.heading(if *recovered { "Export recovered prefix" } else { "Retry recording export" });
                            if *recovered {
                                ui.label("Only the validated prefix will be exported. Later output may be missing; the original incomplete source is retained. The export folder is marked recovered.");
                            } else {
                                ui.label("The source remains private if encoding or publication fails. Existing exported folders are never overwritten.");
                            }
                            settings(ui, draft, PickerTarget::Retry, picker_busy, !*recovered, &mut action);
                            ui.horizontal(|ui| {
                                if ui.add_enabled(!picker_busy, egui::Button::new(if *recovered { "Export recovered prefix" } else { "Export" })).clicked() {
                                    action = Some(Action::Retry { source: *source, settings: draft.clone() });
                                }
                                if ui.button("Back").clicked() { action = Some(Action::BackToLibrary); }
                            });
                        }
                        Dialog::Confirm { kind } => {
                            ui.heading("Delete private recording data?");
                            ui.label(match kind {
                                DeleteKind::Source(_) => "The selected source and its saved export options will be permanently deleted.",
                                DeleteKind::Manifest(_) => "The unfinished saved export options will be permanently deleted.",
                                DeleteKind::Staging(_) => "The selected private staged videos will be permanently deleted.",
                            });
                            ui.horizontal(|ui| {
                                if ui.button("Delete").clicked() { action = Some(Action::Delete(*kind)); }
                                if ui.button("Cancel").clicked() { action = Some(Action::BackToLibrary); }
                            });
                        }
                    });
            });
        if context.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            action = Some(match dialog {
                Dialog::Retry { .. } | Dialog::Confirm { .. } => Action::BackToLibrary,
                _ => Action::Close,
            });
        }
        if action.is_some() {
            self.action = action;
        }
    }
}

fn destination_summary(ui: &mut egui::Ui, settings: &RecordingSettings) {
    match &settings.destination {
        Some(ExportDestination::DesktopPath(path)) => {
            ui.label(format!("Export folder: {}", path.display()));
        }
        Some(_) => {
            ui.label("Choose a folder for this device.");
        }
        None => {
            ui.label("No export folder selected. Open export options to choose one.");
        }
    }
}

fn settings(
    ui: &mut egui::Ui,
    settings: &mut RecordingSettings,
    picker: PickerTarget,
    picker_busy: bool,
    allow_deletion: bool,
    action: &mut Option<Action>,
) {
    ui.separator();
    destination_summary(ui, settings);
    if ui
        .add_enabled(!picker_busy, egui::Button::new("Choose folder…"))
        .clicked()
    {
        *action = Some(Action::ChooseFolder(picker));
    }
    ui.label(
        "A new subdirectory is created for each recording; existing folders are never overwritten.",
    );
    ui.label("Recording folder template");
    ui.text_edit_singleline(&mut settings.directory_template);
    ui.label("MP4 filename template");
    ui.text_edit_singleline(&mut settings.session_template);
    ui.label(
        "Tokens: {recording_id}, {date}, {time}, and {session_index} (required in MP4 names).",
    );
    match settings.validate().and_then(|validated| {
        let id = RecordingId::parse("00112233445566778899aabbccddeeff")
            .map_err(|error| error.to_string())?;
        let start = LocalStart::new(2026, 1, 2, 15, 4, 5).map_err(|error| error.to_string())?;
        let stream = StreamId::from_index(1).ok_or_else(|| "Invalid preview stream".to_owned())?;
        Ok(format!(
            "{}/{}",
            validated
                .templates
                .directory(id, start)
                .map_err(|error| error.to_string())?,
            validated
                .templates
                .session(id, start, stream)
                .map_err(|error| error.to_string())?
        ))
    }) {
        Ok(preview) => {
            ui.label(format!("Example: {preview}"));
        }
        Err(error) => {
            ui.label(egui::RichText::new(error).color(egui::Color32::YELLOW));
        }
    }
    ui.separator();
    egui::ComboBox::from_id_salt("recording-speed")
        .selected_text(speed_label(settings.speed))
        .show_ui(ui, |ui| {
            for speed in [
                PlaybackSpeed::Half,
                PlaybackSpeed::Normal,
                PlaybackSpeed::Double,
                PlaybackSpeed::Quadruple,
                PlaybackSpeed::Octuple,
            ] {
                ui.selectable_value(&mut settings.speed, speed, speed_label(speed));
            }
        });
    ui.checkbox(
        &mut settings.compress_idle,
        "Compress gaps when all sessions are idle",
    );
    if settings.compress_idle {
        ui.horizontal(|ui| {
            ui.label("Idle over");
            ui.add(egui::DragValue::new(&mut settings.idle_threshold_seconds).range(1..=7200));
            ui.label("s becomes");
            ui.add(egui::DragValue::new(&mut settings.idle_target_millis).range(1..=7_200_000));
            ui.label("ms");
        });
    }
    ui.checkbox(
        &mut settings.normalize_typing,
        "Normalize echoed typing (inferred from output)",
    );
    if settings.normalize_typing {
        ui.horizontal(|ui| {
            ui.label("Typing pace");
            ui.add(egui::DragValue::new(&mut settings.typing_characters_per_second).range(5..=60));
            ui.label("characters/s");
        });
    }
    ui.checkbox(&mut settings.limit_long_output, "Limit long output bursts");
    if settings.limit_long_output {
        ui.horizontal(|ui| {
            ui.label("Output over");
            ui.add(
                egui::DragValue::new(&mut settings.long_output_threshold_seconds).range(1..=7200),
            );
            ui.label("s becomes");
            ui.add(
                egui::DragValue::new(&mut settings.long_output_target_millis).range(1..=7_200_000),
            );
            ui.label("ms");
        });
    }
    ui.checkbox(
        &mut settings.reveal_output,
        "Reveal large output progressively",
    );
    if settings.reveal_output {
        ui.horizontal(|ui| {
            ui.label("Output reveal");
            ui.add(egui::DragValue::new(&mut settings.output_lines_per_second).range(20..=200));
            ui.label("lines/s");
        });
    }
    ui.separator();
    ui.strong("Shared export appearance");
    ui.horizontal(|ui| {
        let font_label = ui.label("Font");
        egui::ComboBox::from_id_salt("recording-font")
            .selected_text(settings.appearance.font.label())
            .show_ui(ui, |ui| {
                for font in [
                    RecordingFont::JetBrainsMono,
                    RecordingFont::JetBrainsMonoLigatures,
                    RecordingFont::IosevkaTerm,
                    RecordingFont::JuliaMono,
                    RecordingFont::MapleMono,
                ] {
                    ui.selectable_value(&mut settings.appearance.font, font, font.label());
                }
            })
            .response
            .labelled_by(font_label.id);
        let theme_label = ui.label("Theme");
        egui::ComboBox::from_id_salt("recording-theme")
            .selected_text(settings.appearance.theme.label())
            .show_ui(ui, |ui| {
                for theme in [RecordingTheme::FesTermDark, RecordingTheme::FesTermLight] {
                    ui.selectable_value(&mut settings.appearance.theme, theme, theme.label());
                }
            })
            .response
            .labelled_by(theme_label.id);
    });
    ui.horizontal(|ui| {
        let font_size = ui.label("Font size");
        ui.add(egui::DragValue::new(&mut settings.appearance.font_size_points).range(8..=64))
            .labelled_by(font_size.id);
        ui.label("pt");
        let width = ui.label("Width");
        ui.add(egui::DragValue::new(&mut settings.appearance.width).range(2..=3840))
            .labelled_by(width.id);
        let height = ui.label("Height");
        ui.add(egui::DragValue::new(&mut settings.appearance.height).range(2..=2160))
            .labelled_by(height.id);
    });
    ui.label("Dimensions must be even. Optional fonts must be downloaded in Terminal font settings before export; missing fonts are never substituted.");
    ui.label("The same appearance applies to all videos and is saved beside them as recording-appearance.json.");
    if allow_deletion {
        ui.checkbox(
            &mut settings.delete_source_after_export,
            "Delete private source after every video is successfully published",
        );
        ui.label("Failures before verified publication retain the source. Once deletion begins, it may finish despite an interruption.");
    } else {
        ui.label("The original incomplete source is always retained after recovery.");
    }
    ui.label("Exported videos and third-party backups may contain sensitive output.");
}

fn speed_label(speed: PlaybackSpeed) -> &'static str {
    match speed {
        PlaybackSpeed::Half => "0.5x",
        PlaybackSpeed::Normal => "1x",
        PlaybackSpeed::Double => "2x",
        PlaybackSpeed::Quadruple => "4x",
        PlaybackSpeed::Octuple => "8x",
    }
}

fn library_contents(ui: &mut egui::Ui, library: &LibrarySnapshot, action: &mut Option<Action>) {
    if library.sources.is_empty() && library.manifests.is_empty() && library.staging.is_empty() {
        ui.label("No retained or unfinished recordings.");
    }
    for (index, source) in library.sources.iter().enumerate() {
        ui.separator();
        ui.strong(format!("Recording {}", source.id()));
        match source.state() {
            RecordingSourceState::Empty => {
                ui.label("Empty unfinished source");
            }
            RecordingSourceState::Complete { duration, reason } => {
                ui.label(format!(
                    "Complete: {:.1} seconds ({reason:?})",
                    duration.as_secs_f64()
                ));
            }
            RecordingSourceState::Incomplete { detail, prefix } => {
                ui.label(format!("Incomplete: {detail}"));
                if let Some(prefix) = prefix {
                    ui.label(format!(
                        "Validated prefix: {:.1} seconds, {} session(s); later output may be missing.",
                        prefix.duration().as_secs_f64(),
                        prefix.stream_count()
                    ));
                }
            }
        }
        ui.label(format!(
            "{} bytes{}",
            source.length(),
            if source.is_partial() {
                " · unfinished filename"
            } else {
                ""
            }
        ));
        ui.horizontal(|ui| {
            let recovered = matches!(source.state(), RecordingSourceState::Incomplete { prefix: Some(_), .. });
            let retryable = (matches!(source.state(), RecordingSourceState::Complete { reason, .. } if reason.is_complete()) || recovered)
                && library.manifests.iter().any(|manifest| manifest.id() == source.id());
            if ui.add_enabled(retryable, egui::Button::new(if recovered { "Export recovered prefix…" } else { "Retry export" })).clicked() {
                *action = Some(Action::OpenRetry(index));
            }
            if ui.button("Delete…").clicked() { *action = Some(Action::Confirm(DeleteKind::Source(index))); }
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
        ui.separator();
        ui.label(format!("Unfinished options for {}", manifest.id()));
        if ui.button("Delete options…").clicked() {
            *action = Some(Action::Confirm(DeleteKind::Manifest(index)));
        }
    }
    for (index, staging) in library.staging.iter().enumerate() {
        ui.separator();
        ui.label(format!("Private staging: {}", staging.path().display()));
        if ui.button("Delete staged videos…").clicked() {
            *action = Some(Action::Confirm(DeleteKind::Staging(index)));
        }
    }
}
