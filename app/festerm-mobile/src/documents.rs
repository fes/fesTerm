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
use festerm_ui_egui::icon::{self, Icon};

fn document_button(
    ui: &mut egui::Ui,
    label: &str,
    icon: Icon,
    enabled: bool,
    selected: bool,
) -> egui::Response {
    ui.add_enabled_ui(enabled, |ui| {
        let (rect, response) = ui.allocate_exact_size(egui::vec2(44.0, 44.0), egui::Sense::CLICK);
        let visuals = ui.style().interact(&response);
        ui.painter().rect_filled(
            rect,
            6,
            if selected {
                ui.visuals().selection.bg_fill
            } else {
                visuals.bg_fill
            },
        );
        icon::paint(
            ui.painter(),
            icon,
            rect.shrink(12.0),
            visuals.fg_stroke.color,
        );
        widgets::labelled(response, label, ui.is_enabled()).on_hover_text(label)
    })
    .inner
}

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
    previewing: bool,
    preview: Option<super::workspaces::MarkdownWorkspace>,
    preview_revision: Option<u64>,
    wide_preview: bool,
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
    save_close: Option<(ViewId, u64)>,
    close_requested: Option<ViewId>,
    compare: Option<EditTarget>,
    contents: Option<(ViewId, u64)>,
    preview: super::document_preview::PreviewLoader,
    focus_source: Option<ViewId>,
}

impl DocumentHost {
    pub fn source_field(&self) -> Option<egui::Id> {
        let view = self.active?;
        let entry = self.registry.view(view).ok()?;
        (!self.modal_open()
            && self
                .views
                .iter()
                .any(|entry| entry.id == view && !entry.previewing)
            && entry.available()
            && entry.writable())
        .then(|| body_id(view))
    }

    pub fn dictation_target(&self, ctx: &egui::Context) -> Result<EditTarget, DocumentError> {
        let field = self.source_field().ok_or(DocumentError::ReadOnly)?;
        let captured = ctx
            .data(|data| data.get_temp::<EditTarget>(field.with("rendered-target")))
            .ok_or(DocumentError::InvalidSelection)?;
        let current = self
            .registry
            .target(captured.view, captured.selection.clone())?;
        if current != captured {
            return Err(DocumentError::Stale);
        }
        Ok(current)
    }

    pub fn insert_dictation(
        &mut self,
        ctx: &egui::Context,
        captured: &EditTarget,
        text: &str,
    ) -> Result<(), DocumentError> {
        let current = self.dictation_target(ctx)?;
        let previous_len = self.registry.view(captured.view)?.text().text().len();
        self.registry.replace(captured, &current, text)?;
        let updated = self.registry.view(captured.view)?.text().text();
        let inserted_len = updated.len() - (previous_len - captured.selection.len());
        let caret = updated[..captured.selection.start + inserted_len]
            .chars()
            .count();
        let id = body_id(captured.view);
        let mut state = egui::TextEdit::load_state(ctx, id).unwrap_or_default();
        state
            .cursor
            .set_char_range(Some(egui::text::CCursorRange::one(
                egui::text::CCursor::new(caret),
            )));
        state.store(ctx, id);
        self.focus_source = Some(captured.view);
        Ok(())
    }

    pub fn modal_open(&self) -> bool {
        self.close.is_some()
            || self.close_requested.is_some()
            || self.compare.is_some()
            || self.contents.is_some()
    }

    pub fn bind_picker(&mut self, picker: Option<DocumentPicker>) {
        self.picker = picker;
    }

    pub fn contains(&self, tab: u64) -> bool {
        self.views.iter().any(|view| view.tab == tab)
    }

