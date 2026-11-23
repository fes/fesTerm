//! Same-user native host control. Process observations are never host readiness.
//! CLI management never treats a native process observation as endpoint readiness.
mod definitions;
pub(crate) mod start_diagnostics;
use start_diagnostics::{observe as observe_start, StartStage};
pub mod native;
#[cfg(all(feature = "cloud", feature = "host-runtime"))]
mod reconfiguration;
mod replacement;
mod trusted;
pub use replacement::{ReleaseMetadata, ReplacementReport, ReplacementState};
pub use trusted::TrustedStage;

#[cfg(all(test, feature = "cloud", feature = "host-runtime"))]
pub(crate) fn fixture_stage(root: &std::path::Path) -> TrustedStage {
    let _directory = festerm_sessiond::private_storage::private_directory(root, true).unwrap();
    trusted::fixture(root)
}

#[cfg(all(test, feature = "cloud", feature = "host-runtime"))]
pub(crate) fn fixture_services<'a>(
    store: &'a Store,
    state: &std::path::Path,
) -> std::io::Result<Services<'a, impl CommandRunner>> {
    struct NoNativeCalls;
    impl CommandRunner for NoNativeCalls {
        fn execute(&self, _: &NativeRequest) -> io::Result<NativeOutput> {
            panic!("pure unpublished-registration fixture must not call a native manager")
        }
    }
    definitions::path_text(state)?;
    let services = Services {
        store,
        state: state.into(),
        context: Context {
            platform: Platform::Windows,
            user: "S-1-5-21-1-2-3-1001".into(),
            registration: state.join("fixture-registration"),
        },
        runner: NoNativeCalls,
    };
    services.with_record(|_, _| Ok(()))?;
    Ok(services)
}

use crate::{
    lifecycle::{Binding, Origin, Phase, Settings, Snapshot, Startup, Store},
    protected_state::{self, Lease},
};
use festerm_sessiond::private_storage as storage;
use native::{
    CommandRunner, NativeOutput, NativeRequest, NativeRunner, TaskAction, TaskSnapshot,
    OUTPUT_LIMIT,
};
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    io,
    path::{Path, PathBuf},
};

const RECORD: &str = "native-service.json";
const LOCK: &str = "native-service.lock";

