//! Same-user native host control. Process observations are never host readiness.
//! CLI management never treats a native process observation as endpoint readiness.
mod definitions;
pub mod native;
mod trusted;
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
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
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
                    match storage::open_private_file(directory, Path::new(RECORD), false) {
                        Ok(_) => {
                            return Err(refused(
                                "host.service_lock_missing_reconciliation_required",
                            ))
                        }
                        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                        Err(error) => return Err(error),
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
        let resource = definitions::resource(snapshot, record.epoch);
        if record.version != 2
            || record.installation != snapshot.installation
            || record.origin != snapshot.origin
            || snapshot.binding.as_ref() != Some(&record.binding)
            || record.epoch == 0
            || record.epoch > snapshot.epoch
            || record.resource != resource
            || record.state != self.state
            || record.user != self.context.user
            || record.platform != self.context.platform
            || record.definition != definitions::render(&record)?
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
        Ok(Some(record))
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
                        && Some(xml) == record.native_expected.as_ref()
                        && Some(security) == record.native_security.as_ref() =>
                {
                    record.native_snapshot = observed.xml;
                    self.remember_created_task(&record)?;
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

    fn remember_created_task(&self, record: &Registration) -> io::Result<()> {
        self.with_record(|directory, snapshot| {
            let mut prior = self
                .read_registration(directory, snapshot)?
                .ok_or_else(|| refused("host.service_not_installed"))?;
            if prior.native_snapshot.is_some()
                || record.native_snapshot.is_none()
                || record.native_security.is_none()
                || prior.native_expected.as_ref() != record.native_snapshot.as_ref()
                || prior.native_security != record.native_security
            {
                return Err(refused("host.service_foreign_native_definition"));
            }
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
        let _lease = self.lease()?;
        let snapshot = self.fence(epoch, Phase::Provisioned)?;
        let (current, record) = self.reconciled_registration()?;
        if current != snapshot {
            return Err(refused("host.service_stale_or_unapproved_operation"));
        }
        let record = record.ok_or_else(|| refused("host.service_not_installed"))?;
        if record.removed {
            return Err(refused("host.service_not_installed"));
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
        self.fence(epoch, Phase::Provisioned)?;
        stage.revalidate()?;
        Ok(stage)
    }

    /// Registers only; never starts the incomplete host endpoint.
    pub fn install(&self, stage: &TrustedStage, epoch: u64) -> io::Result<Observation> {
        if self.context.platform == Platform::Windows {
            return Err(refused(
                "host.service_windows_authenticated_graceful_stop_unavailable",
            ));
        }
        self.install_reserved(stage, epoch)
    }

    fn install_reserved(&self, stage: &TrustedStage, epoch: u64) -> io::Result<Observation> {
        let _lease = self.lease()?;
        let snapshot = self.fence(epoch, Phase::Provisioned)?;
        stage.revalidate()?;
        let (_, prior) = self.reconciled_registration()?;
        let new = prior.is_none();
        let mut record = if let Some(record) = prior {
            if record.executable != stage.path
                || record.digest != stage.digest
                || record.removed
                || record.settings != snapshot.settings
            {
                return Err(refused("host.service_explicit_replacement_required"));
            }
            record
        } else {
            let mut record = Registration {
                version: 2,
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
                    self.run(&["--user", "enable", &record.resource])?;
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
                    self.remember_created_task(&record)?;
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

    /// Explicit start requires current protected native-vault policy. A locked
    /// vault propagates its actionable error; it never triggers sign-in/fallback.
    pub fn start(&self, stage: &TrustedStage, epoch: u64) -> io::Result<Observation> {
        self.start_with(stage, epoch, || {
            let enrollment = self.store.owner_enrollment()?;
            let _guard = enrollment.current_policy()?;
            Ok(())
        })
    }

    fn start_with(
        &self,
        stage: &TrustedStage,
        epoch: u64,
        credentials: impl Fn() -> io::Result<()>,
    ) -> io::Result<Observation> {
        if self.context.platform == Platform::Windows {
            return Err(refused(
                "host.service_windows_authenticated_graceful_stop_unavailable",
            ));
        }
        let _lease = self.lease()?;
        let snapshot = self.fence(epoch, Phase::Provisioned)?;
        credentials()?;
        stage.revalidate()?;
        let (_, record) = self.reconciled_registration()?;
        let record = record.ok_or_else(|| refused("host.service_not_installed"))?;
        if record.executable != stage.path
            || record.digest != stage.digest
            || record.settings != snapshot.settings
            || record.removed
        {
            return Err(refused("host.service_explicit_replacement_required"));
        }
        let before = self.observe(&record)?;
        if before == ProcessState::Transitioning {
            self.fence(epoch, Phase::Provisioned)?;
            return Ok(observation(&snapshot, before));
        }
        if before != ProcessState::Running {
            self.fence(epoch, Phase::Provisioned)?;
            credentials()?;
            match self.context.platform {
                Platform::Linux => self.run(&["--user", "start", &record.resource])?,
                Platform::Macos => {
                    if before == ProcessState::Absent {
                        let path = definitions::definition_path(&self.context, &record);
                        self.run(&[
                            "bootstrap",
                            &format!("gui/{}", record.user),
                            definitions::path_text(&path)?,
                        ])?;
                    }
                    self.fence(epoch, Phase::Provisioned)?;
                    self.run(&[
                        "kickstart",
                        &format!("gui/{}/{}", record.user, record.resource),
                    ])?;
                }
                Platform::Windows => unreachable!(),
            }
        }
        self.fence(epoch, Phase::Provisioned)?;
        let mut process = self.observe(&record)?;
        for _ in 0..2 {
            if process == ProcessState::Running || process == ProcessState::Failed {
                break;
            }
            self.fence(epoch, Phase::Provisioned)?;
            std::thread::sleep(std::time::Duration::from_millis(25));
            process = self.observe(&record)?;
        }
        self.fence(epoch, Phase::Provisioned)?;
        if process == ProcessState::Transitioning {
            return Ok(observation(&snapshot, process));
        }
        if process != ProcessState::Running {
            return Err(refused("host.service_start_not_observed_running"));
        }
        Ok(observation(&snapshot, process))
    }

    pub fn stop(&self, epoch: u64) -> io::Result<Observation> {
        let _lease = self.lease()?;
        let snapshot = self.fence(epoch, Phase::StopRequested)?;
        let (_, record) = self.reconciled_registration()?;
        let Some(record) = record else {
            return self.unregistered_observation(&snapshot, &_lease);
        };
        let state = self.observe(&record)?;
        if matches!(state, ProcessState::Running | ProcessState::Transitioning) {
            self.fence(epoch, Phase::StopRequested)?;
            match self.context.platform {
                Platform::Linux => self.run(&["--user", "stop", &record.resource])?,
                Platform::Macos => self.run(&[
                    "bootout",
                    &format!("gui/{}/{}", record.user, record.resource),
                ])?,
                Platform::Windows => {
                    return Err(refused(
                        "host.service_windows_authenticated_graceful_stop_unavailable",
                    ));
                }
            }
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

    /// Local service cleanup only. Does not remove cloud resources, identity,
    /// enrollment/vault state, host artifacts or independent daemon processes.
    pub fn cleanup(&self, epoch: u64) -> io::Result<Observation> {
        let _lease = self.lease()?;
        let snapshot = self.fence(epoch, Phase::CleanupPending)?;
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
