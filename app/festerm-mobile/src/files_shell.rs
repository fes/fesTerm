//! Glue between the shell (tabs, profiles, sheets) and the Files screen.

#[cfg(feature = "live-ssh")]
use super::files::FilesSession;
use super::files::{FilesEvent, Link};
use super::profiles::SftpSpec;
use super::*;

/// What the Files screen was opened for, so "Try again" can reconnect.
#[derive(Default)]
pub(super) struct FilesContext {
    spec: Option<SftpSpec>,
    /// Opened from a saved SFTP profile rather than from a terminal tab.
    pub(super) only: bool,
}

impl MobileApp {
    /// Files for the active terminal's host.
    pub(super) fn open_files_for_active(&mut self, ctx: &egui::Context) {
        let Some(spec) = self.active_spec().map(SftpSpec::from_session) else {
            return;
        };
        self.open_files(spec, false, ctx);
    }

    pub(super) fn open_files(&mut self, spec: SftpSpec, only: bool, ctx: &egui::Context) {
        let same = self.files.as_ref().is_some_and(|session| {
            !matches!(session.link, Link::Failed(_))
                && self
                    .files_ctx
                    .spec
                    .as_ref()
                    .is_some_and(|open| same_target(open, &spec))
        });
        if !same {
            self.files = None;
            if let Err(error) = self.start_files(&spec, ctx) {
                self.notice = Some(error);
                return;
            }
        }
        self.files_ctx.only = only;
        if only {
            self.park_active();
            self.surface = Surface::Terminal;
        }
        self.switch_workspace(workspaces::Workspace::Files);
    }

    #[cfg(feature = "live-ssh")]
    fn start_files(&mut self, spec: &SftpSpec, ctx: &egui::Context) -> Result<(), String> {
        let profiles = self
            .profiles
            .as_mut()
            .ok_or_else(|| "Saved profiles are unavailable".to_owned())?;
        let identity = festerm_ssh::HostIdentity::new(&spec.host, spec.port)
            .map_err(|_| "Invalid SSH host or port".to_owned())?;
        let local_root =
            sftp_link::documents_dir().ok_or_else(|| "App storage is unavailable".to_owned())?;
        let request = sftp_link::SftpRequest {
            host: spec.host.clone(),
            port: spec.port,
            username: spec.username.clone(),
            authentication: profiles.authentication(&spec.auth),
            pin: profiles.pinned_fingerprint(&identity),
            start_dir: spec.start_dir.clone(),
            local_root,
        };
        if let Some(id) = spec.profile.as_ref() {
            profiles.mark_used(id.as_str());
        }
        let connector = sftp_link::SftpConnector::start(request, ctx);
        self.files = Some(Box::new(FilesSession::connecting(
            spec.name.clone(),
            format!("SFTP · {}@{}", spec.username, spec.host),
            Box::new(connector),
        )));
        self.files_ctx.spec = Some(spec.clone());
        Ok(())
    }

    #[cfg(not(feature = "live-ssh"))]
    fn start_files(&mut self, _spec: &SftpSpec, _ctx: &egui::Context) -> Result<(), String> {
        Err("Files are not available in this build".to_owned())
    }

    pub(super) fn close_files(&mut self) {
        self.files = None;
        self.files_ctx = FilesContext::default();
        self.markdown = None;
    }

    pub(super) fn retry_files(&mut self, ctx: &egui::Context) {
        let Some(spec) = self.files_ctx.spec.clone() else {
            return;
        };
        self.files = None;
        if let Err(error) = self.start_files(&spec, ctx) {
            self.notice = Some(error);
        }
    }

    /// Pumps the Files session once per frame and applies what it reports.
    pub(super) fn poll_files(&mut self, ctx: &egui::Context) {
        let events = match self.files.as_mut() {
            Some(session) => session.poll(ctx),
            None => return,
        };
        self.apply_files_events(events);
    }

    pub(super) fn apply_files_events(&mut self, events: Vec<FilesEvent>) {
        for event in events {
            match event {
                FilesEvent::Trust(identity, fingerprint) => {
                    if let Some(profiles) = self.profiles.as_mut() {
                        if let Err(error) = profiles.trust_host(&identity, &fingerprint) {
                            self.notice = Some(format!("Could not save the host key: {error}"));
                        }
                    }
                }
                FilesEvent::OpenMarkdown {
                    name,
                    location,
                    text,
                } => match workspaces::MarkdownWorkspace::from_text(&name, &location, &text) {
                    Ok(markdown) => {
                        self.markdown = Some(markdown);
                        self.switch_workspace(workspaces::Workspace::Markdown);
                    }
                    Err(error) => {
                        if let Some(session) = self.files.as_mut() {
                            session.notice = Some(error);
                        }
                    }
                },
            }
        }
    }

    pub(super) fn show_files_body(&mut self, ui: &mut egui::Ui) {
        let Some(session) = self.files.as_mut() else {
            ui.add_space(theme::space::XL);
            ui.vertical_centered(|ui| ui.weak("No file session is open"));
            return;
        };
        let outcome = files_ui::show(ui, session);
        let ctx = ui.ctx().clone();
        if outcome.retry {
            self.retry_files(&ctx);
        }
        self.apply_files_events(outcome.events);
    }

    pub(super) fn files_heading(&self) -> (String, String) {
        match (self.workspace, &self.files, &self.markdown) {
            (workspaces::Workspace::Markdown, _, Some(markdown)) => {
                (markdown.name().to_owned(), "Markdown preview".to_owned())
            }
            (_, Some(session), _) => (session.title.clone(), session.subtitle.clone()),
            _ => ("Files".to_owned(), String::new()),
        }
    }

    /// Link and transfer sheets for the Files screen.
    pub(super) fn show_files_sheets(&mut self, ctx: &egui::Context, bounds: egui::Rect) {
        if self.workspace != workspaces::Workspace::Files {
            return;
        }
        let Some(session) = self.files.as_mut() else {
            return;
        };
        match session.link.clone() {
            Link::HostKey(model) => {
                if let Some(decision) = forms::host_verification_sheet(ctx, bounds, &model) {
                    session.resolve_host(decision);
                }
            }
            Link::Password { target, retry } => {
                match forms::password_sheet(ctx, bounds, &target, retry, &mut session.password) {
                    forms::PasswordDecision::Submit => session.submit_password(),
                    forms::PasswordDecision::Cancel => session.cancel_connect(),
                    forms::PasswordDecision::None => {}
                }
            }
            Link::Ready => {
                if session.conflict().is_some() {
                    files_ui::conflict_sheet(ctx, bounds, session);
                } else if session.transfers_sheet {
                    files_ui::transfers_sheet(ctx, bounds, session);
                }
            }
            Link::Connecting(_) | Link::Failed(_) => {}
        }
    }
}

fn same_target(left: &SftpSpec, right: &SftpSpec) -> bool {
    left.host == right.host
        && left.port == right.port
        && left.username == right.username
        && left.start_dir == right.start_dir
}
