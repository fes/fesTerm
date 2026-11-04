//! Local source authorities and editor views. Canonical text/history live in
//! festerm-mobile-docs; neither a widget nor UIDocument owns an editor buffer.

use std::collections::{BTreeMap, BTreeSet};

use eframe::egui;
use festerm_document::{DocumentId, DocumentOrigin, LocalOrigin};
use festerm_ios_window::{
    DocumentFile, DocumentFileError, DocumentFileEvent, DocumentFileSnapshot, DocumentPicker,
};
use festerm_mobile_docs::{
    CloseOutcome, DocumentError, Documents, EditTarget, ReadTicket, SaveTicket, SourceSnapshot,
    SourceVersion, ViewId,
};

use super::model::{SurfaceKind, SurfaceState, SurfaceSummary};
use super::theme::{color, space, type_scale};
use super::widgets::{self, ButtonKind};

struct Source {
    file: DocumentFile,
    read: Option<ReadTicket>,
    save: Option<(u64, SaveTicket)>,
}

struct Opening {
    file: DocumentFile,
    existing: BTreeMap<DocumentId, ReadTicket>,
}

struct View {
    tab: u64,
    id: ViewId,
}

#[derive(Default)]
pub(super) struct DocumentHost {
    pub registry: Documents,
    views: Vec<View>,
    pub active: Option<ViewId>,
    picker: Option<DocumentPicker>,
    opening: Option<Opening>,
    sources: BTreeMap<DocumentId, Source>,
    refresh: BTreeSet<DocumentId>,
    writer_epoch: u64,
    operation: u64,
    pub notice: Option<String>,
    close: Option<ViewId>,
    close_requested: Option<ViewId>,
    compare: Option<EditTarget>,
}

impl DocumentHost {
    pub fn modal_open(&self) -> bool {
        self.close.is_some() || self.close_requested.is_some() || self.compare.is_some()
    }

    pub fn bind_picker(&mut self, picker: Option<DocumentPicker>) {
        self.picker = picker;
    }

    pub fn contains(&self, tab: u64) -> bool {
        self.views.iter().any(|view| view.tab == tab)
    }

    pub fn activate(&mut self, tab: u64) -> bool {
        if let Some(view) = self.views.iter().find(|view| view.tab == tab) {
            self.active = Some(view.id);
            true
        } else {
            false
        }
    }

    pub fn surfaces(&self, visible: bool) -> Vec<SurfaceSummary> {
        self.views
            .iter()
            .filter_map(|view| {
                let entry = self.registry.view(view.id).ok()?;
                let (state, status, marker) = if !entry.available() {
                    (SurfaceState::Disconnected, "Access unavailable", " !")
                } else if entry.conflict().is_some() {
                    (
                        SurfaceState::Disconnected,
                        "File changed outside fesTerm",
                        " !",
                    )
                } else if entry.saving() {
                    (SurfaceState::Connecting, "Saving", " *")
                } else if entry.dirty() {
                    (SurfaceState::Modified, "Unsaved changes", " *")
                } else if !entry.writable() {
                    (SurfaceState::Connected, "Read-only", "")
                } else {
                    (SurfaceState::Connected, "Saved", "")
                };
                Some(SurfaceSummary {
                    id: view.tab,
                    title: format!("{}{marker}", entry.origin().file_name()),
                    kind: SurfaceKind::Document,
                    detail: entry.origin().qualified_label(),
                    state,
                    status: status.to_owned(),
                    current: visible && self.active == Some(view.id),
                    unread: false,
                })
            })
            .collect()
    }

    pub fn browse(&mut self) {
        let result = if self.opening.is_some() {
            Err(DocumentFileError::Busy)
        } else {
            self.picker
                .as_ref()
                .ok_or(DocumentFileError::Unsupported)
                .and_then(DocumentPicker::open)
        };
        match result {
            Ok(()) => self.notice = None,
            Err(error) => self.notice = Some(error.message().to_owned()),
        }
    }

    pub fn invalidate_sources(&mut self) {
        for id in self.sources.keys().copied().collect::<Vec<_>>() {
            if let Err(error) = self.registry.set_access(id, false, false) {
                self.notice = Some(error.message());
            }
            self.refresh.insert(id);
        }
    }

