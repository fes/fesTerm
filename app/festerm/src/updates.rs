use std::{
    sync::{
        mpsc::{self, Receiver, TryRecvError},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

use cargo_packager_updater::{semver::Version, url::Url, Config, Update};

const UPDATE_ENDPOINT: &str =
    "https://github.com/fes/fesTerm/releases/latest/download/festerm-update.json";

/// How long an automatic check waits before asking again.
///
/// A day is deliberately coarse: the point is that a user hears about a fix
/// within a day of starting fesTerm, not that they hear about it first.
const AUTOMATIC_CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// How long after launch the first automatic check may run.
///
/// Startup already contends for the network and the disk, and nobody opened
/// fesTerm to find out about fesTerm.
const FIRST_AUTOMATIC_CHECK_DELAY: Duration = Duration::from_secs(5 * 60);

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
    Refreshing(UpdateSummary, RefreshPurpose),
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
            Self::Checking | Self::Refreshing(_, _) | Self::Downloading(_) | Self::Installing(_)
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RefreshPurpose {
    Download,
    Install,
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
    Refreshed(Result<Option<Box<dyn PendingUpdate>>, ()>),
    Downloaded(Result<Box<dyn DownloadedUpdate>, ()>),
    Installed(Result<(), ()>),
}

impl WorkerResult {
    fn matches_status(&self, status: &UpdateStatus) -> bool {
        matches!(
            (self, status),
            (Self::Checked(_), UpdateStatus::Checking)
                | (Self::Refreshed(_), UpdateStatus::Refreshing(_, _))
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
    install_after_download: bool,
    receiver: Option<Receiver<WorkerResult>>,
    worker_spawner: WorkerSpawner,
    schedule: AutomaticSchedule,
}

/// The state behind the occasional background check.
///
/// An automatic check is not a user request: it must never raise an error
/// surface, and it must never re-announce a version the user has already been
/// shown.
#[derive(Default)]
struct AutomaticSchedule {
    enabled: bool,
    launched_at: Option<Instant>,
    last_checked_unix_seconds: Option<u64>,
    acknowledged_version: Option<String>,
    in_flight: bool,
    unsaved: Option<UpdateCheckOutcome>,
}

/// A schedule change the application should persist.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct UpdateCheckOutcome {
    pub(crate) last_checked_unix_seconds: u64,
    pub(crate) acknowledged_version: Option<String>,
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
            install_after_download: false,
            receiver: None,
            worker_spawner: spawn_worker,
            schedule: AutomaticSchedule::default(),
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
    pub(crate) fn installed_for_test() -> Self {
        let mut controller = Self::configured_for_test();
        controller.status = UpdateStatus::Installed(UpdateSummary {
            version: "0.2.0".to_owned(),
            notes: Some("Deterministic test release.".to_owned()),
        });
        controller
    }

    #[cfg(test)]
    pub(crate) fn ready_to_install_for_test() -> Self {
        Self::ready_to_install_result_for_test(Ok(()))
    }

    #[cfg(test)]
    pub(crate) fn ready_to_fail_install_for_test() -> Self {
        Self::ready_to_install_result_for_test(Err(()))
    }

    #[cfg(test)]
    fn ready_to_install_result_for_test(result: Result<(), ()>) -> Self {
        struct Downloaded(Result<(), ()>);
        struct Backend(Result<(), ()>);
        struct Pending(Result<(), ()>);

        impl UpdateBackend for Backend {
            fn check(&self) -> Result<Option<Box<dyn PendingUpdate>>, ()> {
                Ok(Some(Box::new(Pending(self.0))))
            }
        }

        impl PendingUpdate for Pending {
            fn summary(&self) -> UpdateSummary {
                Downloaded(self.0).summary()
            }

            fn download(self: Box<Self>) -> Result<Box<dyn DownloadedUpdate>, ()> {
                panic!("a same-version refresh must reuse the verified download")
            }
        }

        impl DownloadedUpdate for Downloaded {
            fn summary(&self) -> UpdateSummary {
                UpdateSummary {
                    version: "0.2.0".to_owned(),
                    notes: Some("Deterministic test release.".to_owned()),
                }
            }

            fn install(self: Box<Self>) -> Result<(), ()> {
                self.0
            }
        }

        let mut controller = Self::configured_for_test();
        let downloaded = Downloaded(result);
        controller.status = UpdateStatus::ReadyToInstall(downloaded.summary());
        controller.downloaded_update = Some(Box::new(downloaded));
        controller.backend = Some(Arc::new(Backend(result)));
        controller.worker_spawner = run_worker_inline;
        controller
    }

    /// A self-updating controller whose checks never leave the process, for
    /// tests that exercise scheduling rather than the network.
    #[cfg(test)]
    pub(crate) fn inert_for_test() -> Self {
        Self::with_test_backend(Arc::new(NoUpdateBackend))
    }

    #[cfg(test)]
    fn with_test_backend(backend: Arc<dyn UpdateBackend>) -> Self {
        Self {
            status: UpdateStatus::Idle,
            installation_kind: InstallationKind::SelfUpdating,
            backend: Some(backend),
            pending_update: None,
            downloaded_update: None,
            install_after_download: false,
            receiver: None,
            worker_spawner: run_worker_inline,
            schedule: AutomaticSchedule::default(),
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

    /// Restores the poll clock and the badge the user has already seen.
    ///
    /// Restarting fesTerm must not restart the interval; otherwise a user who
    /// opens and closes it all day would be checking all day.
    pub(crate) fn restore_schedule(
        &mut self,
        enabled: bool,
        last_checked_unix_seconds: Option<u64>,
        acknowledged_version: Option<String>,
    ) {
        self.schedule.enabled = enabled;
        self.schedule.last_checked_unix_seconds = last_checked_unix_seconds;
        self.schedule.acknowledged_version = acknowledged_version;
        self.schedule.launched_at.get_or_insert_with(Instant::now);
    }

    /// Applies a change to the preference without disturbing the clock.
    pub(crate) fn set_automatic_checks_enabled(&mut self, enabled: bool) {
        self.schedule.enabled = enabled;
    }

    #[cfg(test)]
    pub(crate) const fn automatic_checks_enabled(&self) -> bool {
        self.schedule.enabled
    }

    /// Returns whether an automatic check is due, without asking the clock.
    fn automatic_check_is_due(&self, since_launch: Duration, now_unix_seconds: u64) -> bool {
        if !self.schedule.enabled || self.schedule.in_flight {
            return false;
        }
        // Package-managed installs are updated by the package manager, and a
        // developer build has nothing to update to.
        if !self.installation_kind.can_install() {
            return false;
        }
        if self.status.is_busy() || matches!(self.status, UpdateStatus::Unavailable(_)) {
            return false;
        }
        // User download/install actions refresh a waiting release independently.
        if matches!(
            self.status,
            UpdateStatus::Available(_)
                | UpdateStatus::ReadyToInstall(_)
                | UpdateStatus::Installed(_)
        ) {
            return false;
        }
        match self.schedule.last_checked_unix_seconds {
            None => since_launch >= FIRST_AUTOMATIC_CHECK_DELAY,
            Some(last) => {
                // A clock that moved backwards (timezone edit, NTP step) must
                // not park the next check somewhere in the future.
                let elapsed = now_unix_seconds.saturating_sub(last);
                elapsed >= AUTOMATIC_CHECK_INTERVAL.as_secs() || now_unix_seconds < last
            }
        }
    }

    /// Starts a background check when one is due. Returns whether it started.
    pub(crate) fn poll_schedule(&mut self, now_unix_seconds: u64) -> bool {
        let launched_at = *self.schedule.launched_at.get_or_insert_with(Instant::now);
        let since_launch = launched_at.elapsed();
        if !self.automatic_check_is_due(since_launch, now_unix_seconds) {
            return false;
        }
        self.schedule.in_flight = true;
        self.schedule.last_checked_unix_seconds = Some(now_unix_seconds);
        self.schedule.unsaved = Some(UpdateCheckOutcome {
            last_checked_unix_seconds: now_unix_seconds,
            acknowledged_version: self.schedule.acknowledged_version.clone(),
        });
        self.begin_check();
        true
    }

    /// Returns the version a badge should announce, if any.
    ///
    /// A version the user has already been shown is not news.
    pub(crate) fn unacknowledged_version(&self) -> Option<&str> {
        let version = match &self.status {
            UpdateStatus::Available(summary)
            | UpdateStatus::Downloading(summary)
            | UpdateStatus::ReadyToInstall(summary) => summary.version.as_str(),
            _ => return None,
        };
        match self.schedule.acknowledged_version.as_deref() {
            Some(acknowledged) if acknowledged == version => None,
            _ => Some(version),
        }
    }

    /// Records that the user has now seen whatever the badge was announcing.
    pub(crate) fn acknowledge_available_version(&mut self) {
        let Some(version) = self.unacknowledged_version().map(str::to_owned) else {
            return;
        };
        self.schedule.acknowledged_version = Some(version.clone());
        self.schedule.unsaved = Some(UpdateCheckOutcome {
            last_checked_unix_seconds: self.schedule.last_checked_unix_seconds.unwrap_or_default(),
            acknowledged_version: Some(version),
        });
    }

    /// Takes the schedule change the application still has to persist.
    pub(crate) fn take_unsaved_outcome(&mut self) -> Option<UpdateCheckOutcome> {
        self.schedule.unsaved.take()
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
        self.install_after_download = false;
        self.status = UpdateStatus::Checking;
        self.receiver = Some((self.worker_spawner)(Box::new(move || {
            WorkerResult::Checked(backend.check())
        })));
    }

    pub(crate) fn begin_download(&mut self) {
        if !self.installation_kind.can_install() || self.pending_update.is_none() {
            return;
        }
        let UpdateStatus::Available(summary) = &self.status else {
            return;
        };
        self.begin_refresh(summary.clone(), RefreshPurpose::Download);
    }

    fn begin_refresh(&mut self, summary: UpdateSummary, purpose: RefreshPurpose) {
        let Some(backend) = self.backend.clone() else {
            self.fail("This build does not contain an update verification key.");
            return;
        };
        self.pending_update = None;
        self.install_after_download = false;
        self.status = UpdateStatus::Refreshing(summary, purpose);
        self.receiver = Some((self.worker_spawner)(Box::new(move || {
            WorkerResult::Refreshed(backend.check())
        })));
    }

    fn start_download(&mut self, update: Box<dyn PendingUpdate>, install_after_download: bool) {
        let summary = update.summary();
        self.install_after_download = install_after_download;
        self.status = UpdateStatus::Downloading(summary);
        self.receiver = Some((self.worker_spawner)(Box::new(move || {
            WorkerResult::Downloaded(update.download())
        })));
    }

    pub(crate) fn begin_install(&mut self) {
        if !self.installation_kind.can_install() || self.downloaded_update.is_none() {
            return;
        }
        let UpdateStatus::ReadyToInstall(summary) = &self.status else {
            return;
        };
        self.begin_refresh(summary.clone(), RefreshPurpose::Install);
    }

    fn start_install(&mut self, update: Box<dyn DownloadedUpdate>) {
        let summary = update.summary();
        self.status = UpdateStatus::Installing(summary);
        self.receiver = Some((self.worker_spawner)(Box::new(move || {
            WorkerResult::Installed(update.install())
        })));
    }

    fn fail(&mut self, message: &'static str) {
        self.pending_update = None;
        self.downloaded_update = None;
        self.install_after_download = false;
        self.receiver = None;
        self.schedule.in_flight = false;
        self.status = UpdateStatus::Failed {
            message,
            retry_check: true,
        };
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
                let automatic = std::mem::take(&mut self.schedule.in_flight);
                if self.status.is_busy() {
                    if automatic {
                        self.status = UpdateStatus::Idle;
                    } else {
                        self.fail("The update worker stopped unexpectedly.");
                    }
                }
                return;
            }
        };
        self.receiver = None;
        if !result.matches_status(&self.status) {
            self.fail("The update worker returned an unexpected result.");
            return;
        }
        let automatic = matches!(result, WorkerResult::Checked(_)) && self.schedule.in_flight;
        if matches!(result, WorkerResult::Checked(_)) {
            self.schedule.in_flight = false;
        }
        match result {
            WorkerResult::Checked(Ok(Some(update))) => {
                self.status = UpdateStatus::Available(update.summary());
                self.pending_update = Some(update);
            }
            WorkerResult::Checked(Ok(None)) => self.status = UpdateStatus::Current,
            WorkerResult::Checked(Err(())) => {
                // Nobody asked, so nobody is told: an automatic check that
                // cannot reach the network leaves the interface as it was and
                // tries again at the next interval.
                self.status = if automatic {
                    UpdateStatus::Idle
                } else {
                    UpdateStatus::Failed {
                        message:
                            "Could not check for updates. Check your network connection and try \
                             again.",
                        retry_check: true,
                    }
                };
            }
            WorkerResult::Refreshed(result) => {
                let UpdateStatus::Refreshing(previous, purpose) = &self.status else {
                    self.fail("The update worker returned an unexpected result.");
                    return;
                };
                let purpose = *purpose;
                let update = match result {
                    Ok(Some(update)) => update,
                    Ok(None) => {
                        self.fail(
                            "The previously offered update is no longer available. \
                             Check for updates again.",
                        );
                        return;
                    }
                    Err(()) => {
                        self.fail(
                            "Could not check the latest release. No update was installed. \
                             Check your network connection and try again.",
                        );
                        return;
                    }
                };
                let latest = update.summary();
                let versions = Version::parse(&previous.version).and_then(|previous| {
                    Version::parse(&latest.version).map(|latest| (previous, latest))
                });
                let (previous, latest) = match versions {
                    Ok(versions) => versions,
                    Err(error) => {
                        tracing::error!(%error, "invalid version during update refresh");
                        self.fail(
                            "The update release has an invalid version. Check for updates again.",
                        );
                        return;
                    }
                };
                if latest.cmp_precedence(&previous).is_lt() {
                    tracing::warn!("latest release is older than the previously offered update");
                    self.fail(
                        "The offered update changed to an older release. Check for updates again.",
                    );
                    return;
                }
                if purpose == RefreshPurpose::Install && latest == previous {
                    let Some(downloaded) = self.downloaded_update.take() else {
                        self.fail(
                            "The verified update is no longer available. Check for updates again.",
                        );
                        return;
                    };
                    self.start_install(downloaded);
                } else {
                    self.downloaded_update = None;
                    self.start_download(update, purpose == RefreshPurpose::Install);
                }
            }
            WorkerResult::Downloaded(Ok(update)) => {
                if std::mem::take(&mut self.install_after_download) {
                    self.start_install(update);
                } else {
                    self.status = UpdateStatus::ReadyToInstall(update.summary());
                    self.downloaded_update = Some(update);
                }
            }
            WorkerResult::Downloaded(Err(())) => {
                self.fail("The update could not be downloaded or its signature was invalid.");
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
#[cfg(test)]
struct NoUpdateBackend;

#[cfg(test)]
impl UpdateBackend for NoUpdateBackend {
    fn check(&self) -> Result<Option<Box<dyn PendingUpdate>>, ()> {
        Ok(None)
    }
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

    #[derive(Clone)]
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

    #[derive(Clone)]
    struct FakeUpdatePlan {
        summary: UpdateSummary,
        download_result: Result<FakeInstallPlan, ()>,
        download_dispatches: Arc<AtomicUsize>,
    }

    #[derive(Clone)]
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
            &UpdateStatus::Refreshing(update_summary(), RefreshPurpose::Download)
        );
        controller.poll();
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

    fn backend_with_plan(plan: FakeUpdatePlan, checks: usize) -> Arc<FakeBackend> {
        FakeBackend::new((0..checks).map(|_| CheckOutcome::Available(plan.clone())))
    }

    fn newer_update_plan(
        version: &str,
        download_result: Result<FakeInstallPlan, ()>,
        downloads: Arc<AtomicUsize>,
    ) -> FakeUpdatePlan {
        let mut plan = update_plan(download_result, downloads);
        plan.summary.version = version.to_owned();
        plan
    }

    const DAY: u64 = 24 * 60 * 60;

    fn scheduled_controller(
        backend: Arc<dyn UpdateBackend>,
        last_checked: u64,
    ) -> UpdateController {
        let mut controller = UpdateController::with_test_backend(backend);
        controller.restore_schedule(true, Some(last_checked), None);
        controller
    }

    #[test]
    fn an_automatic_check_waits_for_the_interval_to_elapse() {
        // The poll is deliberately rare: a user who leaves fesTerm open for a
        // week should see one check a day, not one a frame.
        let backend = FakeBackend::new([CheckOutcome::Current]);
        let mut controller = scheduled_controller(backend.clone(), 1_000 * DAY);

        assert!(!controller.poll_schedule(1_000 * DAY + DAY - 1));
        assert_eq!(backend.checks.load(Ordering::Relaxed), 0);

        assert!(controller.poll_schedule(1_000 * DAY + DAY));
        controller.poll();
        assert_eq!(backend.checks.load(Ordering::Relaxed), 1);
        assert_eq!(controller.status(), &UpdateStatus::Current);
    }

    #[test]
    fn a_first_ever_check_waits_out_the_startup_delay() {
        // Nothing may compete with the first frames of a launch, so an
        // install that has never checked still waits before it does.
        let backend = FakeBackend::new([CheckOutcome::Current]);
        let mut controller = UpdateController::with_test_backend(backend.clone());
        controller.restore_schedule(true, None, None);

        assert!(!controller.poll_schedule(1_000 * DAY));
        assert!(!controller.automatic_check_is_due(Duration::from_secs(60), 1_000 * DAY));
        assert!(controller.automatic_check_is_due(FIRST_AUTOMATIC_CHECK_DELAY, 1_000 * DAY));
        assert_eq!(backend.checks.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn a_clock_that_moved_backwards_makes_a_check_due_rather_than_unreachable() {
        // A timezone edit or an NTP step must not park the next check weeks
        // in the future.
        let backend = FakeBackend::new([CheckOutcome::Current]);
        let mut controller = scheduled_controller(backend.clone(), 2_000 * DAY);

        assert!(controller.poll_schedule(1_000 * DAY));
        assert_eq!(backend.checks.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn turning_the_preference_off_stops_automatic_checks() {
        let backend = FakeBackend::new([CheckOutcome::Current]);
        let mut controller = scheduled_controller(backend.clone(), 1_000 * DAY);
        controller.set_automatic_checks_enabled(false);

        assert!(!controller.automatic_checks_enabled());
        assert!(!controller.poll_schedule(2_000 * DAY));
        assert_eq!(backend.checks.load(Ordering::Relaxed), 0);

        controller.set_automatic_checks_enabled(true);
        assert!(controller.poll_schedule(2_000 * DAY));
    }

    #[test]
    fn a_package_managed_install_never_checks_on_its_own() {
        // The package manager owns the version; an unsolicited check could
        // only ever announce something fesTerm must not act on.
        let backend = FakeBackend::new([]);
        let mut controller = UpdateController::with_test_backend(backend.clone());
        controller.installation_kind = InstallationKind::PackageManaged;
        controller.restore_schedule(true, Some(1_000 * DAY), None);

        assert!(!controller.poll_schedule(2_000 * DAY));
        assert_eq!(backend.checks.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn a_failed_automatic_check_says_nothing() {
        // The user did not ask, so a flaky network or an offline laptop must
        // not raise an error surface.
        let backend = FakeBackend::new([CheckOutcome::Failure]);
        let mut controller = scheduled_controller(backend.clone(), 1_000 * DAY);

        assert!(controller.poll_schedule(2_000 * DAY));
        controller.poll();

        assert_eq!(controller.status(), &UpdateStatus::Idle);
        assert!(controller.unacknowledged_version().is_none());
    }

    #[test]
    fn a_user_requested_check_still_reports_its_failure() {
        let backend = FakeBackend::new([CheckOutcome::Failure]);
        let mut controller = scheduled_controller(backend, 1_000 * DAY);

        controller.begin_check();
        controller.poll();

        assert!(matches!(controller.status(), UpdateStatus::Failed { .. }));
    }

    #[test]
    fn an_automatic_check_records_when_it_ran_for_the_next_launch() {
        let backend = FakeBackend::new([CheckOutcome::Current]);
        let mut controller = scheduled_controller(backend, 1_000 * DAY);

        assert!(controller.poll_schedule(2_000 * DAY));

        assert_eq!(
            controller.take_unsaved_outcome(),
            Some(UpdateCheckOutcome {
                last_checked_unix_seconds: 2_000 * DAY,
                acknowledged_version: None,
            })
        );
        assert_eq!(controller.take_unsaved_outcome(), None);
    }

    #[test]
    fn a_version_the_user_has_already_seen_is_not_announced_again() {
        let backend = FakeBackend::new([CheckOutcome::Available(update_plan(
            Err(()),
            Arc::new(AtomicUsize::new(0)),
        ))]);
        let mut controller = scheduled_controller(backend, 1_000 * DAY);

        assert!(controller.poll_schedule(2_000 * DAY));
        controller.poll();
        assert_eq!(controller.unacknowledged_version(), Some("0.2.0"));

        controller.acknowledge_available_version();

        assert_eq!(controller.unacknowledged_version(), None);
        assert_eq!(
            controller.take_unsaved_outcome(),
            Some(UpdateCheckOutcome {
                last_checked_unix_seconds: 2_000 * DAY,
                acknowledged_version: Some("0.2.0".to_owned()),
            })
        );
        // A restart restores the acknowledgement, so the badge stays quiet.
        let restored_backend = FakeBackend::new([CheckOutcome::Available(update_plan(
            Err(()),
            Arc::new(AtomicUsize::new(0)),
        ))]);
        let mut restored = UpdateController::with_test_backend(restored_backend);
        restored.restore_schedule(true, Some(2_000 * DAY), Some("0.2.0".to_owned()));
        restored.begin_check();
        restored.poll();

        assert_eq!(restored.unacknowledged_version(), None);
    }

    #[test]
    fn an_update_already_waiting_is_not_re_checked_automatically() {
        let backend = FakeBackend::new([CheckOutcome::Available(update_plan(
            Err(()),
            Arc::new(AtomicUsize::new(0)),
        ))]);
        let mut controller = scheduled_controller(backend.clone(), 1_000 * DAY);

        assert!(controller.poll_schedule(2_000 * DAY));
        controller.poll();
        assert!(matches!(controller.status(), UpdateStatus::Available(_)));

        assert!(!controller.poll_schedule(3_000 * DAY));
        assert_eq!(backend.checks.load(Ordering::Relaxed), 1);
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
        let backend = backend_with_plan(
            update_plan(
                Ok(install_plan(Ok(()), Arc::clone(&installs))),
                Arc::clone(&downloads),
            ),
            2,
        );
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
        let backend = backend_with_plan(update_plan(Err(()), Arc::clone(&downloads)), 2);
        let mut controller = UpdateController::with_test_backend(backend);
        begin_available_check(&mut controller);

        controller.begin_download();
        controller.poll();
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
        let backend = backend_with_plan(
            update_plan(Ok(install_plan(Ok(()), Arc::clone(&installs))), downloads),
            3,
        );
        let mut controller = UpdateController::with_test_backend(backend);
        begin_available_check(&mut controller);
        begin_successful_download(&mut controller);

        controller.begin_install();
        assert_eq!(
            controller.status(),
            &UpdateStatus::Refreshing(update_summary(), RefreshPurpose::Install)
        );
        controller.poll();
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
        let backend = backend_with_plan(
            update_plan(Ok(install_plan(Err(()), Arc::clone(&installs))), downloads),
            3,
        );
        let mut controller = UpdateController::with_test_backend(backend);
        begin_available_check(&mut controller);
        begin_successful_download(&mut controller);

        controller.begin_install();
        controller.poll();
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
    fn downloading_rechecks_and_selects_the_latest_release_without_installing_it() {
        let old_downloads = Arc::new(AtomicUsize::new(0));
        let new_downloads = Arc::new(AtomicUsize::new(0));
        let installs = Arc::new(AtomicUsize::new(0));
        let latest = newer_update_plan(
            "0.5.0",
            Ok(install_plan(Ok(()), installs.clone())),
            new_downloads.clone(),
        );
        let latest_summary = latest.summary.clone();
        let backend = FakeBackend::new([
            CheckOutcome::Available(update_plan(Err(()), old_downloads.clone())),
            CheckOutcome::Available(latest),
        ]);
        let mut controller = UpdateController::with_test_backend(backend.clone());
        controller.set_automatic_checks_enabled(false);
        begin_available_check(&mut controller);

        controller.begin_download();
        assert!(controller.status().is_busy());
        controller.begin_check();
        controller.begin_download();
        controller.begin_install();
        assert_eq!(backend.checks.load(Ordering::Relaxed), 2);
        controller.poll();
        assert_eq!(
            controller.status(),
            &UpdateStatus::Downloading(latest_summary.clone())
        );
        controller.poll();
        assert_eq!(
            controller.status(),
            &UpdateStatus::ReadyToInstall(latest_summary)
        );
        assert_eq!(old_downloads.load(Ordering::Relaxed), 0);
        assert_eq!(new_downloads.load(Ordering::Relaxed), 1);
        assert_eq!(installs.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn installing_rechecks_and_verifies_a_newer_release_instead_of_installing_cached_bytes() {
        let old_downloads = Arc::new(AtomicUsize::new(0));
        let old_installs = Arc::new(AtomicUsize::new(0));
        let new_downloads = Arc::new(AtomicUsize::new(0));
        let new_installs = Arc::new(AtomicUsize::new(0));
        let old = update_plan(
            Ok(install_plan(Ok(()), old_installs.clone())),
            old_downloads.clone(),
        );
        let latest = newer_update_plan(
            "0.5.0",
            Ok(install_plan(Ok(()), new_installs.clone())),
            new_downloads.clone(),
        );
        let latest_summary = latest.summary.clone();
        let backend = FakeBackend::new([
            CheckOutcome::Available(old.clone()),
            CheckOutcome::Available(old),
            CheckOutcome::Available(latest),
        ]);
        let mut controller = UpdateController::with_test_backend(backend.clone());
        begin_available_check(&mut controller);
        begin_successful_download(&mut controller);
        controller.set_automatic_checks_enabled(false);

        controller.begin_install();
        assert!(controller.status().is_busy());
        controller.begin_check();
        controller.begin_install();
        controller.begin_download();
        assert_eq!(backend.checks.load(Ordering::Relaxed), 3);
        controller.poll();
        assert_eq!(
            controller.status(),
            &UpdateStatus::Downloading(latest_summary.clone())
        );
        assert_eq!(new_installs.load(Ordering::Relaxed), 0);
        controller.poll();
        assert_eq!(
            controller.status(),
            &UpdateStatus::Installing(latest_summary.clone())
        );
        assert_eq!(new_installs.load(Ordering::Relaxed), 1);
        controller.poll();
        assert_eq!(
            controller.status(),
            &UpdateStatus::Installed(latest_summary)
        );
        assert_eq!(old_downloads.load(Ordering::Relaxed), 1);
        assert_eq!(old_installs.load(Ordering::Relaxed), 0);
        assert_eq!(new_downloads.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn freshness_failures_never_download_or_install_a_stale_release() {
        for purpose in [RefreshPurpose::Download, RefreshPurpose::Install] {
            for outcome in [
                CheckOutcome::Failure,
                CheckOutcome::Current,
                CheckOutcome::Available(newer_update_plan(
                    "0.1.0",
                    Err(()),
                    Arc::new(AtomicUsize::new(0)),
                )),
                CheckOutcome::Available(newer_update_plan(
                    "invalid",
                    Err(()),
                    Arc::new(AtomicUsize::new(0)),
                )),
            ] {
                let downloads = Arc::new(AtomicUsize::new(0));
                let installs = Arc::new(AtomicUsize::new(0));
                let old = update_plan(
                    Ok(install_plan(Ok(()), installs.clone())),
                    downloads.clone(),
                );
                let mut outcomes = vec![CheckOutcome::Available(old.clone())];
                if purpose == RefreshPurpose::Install {
                    outcomes.push(CheckOutcome::Available(old));
                }
                outcomes.push(outcome);
                let mut controller =
                    UpdateController::with_test_backend(FakeBackend::new(outcomes));
                begin_available_check(&mut controller);
                if purpose == RefreshPurpose::Install {
                    begin_successful_download(&mut controller);
                    controller.begin_install();
                } else {
                    controller.begin_download();
                }
                controller.poll();
                assert!(matches!(
                    controller.status(),
                    UpdateStatus::Failed {
                        retry_check: true,
                        ..
                    }
                ));
                assert_eq!(installs.load(Ordering::Relaxed), 0);
                assert_eq!(
                    downloads.load(Ordering::Relaxed),
                    usize::from(purpose == RefreshPurpose::Install)
                );
                assert!(controller.pending_update.is_none());
                assert!(controller.downloaded_update.is_none());
                assert!(!controller.install_after_download);
            }
        }
    }

    #[test]
    fn newer_release_verification_failure_never_installs_the_cached_release() {
        let old_installs = Arc::new(AtomicUsize::new(0));
        let new_downloads = Arc::new(AtomicUsize::new(0));
        let old = update_plan(
            Ok(install_plan(Ok(()), old_installs.clone())),
            Arc::new(AtomicUsize::new(0)),
        );
        let backend = FakeBackend::new([
            CheckOutcome::Available(old.clone()),
            CheckOutcome::Available(old),
            CheckOutcome::Available(newer_update_plan("0.5.0", Err(()), new_downloads.clone())),
        ]);
        let mut controller = UpdateController::with_test_backend(backend);
        begin_available_check(&mut controller);
        begin_successful_download(&mut controller);
        controller.begin_install();
        controller.poll();
        controller.poll();
        assert_eq!(
            controller.status(),
            &UpdateStatus::Failed {
                message: "The update could not be downloaded or its signature was invalid.",
                retry_check: true,
            }
        );
        assert_eq!(new_downloads.load(Ordering::Relaxed), 1);
        assert_eq!(old_installs.load(Ordering::Relaxed), 0);
        assert!(controller.downloaded_update.is_none());
        assert!(!controller.install_after_download);
    }

    #[test]
    fn refresh_worker_disconnect_or_stale_completion_discards_cached_install_authority() {
        for stale in [false, true] {
            let mut controller = UpdateController::ready_to_install_for_test();
            controller.begin_install();
            let (sender, receiver) = mpsc::sync_channel(1);
            if stale {
                sender.send(WorkerResult::Checked(Ok(None))).unwrap();
            }
            drop(sender);
            controller.receiver = Some(receiver);
            controller.poll();
            assert!(matches!(
                controller.status(),
                UpdateStatus::Failed {
                    retry_check: true,
                    ..
                }
            ));
            assert!(controller.downloaded_update.is_none());
            assert!(controller.receiver.is_none());
            controller.begin_install();
            assert!(matches!(controller.status(), UpdateStatus::Failed { .. }));
        }
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