    pub fn activate(&mut self, tab: u64) -> bool {
        if let Some(view) = self.views.iter().find(|view| view.tab == tab) {
            if self.active != Some(view.id) {
                self.contents = None;
            }
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
            previewing: false,
            preview: None,
            preview_revision: None,
            wide_preview: false,
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
                            // Typing or a suspension raced a routine re-read;
                            // the queued read retries, so this is not a conflict.
                            return Ok(());
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
                if let Some((view, pending)) = self.save_close {
                    if pending == operation && self.registry.document_for(view) == Ok(id) {
                        self.save_close = None;
                        self.close_requested = Some(view);
                    }
                }
            }
            DocumentFileEvent::SaveFailed { operation, error } => {
                if self
                    .save_close
                    .is_some_and(|(_, pending)| pending == operation)
                {
                    self.save_close = None;
                }
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
                if self.save_close.is_some_and(|(owner, _)| owner == view) {
                    self.save_close = None;
                }
                widgets::forget_local_text_edit(ctx, body_id(view));
                ctx.data_mut(|data| {
                    data.remove::<EditTarget>(body_id(view).with("rendered-target"))
                });
                self.views.retain(|entry| entry.id != view);
                if let Some(id) = released {
                    self.sources.remove(&id);
                    self.refresh.remove(&id);
                    self.preview.forget(id);
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
                if self.contents.is_some_and(|(owner, _)| owner == view) {
                    self.contents = None;
                }
            }
            Err(error) => self.notice = Some(error.message()),
        }
    }