fn refused(message: &'static str) -> io::Error {
    io::Error::other(message)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Platform {
    Linux,
    Macos,
    Windows,
}

#[derive(Clone)]
struct Context {
    platform: Platform,
    user: String,
    registration: PathBuf,
}

impl Context {
    fn current() -> io::Result<Self> {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            let user = festerm_unix_security::current_user_id()?.to_string();
            let home = festerm_unix_security::current_user_home()?;
            #[cfg(target_os = "linux")]
            return Ok(Self {
                platform: Platform::Linux,
                user,
                registration: home.join(".config/systemd/user"),
            });
            #[cfg(target_os = "macos")]
            return Ok(Self {
                platform: Platform::Macos,
                user,
                registration: home.join("Library/LaunchAgents"),
            });
        }
        #[cfg(windows)]
        {
            let (user, _) = festerm_windows_security::current_user_service_context()?;
            Ok(Self {
                platform: Platform::Windows,
                user,
                registration: PathBuf::new(),
            })
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
        Err(refused("host.service_unsupported_platform"))
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct WindowsManagedExecution {
    path: PathBuf,
    digest: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Registration {
    version: u8,
    installation: String,
    origin: Origin,
    binding: Binding,
    epoch: u64,
    settings: Settings,
    platform: Platform,
    user: String,
    resource: String,
    executable: PathBuf,
    digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    windows_managed: Option<WindowsManagedExecution>,
    #[serde(default)]
    provenance: Option<PathBuf>,
    state: PathBuf,
    definition: String,
    reservation: String,
    native_expected: Option<String>,
    native_submitted: bool,
    native_snapshot: Option<String>,
    native_security: Option<String>,
    retiring_definition: bool,
    removed: bool,
}

/// Observed local manager state only; no cloud, vault or endpoint availability.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProcessState {
    Absent,
    Stopped,
    Running,
    Failed,
    Transitioning,
}

/// Typed result still requiring endpoint/cloud reconciliation by the controller.
#[derive(Serialize)]
pub struct Observation {
    pub process: ProcessState,
    pub installation: String,
    pub origin: Origin,
    pub epoch: u64,
}

/// Native mutations are serialized separately from the short consent lease:
/// revocation can commit while native startup is in progress.
pub struct Services<'a, R = NativeRunner> {
    store: &'a Store,
    state: PathBuf,
    context: Context,
    runner: R,
}

impl<'a> Services<'a, NativeRunner> {
    pub fn current_user(store: &'a Store, state: &Path) -> io::Result<Self> {
        Self::with_runner(store, state, NativeRunner)
    }

    #[cfg(all(feature = "cloud", feature = "host-runtime"))]
    pub(crate) fn stop_current_user(
        store: &'a Store,
        state: &Path,
        epoch: u64,
    ) -> io::Result<Observation> {
        #[cfg(all(windows, feature = "cloud", feature = "host-runtime"))]
        {
            windows_stop_observation(std::time::Duration::from_secs(10), |deadline| {
                Self::current_user(store, state)?.stop_once(epoch, deadline)
            })
        }
        #[cfg(not(all(windows, feature = "cloud", feature = "host-runtime")))]
        {
            Self::current_user(store, state)?.stop(epoch)
        }
    }
}

impl<'a, R: CommandRunner> Services<'a, R> {
    pub fn with_runner(store: &'a Store, state: &Path, runner: R) -> io::Result<Self> {
        definitions::path_text(state)?;
        let services = Self {
            store,
            state: state.into(),
            context: Context::current()?,
            runner,
        };
        services.with_record(|_, _| Ok(()))?;
        Ok(services)
    }

    fn with_record<T>(
        &self,
        action: impl FnOnce(&File, &Snapshot) -> io::Result<T>,
    ) -> io::Result<T> {
        self.store.with_locked(|directory, snapshot| {
            let opened = storage::private_directory(&self.state, false)?;
            if !trusted::same(directory, &opened)? {
                return Err(refused("host.service_foreign_state_directory"));
            }
            action(directory, snapshot)
        })
    }

    fn lease(&self) -> io::Result<native::OperationGuard> {
        self.with_record(|directory, _| {
            let file = match storage::open_private_file(directory, Path::new(LOCK), false) {
                Ok(file) => file,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    for record in [RECORD, replacement::JOURNAL] {
                        match storage::open_private_file(directory, Path::new(record), false) {
                            Ok(_) => {
                                return Err(refused(
                                    "host.service_lock_missing_reconciliation_required",
                                ))
                            }
                            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                            Err(error) => return Err(error),
                        }
                    }
                    storage::open_private_file(directory, Path::new(LOCK), true)?
                }
                Err(error) => return Err(error),
            };
            Lease::exclusive(file).map(native::OperationGuard::enter)
        })
    }

    fn read_registration(
        &self,
        directory: &File,
        snapshot: &Snapshot,
    ) -> io::Result<Option<Registration>> {
        let bytes = match storage::read_private(directory, Path::new(RECORD), 256 * 1024) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let record: Registration =
            serde_json::from_slice(&bytes).map_err(|_| refused("host.service_invalid_receipt"))?;
        self.validate_registration(&record, snapshot)?;
        Ok(Some(record))
    }

    fn validate_registration(&self, record: &Registration, snapshot: &Snapshot) -> io::Result<()> {
        let mut resource = definitions::resource(snapshot, record.epoch);
        if matches!(record.version, 3 | 5) || (record.version == 6 && record.resource != resource) {
            resource = definitions::replacement_resource(
                snapshot,
                record.epoch,
                &record.reservation,
                record.platform,
            )?;
        }
        if !matches!(record.version, 2..=6)
            || (record.version == 6) != record.windows_managed.is_some()
            || record.windows_managed.as_ref().is_some_and(|worker| {
                record.platform != Platform::Windows
                    || record.origin != Origin::Desktop
                    || worker.path
                        != record
                            .executable
                            .with_file_name("festerm-tunnel-worker.exe")
                    || worker.digest.len() != 64
                    || !worker.digest.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
            || record.installation != snapshot.installation
            || record.origin != snapshot.origin
            || snapshot.binding.as_ref() != Some(&record.binding)
            || record.epoch == 0
            || record.epoch > snapshot.epoch
            || record.resource != resource
            || record.state != self.state
            || record.user != self.context.user
            || record.platform != self.context.platform
            || record.definition != definitions::render(record)?
            || uuid::Uuid::parse_str(&record.reservation).is_err()
            || (record.platform == Platform::Windows
                && (record.native_expected.as_ref().is_none_or(|xml| {
                    !xml.contains(&format!("fesTerm-host-reservation:{}", record.reservation))
                }) || record.native_security.as_deref()
                    != Some(format!("task-owner-system-full-control-v1:{}", record.user).as_str())
                    || record.native_snapshot.as_ref().is_some_and(|xml| {
                        Some(xml) != record.native_expected.as_ref() || !record.native_submitted
                    })))
            || (record.platform != Platform::Windows
                && (record.native_expected.is_some()
                    || record.native_submitted
                    || record.native_security.is_some()
                    || record.native_snapshot.is_some()))
        {
            return Err(refused("host.service_foreign_receipt"));
        }
        Ok(())
    }

    fn registration(&self) -> io::Result<(Snapshot, Option<Registration>)> {
        self.with_record(|directory, snapshot| {
            Ok((
                snapshot.clone(),
                self.read_registration(directory, snapshot)?,
            ))
        })
    }

    fn reconciled_registration(&self) -> io::Result<(Snapshot, Option<Registration>)> {
        let (snapshot, registration) = self.registration()?;
        let Some(mut record) = registration else {
            return Ok((snapshot, None));
        };
        if record.platform == Platform::Windows && record.native_snapshot.is_none() {
            let observed = self.task(&record, TaskAction::Query)?;
            match (&observed.xml, &observed.security) {
                (Some(xml), Some(security))
                    if record.native_submitted
                        && Some(security) == record.native_security.as_ref() =>
                {
                    let before = record.clone();
                    record.native_expected = Some(self.expected_task_xml(&record, xml)?);
                    record.native_snapshot = observed.xml;
                    self.remember_created_task(&before, &record)?;
                }
                (None, None) if !record.native_submitted => {}
                (None, None) => {
                    return Err(refused("host.service_task_registration_outcome_unknown"));
                }
                _ => return Err(refused("host.service_foreign_native_definition")),
            }
        }
        Ok((snapshot, Some(record)))
    }

    fn fence(&self, epoch: u64, phase: Phase) -> io::Result<Snapshot> {
        self.with_record(|_, snapshot| {
            if snapshot.epoch != epoch || snapshot.phase != phase || snapshot.binding.is_none() {
                return Err(refused("host.service_stale_or_unapproved_operation"));
            }
            Ok(snapshot.clone())
        })
    }

    fn unregistered_observation(
        &self,
        expected: &Snapshot,
        _operation: &native::OperationGuard,
    ) -> io::Result<Observation> {
        self.require_no_replacement_history()?;
        self.with_record(|directory, current| {
            if current != expected || self.read_registration(directory, current)?.is_some() {
                return Err(refused(
                    "host.service_operation_superseded_reconcile_owned_resource",
                ));
            }
            // Install publishes a retained receipt under this same native lease
            // before any manager effect. No receipt proves only our own absence.
            Ok(observation(current, ProcessState::Absent))
        })
    }

    fn save(&self, record: &Registration, epoch: u64, phase: Phase) -> io::Result<()> {
        self.with_record(|directory, snapshot| {
            if snapshot.epoch != epoch
                || snapshot.phase != phase
                || snapshot.binding.as_ref() != Some(&record.binding)
            {
                return Err(refused(
                    "host.service_operation_superseded_reconcile_owned_resource",
                ));
            }
            storage::replace_private(
                directory,
                Path::new(RECORD),
                &serde_json::to_vec(record).map_err(|_| refused("host.service_invalid_receipt"))?,
            )
        })
    }

    /// Admit only the native preparation of the recorded reservation. Query
    /// and persisted snapshots remain raw, including Scheduler's descriptor.
    fn expected_task_xml(&self, record: &Registration, observed: &str) -> io::Result<String> {
        let expected = record
            .native_expected
            .as_ref()
            .ok_or_else(|| refused("host.service_task_reservation_unconfirmed"))?;
        if expected == observed {
            return Ok(expected.clone());
        }
        let prepared = self.task(record, TaskAction::Prepare(expected.clone()))?;
        if prepared.xml.as_deref() != Some(observed)
            || prepared.security.as_ref() != record.native_security.as_ref()
        {
            return Err(refused("host.service_foreign_native_definition"));
        }
        Ok(observed.into())
    }

    fn remember_created_task(
        &self,
        before: &Registration,
        record: &Registration,
    ) -> io::Result<()> {
        self.with_record(|directory, snapshot| {
            let mut prior = self
                .read_registration(directory, snapshot)?
                .ok_or_else(|| refused("host.service_not_installed"))?;
            if prior != *before
                || !prior.native_submitted
                || prior.native_snapshot.is_some()
                || record.native_snapshot.is_none()
                || record.native_security.is_none()
                || record.native_expected.as_ref() != record.native_snapshot.as_ref()
                || prior.native_security != record.native_security
            {
                return Err(refused("host.service_foreign_native_definition"));
            }
            prior.native_expected.clone_from(&record.native_expected);
            prior.native_snapshot.clone_from(&record.native_snapshot);
            prior.native_security.clone_from(&record.native_security);
            if prior != *record {
                return Err(refused("host.service_foreign_receipt"));
            }
            // Preserve observed exact ownership even if revocation superseded
            // registration; the final operation fence still refuses success.
            storage::replace_private(
                directory,
                Path::new(RECORD),
                &serde_json::to_vec(record).map_err(|_| refused("host.service_invalid_receipt"))?,
            )
        })
    }

    fn request(&self, request: NativeRequest) -> io::Result<NativeOutput> {
        let result = self.runner.execute(&request)?;
        if result.stdout.len() > OUTPUT_LIMIT {
            return Err(refused("host.service_native_output_limit"));
        }
        Ok(result)
    }

    fn command(&self, args: &[&str]) -> io::Result<NativeOutput> {
        let tool = match self.context.platform {
            Platform::Linux => "/usr/bin/systemctl",
            Platform::Macos => "/bin/launchctl",
            Platform::Windows => return Err(refused("host.service_platform_mismatch")),
        };
        self.request(NativeRequest::Process {
            tool: tool.into(),
            args: args.iter().map(Into::into).collect(),
        })
    }

    fn run(&self, args: &[&str]) -> io::Result<()> {
        if !self.command(args)?.success {
            return Err(refused("host.service_native_operation_failed"));
        }
        Ok(())
    }

    fn task(&self, record: &Registration, action: TaskAction) -> io::Result<TaskSnapshot> {
        let output = self.request(NativeRequest::Task {
            name: record.resource.clone(),
            action,
        })?;
        if !output.success {
            return Err(refused("host.service_task_unavailable"));
        }
        serde_json::from_slice(&output.stdout)
            .map_err(|_| refused("host.service_task_invalid_observation"))
    }

    fn observe(&self, record: &Registration) -> io::Result<ProcessState> {
        let definition = definitions::check_file(&self.context, record)?;
        let process = match self.context.platform {
            Platform::Linux => {
                let output = self.command(&["--user", "--no-pager", "show", &record.resource,
                    "--property=Id,LoadState,FragmentPath,DropInPaths,ActiveState,SubState,MainPID,Result,ExecMainCode,ExecMainStatus"])?;
                let process = definitions::linux_state(
                    &output.stdout,
                    &definitions::definition_path(&self.context, record),
                )?;
                if !output.success
                    && process != ProcessState::Absent
                    && !(record.retiring_definition
                        && process == ProcessState::Failed
                        && output.code == 1
                        && definitions::linux_missing_unit(&output.stdout)?)
                {
                    return Err(refused("host.service_observation_unavailable"));
                }
                Ok(process)
            }
            Platform::Macos => {
                self.run(&["print-disabled", &format!("gui/{}", record.user)])?;
                let output =
                    self.command(&["print", &format!("gui/{}/{}", record.user, record.resource)])?;
                if !output.success {
                    if output.code == 113 {
                        return Ok(ProcessState::Absent);
                    }
                    return Err(refused("host.service_launchd_observation_unavailable"));
                }
                definitions::mac_state(
                    &output.stdout,
                    &definitions::definition_path(&self.context, record),
                    record,
                )
            }
            Platform::Windows => {
                let observed = self.task(record, TaskAction::Query)?;
                match (
                    &record.native_snapshot,
                    &record.native_security,
                    &observed.xml,
                    &observed.security,
                ) {
                    (None, _, None, None) if record.native_submitted => {
                        Err(refused("host.service_task_registration_outcome_unknown"))
                    }
                    (_, _, None, None) => Ok(ProcessState::Absent),
                    (Some(expected), Some(security), Some(actual), Some(actual_security))
                        if expected == actual && security == actual_security =>
                    {
                        match observed.state {
                            4 => Ok(ProcessState::Running),
                            2 => Ok(ProcessState::Transitioning),
                            1 => Ok(ProcessState::Stopped),
                            3 if matches!(observed.last_exit, 0 | 0x41303) => {
                                Ok(ProcessState::Stopped)
                            }
                            3 => Ok(ProcessState::Failed),
                            _ => Err(refused("host.service_observation_unqualified")),
                        }
                    }
                    _ => Err(refused("host.service_foreign_native_definition")),
                }
            }
        }?;
        if !definition
            && process != ProcessState::Absent
            && !(self.context.platform == Platform::Linux && record.retiring_definition)
        {
            return Err(refused("host.service_foreign_native_definition"));
        }
        Ok(process)
    }

    pub fn status(&self) -> io::Result<Observation> {
        let _lease = self.lease()?;
        self.require_replacement_settled()?;
        let (snapshot, registration) = self.reconciled_registration()?;
        let process = match registration {
            Some(record) => self.observe(&record)?,
            None => return Err(refused("host.service_not_installed_no_native_observation")),
        };
        Ok(observation(&snapshot, process))
    }

    /// Recover exact release authority for run after a service-manager restart.
    /// Older receipt-only registrations remain removable, not executable trust.
    pub fn reopen_stage(&self, epoch: u64) -> io::Result<TrustedStage> {
        self.reopen_stage_in_phase(epoch, Phase::Provisioned)
    }

    /// Local explicit profile launch may use retained authenticated helpers
    /// while remoting is disabled. This never registers or starts the HOST.
    pub fn reopen_local_profile_stage(&self, epoch: u64) -> io::Result<TrustedStage> {
        let snapshot = self.store.snapshot()?;
        if !matches!(snapshot.phase, Phase::Disabled | Phase::Provisioned) {
            return Err(refused("host.local_profile_reconciliation_required"));
        }
        self.reopen_stage_in_phase(epoch, snapshot.phase)
    }

    fn reopen_stage_in_phase(&self, epoch: u64, phase: Phase) -> io::Result<TrustedStage> {
        let guard = self.lease()?;
        self.reopen_stage_admitted(epoch, phase, &guard)
    }

    fn reopen_stage_admitted(
        &self,
        epoch: u64,
        phase: Phase,
        _guard: &native::OperationGuard,
    ) -> io::Result<TrustedStage> {
        self.require_replacement_settled()?;
        let snapshot = self.fence(epoch, phase)?;
        let (current, record) = self.reconciled_registration()?;
        if current != snapshot {
            return Err(refused("host.service_stale_or_unapproved_operation"));
        }
        let record = record.ok_or_else(|| refused("host.service_not_installed"))?;
        if record.removed {
            return Err(refused("host.service_not_installed"));
        }
        if record.settings != snapshot.settings {
            return Err(refused("host.service_explicit_replacement_required"));
        }
        let provenance = record
            .provenance
            .as_ref()
            .ok_or_else(|| refused("host.service_signed_provenance_required"))?;
        let root = record
            .executable
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| refused("host.service_executable_path"))?;
        let stage = TrustedStage::reopen_release_cache(provenance, root)?;
        if stage.path != record.executable || stage.digest != record.digest {
            return Err(refused("host.service_authenticated_bundle_changed"));
        }
        self.fence(epoch, phase)?;
        stage.revalidate_registration(&record)?;
        Ok(stage)
    }

    /// Registers the exact definition. Linux UserSignIn also activates the
    /// guarded launcher; Manual registration never starts a process.
    pub fn install(&self, stage: &TrustedStage, epoch: u64) -> io::Result<Observation> {
        self.install_policy(stage, epoch, false)
    }

    /// New registrations gain native crash recovery; existing receipts keep their policy.
    pub fn install_with_recovery(
        &self,
        stage: &TrustedStage,
        epoch: u64,
    ) -> io::Result<Observation> {
        self.install_policy(stage, epoch, true)
    }

    fn install_policy(
        &self,
        stage: &TrustedStage,
        epoch: u64,
        recovery: bool,
    ) -> io::Result<Observation> {
        if self.context.platform == Platform::Windows
            && !cfg!(all(feature = "cloud", feature = "host-runtime"))
        {
            return Err(refused(
                "host.service_windows_authenticated_graceful_stop_unavailable",
            ));
        }
        if recovery {
            self.install_reserved_policy(stage, epoch, true)
        } else {
            self.install_reserved(stage, epoch)
        }
    }

    fn install_reserved(&self, stage: &TrustedStage, epoch: u64) -> io::Result<Observation> {
        self.install_reserved_policy(stage, epoch, false)
    }

    fn install_reserved_policy(
        &self,
        stage: &TrustedStage,
        epoch: u64,
        recovery: bool,
    ) -> io::Result<Observation> {
        let _lease = self.lease()?;
        self.require_replacement_settled()?;
        let snapshot = self.fence(epoch, Phase::Provisioned)?;
        stage.revalidate()?;
        let (_, prior) = self.reconciled_registration()?;
        let new = prior.is_none();
        if new && recovery && stage.version() != crate::VERSION {
            return Err(refused("host.native_recovery_current_helpers_required"));
        }
        let mut record = if let Some(record) = prior {
            if record.executable != stage.path
                || record.digest != stage.digest
                || record.removed
                || record.settings != snapshot.settings
            {
                return Err(refused("host.service_explicit_replacement_required"));
            }
            stage.revalidate_registration(&record)?;
            record
        } else {
            let mut record = Registration {
                version: if recovery { 4 } else { 2 },
                installation: snapshot.installation.clone(),
                origin: snapshot.origin,
                binding: snapshot
                    .binding
                    .clone()
                    .ok_or_else(|| refused("host.service_binding_required"))?,
                epoch,
                settings: snapshot.settings.clone(),
                platform: self.context.platform,
                user: self.context.user.clone(),
                resource: definitions::resource(&snapshot, epoch),
                executable: stage.path.clone(),
                digest: stage.digest.clone(),
                windows_managed: None,
                provenance: Some(stage.provenance_path().to_owned()),
                state: self.state.clone(),
                definition: String::new(),
                reservation: uuid::Uuid::new_v4().to_string(),
                native_expected: None,
                native_submitted: false,
                native_snapshot: None,
                native_security: None,
                retiring_definition: false,
                removed: false,
            };
            if recovery
                && self.context.platform == Platform::Windows
                && snapshot.origin == Origin::Desktop
            {
                record.version = 6;
                record.windows_managed = Some(stage.windows_managed_execution()?);
            }
            record.definition = definitions::render(&record)?;
            if self.context.platform == Platform::Windows {
                let prepared =
                    self.task(&record, TaskAction::Prepare(record.definition.clone()))?;
                record.native_expected = Some(
                    prepared
                        .xml
                        .ok_or_else(|| refused("host.service_task_reservation_unconfirmed"))?,
                );
                record.native_security = Some(
                    prepared
                        .security
                        .ok_or_else(|| refused("host.service_task_reservation_unconfirmed"))?,
                );
            }
            self.with_record(|directory, current| {
                if current != &snapshot {
                    return Err(refused("host.service_stale_or_unapproved_operation"));
                }
                protected_state::publish_new(
                    directory,
                    Path::new(RECORD),
                    &serde_json::to_vec(&record)
                        .map_err(|_| refused("host.service_invalid_receipt"))?,
                )
            })?;
            record
        };
        self.fence(epoch, Phase::Provisioned)?;
        if new && self.observe(&record)? != ProcessState::Absent {
            return Err(refused("host.service_foreign_native_definition"));
        }
        match self.context.platform {
            Platform::Linux => {
                definitions::publish_file(&self.context, &record)?;
                self.fence(epoch, Phase::Provisioned)?;
                self.run(&["--user", "daemon-reload"])?;
                if snapshot.settings.startup == Startup::UserSignIn {
                    self.fence(epoch, Phase::Provisioned)?;
                    self.run(&["--user", "enable", "--now", &record.resource])?;
                }
            }
            Platform::Macos => {
                self.run(&["print-disabled", &format!("gui/{}", record.user)])?;
                definitions::publish_file(&self.context, &record)?;
            }
            Platform::Windows => {
                let current = self.task(&record, TaskAction::Query)?;
                if current.xml.is_some() {
                    if record.native_snapshot.as_ref() != current.xml.as_ref()
                        || record.native_security.as_ref() != current.security.as_ref()
                    {
                        return Err(refused("host.service_foreign_native_definition"));
                    }
                } else {
                    self.fence(epoch, Phase::Provisioned)?;
                    if record.native_submitted {
                        return Err(refused("host.service_task_registration_outcome_unknown"));
                    }
                    record.native_submitted = true;
                    self.save(&record, epoch, Phase::Provisioned)?;
                    self.fence(epoch, Phase::Provisioned)?;
                    let before = record.clone();
                    let created =
                        self.task(
                            &record,
                            TaskAction::Create(record.native_expected.clone().ok_or_else(
                                || refused("host.service_task_reservation_unconfirmed"),
                            )?),
                        )?;
                    record.native_snapshot = Some(
                        created
                            .xml
                            .ok_or_else(|| refused("host.service_task_creation_unconfirmed"))?,
                    );
                    record.native_security = Some(
                        created
                            .security
                            .ok_or_else(|| refused("host.service_task_creation_unconfirmed"))?,
                    );
                    self.remember_created_task(&before, &record)?;
                }
            }
        }
        self.fence(epoch, Phase::Provisioned)?;
        let process = self.observe(&record)?;
        let registered = if self.context.platform == Platform::Macos {
            definitions::check_file(&self.context, &record)?
        } else {
            process != ProcessState::Absent
        };
        if !registered {
            return Err(refused("host.service_registration_not_observed"));
        }
        Ok(observation(&snapshot, process))
    }

    pub(crate) fn require_recovery_registration(&self) -> io::Result<Option<TrustedStage>> {
        self.require_managed_registration(false)
    }

    pub(crate) fn require_windows_worker_registration(&self) -> io::Result<Option<TrustedStage>> {
        self.require_managed_registration(true)
    }

    fn require_managed_registration(
        &self,
        worker_required: bool,
    ) -> io::Result<Option<TrustedStage>> {
        let (snapshot, record) = self.registration()?;
        let record = record
            .filter(|record| matches!(record.version, 4..=6) && !record.removed)
            .ok_or_else(|| refused("host.native_recovery_registration_required"))?;
        if worker_required && record.version != 6 {
            return Err(refused("host.windows_managed_worker_registration_required"));
        }
        if record.version == 6 {
            let stage = self.reopen_stage(snapshot.epoch)?;
            stage.require_running_worker(&record, &std::env::current_exe()?)?;
            Ok(Some(stage))
        } else {
            Ok(None)
        }
    }

    pub(crate) fn require_runtime_execution(&self, stage: &TrustedStage) -> io::Result<()> {
        let (_, record) = self.registration()?;
        let record = record.ok_or_else(|| refused("host.service_not_installed"))?;
        stage.revalidate_registration(&record)?;
        if record.version == 6 {
            stage.require_running_worker(&record, &std::env::current_exe()?)?;
        }
        Ok(())
    }

    /// Explicit start requires current protected native-vault policy. A locked
    /// vault propagates its actionable error; it never triggers sign-in/fallback.
    pub fn start(&self, stage: &TrustedStage, epoch: u64) -> io::Result<Observation> {
        #[cfg(all(feature = "cloud", feature = "host-runtime"))]
        {
            let repeated = std::cell::Cell::new(false);
            self.start_with(stage, epoch, || {
                self.require_start_credentials_observed(epoch, repeated.replace(true))
            })
        }
        #[cfg(not(all(feature = "cloud", feature = "host-runtime")))]
        self.start_with(stage, epoch, || self.require_start_credentials())
    }

    #[cfg(all(feature = "cloud", feature = "host-runtime"))]
    fn require_start_credentials_observed(&self, epoch: u64, repeated: bool) -> io::Result<()> {
        self.require_start_credentials_observed_with(epoch, repeated, || {
            self.store.owner_enrollment()
        })
    }

    #[cfg(all(feature = "cloud", feature = "host-runtime"))]
    fn require_start_credentials_observed_with(
        &self,
        epoch: u64,
        repeated: bool,
        open: impl FnOnce() -> io::Result<crate::enrollment::OwnerEnrollment<'a>>,
    ) -> io::Result<()> {
        use crate::enrollment::{
            observe_owner_result, OwnerAdmissionEvent, OwnerAdmissionStage as Stage,
        };
        let mut observe = |event| self.start_owner_event(epoch, repeated, false, event);
        observe(OwnerAdmissionEvent::Begin(Stage::StartCredentials));
        let result = (|| {
            let enrollment = observe_owner_result(Stage::StoreOpen, &mut observe, open)?;
            self.require_start_credentials_with_observed(&enrollment, &mut |event| {
                self.start_owner_event(epoch, repeated, true, event)
            })
        })();
        observe(match &result {
            Ok(_) => OwnerAdmissionEvent::Pass(Stage::StartCredentials),
            Err(error) => OwnerAdmissionEvent::Fail(
                Stage::StartCredentials,
                crate::enrollment::OwnerAdmissionFailure::from_io(error),
            ),
        });
        result
    }

    #[cfg(all(feature = "cloud", feature = "host-runtime"))]
    fn start_owner_event(
        &self,
        epoch: u64,
        repeated: bool,
        noninteractive: bool,
        event: crate::enrollment::OwnerAdmissionEvent,
    ) {
        use crate::enrollment::OwnerAdmissionEvent;
        let (stage, outcome, category) = match event {
            OwnerAdmissionEvent::Begin(stage) => (stage, "begin", "none"),
            OwnerAdmissionEvent::Pass(stage) => (stage, "ok", "none"),
            OwnerAdmissionEvent::Fail(stage, reason) => (stage, "error", reason.code()),
        };
        festerm_tunnel_bridge::worker_log::WorkerLog::new(&self.state).event(
            if noninteractive {
                "host.owner_start_noninteractive"
            } else {
                "host.start_credentials"
            },
            &[
                ("epoch", &epoch.to_string()),
                (
                    "check",
                    if repeated {
                        "before_mutation"
                    } else {
                        "initial"
                    },
                ),
                ("stage", stage.code()),
                ("outcome", outcome),
                ("category", category),
            ],
        );
    }

    #[cfg(not(all(feature = "cloud", feature = "host-runtime")))]
    fn require_start_credentials(&self) -> io::Result<()> {
        let enrollment = self.store.owner_enrollment()?;
        self.require_start_credentials_with(&enrollment)
    }

    #[cfg(any(all(test, unix), not(all(feature = "cloud", feature = "host-runtime"))))]
    fn require_start_credentials_with(
        &self,
        enrollment: &crate::enrollment::OwnerEnrollment<'_>,
    ) -> io::Result<()> {
        self.require_start_credentials_with_observed(enrollment, &mut |_| {})
    }

    fn require_start_credentials_with_observed(
        &self,
        enrollment: &crate::enrollment::OwnerEnrollment<'_>,
        observe: &mut dyn FnMut(crate::enrollment::OwnerAdmissionEvent),
    ) -> io::Result<()> {
        let expected = self.store.snapshot()?;
        if expected.settings.ordinary_access == crate::lifecycle::OrdinaryAccess::ProviderOwner {
            #[cfg(all(feature = "cloud", feature = "host-runtime"))]
            let current = enrollment.current_snapshot_noninteractive_observed(observe)?;
            #[cfg(not(all(feature = "cloud", feature = "host-runtime")))]
            let current = {
                let _ = observe;
                enrollment.current_snapshot_noninteractive()?
            };
            if current.installation() != &expected || expected.phase != Phase::Provisioned {
                return Err(refused("host.runtime_consent_changed"));
            }
            // The provider role is authorized separately. An authenticated
            // installation binding is required, never an invented admin key.
        } else {
            let _guard = enrollment.current_policy()?;
        }
        Ok(())
    }

    fn start_with(
        &self,
        stage: &TrustedStage,
        epoch: u64,
        credentials: impl Fn() -> io::Result<()>,
    ) -> io::Result<Observation> {
        self.start_checked(stage, epoch, credentials, || Ok(()))
    }

    #[cfg(all(target_os = "macos", feature = "cloud", feature = "host-runtime"))]
    pub(crate) fn start_from_desktop_parent(
        &self,
        epoch: u64,
        check: impl Fn() -> io::Result<()>,
    ) -> io::Result<Observation> {
        check()?;
        // One existing native operation covers publisher/self admission AND
        // actual start. Every early error drains its captured workers before
        // this dedicated process may exit; no nested lease or deadline reset.
        let guard =
            observe_start(&self.state, StartStage::ServiceLease, self.lease())?.drain_before_drop();
        let stage = self.reopen_stage_admitted(epoch, Phase::Provisioned, &guard)?;
        stage.require_current_executable()?;
        let repeated = std::cell::Cell::new(false);
        self.start_admitted(
            &stage,
            epoch,
            || self.require_start_credentials_observed(epoch, repeated.replace(true)),
            check,
            guard,
        )
    }

    fn start_checked(
        &self,
        stage: &TrustedStage,
        epoch: u64,
        credentials: impl Fn() -> io::Result<()>,
        check: impl Fn() -> io::Result<()>,
    ) -> io::Result<Observation> {
        if self.context.platform == Platform::Windows
            && !cfg!(all(feature = "cloud", feature = "host-runtime"))
        {
            return Err(refused(
                "host.service_windows_authenticated_graceful_stop_unavailable",
            ));
        }
        check()?;
        let guard = observe_start(&self.state, StartStage::ServiceLease, self.lease())?;
        self.start_admitted(stage, epoch, credentials, check, guard)
    }

    fn start_admitted(
        &self,
        stage: &TrustedStage,
        epoch: u64,
        credentials: impl Fn() -> io::Result<()>,
        check: impl Fn() -> io::Result<()>,
        guard: native::OperationGuard,
    ) -> io::Result<Observation> {
        self.require_replacement_settled()?;
        let snapshot = observe_start(
            &self.state,
            StartStage::InitialFence,
            self.fence(epoch, Phase::Provisioned),
        )?;
        credentials()?;
        check()?;
        stage.revalidate()?;
        let (_, record) = observe_start(
            &self.state,
            StartStage::RegistrationRead,
            self.reconciled_registration(),
        )?;
        let record = record.ok_or_else(|| refused("host.service_not_installed"))?;
        if record.executable != stage.path
            || record.digest != stage.digest
            || record.settings != snapshot.settings
            || record.removed
        {
            return Err(refused("host.service_explicit_replacement_required"));
        }
        stage.revalidate_registration(&record)?;
        let before = observe_start(
            &self.state,
            StartStage::BeforeObservation,
            self.observe(&record),
        )?;
        if before == ProcessState::Transitioning {
            observe_start(
                &self.state,
                StartStage::TransitioningFinalFence,
                self.fence(epoch, Phase::Provisioned),
            )?;
            return Ok(observation(&snapshot, before));
        }
        if before != ProcessState::Running {
            observe_start(
                &self.state,
                StartStage::BeforeManagerFence,
                self.fence(epoch, Phase::Provisioned),
            )?;
            credentials()?;
            check()?;
            check()?;
            match self.context.platform {
                Platform::Linux => observe_start(
                    &self.state,
                    StartStage::Manager,
                    self.run(&["--user", "start", &record.resource]),
                )?,
                Platform::Macos => {
                    if before == ProcessState::Absent {
                        let path = definitions::definition_path(&self.context, &record);
                        observe_start(
                            &self.state,
                            StartStage::Manager,
                            self.run(&[
                                "bootstrap",
                                &format!("gui/{}", record.user),
                                definitions::path_text(&path)?,
                            ]),
                        )?;
                    }
                    observe_start(
                        &self.state,
                        StartStage::BeforeManagerFence,
                        self.fence(epoch, Phase::Provisioned),
                    )?;
                    check()?;
                    observe_start(
                        &self.state,
                        StartStage::Manager,
                        self.run(&[
                            "kickstart",
                            &format!("gui/{}/{}", record.user, record.resource),
                        ]),
                    )?;
                }
                Platform::Windows => {
                    observe_start(
                        &self.state,
                        StartStage::Manager,
                        self.task(&record, TaskAction::Start),
                    )?;
                }
            }
        }
        observe_start(
            &self.state,
            StartStage::AfterManagerFence,
            self.start_observation_fence(&snapshot, &guard, &check),
        )?;
        let mut process = observe_start(
            &self.state,
            StartStage::AfterObservation,
            self.observe(&record),
        )?;
        for _ in 0..2 {
            if process == ProcessState::Running || process == ProcessState::Failed {
                break;
            }
            observe_start(
                &self.state,
                StartStage::AfterManagerFence,
                self.start_observation_fence(&snapshot, &guard, &check),
            )?;
            std::thread::sleep(std::time::Duration::from_millis(25));
            process = observe_start(
                &self.state,
                StartStage::AfterObservation,
                self.observe(&record),
            )?;
        }
        observe_start(
            &self.state,
            StartStage::FinalFence,
            self.start_observation_fence(&snapshot, &guard, &check),
        )?;
        if process == ProcessState::Transitioning {
            return Ok(observation(&snapshot, process));
        }
        if process != ProcessState::Running {
            return Err(refused("host.service_start_not_observed_running"));
        }
        check()?;
        Ok(observation(&snapshot, process))
    }

    // Starting or observing the manager can overlap its protected journal writes.
    // Retain the original operation deadline; wait only for a fresh matching
    // post-manager snapshot, without replaying manager or credential work.
    fn start_observation_fence(
        &self,
        expected: &Snapshot,
        guard: &native::OperationGuard,
        check: &impl Fn() -> io::Result<()>,
    ) -> io::Result<Snapshot> {
        let mut last_busy = None;
        loop {
            observe_start(&self.state, StartStage::ObservationCallback, check())?;
            if let Some(error) = last_busy.take() {
                if guard.remaining().is_err() {
                    return observe_start(
                        &self.state,
                        StartStage::ObservationBudgetExhausted,
                        Err(error),
                    );
                }
            }
            let result = self
                .fence(expected.epoch, Phase::Provisioned)
                .and_then(|current| {
                    if &current != expected {
                        return Err(refused("host.service_stale_or_unapproved_operation"));
                    }
                    Ok(current)
                });
            match result {
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    let Ok(remaining) = guard.remaining() else {
                        return observe_start(
                            &self.state,
                            StartStage::ObservationBudgetExhausted,
                            Err(error),
                        );
                    };
                    std::thread::sleep(remaining.min(std::time::Duration::from_millis(25)));
                    if guard.remaining().is_err() {
                        return observe_start(
                            &self.state,
                            StartStage::ObservationBudgetExhausted,
                            Err(error),
                        );
                    }
                    last_busy = Some(error);
                }
                result => return result,
            }
        }
    }

    pub fn stop(&self, epoch: u64) -> io::Result<Observation> {
        #[cfg(all(feature = "cloud", feature = "host-runtime"))]
        if self.context.platform == Platform::Windows {
            return self.stop_windows_revocation(epoch, std::time::Duration::from_secs(10));
        }
        self.stop_once(
            epoch,
            std::time::Instant::now() + std::time::Duration::from_secs(10),
        )
    }

    #[cfg(all(feature = "cloud", feature = "host-runtime"))]
    fn stop_windows_revocation(
        &self,
        epoch: u64,
        wait: std::time::Duration,
    ) -> io::Result<Observation> {
        windows_stop_observation(wait, |deadline| self.stop_once(epoch, deadline))
    }

    fn stop_once(&self, epoch: u64, deadline: std::time::Instant) -> io::Result<Observation> {
        let _lease = self.lease()?;
        let snapshot = self.fence(epoch, Phase::StopRequested)?;
        #[cfg(all(feature = "cloud", feature = "host-runtime"))]
        if self.context.platform == Platform::Windows {
            _lease.limit_until(deadline);
        }
        if let Some(result) = self.stop_replacement(
            epoch,
            deadline.saturating_duration_since(std::time::Instant::now()),
        )? {
            return Ok(result);
        }
        let (_, record) = self.reconciled_registration()?;
        let Some(record) = record else {
            return self.unregistered_observation(&snapshot, &_lease);
        };
        #[cfg(all(feature = "cloud", feature = "host-runtime"))]
        if self.context.platform == Platform::Windows {
            return self.wait_windows_revocation(
                &snapshot,
                &record,
                deadline.saturating_duration_since(std::time::Instant::now()),
            );
        }
        let state = self.observe(&record)?;
        if matches!(state, ProcessState::Running | ProcessState::Transitioning) {
            self.fence(epoch, Phase::StopRequested)?;
            match self.context.platform {
                Platform::Linux => self.run(&["--user", "stop", &record.resource])?,
                Platform::Macos => {
                    // This exact job can become absent after the Running
                    // observation but before launchctl bootout.
                    // Execution errors remain uncertain; only a completed
                    // nonzero command permits this read-only absence proof.
                    let output = self.command(&[
                        "bootout",
                        &format!("gui/{}/{}", record.user, record.resource),
                    ])?;
                    if !output.success {
                        let original = refused("host.service_native_operation_failed");
                        if output.code <= 0 {
                            // A signal or inconsistent exit is not a completed
                            // launchctl refusal whose outcome can be reconciled.
                            return Err(original);
                        }
                        let absent =
                            self.macos_stop_observation(&snapshot, &record, deadline, true);
                        if absent.is_ok() {
                            return Ok(observation(&snapshot, ProcessState::Absent));
                        }
                        return Err(original);
                    }
                }
                Platform::Windows => {
                    return Err(refused(
                        "host.service_windows_authenticated_graceful_stop_unavailable",
                    ));
                }
            }
        }
        if self.context.platform == Platform::Macos {
            return self.macos_stop_observation(&snapshot, &record, deadline, false);
        }
        self.fence(epoch, Phase::StopRequested)?;
        let process = self.observe(&record)?;
        if !matches!(
            process,
            ProcessState::Stopped | ProcessState::Failed | ProcessState::Absent
        ) {
            return Err(refused("host.service_stop_not_observed"));
        }
        Ok(observation(&snapshot, process))
    }

    // The manager mutation is outside this loop. Retain its operation lease,
    // captured consent and registration; retry only fresh read-only checks.
    fn macos_stop_observation(
        &self,
        snapshot: &Snapshot,
        record: &Registration,
        deadline: std::time::Instant,
        absent_only: bool,
    ) -> io::Result<Observation> {
        let owned = || {
            self.with_record(|directory, current| {
                if current != snapshot
                    || self.read_registration(directory, current)?.as_ref() != Some(record)
                    || !definitions::check_file(&self.context, record)?
                {
                    return Err(refused("host.service_stop_not_observed"));
                }
                Ok(())
            })
        };
        loop {
            let result = (|| {
                owned()?;
                let process = self.observe(record)?;
                if (absent_only && process != ProcessState::Absent)
                    || !matches!(
                        process,
                        ProcessState::Stopped | ProcessState::Failed | ProcessState::Absent
                    )
                {
                    return Err(refused("host.service_stop_not_observed"));
                }
                owned()?;
                Ok(observation(snapshot, process))
            })();
            match result {
                Err(error)
                    if error.kind() == io::ErrorKind::WouldBlock
                        || (error.kind() == io::ErrorKind::Other
                            && error.to_string() == "host.service_observation_unqualified") =>
                {
                    // launchd may return a definition whose state cannot yet
                    // qualify the stop. Never accept it as stopped: obtain a
                    // new read under the same deadline and authority checks.
                    #[cfg(test)]
                    if error.kind() == io::ErrorKind::WouldBlock {
                        tests::macos_stop_busy_observed();
                    }
                    let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                    if remaining.is_zero() {
                        return Err(error);
                    }
                    std::thread::sleep(remaining.min(std::time::Duration::from_millis(25)));
                    if std::time::Instant::now() >= deadline {
                        return Err(error);
                    }
                }
                result => return result,
            }
        }
    }

    #[cfg(all(feature = "cloud", feature = "host-runtime"))]
    fn wait_windows_revocation(
        &self,
        snapshot: &Snapshot,
        record: &Registration,
        wait: std::time::Duration,
    ) -> io::Result<Observation> {
        self.wait_windows_resources_revocation(snapshot, &[record], wait)
    }

    #[cfg(all(feature = "cloud", feature = "host-runtime"))]
    fn wait_windows_resources_revocation(
        &self,
        snapshot: &Snapshot,
        records: &[&Registration],
        wait: std::time::Duration,
    ) -> io::Result<Observation> {
        let deadline = std::time::Instant::now() + wait;
        loop {
            self.fence(snapshot.epoch, Phase::StopRequested)?;
            let released = crate::controller::revoked_runtime_released(self.store, snapshot)?;
            let mut process = ProcessState::Absent;
            let mut quiescent = true;
            for record in records {
                let observed = self.observe(record)?;
                quiescent &= matches!(
                    observed,
                    ProcessState::Stopped | ProcessState::Failed | ProcessState::Absent
                );
                if observed != ProcessState::Absent {
                    process = observed;
                }
            }
            self.fence(snapshot.epoch, Phase::StopRequested)?;
            if released && quiescent {
                return Ok(observation(snapshot, process));
            }
            if std::time::Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "host.runtime_stop_pending",
                ));
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
    }

    /// Local service cleanup only. Does not remove cloud resources, identity,
    /// enrollment/vault state, host artifacts or independent daemon processes.
    pub fn cleanup(&self, epoch: u64) -> io::Result<Observation> {
        let _lease = self.lease()?;
        let snapshot = self.fence(epoch, Phase::CleanupPending)?;
        if let Some(result) = self.cleanup_replacement(epoch)? {
            return Ok(result);
        }
        let (_, record) = self.reconciled_registration()?;
        let Some(mut record) = record else {
            return self.unregistered_observation(&snapshot, &_lease);
        };
        let before = self.observe(&record)?;
        if record.removed {
            if before != ProcessState::Absent
                || (self.context.platform != Platform::Windows
                    && definitions::check_file(&self.context, &record)?)
            {
                return Err(refused("host.service_removed_resource_reappeared"));
            }
            self.fence(epoch, Phase::CleanupPending)?;
            return Ok(observation(&snapshot, ProcessState::Absent));
        }
        if !matches!(
            before,
            ProcessState::Stopped | ProcessState::Failed | ProcessState::Absent
        ) {
            return Err(refused("host.service_stop_before_cleanup"));
        }
        self.fence(epoch, Phase::CleanupPending)?;
        match self.context.platform {
            Platform::Linux => {
                if definitions::check_file(&self.context, &record)? {
                    self.run(&["--user", "disable", "--no-reload", &record.resource])?;
                } else if !record.retiring_definition {
                    return Err(refused(
                        "host.service_definition_removed_without_owned_intent",
                    ));
                }
                self.fence(epoch, Phase::CleanupPending)?;
                record.retiring_definition = true;
                self.save(&record, epoch, Phase::CleanupPending)?;
                definitions::remove_file(&self.context, &record)?;
                self.fence(epoch, Phase::CleanupPending)?;
                self.run(&["--user", "daemon-reload"])?;
                if self.observe(&record)? == ProcessState::Failed {
                    self.fence(epoch, Phase::CleanupPending)?;
                    self.run(&["--user", "reset-failed", &record.resource])?;
                }
            }
            Platform::Macos => {
                if before != ProcessState::Absent {
                    self.run(&[
                        "bootout",
                        &format!("gui/{}/{}", record.user, record.resource),
                    ])?;
                }
                self.fence(epoch, Phase::CleanupPending)?;
                definitions::remove_file(&self.context, &record)?;
            }
            Platform::Windows => {
                if before != ProcessState::Absent {
                    self.task(&record, TaskAction::Remove)?;
                }
            }
        }
        if self.observe(&record)? != ProcessState::Absent {
            return Err(refused("host.service_cleanup_not_observed_absent"));
        }
        record.removed = true;
        self.save(&record, epoch, Phase::CleanupPending)?;
        Ok(observation(&snapshot, ProcessState::Absent))
    }
}

#[cfg(all(feature = "cloud", feature = "host-runtime"))]
fn windows_stop_observation(
    wait: std::time::Duration,
    mut read: impl FnMut(std::time::Instant) -> io::Result<Observation>,
) -> io::Result<Observation> {
    let deadline = std::time::Instant::now() + wait;
    let mut attempted = false;
    loop {
        if attempted && std::time::Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "host.runtime_stop_pending",
            ));
        }
        attempted = true;
        // Revocation is already durable. Retry same-epoch observations and
        // existing fenced receipt reconciliation, never manager mutations.
        match read(deadline) {
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                if remaining.is_zero() {
                    return Err(io::Error::new(
                        io::ErrorKind::WouldBlock,
                        "host.runtime_stop_pending",
                    ));
                }
                std::thread::sleep(remaining.min(std::time::Duration::from_millis(25)));
            }
            result => return result,
        }
    }
}

fn observation(snapshot: &Snapshot, process: ProcessState) -> Observation {
    Observation {
        installation: snapshot.installation.clone(),
        origin: snapshot.origin,
        epoch: snapshot.epoch,
        process,
    }
}

#[cfg(test)]
mod tests;
