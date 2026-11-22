//! Application policy for durable, report-only GUI SFTP upload recovery.
use eframe::egui;
#[path = "sftp_recovery/store.rs"]
mod store;
use festerm_ssh::{
    UploadRecoveryError as Error, UploadRecoveryFuture, UploadRecoveryRecord, UploadRecoverySink,
};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};
pub(crate) use store::{Endpoint, ListedReport, Report};
use store::{Root, Token};

#[derive(Clone)]
pub(crate) struct Ledger {
    path: Option<PathBuf>,
}
impl Ledger {
    pub(crate) fn production() -> Self {
        #[cfg(not(test))]
        let path = directories::ProjectDirs::from("com", "fes", "fesTerm")
            .map(|p| p.data_local_dir().join("sftp-upload-recovery-v1"));
        #[cfg(test)]
        let path = None; // Tests may only inject an owned private root.
        Self { path }
    }
    fn begin(&self, endpoint: Endpoint, temporary: String) -> Result<Token, Error> {
        let path = self.path.as_ref().ok_or(Error::Unavailable)?;
        Arc::new(Root::open(path, true)?).begin(endpoint, temporary)
    }
    fn list(&self) -> Result<Vec<ListedReport>, Error> {
        let Some(path) = &self.path else {
            return Ok(Vec::new());
        };
        match std::fs::symlink_metadata(path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if store::root_is_missing(path)? {
                    return Ok(Vec::new());
                }
            }
            Err(_) => return Err(Error::Unavailable),
            Ok(_) => {} // Existing aliases must reach the no-follow root check.
        }
        Root::open(path, false)?.list()
    }
    fn acknowledge(&self, expected: &Report) -> Result<(), Error> {
        Root::open(self.path.as_ref().ok_or(Error::Unavailable)?, false)?.acknowledge(expected)
    }
}

pub(crate) type Reporter = Arc<dyn Fn(&str, Option<&str>) + Send + Sync>;
pub(crate) type ScopeCheck = Arc<dyn Fn() -> bool + Send + Sync>;
pub(crate) type ScopeFactory = Arc<dyn Fn() -> ScopeCheck + Send + Sync>;
struct Sink {
    ledger: Ledger,
    endpoint: Endpoint,
    closed: Arc<AtomicBool>,
    report: Reporter,
    scope: ScopeFactory,
}
struct Record {
    token: Option<Token>,
    current: ScopeCheck,
}
pub(crate) fn sink(
    endpoint: Endpoint,
    closed: Arc<AtomicBool>,
    report: Reporter,
    scope: ScopeFactory,
) -> Arc<dyn UploadRecoverySink> {
    Arc::new(Sink {
        ledger: Ledger::production(),
        endpoint,
        closed,
        report,
        scope,
    })
}
impl UploadRecoverySink for Sink {
    fn cancelled(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }
    fn begin<'a>(
        &'a self,
        temporary: &'a str,
    ) -> UploadRecoveryFuture<'a, Box<dyn UploadRecoveryRecord>> {
        let current = (self.scope)();
        let (ledger, endpoint, temporary) = (
            self.ledger.clone(),
            self.endpoint.clone(),
            temporary.to_owned(),
        );
        Box::pin(async move {
            let token = tokio::task::spawn_blocking(move || ledger.begin(endpoint, temporary))
                .await
                .map_err(|_| Error::Unavailable)??;
            Ok(Box::new(Record {
                token: Some(token),
                current,
            }) as Box<dyn UploadRecoveryRecord>)
        })
    }
    fn report_update_failure(&self, temporary: &str, destination: Option<&str>) {
        (self.report)(temporary, destination);
    }
}
impl UploadRecoveryRecord for Record {
    fn cancelled(&self) -> bool {
        !(self.current)()
    }
    fn before_publication<'a>(&'a mut self, destination: &'a str) -> UploadRecoveryFuture<'a, ()> {
        let token = self.token.take();
        let destination = destination.to_owned();
        Box::pin(async move {
            let mut token = token.ok_or(Error::Changed)?;
            let (returned, result) = tokio::task::spawn_blocking(move || {
                let result = token.checkpoint(destination);
                (token, result)
            })
            .await
            .map_err(|_| Error::Unavailable)?;
            self.token = Some(returned);
            result
        })
    }
    fn retire(&mut self) -> UploadRecoveryFuture<'_, ()> {
        let token = self.token.take();
        Box::pin(async move {
            let token = token.ok_or(Error::Changed)?;
            let (returned, result) = tokio::task::spawn_blocking(move || {
                let result = token.retire();
                (token, result)
            })
            .await
            .map_err(|_| Error::Unavailable)?;
            self.token = Some(returned);
            result
        })
    }
}