    /// Returns true only when an authorized source attached a new view.
    pub fn poll(&mut self, next_tab: &mut u64) -> bool {
        if let Some(result) = self.picker.as_ref().and_then(DocumentPicker::take_result) {
            match result {
                Ok(file) => {
                    if self.opening.is_some() {
                        self.notice = Some(DocumentFileError::Busy.message().to_owned());
                    } else {
                        let existing = self
                            .registry
                            .views()
                            .filter_map(|(view, id)| {
                                self.registry
                                    .read_ticket(view)
                                    .ok()
                                    .map(|ticket| (id, ticket))
                            })
                            .collect();
                        match file.open() {
                            Ok(()) => self.opening = Some(Opening { file, existing }),
                            Err(error) => self.notice = Some(error.message().to_owned()),
                        }
                    }
                }
                Err(error) => self.notice = Some(error.message().to_owned()),
            }
        }
        let mut attached = false;
        if let Some(opening) = &self.opening {
            for event in opening.file.take_events() {
                match event {
                    DocumentFileEvent::Loaded(snapshot) => {
                        let opening = self.opening.take().expect("pending native open");
                        match self.attach(opening, snapshot, next_tab) {
                            Ok(()) => attached = true,
                            Err(error) => self.notice = Some(error),
                        }
                        break;
                    }
                    DocumentFileEvent::Unavailable(error) => {
                        self.opening = None;
                        self.notice = Some(error.message().to_owned());
                        break;
                    }
                    _ => self.notice = Some("Unexpected result while opening a file.".to_owned()),
                }
            }
        }
        let epoch = festerm_ios_window::local_file_mutation_epoch();
        if epoch != self.writer_epoch {
            self.writer_epoch = epoch;
            self.refresh.extend(self.sources.keys().copied());
        }
        let events: Vec<_> = self
            .sources
            .iter()
            .flat_map(|(id, source)| {
                source
                    .file
                    .take_events()
                    .into_iter()
                    .map(|event| (*id, event))
            })
            .collect();
        for (id, event) in events {
            if let Err(error) = self.source_event(id, event) {
                self.notice = Some(error);
            }
        }
        for id in self.refresh.clone() {
            match self.start_refresh(id) {
                Ok(()) => {
                    self.refresh.remove(&id);
                }
                Err(DocumentFileError::Busy) => {}
                Err(error) => {
                    self.refresh.remove(&id);
                    self.unavailable(id);
                    self.notice = Some(error.message().to_owned());
                }
            }
        }
        attached
    }

    fn attach(
        &mut self,
        opening: Opening,
        snapshot: DocumentFileSnapshot,
        next_tab: &mut u64,
    ) -> Result<(), String> {
        let origin: DocumentOrigin = LocalOrigin::new(snapshot.path)
            .map_err(|_| "The selected file has no usable text-document identity.".to_owned())?
            .into();
        let existing = self.registry.views().find_map(|(_, id)| {
            self.registry
                .get(id)
                .ok()
                .filter(|entry| entry.origin().key() == origin.key())
                .map(|_| id)
        });
        if existing.is_some_and(|id| self.registry.get(id).is_ok_and(|entry| entry.saving())) {
            return Err(DocumentError::Busy.message());
        }
        let next = next_tab
            .checked_add(1)
            .ok_or_else(|| "The open-surface identifier limit was reached.".to_owned())?;
        let view = self
            .registry
            .open(
                origin,
                &snapshot.bytes,
                Some(SourceVersion(snapshot.version)),
                snapshot.writable,
            )
            .map_err(|error| error.message())?;
        let id = self
            .registry
            .document_for(view)
            .map_err(|error| error.message())?;
        let observed = if existing.is_some() {
            opening
                .existing
                .get(&id)
                .ok_or(DocumentError::Stale)
                .and_then(|ticket| {
                    self.registry.observe(
                        ticket,
                        SourceSnapshot {
                            bytes: snapshot.bytes,
                            version: SourceVersion(snapshot.version),
                        },
                    )
                })
        } else {
            Ok(false)
        };
        self.registry
            .set_access(id, true, snapshot.writable)
            .map_err(|error| error.message())?;
        self.sources.insert(
            id,
            Source {
                file: opening.file,
                read: None,
                save: None,
            },
        );
        self.views.push(View {
            tab: *next_tab,
            id: view,
        });
        *next_tab = next;
        self.active = Some(view);
        match observed {
            Ok(_) => self.notice = None,
            Err(DocumentError::Stale) => {
                self.refresh.insert(id);
                self.notice = Some(
                    "The open document changed during the read; checking its source again."
                        .to_owned(),
                );
            }
            Err(error) => self.notice = Some(error.message()),
        }
        Ok(())
    }

