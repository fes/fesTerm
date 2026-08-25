use std::{
    sync::{
        mpsc::{self, Receiver, TryRecvError},
        Arc,
    },
    thread,
};

use cargo_packager_updater::{semver::Version, url::Url, Config, Update};

const UPDATE_ENDPOINT: &str =
    "https://github.com/fes/fesTerm/releases/latest/download/festerm-update.json";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InstallationKind {
    Developer,
    PackageManaged,
    SelfUpdating,
}

impl InstallationKind {
    fn from_build_marker(marker: Option<&str>) -> Self {
        match marker {
            Some("managed") => Self::PackageManaged,
            Some("app" | "appimage" | "nsis") => Self::SelfUpdating,
            Some(_) | None => Self::Developer,
        }
    }

    pub(crate) const fn can_install(self) -> bool {
        matches!(self, Self::SelfUpdating)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct UpdateSummary {
    pub(crate) version: String,
    pub(crate) notes: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum UpdateStatus {
    Unavailable(&'static str),
    Idle,
    Checking,
    Current,
    Available(UpdateSummary),
    Downloading(UpdateSummary),
    ReadyToInstall(UpdateSummary),
    Installing(UpdateSummary),
    Installed(UpdateSummary),
    Failed {
        message: &'static str,
        retry_check: bool,
    },
}

impl UpdateStatus {
    pub(crate) const fn is_busy(&self) -> bool {
        matches!(
            self,
            Self::Checking | Self::Downloading(_) | Self::Installing(_)
        )
    }
}

trait UpdateBackend: Send + Sync {
    fn check(&self) -> Result<Option<Box<dyn PendingUpdate>>, ()>;
}

trait PendingUpdate: Send {
    fn summary(&self) -> UpdateSummary;
    fn download(self: Box<Self>) -> Result<Box<dyn DownloadedUpdate>, ()>;
}

trait DownloadedUpdate: Send {
    fn summary(&self) -> UpdateSummary;
    fn install(self: Box<Self>) -> Result<(), ()>;
}

struct CargoUpdateBackend {
    public_key: String,
}

impl UpdateBackend for CargoUpdateBackend {
    fn check(&self) -> Result<Option<Box<dyn PendingUpdate>>, ()> {
        let endpoint = Url::parse(UPDATE_ENDPOINT).map_err(|error| {
            tracing::error!(%error, "invalid compiled update endpoint");
        })?;
        let version = Version::parse(env!("CARGO_PKG_VERSION")).map_err(|error| {
            tracing::error!(%error, "invalid compiled application version");
        })?;
        cargo_packager_updater::check_update(
            version,
            Config {
                endpoints: vec![endpoint],
                pubkey: self.public_key.clone(),
                windows: None,
            },
        )
        .map(|update| {
            update.map(|update| Box::new(CargoPendingUpdate(update)) as Box<dyn PendingUpdate>)
        })
        .map_err(|error| {
            tracing::warn!(%error, "update check failed");
        })
    }
}

struct CargoPendingUpdate(Update);

impl PendingUpdate for CargoPendingUpdate {
    fn summary(&self) -> UpdateSummary {
        summary(&self.0)
    }

    fn download(self: Box<Self>) -> Result<Box<dyn DownloadedUpdate>, ()> {
        let Self(update) = *self;
        let bytes = update.download().map_err(|error| {
            tracing::warn!(%error, "update download or verification failed");
        })?;
        Ok(Box::new(CargoDownloadedUpdate { update, bytes }))
    }
}

struct CargoDownloadedUpdate {
    update: Update,
    bytes: Vec<u8>,
}

impl DownloadedUpdate for CargoDownloadedUpdate {
    fn summary(&self) -> UpdateSummary {
        summary(&self.update)
    }

    fn install(self: Box<Self>) -> Result<(), ()> {
        let Self { update, bytes } = *self;
        update.install(bytes).map_err(|error| {
            tracing::error!(%error, "verified update installation failed");
        })
    }
}

enum WorkerResult {
    Checked(Result<Option<Box<dyn PendingUpdate>>, ()>),
    Downloaded(Result<Box<dyn DownloadedUpdate>, ()>),
    Installed(Result<(), ()>),
}

impl WorkerResult {
    fn matches_status(&self, status: &UpdateStatus) -> bool {
        matches!(
            (self, status),
            (Self::Checked(_), UpdateStatus::Checking)
                | (Self::Downloaded(_), UpdateStatus::Downloading(_))
                | (Self::Installed(_), UpdateStatus::Installing(_))
        )
    }
}

type WorkerTask = Box<dyn FnOnce() -> WorkerResult + Send>;
type WorkerSpawner = fn(WorkerTask) -> Receiver<WorkerResult>;

pub(crate) struct UpdateController {
    status: UpdateStatus,
    installation_kind: InstallationKind,
    backend: Option<Arc<dyn UpdateBackend>>,
    pending_update: Option<Box<dyn PendingUpdate>>,
    downloaded_update: Option<Box<dyn DownloadedUpdate>>,
    receiver: Option<Receiver<WorkerResult>>,
    worker_spawner: WorkerSpawner,
}

impl UpdateController {
    pub(crate) fn from_build() -> Self {
        Self::new(
            option_env!("FESTERM_INSTALLATION_KIND"),
            option_env!("FESTERM_UPDATE_PUBLIC_KEY"),
        )
    }

    fn new(installation_marker: Option<&str>, public_key: Option<&str>) -> Self {
        let installation_kind = InstallationKind::from_build_marker(installation_marker);
        let public_key = public_key
            .map(str::trim)
            .filter(|key| !key.is_empty())
            .map(str::to_owned);
        let status = if installation_kind == InstallationKind::Developer {
            UpdateStatus::Unavailable("Update checks are available in packaged releases.")
        } else if public_key.is_none() {
            UpdateStatus::Unavailable("This build does not contain an update verification key.")
        } else {
            UpdateStatus::Idle
        };
        let backend = public_key.map(|public_key| {
            Arc::new(CargoUpdateBackend { public_key }) as Arc<dyn UpdateBackend>
        });
        Self {
            status,
            installation_kind,
            backend,
            pending_update: None,
            downloaded_update: None,
            receiver: None,
            worker_spawner: spawn_worker,
        }
    }

    #[cfg(test)]
    pub(crate) fn unavailable_for_test() -> Self {
        Self::new(None, None)
    }

    #[cfg(test)]
    pub(crate) fn configured_for_test() -> Self {
        Self::new(Some("app"), Some("test public key"))
    }

    #[cfg(test)]
    fn with_test_backend(backend: Arc<dyn UpdateBackend>) -> Self {
        Self {
            status: UpdateStatus::Idle,
            installation_kind: InstallationKind::SelfUpdating,
            backend: Some(backend),
            pending_update: None,
            downloaded_update: None,
            receiver: None,
            worker_spawner: run_worker_inline,
        }
    }

    pub(crate) const fn status(&self) -> &UpdateStatus {
        &self.status
    }

    pub(crate) const fn installation_kind(&self) -> InstallationKind {
        self.installation_kind
    }

    pub(crate) const fn endpoint() -> &'static str {
        UPDATE_ENDPOINT
    }

    pub(crate) fn begin_check(&mut self) {
        if self.status.is_busy() || matches!(self.status, UpdateStatus::Unavailable(_)) {
            return;
        }
        let Some(backend) = self.backend.clone() else {
            self.status = UpdateStatus::Unavailable(
                "This build does not contain an update verification key.",
            );
            return;
        };
        self.pending_update = None;
        self.downloaded_update = None;
        self.status = UpdateStatus::Checking;
        self.receiver = Some((self.worker_spawner)(Box::new(move || {
            WorkerResult::Checked(backend.check())
        })));
    }

    pub(crate) fn begin_download(&mut self) {
        if !self.installation_kind.can_install() || self.status.is_busy() {
            return;
        }
        let Some(update) = self.pending_update.take() else {
            return;
        };
        let summary = update.summary();
        self.status = UpdateStatus::Downloading(summary);
        self.receiver = Some((self.worker_spawner)(Box::new(move || {
            WorkerResult::Downloaded(update.download())
        })));
    }

    pub(crate) fn begin_install(&mut self) {
        if !self.installation_kind.can_install() || self.status.is_busy() {
            return;
        }
        let Some(update) = self.downloaded_update.take() else {
            return;
        };
        let summary = update.summary();
        self.status = UpdateStatus::Installing(summary);
        self.receiver = Some((self.worker_spawner)(Box::new(move || {
            WorkerResult::Installed(update.install())
        })));
    }

    pub(crate) fn poll(&mut self) {
        let Some(receiver) = self.receiver.as_ref() else {
            return;
        };
        let result = match receiver.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => {
                self.receiver = None;
                if self.status.is_busy() {
                    self.status = UpdateStatus::Failed {
                        message: "The update worker stopped unexpectedly.",
                        retry_check: true,
                    };
                }
                return;
            }
        };
        self.receiver = None;
        if !result.matches_status(&self.status) {
            self.status = UpdateStatus::Failed {
                message: "The update worker returned an unexpected result.",
                retry_check: true,
            };
            return;
        }
        match result {
            WorkerResult::Checked(Ok(Some(update))) => {
                self.status = UpdateStatus::Available(update.summary());
                self.pending_update = Some(update);
            }
            WorkerResult::Checked(Ok(None)) => self.status = UpdateStatus::Current,
            WorkerResult::Checked(Err(())) => {
                self.status = UpdateStatus::Failed {
                    message:
                        "Could not check for updates. Check your network connection and try again.",
                    retry_check: true,
                };
            }
            WorkerResult::Downloaded(Ok(update)) => {
                self.status = UpdateStatus::ReadyToInstall(update.summary());
                self.downloaded_update = Some(update);
            }
            WorkerResult::Downloaded(Err(())) => {
                self.status = UpdateStatus::Failed {
                    message: "The update could not be downloaded or its signature was invalid.",
                    retry_check: true,
                };
            }
            WorkerResult::Installed(Ok(())) => {
                let summary = match &self.status {
                    UpdateStatus::Installing(summary) => summary.clone(),
                    _ => return,
                };
                self.status = UpdateStatus::Installed(summary);
            }
            WorkerResult::Installed(Err(())) => {
                self.status = UpdateStatus::Failed {
                    message: "The verified update could not be installed.",
                    retry_check: true,
                };
            }
        }
    }
}

fn spawn_worker(work: WorkerTask) -> Receiver<WorkerResult> {
    let (sender, receiver) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let _ = sender.send(work());
    });
    receiver
}