    pub fn duplicate(&mut self, view: ViewId, next_tab: &mut u64) {
        let result = (|| {
            let next = next_tab.checked_add(1).ok_or(DocumentError::Capacity)?;
            let id = self.registry.duplicate(view)?;
            self.views.push(View {
                tab: *next_tab,
                id,
                previewing: false,
                preview: None,
                preview_revision: None,
                wide_preview: false,
            });
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
        ui.horizontal(|ui| {
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
            if document_button(ui, "Save", Icon::Save, can_save, false).clicked() {
                self.save(view);
                acted = true;
            }
            let undo = writable
                && self
                    .registry
                    .view(view)
                    .is_ok_and(|entry| entry.text().can_undo());
            let redo = writable
                && self
                    .registry
                    .view(view)
                    .is_ok_and(|entry| entry.text().can_redo());
            if document_button(ui, "Undo", Icon::Undo, undo, false).clicked() {
                let result = self.registry.undo(view).map(|_| ());
                self.record(result);
                acted = true;
            }
            if document_button(ui, "Redo", Icon::Redo, redo, false).clicked() {
                let result = self.registry.redo(view).map(|_| ());
                self.record(result);
                acted = true;
            }
        });
        acted
    }

    pub fn show(&mut self, ui: &mut egui::Ui, composer: bool) {
        if let Some(notice) = &self.notice {
            ui.add(egui::Label::new(egui::RichText::new(notice).color(color::DANGER)).wrap());
        }
        let Some(view) = self.active else {
            widgets::screen_title(ui, "Documents", "Local text and Markdown");
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
        if !composer {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = if ui.available_width() < 300.0 {
                    0.0
                } else {
                    4.0
                };
                self.show_actions(ui, view);
                self.show_modes(ui, view);
            });
        }
        if let Some(entry) = self.registry.view(view).ok().filter(|_| !composer) {
            let status = if !entry.available() {
                Some("Access unavailable — reopen through Files")
            } else if entry.conflict().is_some() {
                Some("Changed outside fesTerm — edits preserved")
            } else if !entry.writable() {
                Some("Read-only")
            } else if entry.saving() {
                Some("Saving…")
            } else if entry.dirty() {
                Some("Unsaved changes")
            } else {
                None
            };
            if let Some(status) = status {
                ui.small(status);
            }
            if entry.conflict().is_some()
                && widgets::button(ui, ButtonKind::Secondary, "Compare / resolve", true).clicked()
            {
                match self.registry.target(view, 0..0) {
                    Ok(target) => self.compare = Some(target),
                    Err(error) => self.notice = Some(error.message()),
                }
            }
        }
        if self
            .views
            .iter()
            .any(|entry| entry.id == view && entry.previewing)
        {
            self.show_preview(ui, view);
        } else {
            self.show_source(ui, view);
        }
    }

    fn show_modes(&mut self, ui: &mut egui::Ui, view: ViewId) -> bool {
        let markdown = self.registry.view(view).is_ok_and(|entry| {
            entry
                .origin()
                .file_name()
                .rsplit_once('.')
                .is_some_and(|(_, extension)| {
                    ["md", "markdown", "mdown", "mkd"]
                        .iter()
                        .any(|candidate| extension.eq_ignore_ascii_case(candidate))
                })
        });
        if !markdown {
            return false;
        }
        let Some(index) = self.views.iter().position(|entry| entry.id == view) else {
            return false;
        };
        let enabled = !self.modal_open();
        let current = self
            .registry
            .view(view)
            .ok()
            .map(|entry| entry.text().revision());
        let contents_ready = self.views[index].preview_revision.is_some()
            && self.views[index].preview_revision == current;
        let mut acted = false;
        ui.horizontal(|ui| {
            for (label, previewing) in [("Edit", false), ("Preview", true)] {
                if document_button(
                    ui,
                    label,
                    if previewing {
                        Icon::RenderedView
                    } else {
                        Icon::SourceView
                    },
                    enabled,
                    self.views[index].previewing == previewing,
                )
                .clicked()
                {
                    self.views[index].previewing = previewing;
                    widgets::blur_local_text_edit(ui.ctx(), body_id(view));
                    self.focus_source = (!previewing).then_some(view);
                    acted = true;
                }
            }
            if self.views[index].previewing
                && !self.views[index].wide_preview
                && ui.available_width() >= 44.0
                && document_button(
                    ui,
                    "Contents",
                    Icon::Outline,
                    enabled && contents_ready,
                    false,
                )
                .clicked()
            {
                self.contents = current.map(|revision| (view, revision));
                acted = true;
            }
        });
        acted
    }

    fn show_preview(&mut self, ui: &mut egui::Ui, view: ViewId) {
        let result = self
            .registry
            .document_for(view)
            .map_err(|error| error.message())
            .and_then(|id| {
                let entry = self.registry.get(id).map_err(|error| error.message())?;
                self.preview.document(id, entry, ui.ctx())
            });
        match result {
            Ok(Some(document)) => {
                let id = self
                    .registry
                    .document_for(view)
                    .expect("live preview document");
                let revision = self
                    .registry
                    .view(view)
                    .expect("live preview view")
                    .text()
                    .revision();
                for entry in &mut self.views {
                    if self.registry.document_for(entry.id).ok() == Some(id) {
                        if let Some(preview) = &mut entry.preview {
                            preview.update_shared(document.clone());
                            entry.preview_revision = Some(revision);
                        }
                    }
                }
                let Some(entry) = self.views.iter_mut().find(|entry| entry.id == view) else {
                    return;
                };
                if let Some(preview) = &mut entry.preview {
                    preview.update_shared(document);
                } else {
                    let name = self
                        .registry
                        .view(view)
                        .expect("live preview view")
                        .origin()
                        .file_name();
                    entry.preview = Some(super::workspaces::MarkdownWorkspace::from_shared(
                        name,
                        "Local document",
                        document,
                    ));
                }
                entry.preview_revision = Some(revision);
                let wide = super::workspaces::LayoutTier::for_size(ui.available_size())
                    == super::workspaces::LayoutTier::Wide;
                if entry.wide_preview != wide {
                    entry.wide_preview = wide;
                    ui.ctx().request_repaint();
                }
                ui.push_id(("document-preview", view.raw()), |ui| {
                    entry
                        .preview
                        .as_mut()
                        .expect("prepared preview")
                        .show_projection(ui);
                });
            }
            Ok(None) => {
                ui.spinner();
                ui.label("Updating Markdown preview…");
            }
            Err(error) => {
                ui.colored_label(color::DANGER, error);
                widgets::footnote(ui, "Your source is unchanged. Return to Edit to continue.");
                if widgets::button(ui, ButtonKind::Secondary, "Retry preview", true).clicked() {
                    if let Ok(id) = self.registry.document_for(view) {
                        self.preview.forget(id);
                        ui.ctx().request_repaint();
                    }
                }
            }
        }
    }

    fn record(&mut self, result: Result<(), DocumentError>) {
        if let Err(error) = result {
            self.notice = Some(error.message());
        }
    }

    fn show_source(&mut self, ui: &mut egui::Ui, view: ViewId) {
        let id = body_id(view);
        let focus = ui.is_enabled() && self.focus_source.take() == Some(view);
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
        let inactive_cursor = (!ui.is_enabled())
            .then(|| state.cursor.char_range())
            .flatten();
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
        if focus && writable {
            output.response.request_focus();
            ui.ctx().request_repaint();
        }
        if output.response.changed() {
            let result = self.registry.sync(&target, &text).map(|_| ());
            self.record(result);
        }
        // Never retain an independent widget history across document actions.
        let mut state = output.state;
        if let Some(cursor) = inactive_cursor {
            state.cursor.set_char_range(Some(cursor));
        }
        state.set_undoer(egui::util::undoer::Undoer::with_settings(
            egui::util::undoer::Settings {
                max_undos: 1,
                ..Default::default()
            },
        ));
        if let Some(range) = state.cursor.char_range() {
            let range = range.as_sorted_char_range();
            if let Ok(entry) = self.registry.view(view) {
                let canonical = entry.text().text();
                if range.end.0 <= canonical.chars().count() {
                    let start = canonical
                        .chars()
                        .take(range.start.0)
                        .map(char::len_utf8)
                        .sum();
                    let end = canonical
                        .chars()
                        .take(range.end.0)
                        .map(char::len_utf8)
                        .sum();
                    if let Ok(target) = self.registry.target(view, start..end) {
                        ui.ctx()
                            .data_mut(|data| data.insert_temp(id.with("rendered-target"), target));
                    }
                }
            }
        }
        state.store(ui.ctx(), id);
        if output.response.lost_focus() {
            let result = self.registry.end_typing(view);
            self.record(result);
        }
    }

    pub fn show_sheets(
        &mut self,
        ctx: &egui::Context,
        bounds: egui::Rect,
        documents_visible: bool,
    ) {
        if !documents_visible {
            self.contents = None;
        }
        if let Some(view) = self.close_requested.take() {
            self.close_view(view, false, ctx);
        }
        let Some(view) = self.close else {
            self.show_compare(ctx, bounds);
            self.show_contents(ctx, bounds);
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
        let mut save = false;
        let saving = self.registry.view(view).is_ok_and(|entry| entry.saving());
        let sheet = widgets::bottom_sheet(ctx, "document-dirty-close", bounds, |ui| {
            cancel |= widgets::sheet_title(ui, "Unsaved document", None);
            widgets::footnote(ui, &label);
            ui.label("Save changes before closing this view?");
            if let Some(notice) = &self.notice {
                ui.add(egui::Label::new(egui::RichText::new(notice).color(color::DANGER)).wrap());
            }
            save = widgets::button(
                ui,
                ButtonKind::Primary,
                if saving { "Saving..." } else { "Save" },
                !saving,
            )
            .clicked();
            discard =
                widgets::button(ui, ButtonKind::Destructive, "Discard changes", !saving).clicked();
            cancel |= widgets::button(ui, ButtonKind::Secondary, "Cancel", true).clicked();
        });
        if save {
            self.save(view);
            if self.registry.view(view).is_ok_and(|entry| entry.saving()) {
                self.save_close = Some((view, self.operation));
            }
        } else if discard {
            self.close_view(view, true, ctx);
        } else if cancel || sheet.dismissed {
            self.close = None;
            self.save_close = None;
        }
    }
}

impl DocumentHost {
    fn show_contents(&mut self, ctx: &egui::Context, bounds: egui::Rect) {
        let Some((view, revision)) = self.contents else {
            return;
        };
        if self.active != Some(view) {
            self.contents = None;
            ctx.request_repaint();
            return;
        }
        if self
            .views
            .iter()
            .any(|entry| entry.id == view && entry.wide_preview)
        {
            self.contents = None;
            ctx.request_repaint();
            return;
        }
        let current = self
            .registry
            .view(view)
            .ok()
            .map(|entry| entry.text().revision());
        if current != Some(revision)
            || self
                .views
                .iter()
                .find(|entry| entry.id == view)
                .is_none_or(|entry| !entry.previewing || entry.preview_revision != Some(revision))
        {
            self.contents = None;
            self.notice =
                Some("Preview changed. Reopen Contents after it finishes updating.".to_owned());
            return;
        }
        let Some(preview) = self
            .views
            .iter_mut()
            .find(|entry| entry.id == view)
            .and_then(|entry| entry.preview.as_mut())
        else {
            self.contents = None;
            self.notice =
                Some("Preview is still updating. Open Contents after it finishes.".to_owned());
            return;
        };
        let mut close = false;
        let sheet = widgets::bottom_sheet(ctx, "document-contents", bounds, |ui| {
            close |= widgets::sheet_title(ui, "Contents", None);
            ui.push_id(("document-contents", view.raw()), |ui| {
                close |= preview.show_contents(ui, false);
            });
        });
        if close || sheet.dismissed {
            self.contents = None;
        }
    }

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
        let mut header_close = false;
        let sheet = widgets::bottom_sheet_with_footer(
            ctx,
            "document-compare",
            bounds,
            |ui| {
                header_close |= widgets::sheet_title(ui, "File changed outside fesTerm", None);
                widgets::footnote(ui, "External version (read-only)");
                egui::ScrollArea::vertical()
                    .max_height(bounds.height() * 0.3)
                    .show(ui, |ui| {
                        ui.add(egui::Label::new(egui::RichText::new(&outside).monospace()).wrap());
                    });
                widgets::footnote(ui, "Keep edits preserves your buffer. A later explicit Save replaces this external version. Reload discards your edits and history.");
            },
            |ui| {
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
            },
        );
        if let Some(keep) = decision {
            let result = self.resolve(&target, keep);
            self.record(result);
            self.compare = None;
        } else if cancel || header_close || sheet.dismissed {
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
        let bounds = ui.max_rect();
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
                egui::Popup::menu(&header.more).width(280.0).show(|ui| {
                    egui::ScrollArea::vertical()
                        .max_height((bounds.height() - 88.0).max(0.0))
                        .show(ui, |ui| {
                            if ui.button("Browse in Files").clicked() {
                                self.documents.browse();
                                ui.close();
                            }
                            if let Some(view) = self.documents.active {
                                let revision = self
                                    .documents
                                    .registry
                                    .view(view)
                                    .ok()
                                    .map(|entry| entry.text().revision());
                                let ready = self.documents.views.iter().any(|entry| {
                                    entry.id == view
                                        && entry.previewing
                                        && entry.preview_revision == revision
                                });
                                if ui
                                    .add_enabled(ready, egui::Button::new("Contents"))
                                    .clicked()
                                {
                                    self.documents.contents =
                                        revision.map(|revision| (view, revision));
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
            });
        if back {
            self.shell.tab = super::shell::Tab::Sessions;
            self.surface = Surface::Tabs;
            return;
        }
        match chip {
            terminal_chrome::ChipAction::Select(id) => {
                self.activate_tab(id);
                return;
            }
            terminal_chrome::ChipAction::Menu(id) => self.request_tab_menu(id),
            terminal_chrome::ChipAction::All => self.shell.sessions_sheet = true,
            terminal_chrome::ChipAction::New => {
                self.shell.tab = super::shell::Tab::Sessions;
                self.surface = Surface::Tabs;
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
                        && self.tab_menu.is_none()
                        && self.confirm_close.is_none()
                        && (!self.dictation_owns_frame || self.dictation_source_handoff)
                        && !self.documents.modal_open(),
                    |ui| self.documents.show(ui, self.dictation_owns_frame),
                );
            });
    }
}