    fn view_for(&self, id: DocumentId) -> Result<ViewId, DocumentError> {
        self.registry
            .views()
            .find_map(|(view, document)| (document == id).then_some(view))
            .ok_or(DocumentError::Closed)
    }

    fn unavailable(&mut self, id: DocumentId) {
        if let Err(error) = self.registry.set_access(id, false, false) {
            self.notice = Some(error.message());
        }
        self.sources.remove(&id);
    }

    fn start_refresh(&mut self, id: DocumentId) -> Result<(), DocumentFileError> {
        let view = self
            .view_for(id)
            .map_err(|_| DocumentFileError::Unavailable)?;
        let ticket = self
            .registry
            .read_ticket(view)
            .map_err(|_| DocumentFileError::Unavailable)?;
        let source = self
            .sources
            .get_mut(&id)
            .ok_or(DocumentFileError::Unavailable)?;
        if source.read.is_some() || source.save.is_some() {
            return Err(DocumentFileError::Busy);
        }
        source.file.refresh()?;
        source.read = Some(ticket);
        Ok(())
    }

    fn source_event(&mut self, id: DocumentId, event: DocumentFileEvent) -> Result<(), String> {
        match event {
            DocumentFileEvent::Loaded(snapshot) => {
                let ticket = self
                    .sources
                    .get_mut(&id)
                    .and_then(|source| source.read.take());
                if let Some(ticket) = ticket {
                    let observed = self.registry.observe(
                        &ticket,
                        SourceSnapshot {
                            bytes: snapshot.bytes,
                            version: SourceVersion(snapshot.version),
                        },
                    );
                    if let Err(error) = observed {
                        if error == DocumentError::Stale {
                            self.refresh.insert(id);
                            // An obsolete read may revoke write access, but
                            // only a fresh read may restore source authority.
                            if !snapshot.writable {
                                let available = self
                                    .registry
                                    .get(id)
                                    .map_err(|error| error.message())?
                                    .available();
                                self.registry
                                    .set_access(id, available, false)
                                    .map_err(|error| error.message())?;
                            }
                        }
                        return Err(error.message());
                    }
                    let entry = self.registry.get(id).map_err(|error| error.message())?;
                    if !entry.available() || entry.writable() != snapshot.writable {
                        self.registry
                            .set_access(id, true, snapshot.writable)
                            .map_err(|error| error.message())?;
                    }
                } else {
                    // UIKit may reload automatically. Recheck under a ticket
                    // captured before our own read, never at late publication.
                    self.refresh.insert(id);
                }
            }
            DocumentFileEvent::Saved { operation, version } => {
                let ticket = self.take_save(id, operation)?;
                self.registry
                    .finish_save(&ticket, SourceVersion(version))
                    .map_err(|error| error.message())?;
            }
            DocumentFileEvent::SaveFailed { operation, error } => {
                let ticket = self.take_save(id, operation)?;
                self.registry
                    .fail_save(&ticket)
                    .map_err(|error| error.message())?;
                match error {
                    DocumentFileError::Changed => {
                        self.refresh.insert(id);
                    }
                    DocumentFileError::Unavailable => self.unavailable(id),
                    DocumentFileError::ReadOnly => self
                        .registry
                        .set_access(id, true, false)
                        .map_err(|error| error.message())?,
                    _ => {}
                }
                return Err(error.message().to_owned());
            }
            DocumentFileEvent::Unavailable(error) => {
                if let Some(source) = self.sources.get_mut(&id) {
                    source.read = None;
                }
                self.unavailable(id);
                return Err(error.message().to_owned());
            }
        }
        Ok(())
    }

