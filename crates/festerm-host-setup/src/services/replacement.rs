use super::*;

pub(super) const JOURNAL: &str = "native-service-update.json";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReplacementState {
    Prepared,
    Applying,
    Committed,
    Aborted,
}

#[cfg(all(test, unix))]
mod tests;
#[cfg(all(test, feature = "cloud", feature = "host-runtime"))]
mod windows_tests;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Replacement {
    version: u8,
    operation: String,
    epoch: u64,
    from_version: String,
    to_version: String,
    bundle_digest: String,
    old: Registration,
    new: Registration,
    state: ReplacementState,
}

#[derive(Serialize)]
pub struct ReplacementReport {
    pub operation: String,
    pub from_version: String,
    pub to_version: String,
    pub state: ReplacementState,
    pub previous_provenance: Option<PathBuf>,
    pub candidate_provenance: Option<PathBuf>,
    pub candidate_executable: PathBuf,
    pub candidate_bundle_sha256: String,
    pub meaning: &'static str,
}

fn version(value: &str) -> io::Result<[u64; 3]> {
    let parts: Vec<_> = value.split('.').collect();
    if value.len() > 32 || parts.len() != 3 {
        return Err(refused("host.update_stable_version_required"));
    }
    let mut result = [0; 3];
    for (index, part) in parts.iter().enumerate() {
        if part.is_empty()
            || (part.len() > 1 && part.starts_with('0'))
            || !part.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err(refused("host.update_stable_version_required"));
        }
        result[index] = part
            .parse()
            .map_err(|_| refused("host.update_stable_version_required"))?;
    }
    Ok(result)
}

impl Replacement {
    fn pending(&self) -> bool {
        matches!(
            self.state,
            ReplacementState::Prepared | ReplacementState::Applying
        )
    }

    fn report(&self) -> ReplacementReport {
        ReplacementReport {
            operation: self.operation.clone(),
            from_version: self.from_version.clone(),
            to_version: self.to_version.clone(),
            state: self.state,
            previous_provenance: self.old.provenance.clone(),
            candidate_provenance: self.new.provenance.clone(),
            candidate_executable: self.new.executable.clone(),
            candidate_bundle_sha256: self.bundle_digest.clone(),
            meaning: match self.state {
                ReplacementState::Committed => {
                    "stopped-registration-history-not-current-liveness-enable-or-ready"
                }
                ReplacementState::Aborted => {
                    "native-cleanup-history-not-current-absence-cloud-not-reconciled"
                }
                _ => "outcome-unknown-preserve-both-receipts-retry-exact-update-or-owned-cleanup",
            },
        }
    }
}