#[cfg(test)]
fn run_worker_inline(work: WorkerTask) -> Receiver<WorkerResult> {
    let (sender, receiver) = mpsc::sync_channel(1);
    let _ = sender.send(work());
    receiver
}

fn summary(update: &Update) -> UpdateSummary {
    UpdateSummary {
        version: update.version.clone(),
        notes: update.body.clone(),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Mutex,
        },
    };

    use super::*;

    enum CheckOutcome {
        Current,
        Available(FakeUpdatePlan),
        Failure,
    }

    struct FakeBackend {
        outcomes: Mutex<VecDeque<CheckOutcome>>,
        checks: AtomicUsize,
    }

    impl FakeBackend {
        fn new(outcomes: impl IntoIterator<Item = CheckOutcome>) -> Arc<Self> {
            Arc::new(Self {
                outcomes: Mutex::new(outcomes.into_iter().collect()),
                checks: AtomicUsize::new(0),
            })
        }
    }

    impl UpdateBackend for FakeBackend {
        fn check(&self) -> Result<Option<Box<dyn PendingUpdate>>, ()> {
            self.checks.fetch_add(1, Ordering::Relaxed);
            match self
                .outcomes
                .lock()
                .expect("fake updater outcomes mutex should not be poisoned")
                .pop_front()
                .expect("fake updater should have a scripted check outcome")
            {
                CheckOutcome::Current => Ok(None),
                CheckOutcome::Available(update) => Ok(Some(Box::new(FakePendingUpdate(update)))),
                CheckOutcome::Failure => Err(()),
            }
        }
    }

    struct FakeUpdatePlan {
        summary: UpdateSummary,
        download_result: Result<FakeInstallPlan, ()>,
        download_dispatches: Arc<AtomicUsize>,
    }

    struct FakeInstallPlan {
        result: Result<(), ()>,
        install_dispatches: Arc<AtomicUsize>,
    }

    struct FakePendingUpdate(FakeUpdatePlan);

    impl PendingUpdate for FakePendingUpdate {
        fn summary(&self) -> UpdateSummary {
            self.0.summary.clone()
        }

        fn download(self: Box<Self>) -> Result<Box<dyn DownloadedUpdate>, ()> {
            let Self(plan) = *self;
            plan.download_dispatches.fetch_add(1, Ordering::Relaxed);
            let install = plan.download_result?;
            Ok(Box::new(FakeDownloadedUpdate {
                summary: plan.summary,
                install,
            }))
        }
    }

    struct FakeDownloadedUpdate {
        summary: UpdateSummary,
        install: FakeInstallPlan,
    }

    impl DownloadedUpdate for FakeDownloadedUpdate {
        fn summary(&self) -> UpdateSummary {
            self.summary.clone()
        }

        fn install(self: Box<Self>) -> Result<(), ()> {
            self.install
                .install_dispatches
                .fetch_add(1, Ordering::Relaxed);
            self.install.result
        }
    }

    fn update_summary() -> UpdateSummary {
        UpdateSummary {
            version: "0.2.0".to_owned(),
            notes: Some("Deterministic test release.".to_owned()),
        }
    }

    fn update_plan(
        download_result: Result<FakeInstallPlan, ()>,
        download_dispatches: Arc<AtomicUsize>,
    ) -> FakeUpdatePlan {
        FakeUpdatePlan {
            summary: update_summary(),
            download_result,
            download_dispatches,
        }
    }

    fn install_plan(
        result: Result<(), ()>,
        install_dispatches: Arc<AtomicUsize>,
    ) -> FakeInstallPlan {
        FakeInstallPlan {
            result,
            install_dispatches,
        }
    }

    fn begin_available_check(controller: &mut UpdateController) {
        controller.begin_check();
        assert_eq!(controller.status(), &UpdateStatus::Checking);
        controller.poll();
        assert_eq!(
            controller.status(),
            &UpdateStatus::Available(update_summary())
        );
    }

    fn begin_successful_download(controller: &mut UpdateController) {
        controller.begin_download();
        assert_eq!(
            controller.status(),
            &UpdateStatus::Downloading(update_summary())
        );
        controller.poll();
        assert_eq!(
            controller.status(),
            &UpdateStatus::ReadyToInstall(update_summary())
        );
    }

    #[test]
    fn developer_builds_never_contact_the_update_endpoint() {
        let mut controller = UpdateController::new(None, Some("public key"));
        controller.begin_check();

        assert!(matches!(
            controller.status(),
            UpdateStatus::Unavailable("Update checks are available in packaged releases.")
        ));
        assert!(controller.receiver.is_none());
    }

    #[test]
    fn packaged_builds_fail_closed_without_a_public_key() {
        let controller = UpdateController::new(Some("app"), None);

        assert!(matches!(
            controller.status(),
            UpdateStatus::Unavailable("This build does not contain an update verification key.")
        ));
    }

    #[test]
    fn package_managers_can_check_but_cannot_install() {
        let controller = UpdateController::new(Some("managed"), Some("public key"));

        assert_eq!(
            controller.installation_kind(),
            InstallationKind::PackageManaged
        );
        assert!(!controller.installation_kind().can_install());
        assert_eq!(controller.status(), &UpdateStatus::Idle);
    }

    #[test]
    fn known_self_update_formats_are_installable() {
        for marker in ["app", "appimage", "nsis"] {
            assert!(InstallationKind::from_build_marker(Some(marker)).can_install());
        }
        assert_eq!(
            InstallationKind::from_build_marker(Some("wix")),
            InstallationKind::Developer
        );
    }

    #[test]
    fn check_reports_no_update() {
        let backend = FakeBackend::new([CheckOutcome::Current]);
        let mut controller = UpdateController::with_test_backend(backend);

        controller.begin_check();
        assert_eq!(controller.status(), &UpdateStatus::Checking);
        controller.poll();

        assert_eq!(controller.status(), &UpdateStatus::Current);
        assert!(controller.pending_update.is_none());
    }

    #[test]
    fn check_reports_an_available_update_without_downloading_it() {
        let downloads = Arc::new(AtomicUsize::new(0));
        let installs = Arc::new(AtomicUsize::new(0));
        let backend = FakeBackend::new([CheckOutcome::Available(update_plan(
            Ok(install_plan(Ok(()), installs)),
            Arc::clone(&downloads),
        ))]);
        let mut controller = UpdateController::with_test_backend(backend);

        begin_available_check(&mut controller);

        assert!(controller.pending_update.is_some());
        assert_eq!(downloads.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn download_successfully_reaches_ready_to_install() {
        let downloads = Arc::new(AtomicUsize::new(0));
        let installs = Arc::new(AtomicUsize::new(0));
        let backend = FakeBackend::new([CheckOutcome::Available(update_plan(
            Ok(install_plan(Ok(()), Arc::clone(&installs))),
            Arc::clone(&downloads),
        ))]);
        let mut controller = UpdateController::with_test_backend(backend);
        begin_available_check(&mut controller);

        begin_successful_download(&mut controller);

        assert!(controller.downloaded_update.is_some());
        assert_eq!(downloads.load(Ordering::Relaxed), 1);
        assert_eq!(installs.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn check_failure_is_retryable_and_content_safe() {
        let backend = FakeBackend::new([CheckOutcome::Failure]);
        let mut controller = UpdateController::with_test_backend(backend);

        controller.begin_check();
        controller.poll();

        assert_eq!(
            controller.status(),
            &UpdateStatus::Failed {
                message:
                    "Could not check for updates. Check your network connection and try again.",
                retry_check: true,
            }
        );
    }

    #[test]
    fn download_failure_is_retryable_and_content_safe() {
        let downloads = Arc::new(AtomicUsize::new(0));
        let backend = FakeBackend::new([CheckOutcome::Available(update_plan(
            Err(()),
            Arc::clone(&downloads),
        ))]);
        let mut controller = UpdateController::with_test_backend(backend);
        begin_available_check(&mut controller);

        controller.begin_download();
        controller.poll();

        assert_eq!(
            controller.status(),
            &UpdateStatus::Failed {
                message: "The update could not be downloaded or its signature was invalid.",
                retry_check: true,
            }
        );
        assert_eq!(downloads.load(Ordering::Relaxed), 1);
        assert!(controller.downloaded_update.is_none());
    }

    #[test]
    fn successful_install_is_dispatched_and_reported() {
        let downloads = Arc::new(AtomicUsize::new(0));
        let installs = Arc::new(AtomicUsize::new(0));
        let backend = FakeBackend::new([CheckOutcome::Available(update_plan(
            Ok(install_plan(Ok(()), Arc::clone(&installs))),
            downloads,
        ))]);
        let mut controller = UpdateController::with_test_backend(backend);
        begin_available_check(&mut controller);
        begin_successful_download(&mut controller);

        controller.begin_install();
        assert_eq!(
            controller.status(),
            &UpdateStatus::Installing(update_summary())
        );
        assert_eq!(installs.load(Ordering::Relaxed), 1);
        controller.poll();

        assert_eq!(
            controller.status(),
            &UpdateStatus::Installed(update_summary())
        );
    }

    #[test]
    fn install_failure_is_retryable_and_content_safe() {
        let downloads = Arc::new(AtomicUsize::new(0));
        let installs = Arc::new(AtomicUsize::new(0));
        let backend = FakeBackend::new([CheckOutcome::Available(update_plan(
            Ok(install_plan(Err(()), Arc::clone(&installs))),
            downloads,
        ))]);
        let mut controller = UpdateController::with_test_backend(backend);
        begin_available_check(&mut controller);
        begin_successful_download(&mut controller);

        controller.begin_install();
        controller.poll();

        assert_eq!(installs.load(Ordering::Relaxed), 1);
        assert_eq!(
            controller.status(),
            &UpdateStatus::Failed {
                message: "The verified update could not be installed.",
                retry_check: true,
            }
        );
    }

    #[test]
    fn retry_after_failure_starts_a_fresh_check() {
        let backend = FakeBackend::new([CheckOutcome::Failure, CheckOutcome::Current]);
        let mut controller = UpdateController::with_test_backend(backend.clone());

        controller.begin_check();
        controller.poll();
        assert!(matches!(
            controller.status(),
            UpdateStatus::Failed {
                retry_check: true,
                ..
            }
        ));

        controller.begin_check();
        assert_eq!(controller.status(), &UpdateStatus::Checking);
        controller.poll();

        assert_eq!(controller.status(), &UpdateStatus::Current);
        assert_eq!(backend.checks.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn worker_channel_disconnect_becomes_a_retryable_failure() {
        let backend = FakeBackend::new([]);
        let mut controller = UpdateController::with_test_backend(backend);
        let (sender, receiver) = mpsc::sync_channel(1);
        drop(sender);
        controller.status = UpdateStatus::Checking;
        controller.receiver = Some(receiver);

        controller.poll();

        assert_eq!(
            controller.status(),
            &UpdateStatus::Failed {
                message: "The update worker stopped unexpectedly.",
                retry_check: true,
            }
        );
        assert!(controller.receiver.is_none());
    }

    #[test]
    fn stale_worker_completion_fails_closed() {
        let backend = FakeBackend::new([]);
        let mut controller = UpdateController::with_test_backend(backend);
        let (sender, receiver) = mpsc::sync_channel(1);
        sender
            .send(WorkerResult::Checked(Ok(None)))
            .expect("test worker result should be received");
        controller.status = UpdateStatus::Downloading(update_summary());
        controller.receiver = Some(receiver);

        controller.poll();

        assert_eq!(
            controller.status(),
            &UpdateStatus::Failed {
                message: "The update worker returned an unexpected result.",
                retry_check: true,
            }
        );
        assert!(controller.receiver.is_none());
    }
}