    fn take_save(&mut self, id: DocumentId, operation: u64) -> Result<SaveTicket, String> {
        let source = self
            .sources
            .get_mut(&id)
            .ok_or_else(|| DocumentError::Closed.message())?;
        if source
            .save
            .as_ref()
            .is_none_or(|(pending, _)| *pending != operation)
        {
            return Err(
                "A stale file-save result was refused; editor content was kept.".to_owned(),
            );
        }
        Ok(source.save.take().expect("matching pending save").1)
    }

    pub fn save(&mut self, view: ViewId) {
        let result = (|| {
            let id = self
                .registry
                .document_for(view)
                .map_err(|error| error.message())?;
            let source = self
                .sources
                .get_mut(&id)
                .ok_or_else(|| DocumentError::Unavailable.message())?;
            if source.read.is_some() || source.save.is_some() {
                return Err(DocumentFileError::Busy.message().to_owned());
            }
            let operation = self
                .operation
                .checked_add(1)
                .ok_or_else(|| "The file-operation identifier limit was reached.".to_owned())?;
            let (ticket, bytes) = self
                .registry
                .prepare_save(view, false)
                .map_err(|error| error.message())?;
            if let Err(error) = source.file.save(operation, ticket.expected.0, bytes) {
                self.registry
                    .fail_save(&ticket)
                    .map_err(|error| error.message())?;
                return Err(error.message().to_owned());
            }
            self.operation = operation;
            source.save = Some((operation, ticket));
            Ok(())
        })();
        self.notice = result.err();
    }

    pub fn close_tab(&mut self, tab: u64, ctx: &egui::Context) {
        if let Some(view) = self.views.iter().find(|view| view.tab == tab) {
            self.close_view(view.id, false, ctx);
        }
    }

    pub fn request_close(&mut self, tab: u64) {
        self.close_requested = self
            .views
            .iter()
            .find(|view| view.tab == tab)
            .map(|view| view.id);
    }

    pub fn refresh_active(&mut self) {
        if let Some(view) = self.active {
            match self.registry.document_for(view) {
                Ok(id) => {
                    self.refresh.insert(id);
                }
                Err(error) => self.notice = Some(error.message()),
            }
        }
    }

    fn close_view(&mut self, view: ViewId, discard: bool, ctx: &egui::Context) {
        match self.registry.close(view, discard) {
            Ok(CloseOutcome::NeedsConfirmation) => self.close = Some(view),
            Ok(CloseOutcome::Closed { released }) => {
                widgets::forget_local_text_edit(ctx, body_id(view));
                self.views.retain(|entry| entry.id != view);
                if let Some(id) = released {
                    self.sources.remove(&id);
                    self.refresh.remove(&id);
                }
                if self.active == Some(view) {
                    self.active = None;
                }
                self.close = None;
                if self
                    .compare
                    .as_ref()
                    .is_some_and(|target| target.view == view)
                {
                    self.compare = None;
                }
            }
            Err(error) => self.notice = Some(error.message()),
        }
    }

    pub fn duplicate(&mut self, view: ViewId, next_tab: &mut u64) {
        let result = (|| {
            let next = next_tab.checked_add(1).ok_or(DocumentError::Capacity)?;
            let id = self.registry.duplicate(view)?;
            self.views.push(View { tab: *next_tab, id });
            *next_tab = next;
            self.active = Some(id);
            Ok::<_, DocumentError>(())
        })();
        if let Err(error) = result {
            self.notice = Some(error.message());
        }
    }

    fn show_actions(&mut self, ui: &mut egui::Ui, view: ViewId) -> bool {
        let mut acted = false;
        ui.horizontal_wrapped(|ui| {
            let writable = self.registry.view(view).is_ok_and(|entry| entry.writable());
            let can_save = self.registry.document_for(view).is_ok_and(|id| {
                !self.refresh.contains(&id)
                    && self
                        .sources
                        .get(&id)
                        .is_some_and(|source| source.read.is_none() && source.save.is_none())
                    && self.registry.get(id).is_ok_and(|entry| {
                        entry.writable()
                            && entry.dirty()
                            && !entry.saving()
                            && entry.conflict().is_none_or(|change| change.kept_edits())
                    })
            });
            let size = egui::vec2(80.0, space::BUTTON_HEIGHT);
            if widgets::button_sized(ui, ButtonKind::Primary, "Save", can_save, size).clicked() {
                self.save(view);
                acted = true;
            }
            if widgets::button_sized(ui, ButtonKind::Secondary, "Undo", writable, size).clicked() {
                let result = self.registry.undo(view).map(|_| ());
                self.record(result);
                acted = true;
            }
            if widgets::button_sized(ui, ButtonKind::Secondary, "Redo", writable, size).clicked() {
                let result = self.registry.redo(view).map(|_| ());
                self.record(result);
                acted = true;
            }
        });
        acted
    }