#[derive(Clone, Debug)]
pub(crate) enum Action {
    Review,
    Copy(String),
    Acknowledge(String),
}
#[derive(Default)]
pub(crate) struct RecoveryView {
    open: bool,
    started: bool,
    auto_open: bool,
    rows: Vec<ListedReport>,
    error: Option<Error>,
    pending: Option<std::sync::mpsc::Receiver<Result<Vec<ListedReport>, Error>>>,
    confirmation: Option<String>,
    ledger: Option<Ledger>,
}
impl RecoveryView {
    pub(crate) fn is_open(&self) -> bool {
        self.open
    }
    fn ledger(&self) -> Ledger {
        self.ledger.clone().unwrap_or_else(Ledger::production)
    }
    fn load(&mut self, ack: Option<Report>, context: &egui::Context) {
        if self.pending.is_some() {
            self.error = Some(Error::Busy);
            return;
        }
        let ledger = self.ledger();
        let (send, receive) = std::sync::mpsc::sync_channel(1);
        let wake = context.clone();
        let spawned = std::thread::Builder::new()
            .name("sftp-recovery-report".into())
            .spawn(move || {
                let result = (|| {
                    if let Some(expected) = ack {
                        ledger.acknowledge(&expected)?;
                    }
                    ledger.list()
                })();
                let _ = send.send(result);
                wake.request_repaint();
            });
        if spawned.is_err() {
            self.error = Some(Error::Unavailable);
        } else {
            self.pending = Some(receive);
            self.error = None;
        }
    }
    fn poll(&mut self, context: &egui::Context) {
        if !self.started {
            self.started = true;
            if self.ledger().path.is_some() {
                self.auto_open = true;
                self.load(None, context);
            }
        }
        let received = self.pending.as_ref().and_then(|r| match r.try_recv() {
            Ok(v) => Some(v),
            Err(std::sync::mpsc::TryRecvError::Disconnected) => Some(Err(Error::Unavailable)),
            Err(_) => None,
        });
        if let Some(result) = received {
            self.pending = None;
            match result {
                Ok(rows) => {
                    self.rows = rows;
                    self.error = None;
                }
                Err(e) => self.error = Some(e),
            }
            if self.auto_open && (!self.rows.is_empty() || self.error.is_some()) {
                self.open = true;
            }
            self.auto_open = false;
        }
        if self.pending.is_some() {
            context.request_repaint_after(std::time::Duration::from_millis(50));
        }
    }
    fn act(&mut self, action: Action, context: &egui::Context) {
        match action {
            Action::Review => {
                self.open = true;
                self.confirmation = None;
                self.load(None, context);
            }
            Action::Copy(id) => {
                if let Some(row) = self.rows.iter().find(|row| row.report.id == id) {
                    context.copy_text(report_text(&row.report));
                }
            }
            Action::Acknowledge(id) => {
                let Some(row) = self.rows.iter().find(|row| row.report.id == id) else {
                    self.error = Some(Error::Changed);
                    return;
                };
                if row.active {
                    self.error = Some(Error::Busy);
                    return;
                }
                self.confirmation = None;
                self.load(Some(row.report.clone()), context);
            }
        }
    }
}
fn report_text(r: &Report) -> String {
    format!("Endpoint: {}@{}:{}\nVerified SSH key: {}\nTemporary (may exist): {}\nDestination: {}\nNo remote inspection, deletion or resume has run.",r.endpoint.username,r.endpoint.host,r.endpoint.port,r.endpoint.fingerprint,r.temporary,r.destination.as_deref().unwrap_or("not recorded before publication"))
}
impl super::FesTermApp {
    pub(super) fn show_upload_recovery(&mut self, context: &egui::Context) {
        let view = &mut self.overlays.upload_recovery;
        view.poll(context);
        if let Some(action) = self.state.take_upload_recovery_action() {
            view.act(action, context);
        }
        if !view.open {
            return;
        }
        // Other foreground confirmations keep ownership. Recovery remains queued.
        if self.overlays.blocks_terminal_input_except_upload_recovery() {
            return;
        }
        let view = &mut self.overlays.upload_recovery;
        let mut action = None;
        let mut close = false;
        crate::software_background::show_modal(
            context,
            egui::Modal::new(egui::Id::new("interrupted_sftp_uploads")),
            |ui| {
                ui.set_width((context.content_rect().width() - 40.0).clamp(160.0, 620.0));
                ui.heading("Interrupted uploads");
                ui.label("These saved reports identify files that may exist. No remote inspection or action has run.");
                if let Some(error) = view.error {
                    ui.colored_label(festerm_ui_egui::theme::STATUS_ERROR, error.message());
                }
                if view.pending.is_some() {
                    ui.label("Reading local reports…");
                } else if view.rows.is_empty() {
                    ui.label("No interrupted-upload reports are saved.");
                }
                egui::ScrollArea::vertical()
                    .max_height((context.content_rect().height() - 240.0).max(80.0))
                    .show(ui, |ui| {
                        for row in &view.rows {
                            ui.group(|ui| {
                                ui.label(&row.report.endpoint.label);
                                ui.label(format!(
                                    "{}@{}:{}",
                                    row.report.endpoint.username,
                                    row.report.endpoint.host,
                                    row.report.endpoint.port
                                ));
                                ui.label(format!("Verified SSH key: {}", row.report.endpoint.fingerprint));
                                ui.label(egui::RichText::new(&row.report.temporary).monospace());
                                if let Some(path) = &row.report.destination {
                                    ui.label(format!("Publication may have involved: {path}"));
                                }
                                if row.active {
                                    ui.label("An upload owner is still active; this report cannot be acknowledged yet.");
                                }
                                ui.horizontal_wrapped(|ui| {
                                    if ui.button("Copy report").clicked() {
                                        action = Some(crate::tabs::AppCommand::CopyInterruptedUpload {
                                            id: row.report.id.clone(),
                                        });
                                    }
                                    if ui.add_enabled(
                                        !row.active && view.pending.is_none(),
                                        egui::Button::new("Acknowledge report…"),
                                    ).clicked() {
                                        view.confirmation = Some(row.report.id.clone());
                                    }
                                });
                            });
                        }
                    });
                if let Some(id) = view.confirmation.clone() {
                    ui.separator();
                    ui.label("Acknowledging removes only this local report. Inspect or copy the paths first. No remote file will be deleted or resumed.");
                    ui.horizontal_wrapped(|ui| {
                        if ui.button("Acknowledge and forget report").clicked() {
                            action =
                                Some(crate::tabs::AppCommand::AcknowledgeInterruptedUpload { id });
                        }
                        if ui.button("Cancel acknowledgement").clicked() {
                            view.confirmation = None;
                        }
                    });
                }
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .add_enabled(view.pending.is_none(), egui::Button::new("Refresh reports"))
                        .clicked()
                    {
                        action = Some(crate::tabs::AppCommand::ReviewInterruptedUploads);
                    }
                    if ui.button("Close").clicked() {
                        close = true;
                    }
                });
            },
        );
        if close {
            view.open = false;
            view.auto_open = false;
            view.confirmation = None;
        }
        if let Some(command) = action {
            self.state.dispatch(command, context);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::{kittest::Queryable, Harness};
    fn endpoint() -> Endpoint {
        Endpoint {
            host: "owned.test".into(),
            port: 22,
            username: "owner".into(),
            fingerprint: "SHA256:verified-owned-fixture".into(),
            profile: Some("Owned profile".into()),
            label: "Owned upload".into(),
        }
    }
    fn ledger() -> (
        festerm_sessiond::private_storage::PrivateTestDirectory,
        Ledger,
    ) {
        let p = festerm_sessiond::private_storage::private_test_directory();
        let l = Ledger {
            path: Some(p.path().join("reports")),
        };
        (p, l)
    }
    #[test]
    #[cfg(unix)]
    fn upload_recovery_dangling_root_is_unavailable_not_a_false_empty_list() {
        let (private, mut ledger) = ledger();
        let alias = private.path().join("alias");
        std::os::unix::fs::symlink(private.path().join("missing"), &alias).unwrap();
        ledger.path = Some(alias);
        assert_eq!(ledger.list().err(), Some(Error::Unavailable));
        assert!(private.path().join("missing").symlink_metadata().is_err());
    }
    #[test]
    fn upload_recovery_missing_parents_are_observed_without_creation_or_alias_following() {
        let private = festerm_sessiond::private_storage::private_test_directory();
        let missing = private.path().join("missing");
        let ledger = Ledger {
            path: Some(missing.join("application").join("reports")),
        };
        assert!(ledger.list().unwrap().is_empty());
        assert!(
            !missing.exists(),
            "observing absent reports must not create application parents"
        );
        #[cfg(unix)]
        {
            let alias = private.path().join("alias");
            std::os::unix::fs::symlink(&missing, &alias).unwrap();
            let ledger = Ledger {
                path: Some(alias.join("reports")),
            };
            assert_eq!(ledger.list().err(), Some(Error::Unavailable));
            assert!(!missing.exists());
        }
    }

    #[test]
    fn upload_recovery_private_errors_never_render_unknown_record_text() {
        let (_private, ledger) = ledger();
        let token = ledger
            .begin(endpoint(), "/owned/file.festerm-part".into())
            .unwrap();
        drop(token);
        let path = ledger.path.as_ref().unwrap();
        let directory = festerm_sessiond::private_storage::private_directory(path, false).unwrap();
        festerm_sessiond::private_storage::replace_private(
            &directory,
            std::path::Path::new("reports.json"),
            b"PRIVATE_PASSWORD_AND_CREDENTIAL_REFERENCE",
        )
        .unwrap();
        let error = ledger.list().err().unwrap();
        assert_eq!(error, Error::Changed);
        assert!(!error.message().contains("PRIVATE"));
    }
    #[test]
    fn upload_recovery_record_scope_retirement_releases_actual_lock_without_erasing_report() {
        let (_private, ledger) = ledger();
        let scope = Arc::new(AtomicBool::new(true));
        let observed = scope.clone();
        let scoped: ScopeFactory = Arc::new(move || {
            let current = observed.clone();
            Arc::new(move || current.load(Ordering::Acquire))
        });
        let owner = Sink {
            ledger: ledger.clone(),
            endpoint: endpoint(),
            closed: Arc::new(AtomicBool::new(false)),
            report: Arc::new(|_, _| panic!("no notice expected")),
            scope: scoped,
        };
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let mut report = owner.begin("/owned/file.festerm-part").await.unwrap();
                assert!(ledger.list().unwrap()[0].active);
                report.before_publication("/owned/file").await.unwrap();
                scope.store(false, Ordering::Release);
                assert!(report.cancelled());
                drop(report);
                let rows = ledger.list().unwrap();
                assert_eq!(rows.len(), 1);
                assert!(!rows[0].active);
                assert_eq!(rows[0].report.destination.as_deref(), Some("/owned/file"));
            });
    }

    fn wait_view(harness: &mut Harness<super::super::FesTermApp>) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            harness.run_steps(1);
            if harness.state().overlays.upload_recovery.pending.is_none() {
                break;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }
    #[test]
    fn upload_recovery_startup_close_review_and_acknowledge_are_report_only() {
        let (_private, ledger) = ledger();
        let token = ledger
            .begin(endpoint(), "/owned/file.festerm-part".into())
            .unwrap();
        drop(token);
        let mut app = super::super::FesTermApp::for_test_with_configuration(
            festerm_config::Configuration::empty(),
        );
        app.overlays.upload_recovery.ledger = Some(ledger.clone());
        let mut harness = Harness::builder()
            .with_size(egui::vec2(620.0, 700.0))
            .build_ui_state(
                |ui, app: &mut super::super::FesTermApp| app.show_upload_recovery(ui.ctx()),
                app,
            );
        wait_view(&mut harness);
        harness.get_by_label("Interrupted uploads");
        harness.get_by_label("/owned/file.festerm-part");
        harness.run_steps(3);
        harness.get_by_label("Close").click();
        harness.run_steps(2);
        assert_eq!(ledger.list().unwrap().len(), 1);
        assert!(!harness.state().overlays.upload_recovery.is_open());
        let context = harness.ctx.clone();
        harness
            .state_mut()
            .state
            .dispatch(crate::tabs::AppCommand::ReviewInterruptedUploads, &context);
        wait_view(&mut harness);
        harness.run_steps(3);
        harness.get_by_label("Acknowledge report…").click();
        harness.run_steps(1);
        assert_eq!(
            ledger.list().unwrap().len(),
            1,
            "confirmation is not acknowledgement"
        );
        harness.run_steps(3);
        harness
            .get_by_label("Acknowledge and forget report")
            .click();
        harness.run_steps(1);
        wait_view(&mut harness);
        assert!(ledger.list().unwrap().is_empty());
        harness.get_by_label("No interrupted-upload reports are saved.");
        assert_eq!(
            harness.state().state.tabs().len(),
            1,
            "no transport/session was opened"
        );
    }
    #[test]
    fn upload_recovery_central_action_refuses_active_owner_and_keeps_endpoint_paths() {
        let (_private, ledger) = ledger();
        let token = ledger
            .begin(endpoint(), "/owned/active.festerm-part".into())
            .unwrap();
        let context = egui::Context::default();
        let mut view = RecoveryView {
            ledger: Some(ledger.clone()),
            started: true,
            ..Default::default()
        };
        view.rows = ledger.list().unwrap();
        view.act(Action::Acknowledge(token.report.id.clone()), &context);
        assert_eq!(view.error, Some(Error::Busy));
        assert_eq!(ledger.list().unwrap().len(), 1);
        let copied = report_text(&token.report);
        assert!(copied.contains("owner@owned.test:22"));
        assert!(copied.contains("SHA256:verified-owned-fixture"));
        assert!(copied.contains("/owned/active.festerm-part"));
        assert!(!copied.contains("credential_reference"));
        drop(token);
    }
}