impl<R: CommandRunner> Services<'_, R> {
    fn reconcile_replacement_tasks(&self, record: &mut Replacement) -> io::Result<()> {
        if self.context.platform != Platform::Windows {
            return Ok(());
        }
        let prior = record.clone();
        for target in [&mut record.old, &mut record.new] {
            if target.native_snapshot.is_some() {
                continue;
            }
            let observed = self.task(target, TaskAction::Query)?;
            match (&observed.xml, &observed.security) {
                (Some(xml), Some(security))
                    if target.native_submitted
                        && Some(xml) == target.native_expected.as_ref()
                        && Some(security) == target.native_security.as_ref() =>
                {
                    target.native_snapshot = observed.xml;
                }
                (None, None) if !target.native_submitted => {}
                (None, None) => {
                    return Err(refused("host.service_task_registration_outcome_unknown"));
                }
                _ => return Err(refused("host.service_foreign_native_definition")),
            }
        }
        if *record == prior {
            return Ok(());
        }
        self.with_record(|directory, snapshot| {
            let bytes = storage::read_private(directory, Path::new(JOURNAL), 512 * 1024)?;
            let current: Replacement = serde_json::from_slice(&bytes)
                .map_err(|_| refused("host.update_invalid_journal"))?;
            if current != prior {
                return Err(refused("host.update_conflicting_recovery_intent"));
            }
            self.validate_registration(&record.old, snapshot)?;
            self.validate_registration(&record.new, snapshot)?;
            // Retain exact observed ownership even if revocation won the epoch
            // race. This cannot commit the update or restore consent.
            storage::replace_private(
                directory,
                Path::new(JOURNAL),
                &serde_json::to_vec(record).map_err(|_| refused("host.update_invalid_journal"))?,
            )
        })
    }

    fn replacement(&self) -> io::Result<Option<Replacement>> {
        self.with_record(|directory, snapshot| {
            let bytes = match storage::read_private(directory, Path::new(JOURNAL), 512 * 1024) {
                Ok(bytes) => bytes,
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(error),
            };
            let record: Replacement = serde_json::from_slice(&bytes)
                .map_err(|_| refused("host.update_invalid_journal"))?;
            self.validate_registration(&record.old, snapshot)?;
            self.validate_registration(&record.new, snapshot)?;
            if record.version != 1
                || record.epoch == 0
                || record.epoch > snapshot.epoch
                || version(&record.from_version)? >= version(&record.to_version)?
                || record.new.version != 3
                || record.old.resource == record.new.resource
                || record.old.settings != record.new.settings
                || record.operation != record.new.reservation
                || record.bundle_digest.len() != 64
                || !record
                    .bundle_digest
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit())
                || record.new.removed
                || record.new.retiring_definition
            {
                return Err(refused("host.update_foreign_journal"));
            }
            Ok(Some(record))
        })
    }

    fn save_replacement(&self, record: &Replacement, epoch: u64, phase: Phase) -> io::Result<()> {
        self.with_record(|directory, snapshot| {
            if snapshot.epoch != epoch
                || snapshot.phase != phase
                || snapshot.binding.as_ref() != Some(&record.old.binding)
                || (phase == Phase::Disabled && snapshot.settings != record.new.settings)
            {
                return Err(refused("host.update_superseded_reconcile_owned_resources"));
            }
            storage::replace_private(
                directory,
                Path::new(JOURNAL),
                &serde_json::to_vec(record).map_err(|_| refused("host.update_invalid_journal"))?,
            )
        })
    }

    pub(super) fn require_no_replacement_history(&self) -> io::Result<()> {
        // Update history proves prior registration, not never-published absence.
        if self.replacement()?.is_some() {
            return Err(refused(
                "host.update_missing_registration_reconciliation_required",
            ));
        }
        Ok(())
    }

    pub(super) fn require_replacement_settled(&self) -> io::Result<()> {
        if self.replacement()?.is_some_and(|record| record.pending()) {
            return Err(refused("host.update_recovery_required"));
        }
        Ok(())
    }

    pub fn replacement_status(&self) -> io::Result<Option<ReplacementReport>> {
        let exists = self.with_record(|directory, _| {
            match storage::open_private_file(directory, Path::new(JOURNAL), false) {
                Ok(_) => Ok(true),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
                Err(error) => Err(error),
            }
        })?;
        if !exists {
            return Ok(None);
        }
        let _lease = self.lease()?;
        self.replacement()
            .map(|record| record.map(|record| record.report()))
    }

    /// Run the new trusted version's updater after explicit disable. Neither
    /// old/new executables nor independent session daemons are launched/stopped.
    pub fn replace_stopped(
        &self,
        stage: &TrustedStage,
        from_version: &str,
        epoch: u64,
    ) -> io::Result<ReplacementReport> {
        self.replace_stopped_with(
            stage,
            from_version,
            epoch,
            TrustedStage::authenticate_registered,
        )
    }

    fn replace_stopped_with(
        &self,
        stage: &TrustedStage,
        from_version: &str,
        epoch: u64,
        authenticate: impl Fn(&Registration, &str) -> io::Result<()>,
    ) -> io::Result<ReplacementReport> {
        if self.context.platform == Platform::Windows
            && !cfg!(all(feature = "cloud", feature = "host-runtime"))
        {
            return Err(refused(
                "host.service_windows_authenticated_graceful_stop_unavailable",
            ));
        }
        let _operation = self.lease()?;
        let snapshot = self.fence(epoch, Phase::Disabled)?;
        let _runtime = self.with_record(|directory, current| {
            if current != &snapshot {
                return Err(refused("host.runtime_consent_changed"));
            }
            match storage::open_private_file(directory, Path::new("host-runtime.lock"), false) {
                Ok(file) => Lease::exclusive(file).map(Some),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    match storage::open_private_file(
                        directory,
                        Path::new("host-runtime.json"),
                        false,
                    ) {
                        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
                        _ => Err(refused("host.runtime_lock_missing_reconciliation_required")),
                    }
                }
                Err(error) => Err(error),
            }
        })?;
        stage.sessiond_path()?;
        if stage.version() != crate::VERSION || version(from_version)? >= version(stage.version())?
        {
            return Err(refused(
                "host.update_strictly_newer_current_release_required",
            ));
        }
        let current = self
            .registration()?
            .1
            .ok_or_else(|| refused("host.service_not_installed"))?;
        let mut transaction = match self.replacement()? {
            Some(record)
                if record.pending()
                    || (record.state == ReplacementState::Committed
                        && record.from_version == from_version
                        && record.new.executable == stage.path) =>
            {
                if record.from_version != from_version
                    || record.to_version != stage.version()
                    || record.new.executable != stage.path
                    || record.new.digest != stage.digest
                    || record.new.provenance.as_deref() != Some(stage.provenance_path())
                    || record.bundle_digest != stage.bundle_digest()
                    || (current != record.old && current != record.new)
                {
                    return Err(refused("host.update_conflicting_recovery_intent"));
                }
                record
            }
            _ => {
                if current.removed
                    || current.retiring_definition
                    || current.settings != snapshot.settings
                {
                    return Err(refused("host.update_current_registration_required"));
                }
                authenticate(&current, from_version)?;
                let mut new = current.clone();
                new.version = 3;
                new.epoch = epoch;
                new.reservation = uuid::Uuid::new_v4().to_string();
                new.resource = definitions::replacement_resource(
                    &snapshot,
                    epoch,
                    &new.reservation,
                    self.context.platform,
                )?;
                new.executable = stage.path.clone();
                new.digest = stage.digest.clone();
                new.provenance = Some(stage.provenance_path().to_owned());
                new.definition = definitions::render(&new)?;
                if self.context.platform == Platform::Windows {
                    new.native_submitted = false;
                    new.native_snapshot = None;
                    let prepared = self.task(&new, TaskAction::Prepare(new.definition.clone()))?;
                    new.native_expected = Some(
                        prepared
                            .xml
                            .ok_or_else(|| refused("host.service_task_reservation_unconfirmed"))?,
                    );
                    new.native_security = Some(
                        prepared
                            .security
                            .ok_or_else(|| refused("host.service_task_reservation_unconfirmed"))?,
                    );
                }
                Replacement {
                    version: 1,
                    operation: new.reservation.clone(),
                    epoch,
                    from_version: from_version.into(),
                    to_version: stage.version().into(),
                    bundle_digest: stage.bundle_digest().into(),
                    old: current.clone(),
                    new,
                    state: ReplacementState::Prepared,
                }
            }
        };
        if transaction.new.settings != snapshot.settings {
            return Err(refused("host.update_settings_changed_reconfirm_or_cleanup"));
        }
        authenticate(&transaction.old, from_version)?;
        authenticate(&transaction.new, stage.version())?;
        self.validate_registration(&transaction.new, &snapshot)?;
        if self.context.platform == Platform::Windows
            && transaction.state != ReplacementState::Prepared
        {
            self.reconcile_replacement_tasks(&mut transaction)?;
        }
        let mut old = transaction.old.clone();
        old.retiring_definition = transaction.state != ReplacementState::Prepared;
        if transaction.state == ReplacementState::Prepared
            && ((self.context.platform != Platform::Windows
                && (!definitions::check_file(&self.context, &old)?
                    || definitions::check_file(&self.context, &transaction.new)?))
                || self.observe(&transaction.new)? != ProcessState::Absent)
        {
            return Err(refused("host.update_unreserved_native_resource"));
        }
        if !matches!(
            self.observe(&old)?,
            ProcessState::Absent | ProcessState::Stopped | ProcessState::Failed
        ) {
            return Err(refused("host.update_confirmed_stop_required"));
        }
        if transaction.state == ReplacementState::Committed {
            let observed = self.observe(&transaction.new)?;
            if current != transaction.new
                || (self.context.platform != Platform::Windows
                    && !definitions::check_file(&self.context, &transaction.new)?)
                || !matches!(observed, ProcessState::Absent | ProcessState::Stopped)
                || (self.context.platform != Platform::Macos && observed == ProcessState::Absent)
            {
                return Err(refused("host.update_committed_definition_changed"));
            }
            return Ok(transaction.report());
        }
        transaction.epoch = epoch;
        self.save_replacement(&transaction, epoch, Phase::Disabled)?;
        transaction.state = ReplacementState::Applying;
        self.save_replacement(&transaction, epoch, Phase::Disabled)?;
        self.retire_update_resource(&transaction.old, epoch, Phase::Disabled)?;
        self.fence(epoch, Phase::Disabled)?;
        stage.sessiond_path()?;
        if self.context.platform == Platform::Windows {
            if !transaction.new.native_submitted {
                if self.observe(&transaction.new)? != ProcessState::Absent {
                    return Err(refused("host.update_unreserved_native_resource"));
                }
                transaction.new.native_submitted = true;
                self.save_replacement(&transaction, epoch, Phase::Disabled)?;
                self.fence(epoch, Phase::Disabled)?;
                self.task(
                    &transaction.new,
                    TaskAction::Create(
                        transaction
                            .new
                            .native_expected
                            .clone()
                            .ok_or_else(|| refused("host.service_task_reservation_unconfirmed"))?,
                    ),
                )?;
            }
            self.reconcile_replacement_tasks(&mut transaction)?;
        } else {
            definitions::publish_file(&self.context, &transaction.new)?;
        }
        self.fence(epoch, Phase::Disabled)?;
        if self.context.platform == Platform::Linux {
            self.run(&["--user", "daemon-reload"])?;
        }
        let state = self.observe(&transaction.new)?;
        if (self.context.platform != Platform::Windows
            && !definitions::check_file(&self.context, &transaction.new)?)
            || !matches!(state, ProcessState::Absent | ProcessState::Stopped)
            || (self.context.platform != Platform::Macos && state == ProcessState::Absent)
        {
            return Err(refused("host.update_new_registration_unconfirmed"));
        }
        stage.sessiond_path()?;
        self.save(&transaction.new, epoch, Phase::Disabled)?;
        authenticate(&transaction.new, stage.version())?;
        transaction.state = ReplacementState::Committed;
        self.save_replacement(&transaction, epoch, Phase::Disabled)?;
        Ok(transaction.report())
    }

    fn retire_update_resource(
        &self,
        original: &Registration,
        epoch: u64,
        phase: Phase,
    ) -> io::Result<()> {
        let mut record = original.clone();
        record.retiring_definition = true;
        self.fence(epoch, phase)?;
        let before = self.observe(&record)?;
        if !matches!(
            before,
            ProcessState::Absent | ProcessState::Stopped | ProcessState::Failed
        ) {
            return Err(refused("host.update_confirmed_stop_required"));
        }
        match self.context.platform {
            Platform::Linux => {
                if definitions::check_file(&self.context, &record)? {
                    self.run(&["--user", "disable", "--no-reload", &record.resource])?;
                }
                self.fence(epoch, phase)?;
                definitions::remove_file(&self.context, &record)?;
                self.fence(epoch, phase)?;
                self.run(&["--user", "daemon-reload"])?;
                if self.observe(&record)? == ProcessState::Failed {
                    self.fence(epoch, phase)?;
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
                self.fence(epoch, phase)?;
                definitions::remove_file(&self.context, &record)?;
            }
            Platform::Windows => {
                if before != ProcessState::Absent {
                    self.fence(epoch, phase)?;
                    self.task(&record, TaskAction::Remove)?;
                }
            }
        }
        if self.observe(&record)? != ProcessState::Absent {
            return Err(refused("host.update_old_registration_not_absent"));
        }
        self.fence(epoch, phase)?;
        Ok(())
    }

    pub(super) fn stop_replacement(&self, epoch: u64) -> io::Result<Option<Observation>> {
        let Some(mut record) = self.replacement()?.filter(Replacement::pending) else {
            return Ok(None);
        };
        #[cfg(all(feature = "cloud", feature = "host-runtime"))]
        let started = std::time::Instant::now();
        self.reconcile_replacement_tasks(&mut record)?;
        #[cfg(all(feature = "cloud", feature = "host-runtime"))]
        if self.context.platform == Platform::Windows {
            return self
                .wait_windows_resources_revocation(
                    &self.fence(epoch, Phase::StopRequested)?,
                    &[&record.old, &record.new],
                    std::time::Duration::from_secs(10).saturating_sub(started.elapsed()),
                )
                .map(Some);
        }
        if self.context.platform == Platform::Windows {
            return Err(refused(
                "host.service_windows_authenticated_graceful_stop_unavailable",
            ));
        }
        for mut target in [record.old, record.new] {
            target.retiring_definition = true;
            self.fence(epoch, Phase::StopRequested)?;
            if matches!(
                self.observe(&target)?,
                ProcessState::Running | ProcessState::Transitioning
            ) {
                match self.context.platform {
                    Platform::Linux => self.run(&["--user", "stop", &target.resource])?,
                    Platform::Macos => self.run(&[
                        "bootout",
                        &format!("gui/{}/{}", target.user, target.resource),
                    ])?,
                    Platform::Windows => {
                        return Err(refused(
                            "host.service_windows_authenticated_graceful_stop_unavailable",
                        ))
                    }
                }
            }
            if !matches!(
                self.observe(&target)?,
                ProcessState::Absent | ProcessState::Stopped | ProcessState::Failed
            ) {
                return Err(refused("host.service_stop_not_observed"));
            }
        }
        Ok(Some(observation(
            &self.fence(epoch, Phase::StopRequested)?,
            ProcessState::Stopped,
        )))
    }

    pub(super) fn cleanup_replacement(&self, epoch: u64) -> io::Result<Option<Observation>> {
        self.cleanup_replacement_with(epoch, TrustedStage::authenticate_registered)
    }

    fn cleanup_replacement_with(
        &self,
        epoch: u64,
        authenticate: impl Fn(&Registration, &str) -> io::Result<()>,
    ) -> io::Result<Option<Observation>> {
        let Some(mut transaction) = self.replacement()?.filter(Replacement::pending) else {
            return Ok(None);
        };
        self.reconcile_replacement_tasks(&mut transaction)?;
        if self.context.platform == Platform::Windows {
            authenticate(&transaction.old, &transaction.from_version)?;
            authenticate(&transaction.new, &transaction.to_version)?;
        }
        transaction.state = ReplacementState::Applying;
        self.save_replacement(&transaction, epoch, Phase::CleanupPending)?;
        self.retire_update_resource(&transaction.old, epoch, Phase::CleanupPending)?;
        self.retire_update_resource(&transaction.new, epoch, Phase::CleanupPending)?;
        let mut current = self
            .registration()?
            .1
            .ok_or_else(|| refused("host.service_not_installed"))?;
        current.retiring_definition = true;
        current.removed = true;
        self.save(&current, epoch, Phase::CleanupPending)?;
        transaction.state = ReplacementState::Aborted;
        self.save_replacement(&transaction, epoch, Phase::CleanupPending)?;
        Ok(Some(observation(
            &self.fence(epoch, Phase::CleanupPending)?,
            ProcessState::Absent,
        )))
    }
}