    pub fn show(&mut self, ui: &mut egui::Ui, compact: bool) {
        if let Some(notice) = &self.notice {
            ui.add(egui::Label::new(egui::RichText::new(notice).color(color::DANGER)).wrap());
        }
        let Some(view) = self.active else {
            widgets::screen_title(ui, "Documents", "Local text and Markdown");
            widgets::footnote(
                ui,
                "Open a file in place through Files. Your terminal sessions remain independent.",
            );
            if widgets::button(
                ui,
                ButtonKind::Primary,
                "Browse in Files",
                self.opening.is_none(),
            )
            .clicked()
            {
                self.browse();
            }
            if self.opening.is_some() {
                ui.label("Opening selected file…");
            }
            widgets::section_header(ui, "Open documents", None, false);
            for surface in self.surfaces(false) {
                let mut row = widgets::RowSpec::new(&surface.title);
                row.icon = Some(festerm_ui_egui::icon::Icon::SourceView);
                row.subtitle = Some(&surface.status);
                if widgets::list_row(ui, &row).clicked() {
                    self.activate(surface.id);
                }
            }
            return;
        };
        if !compact {
            self.show_actions(ui, view);
        }
        if let Ok(entry) = self.registry.view(view) {
            ui.small(format!(
                "{} · {}",
                entry.origin().file_name(),
                if !entry.available() {
                    "Access unavailable — reopen through Files"
                } else if entry.conflict().is_some() {
                    "Changed outside fesTerm — edits preserved"
                } else if !entry.writable() {
                    "Read-only"
                } else if entry.saving() {
                    "Saving…"
                } else if entry.dirty() {
                    "Unsaved changes"
                } else {
                    "Saved"
                }
            ));
            if entry.conflict().is_some()
                && widgets::button(ui, ButtonKind::Secondary, "Compare / resolve", true).clicked()
            {
                match self.registry.target(view, 0..0) {
                    Ok(target) => self.compare = Some(target),
                    Err(error) => self.notice = Some(error.message()),
                }
            }
        }
        self.show_source(ui, view);
    }

    fn record(&mut self, result: Result<(), DocumentError>) {
        if let Err(error) = result {
            self.notice = Some(error.message());
        }
    }

