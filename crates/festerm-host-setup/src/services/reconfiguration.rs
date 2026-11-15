//! Explicit Enable completion, never startup recovery or binary replacement.
use super::*;
use crate::enrollment::{EnrollmentSnapshot, OwnerEnrollment};
use tokio_util::sync::CancellationToken;

impl<R: CommandRunner> Services<'_, R> {
    pub(crate) fn complete_setup(
        &self,
        selected: &TrustedStage,
        expected: &Snapshot,
        owner: &EnrollmentSnapshot,
        cancel: &CancellationToken,
    ) -> io::Result<Observation> {
        let owners = self.store.owner_enrollment()?;
        self.complete_setup_with(selected, expected, owner, &owners, cancel, |record| {
            let provenance = record
                .provenance
                .as_ref()
                .ok_or_else(|| refused("host.service_signed_provenance_required"))?;
            let root = record
                .executable
                .parent()
                .and_then(Path::parent)
                .ok_or_else(|| refused("host.service_executable_path"))?;
            TrustedStage::reopen_release_cache(provenance, root)
        })
    }

    pub(super) fn complete_setup_with(
        &self,
        selected: &TrustedStage,
        expected: &Snapshot,
        owner: &EnrollmentSnapshot,
        owners: &OwnerEnrollment<'_>,
        cancel: &CancellationToken,
        reopen: impl FnOnce(&Registration) -> io::Result<TrustedStage>,
    ) -> io::Result<Observation> {
        if cancel.is_cancelled() {
            return Err(refused("host.cloud_cancelled"));
        }
        let operation = self.lease()?;
        self.require_replacement_settled()?;
        let (snapshot, prior) = self.reconciled_registration()?;
        if &snapshot != expected || snapshot.phase != Phase::Provisioned {
            return Err(refused("host.service_stale_or_unapproved_operation"));
        }
        let Some(old) = prior else {
            if cancel.is_cancelled() {
                return Err(refused("host.cloud_cancelled"));
            }
            drop(operation);
            return self.install_with_recovery(selected, snapshot.epoch);
        };
        // Preserve the original resumable registration path when no policy or
        // cache-location change needs this narrower stopped operation.
        if old.settings == snapshot.settings && old.executable == selected.path {
            if cancel.is_cancelled() {
                return Err(refused("host.cloud_cancelled"));
            }
            drop(operation);
            return self.install_with_recovery(selected, snapshot.epoch);
        }
        if old.removed
            || old.retiring_definition
            || old.settings.startup != snapshot.settings.startup
            || old.settings.friendly_name != snapshot.settings.friendly_name
        {
            return Err(refused("host.service_explicit_replacement_required"));
        }
        selected.sessiond_path()?;
        let registered = reopen(&old)?;
        registered.sessiond_path()?;
        // Two independently authenticated stages of the SAME complete release;
        // use the registered path, never adopt the selected cache location.
        if selected.version() != crate::VERSION
            || registered.version() != selected.version()
            || registered.bundle_digest() != selected.bundle_digest()
            || registered.path != old.executable
            || registered.digest != old.digest
            || selected.digest != old.digest
        {
            return Err(refused("host.service_authenticated_bundle_changed"));
        }
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
        let process = self.observe(&old)?;
        if (self.context.platform != Platform::Windows
            && !definitions::check_file(&self.context, &old)?)
            || (process != ProcessState::Stopped
                && !(self.context.platform == Platform::Macos && process == ProcessState::Absent))
        {
            return Err(refused("host.service_confirmed_stop_required"));
        }
        let mut new = old.clone();
        new.settings.allow_creation = snapshot.settings.allow_creation;
        if new.settings != snapshot.settings || definitions::render(&new)? != old.definition {
            return Err(refused("host.service_explicit_replacement_required"));
        }
        registered.sessiond_path()?;
        selected.sessiond_path()?;
        owners.with_current_snapshot(&snapshot, owner, |directory, current| {
            if cancel.is_cancelled() {
                return Err(refused("host.cloud_cancelled"));
            }
            let opened = storage::private_directory(&self.state, false)?;
            if !trusted::same(directory, &opened)?
                || self.read_registration(directory, current)?.as_ref() != Some(&old)
            {
                return Err(refused("host.service_stale_or_unapproved_operation"));
            }
            registered.sessiond_path()?;
            selected.sessiond_path()?;
            self.validate_registration(&new, current)?;
            if cancel.is_cancelled() {
                return Err(refused("host.cloud_cancelled"));
            }
            if new != old {
                // Lifecycle already contains the reviewed target. Only this
                // single protected receipt changes; an interrupted write leaves
                // the old mismatch fail-closed until another explicit Enable.
                storage::replace_private(
                    directory,
                    Path::new(RECORD),
                    &serde_json::to_vec(&new)
                        .map_err(|_| refused("host.service_invalid_receipt"))?,
                )?;
            }
            Ok(observation(current, process))
        })
    }
}