    fn show_source(&mut self, ui: &mut egui::Ui, view: ViewId) {
        let id = body_id(view);
        if ui.ctx().memory(|memory| memory.has_focus(id)) {
            let (redo, undo) = ui.input_mut(|input| {
                let redo = input.consume_key(
                    egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
                    egui::Key::Z,
                ) | input.consume_key(egui::Modifiers::COMMAND, egui::Key::Y);
                (
                    redo,
                    input.consume_key(egui::Modifiers::COMMAND, egui::Key::Z),
                )
            });
            if redo {
                let result = self.registry.redo(view).map(|_| ());
                self.record(result);
            } else if undo {
                let result = self.registry.undo(view).map(|_| ());
                self.record(result);
            }
        }
        let Ok(entry) = self.registry.view(view) else {
            return;
        };
        let writable = entry.writable();
        let mut read_only = entry.text().text();
        let mut text = entry.text().text().to_owned();
        let target = match self.registry.target(view, 0..0) {
            Ok(target) => target,
            Err(error) => {
                self.notice = Some(error.message());
                return;
            }
        };
        let mut state = egui::TextEdit::load_state(ui.ctx(), id).unwrap_or_default();
        state.set_undoer(egui::util::undoer::Undoer::with_settings(
            egui::util::undoer::Settings {
                max_undos: 1,
                ..Default::default()
            },
        ));
        state.store(ui.ctx(), id);
        let output = egui::ScrollArea::vertical()
            .id_salt(("document-scroll", view.raw()))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let edit = if writable {
                    egui::TextEdit::multiline(&mut text)
                } else {
                    egui::TextEdit::multiline(&mut read_only)
                };
                widgets::local_text_edit(
                    ui,
                    id,
                    edit.code_editor()
                        .font(egui::FontId::monospace(type_scale::BODY))
                        .desired_width(ui.available_width())
                        .desired_rows(12)
                        .hint_text("Start writing")
                        .frame(egui::Frame::NONE)
                        .margin(egui::vec2(space::SM, space::SM)),
                    false,
                    true,
                )
            })
            .inner;
        if output.response.changed() {
            let result = self.registry.sync(&target, &text).map(|_| ());
            self.record(result);
        }
        // Never retain an independent widget history across document actions.
        let mut state = output.state;
        state.set_undoer(egui::util::undoer::Undoer::with_settings(
            egui::util::undoer::Settings {
                max_undos: 1,
                ..Default::default()
            },
        ));
        state.store(ui.ctx(), id);
        if output.response.lost_focus() {
            let result = self.registry.end_typing(view);
            self.record(result);
        }
    }

    pub fn show_close(&mut self, ctx: &egui::Context, bounds: egui::Rect) {
        if let Some(view) = self.close_requested.take() {
            self.close_view(view, false, ctx);
        }
        let Some(view) = self.close else {
            self.show_compare(ctx, bounds);
            return;
        };
        let label = match self.registry.view(view) {
            Ok(entry) => entry.origin().qualified_label(),
            Err(_) => {
                self.close = None;
                return;
            }
        };
        let mut discard = false;
        let mut cancel = false;
        widgets::bottom_sheet(ctx, "document-dirty-close", bounds, |ui| {
            cancel |= widgets::sheet_title(ui, "Unsaved document", None);
            widgets::footnote(ui, &label);
            ui.label("Discard unsaved changes in the last view? This cannot be undone.");
            discard =
                widgets::button(ui, ButtonKind::Destructive, "Discard changes", true).clicked();
            cancel |= widgets::button(ui, ButtonKind::Secondary, "Keep editing", true).clicked();
        });
        if discard {
            self.close_view(view, true, ctx);
        } else if cancel {
            self.close = None;
        }
    }
}

impl DocumentHost {
    fn resolve(&mut self, target: &EditTarget, keep: bool) -> Result<(), DocumentError> {
        if self
            .registry
            .target(target.view, target.selection.clone())?
            != *target
        {
            return Err(DocumentError::Stale);
        }
        if keep {
            self.registry.keep_edits(target.view)
        } else {
            self.registry.reload(target.view)
        }
    }

    fn show_compare(&mut self, ctx: &egui::Context, bounds: egui::Rect) {
        let Some(target) = self.compare.clone() else {
            return;
        };
        let current = self.registry.target(target.view, target.selection.clone());
        if current.as_ref() != Ok(&target) {
            self.compare = None;
            self.notice = Some(DocumentError::Stale.message());
            return;
        }
        let outside = self.registry.view(target.view).and_then(|entry| {
            let change = entry.conflict().ok_or(DocumentError::Conflict)?;
            festerm_document::TextDocument::from_bytes(
                change.bytes(),
                festerm_mobile_docs::MOBILE_DOCUMENT_BOUNDS,
            )
            .map(|document| document.text().to_owned())
            .map_err(DocumentError::from)
        });
        let outside = match outside {
            Ok(text) => text,
            Err(error) => {
                self.compare = None;
                self.notice = Some(error.message());
                return;
            }
        };
        let writable = self
            .registry
            .view(target.view)
            .is_ok_and(|entry| entry.writable());
        let mut decision = None;
        let mut cancel = false;
        widgets::bottom_sheet(ctx, "document-compare", bounds, |ui| {
            cancel |= widgets::sheet_title(ui, "File changed outside fesTerm", None);
            widgets::footnote(ui, "External version (read-only)");
            egui::ScrollArea::vertical()
                .max_height(bounds.height() * 0.3)
                .show(ui, |ui| {
                    ui.add(egui::Label::new(egui::RichText::new(&outside).monospace()).wrap());
                });
            widgets::footnote(ui, "Keep edits preserves your buffer. A later explicit Save replaces this external version. Reload discards your edits and history.");
            ui.horizontal_wrapped(|ui| {
                if widgets::button_sized(
                    ui,
                    ButtonKind::Primary,
                    "Keep my edits",
                    writable,
                    egui::vec2(160.0, space::BUTTON_HEIGHT),
                )
                .clicked()
                {
                    decision = Some(true);
                }
                if widgets::button_sized(
                    ui,
                    ButtonKind::Destructive,
                    "Reload and discard edits",
                    true,
                    egui::vec2(244.0, space::BUTTON_HEIGHT),
                )
                .clicked()
                {
                    decision = Some(false);
                }
                cancel |= widgets::button_sized(
                    ui,
                    ButtonKind::Secondary,
                    "Cancel",
                    true,
                    egui::vec2(96.0, space::BUTTON_HEIGHT),
                )
                .clicked();
            });
        });
        if let Some(keep) = decision {
            let result = self.resolve(&target, keep);
            self.record(result);
            self.compare = None;
        } else if cancel {
            self.compare = None;
        }
    }
}

fn body_id(view: ViewId) -> egui::Id {
    egui::Id::new(("document-source", view.raw()))
}

#[cfg(all(test, not(target_os = "ios")))]
#[path = "documents_tests.rs"]
mod tests;

impl super::MobileApp {
    pub(super) fn show_documents(&mut self, ui: &mut egui::Ui) {
        use super::{terminal_chrome, theme, Surface};
        let surfaces = self.surfaces();
        let compact = ui.available_height() < 360.0;
        let mut back = false;
        let mut chip = terminal_chrome::ChipAction::None;
        egui::Panel::top("mobile-document-header")
            .frame(egui::Frame::new().fill(color::TERMINAL_CHROME))
            .show_separator_line(false)
            .show(ui, |ui| {
                let center = if self.documents.active.is_some() {
                    terminal_chrome::HeaderCenter::Chips(&surfaces)
                } else {
                    terminal_chrome::HeaderCenter::Title {
                        title: "Documents",
                        subtitle: "Authorized local sources",
                    }
                };
                let header = terminal_chrome::header(ui, center);
                back = header.back;
                chip = header.chip;
                egui::Popup::menu(&header.more).show(|ui| {
                    if ui.button("Browse in Files").clicked() {
                        self.documents.browse();
                        ui.close();
                    }
                    if let Some(view) = self.documents.active {
                        if compact && self.documents.show_actions(ui, view) {
                            ui.close();
                        }
                        if ui.button("New view of this document").clicked() {
                            self.documents.duplicate(view, &mut self.next_tab);
                            ui.close();
                        }
                        if ui.button("Refresh source").clicked() {
                            self.documents.refresh_active();
                            ui.close();
                        }
                        if ui.button("Close document view").clicked() {
                            let tab = self
                                .documents
                                .views
                                .iter()
                                .find(|entry| entry.id == view)
                                .map(|entry| entry.tab);
                            if let Some(tab) = tab {
                                self.documents.close_tab(tab, ui.ctx());
                            }
                            ui.close();
                        }
                    }
                });
            });
        if back {
            if self.documents.active.take().is_none() {
                self.surface = Surface::Tabs;
            }
            return;
        }
        match chip {
            terminal_chrome::ChipAction::Select(id) => {
                self.activate_tab(id);
                return;
            }
            terminal_chrome::ChipAction::Close(id) => self.request_surface_close(id),
            terminal_chrome::ChipAction::All => self.shell.sessions_sheet = true,
            terminal_chrome::ChipAction::New => {
                self.documents.active = None;
                return;
            }
            terminal_chrome::ChipAction::None => {}
        }
        egui::Frame::new()
            .inner_margin(egui::Margin::symmetric(
                theme::space::MD as i8,
                theme::space::SM as i8,
            ))
            .show(ui, |ui| {
                ui.add_enabled_ui(
                    self.lifecycle.get().active
                        && !self.shell.sessions_sheet
                        && !self.documents.modal_open(),
                    |ui| self.documents.show(ui, compact),
                );
            });
    }
}
