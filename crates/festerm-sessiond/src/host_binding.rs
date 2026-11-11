use super::*;
use crate::native_binding::{self, Binding, BindingPhase, Receipt};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportedBinding {
    pub binding: Binding,
    pub(super) lease_reserved: bool,
    pub(super) acknowledged: bool,
    pub(super) activated: bool,
    pub(super) released: bool,
}

fn uncertain(_: io::Error) -> ControlError {
    ControlError::CommittedButNotPublished
}

fn imported_record(binding: &Binding) -> Result<Record, ControlError> {
    let recipe = Recipe {
        shell: None,
        cwd: None,
        args: Vec::new(),
    };
    Ok(Record {
        owner: binding.principal.clone(),
        logical: binding.logical.clone(),
        display_name: format!("Imported {}", &binding.logical.as_str()[..8]),
        recipe: recipe.clone(),
        effective_recipe: recipe,
        lifetime: Lifetime::Persistent {},
        epoch: Epoch::new(1).map_err(|_| ControlError::Invalid)?,
        revoked: true,
        name: binding.identity.name.clone(),
        launch: binding.operation.clone(),
        generation: GenerationId::new(binding.identity.created_at_unix_ms)
            .map_err(|_| ControlError::Invalid)?,
        fingerprint: binding.spawn_fingerprint.clone(),
        phase: Phase::Running,
        identity: Some(Identity {
            pid: binding.identity.pid,
            endpoint: binding.identity.endpoint.clone(),
        }),
        artifacts_retired: false,
        binding: Some(ImportedBinding {
            binding: binding.clone(),
            lease_reserved: false,
            acknowledged: false,
            activated: false,
            released: false,
        }),
    })
}

impl SessionControl {
    /// Read the original protected private-sharing operation, never infer it
    /// from a name, PID, native receipt or caller-supplied descriptor.
    pub fn private_binding(
        &self,
        operation: &OperationId,
    ) -> Result<Option<Receipt>, ControlError> {
        let tx = self.transaction()?;
        let mut found = None;
        for record in tx
            .state
            .sessions
            .values()
            .filter(|record| &record.launch == operation)
        {
            let imported = record
                .binding
                .as_ref()
                .ok_or(ControlError::OperationConflict)?;
            if imported.binding.source_authority.is_legacy() || found.is_some() {
                return Err(ControlError::OperationConflict);
            }
            found = Some(Receipt {
                version: 1,
                binding: imported.binding.clone(),
                phase: if imported.released {
                    BindingPhase::Released
                } else if imported.activated && record.revoked {
                    BindingPhase::Revoked
                } else if imported.activated {
                    BindingPhase::Active
                } else {
                    BindingPhase::Prepared
                },
            });
        }
        Ok(found)
    }

    pub fn has_binding_records(&self) -> Result<bool, ControlError> {
        let tx = self.transaction()?;
        tx.save()?;
        Ok(tx
            .state
            .sessions
            .values()
            .any(|record| record.binding.is_some()))
    }
    pub fn binding_namespace(&self) -> Result<(OperationId, Epoch), ControlError> {
        let mut tx = self.transaction()?;
        if tx.state.namespace.is_none() {
            tx.state.namespace = Some(OperationId::new());
            tx.save()?;
        }
        tx.save()?;
        let namespace = tx.state.namespace.as_ref().ok_or(ControlError::Storage)?;
        match storage::read_private(&tx.directory, Path::new("host-binding-namespace.json"), 256) {
            Ok(bytes)
                if serde_json::from_slice::<OperationId>(&bytes).ok().as_ref()
                    == Some(namespace) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                storage::write_new_private(
                    &tx.directory,
                    Path::new("host-binding-namespace.json"),
                    &serde_json::to_vec(namespace).map_err(|_| ControlError::Storage)?,
                )
                .map_err(storage_error)?;
            }
            _ => return Err(ControlError::Storage),
        }
        Ok((
            tx.state.namespace.clone().ok_or(ControlError::Storage)?,
            tx.state.issuance_epoch,
        ))
    }

    /// Local composition only. `admit` must acquire current protected owner,
    /// source-consent and migration-operation guards, not asserted wire metadata.
    /// Every guard is dropped before the authenticated native exchange.
    pub fn bind_existing<G>(
        &self,
        binding: &Binding,
        mut admit: impl FnMut(&Binding) -> Result<G, ControlError>,
    ) -> Result<Receipt, ControlError> {
        binding.validate().map_err(|_| ControlError::Invalid)?;
        if binding.control_root != self.root {
            return Err(ControlError::Denied);
        }
        {
            let _guard = admit(binding)?;
        }
        if let Some(receipt) = native_binding::probe(
            &binding.runtime_root,
            &binding.identity,
            &binding.spawn_fingerprint,
        )
        .map_err(|_| ControlError::Unknown)?
        {
            if receipt.binding != *binding {
                return Err(ControlError::Denied);
            }
            if matches!(
                receipt.phase,
                BindingPhase::Released | BindingPhase::Revoked
            ) {
                return Err(ControlError::Revoked);
            }
        }
        {
            let mut tx = self.transaction()?;
            if tx.state.issuance_epoch != binding.issuance_epoch
                || tx.state.namespace.as_ref() != Some(&binding.namespace)
            {
                return Err(ControlError::Stale);
            }
            let _guard = admit(binding)?;
            if !binding.source_authority.is_legacy()
                && tx.state.sessions.values().any(|record| {
                    record.launch == binding.operation
                        && record
                            .binding
                            .as_ref()
                            .is_none_or(|imported| imported.binding != *binding)
                })
            {
                return Err(ControlError::OperationConflict);
            }
            if let Some(record) = tx.state.sessions.get(&binding.logical) {
                let imported = record.binding.as_ref().ok_or(ControlError::AlreadyExists)?;
                if imported.binding != *binding {
                    return Err(ControlError::OperationConflict);
                }
                if imported.released || imported.activated && record.revoked {
                    return Err(ControlError::Revoked);
                }
            } else {
                if tx.state.sessions.len() >= MAX_SESSIONS
                    || tx.state.sessions.values().any(|record| {
                        record.generation.get() == binding.identity.created_at_unix_ms
                            && record.root(&self.root) == binding.runtime_root
                    })
                {
                    return Err(ControlError::AlreadyExists);
                }
                let record = imported_record(binding)?;
                tx.state.last_generation = tx.state.last_generation.max(record.generation.get());
                tx.state.sessions.insert(binding.logical.clone(), record);
                tx.save()?;
            }
            let record = &tx.state.sessions[&binding.logical];
            let imported = record.binding.as_ref().ok_or(ControlError::Storage)?;
            let guard = if imported.lease_reserved {
                LeaseGuard::acquire(&binding.runtime_root, record.generation)
            } else {
                LeaseGuard::reserve(&binding.runtime_root, record.generation)
            }
            .map_err(storage_error)?;
            match guard.read() {
                Ok(lease) if record.lease_matches(&lease) => {}
                Err(error)
                    if error.kind() == io::ErrorKind::NotFound && !imported.lease_reserved =>
                {
                    guard
                        .create(&DaemonLease {
                            version: 1,
                            name: record.name.clone(),
                            generation: record.generation,
                            revision: 1,
                            lifetime: Lifetime::Persistent {},
                            temporary: None,
                            deadline_ms: None,
                            connected: false,
                            stopped: false,
                            remote_revoked: true,
                            pending: None,
                            last_attachment: None,
                            grant: record.grant(),
                            writer_epoch: Epoch::new(1).map_err(|_| ControlError::Invalid)?,
                            native_pid: Some(binding.identity.pid),
                        })
                        .map_err(storage_error)?;
                }
                _ => return Err(ControlError::Unknown),
            }
            tx.state
                .sessions
                .get_mut(&binding.logical)
                .ok_or(ControlError::Storage)?
                .binding
                .as_mut()
                .ok_or(ControlError::Storage)?
                .lease_reserved = true;
            tx.save()?;
        }
        let receipt = native_binding::bind(binding).map_err(uncertain)?;
        if receipt.binding != *binding
            || matches!(
                receipt.phase,
                BindingPhase::Released | BindingPhase::Revoked
            )
        {
            return Err(ControlError::Revoked);
        }
        let mut tx = self.transaction()?;
        let _guard = admit(binding)?;
        let record = tx
            .state
            .sessions
            .get_mut(&binding.logical)
            .ok_or(ControlError::Unknown)?;
        let imported = record.binding.as_mut().ok_or(ControlError::Unknown)?;
        if imported.binding != *binding || imported.released || imported.activated && record.revoked
        {
            return Err(ControlError::Revoked);
        }
        imported.acknowledged = true;
        tx.save()?;
        Ok(receipt)
    }

    /// Publish exactly the previously authenticated persistent binding. Initial
    /// private preparation is distinguishable from a later revoked grant.
    pub fn switch_binding<G>(
        &self,
        binding: &Binding,
        mut admit: impl FnMut(&Binding) -> Result<G, ControlError>,
    ) -> Result<Receipt, ControlError> {
        if binding.control_root != self.root {
            return Err(ControlError::Denied);
        }
        {
            let _guard = admit(binding)?;
        }
        let observed = native_binding::probe(
            &binding.runtime_root,
            &binding.identity,
            &binding.spawn_fingerprint,
        )
        .map_err(|_| ControlError::Unknown)?
        .ok_or(ControlError::Unknown)?;
        if observed.binding != *binding
            || matches!(
                observed.phase,
                BindingPhase::Released | BindingPhase::Revoked
            )
        {
            return Err(ControlError::Revoked);
        }
        {
            let mut tx = self.transaction()?;
            if tx.state.issuance_epoch != binding.issuance_epoch
                || tx.state.namespace.as_ref() != Some(&binding.namespace)
            {
                return Err(ControlError::Stale);
            }
            let _guard = admit(binding)?;
            let record = tx
                .state
                .sessions
                .get_mut(&binding.logical)
                .ok_or(ControlError::Unknown)?;
            let imported = record.binding.as_ref().ok_or(ControlError::Denied)?;
            if imported.binding != *binding || !imported.acknowledged {
                return Err(ControlError::Denied);
            }
            if imported.released || imported.activated && record.revoked {
                return Err(ControlError::Revoked);
            }
            let guard = LeaseGuard::acquire(&binding.runtime_root, record.generation)
                .map_err(storage_error)?;
            let mut lease = guard.read().map_err(storage_error)?;
            if !record.lease_matches(&lease) || lease.stopped {
                return Err(ControlError::Unknown);
            }
            if lease.remote_revoked && lease.grant.revision.get() != 1 {
                return Err(ControlError::Revoked);
            }
            if !imported.activated {
                if record.epoch.get() != 1 || !record.revoked || lease.grant.revision.get() != 1 {
                    return Err(ControlError::Revoked);
                }
                record.revoked = false;
                record.epoch = record.epoch.next().map_err(|_| ControlError::Storage)?;
                record
                    .binding
                    .as_mut()
                    .ok_or(ControlError::Storage)?
                    .activated = true;
                tx.save()?;
            }
            let record = &tx.state.sessions[&binding.logical];
            lease.remote_revoked = false;
            lease.grant = record.grant();
            guard.replace(&mut lease).map_err(uncertain)?;
        }
        let receipt = native_binding::probe(
            &binding.runtime_root,
            &binding.identity,
            &binding.spawn_fingerprint,
        )
        .map_err(uncertain)?
        .ok_or(ControlError::CommittedButNotPublished)?;
        if receipt.binding != *binding || receipt.phase != BindingPhase::Active {
            return Err(ControlError::Unknown);
        }
        let _guard = admit(binding)?;
        Ok(receipt)
    }

    /// Exact local reduction only. Revoke the retained native lease before
    /// asking the daemon to release its watch. Never signal/stop a process,
    /// change its writer, retire its artifacts, or restore a legacy grant.
    pub fn release_existing<G>(
        &self,
        binding: &Binding,
        admit: impl FnMut(&Binding) -> Result<G, ControlError>,
    ) -> Result<Receipt, ControlError> {
        self.release_existing_with(
            binding,
            admit,
            || {
                detached::host_generation_state(
                    &binding.runtime_root,
                    &binding.identity,
                    &binding.spawn_fingerprint,
                )
                .map_err(|_| ControlError::Unknown)
            },
            || native_binding::release(binding).map_err(uncertain),
        )
    }

    fn release_existing_with<G>(
        &self,
        binding: &Binding,
        mut admit: impl FnMut(&Binding) -> Result<G, ControlError>,
        mut native_live: impl FnMut() -> Result<bool, ControlError>,
        native_release: impl FnOnce() -> Result<Receipt, ControlError>,
    ) -> Result<Receipt, ControlError> {
        binding.validate().map_err(|_| ControlError::Invalid)?;
        if binding.control_root != self.root {
            return Err(ControlError::Denied);
        }
        {
            let tx = self.transaction()?;
            let _guard = admit(binding)?;
            if tx.state.namespace.as_ref() != Some(&binding.namespace)
                || tx.state.issuance_epoch != binding.issuance_epoch
            {
                return Err(ControlError::Stale);
            }
            if let Some(record) = tx.state.sessions.get(&binding.logical) {
                let imported = record.binding.as_ref().ok_or(ControlError::Denied)?;
                if imported.binding != *binding || record.fingerprint != binding.spawn_fingerprint {
                    return Err(ControlError::Denied);
                }
                if imported.released {
                    return Ok(Receipt {
                        version: 1,
                        binding: binding.clone(),
                        phase: BindingPhase::Released,
                    });
                }
            }
        }
        // This is the existing exact registry/fingerprint/lifetime-lease proof,
        // never an inference from an IPC, capability or HOST-lease I/O failure.
        // No protected transaction or owner guard spans native observation/IPC.
        let observation = native_live();
        let absent = observation == Ok(false);
        {
            let mut tx = self.transaction()?;
            let _guard = admit(binding)?;
            if tx.state.namespace.as_ref() != Some(&binding.namespace)
                || tx.state.issuance_epoch != binding.issuance_epoch
            {
                return Err(ControlError::Stale);
            }
            if !tx.state.sessions.contains_key(&binding.logical) {
                if !absent {
                    return Err(ControlError::Unknown);
                }
                if tx.state.sessions.len() >= MAX_SESSIONS
                    || tx.state.sessions.values().any(|record| {
                        record.root(&self.root) == binding.runtime_root
                            && (record.generation.get() == binding.identity.created_at_unix_ms
                                || record.name == binding.identity.name)
                    })
                {
                    return Err(ControlError::AlreadyExists);
                }
                let mut record = imported_record(binding)?;
                record.phase = Phase::Absent;
                tx.state.last_generation = tx.state.last_generation.max(record.generation.get());
                tx.state.sessions.insert(binding.logical.clone(), record);
            }
            let record = tx
                .state
                .sessions
                .get_mut(&binding.logical)
                .ok_or(ControlError::Unknown)?;
            let imported = record.binding.as_ref().ok_or(ControlError::Denied)?;
            if imported.binding != *binding {
                return Err(ControlError::Denied);
            }
            let lease = if imported.lease_reserved {
                match LeaseGuard::acquire(&binding.runtime_root, record.generation) {
                    Ok(guard) => match guard.read() {
                        Ok(lease) => Some((guard, lease)),
                        Err(error) if absent && error.kind() == io::ErrorKind::NotFound => None,
                        Err(error) => return Err(storage_error(error)),
                    },
                    Err(error) if absent && error.kind() == io::ErrorKind::NotFound => {
                        let directory = storage::private_directory(&binding.runtime_root, false)
                            .map_err(storage_error)?;
                        let name = format!("host-daemon-{}.json", record.generation.get());
                        match storage::open_private_file(&directory, Path::new(&name), false) {
                            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                            Ok(_) => return Err(ControlError::Unknown),
                            Err(error) => return Err(storage_error(error)),
                        }
                    }
                    Err(error) => return Err(storage_error(error)),
                }
            } else {
                None
            };
            if let Some((guard, mut lease)) = lease {
                if !record.lease_matches(&lease) || lease.stopped && !absent {
                    return Err(ControlError::Unknown);
                }
                let revision = record.epoch.get().max(lease.grant.revision.get());
                record.epoch = Epoch::new(revision)
                    .and_then(Epoch::next)
                    .map_err(|_| ControlError::Storage)?;
                record.revoked = true;
                lease.pending = None;
                lease.remote_revoked = true;
                lease.grant = record.grant();
                guard.replace(&mut lease).map_err(uncertain)?;
            } else {
                record.revoked = true;
            }
            tx.save()?;
        }
        // A failed observation cannot authorize absence, but must not prevent
        // reduction of a still-present, exactly matched HOST lease.
        observation?;
        let receipt = if absent {
            if native_live()? {
                return Err(ControlError::Unknown);
            }
            Receipt {
                version: 1,
                binding: binding.clone(),
                phase: BindingPhase::Released,
            }
        } else {
            match native_release() {
                Ok(receipt) => receipt,
                Err(error) => {
                    if native_live()? {
                        return Err(error);
                    }
                    Receipt {
                        version: 1,
                        binding: binding.clone(),
                        phase: BindingPhase::Released,
                    }
                }
            }
        };
        if receipt.binding != *binding || receipt.phase != BindingPhase::Released {
            return Err(ControlError::Unknown);
        }
        let mut tx = self.transaction()?;
        let _guard = admit(binding)?;
        if tx.state.namespace.as_ref() != Some(&binding.namespace)
            || tx.state.issuance_epoch != binding.issuance_epoch
        {
            return Err(ControlError::Stale);
        }
        let record = tx
            .state
            .sessions
            .get_mut(&binding.logical)
            .ok_or(ControlError::Unknown)?;
        let imported = record.binding.as_mut().ok_or(ControlError::Denied)?;
        if imported.binding != *binding || !record.revoked {
            return Err(ControlError::Denied);
        }
        imported.released = true;
        tx.save()?;
        Ok(receipt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release_fixture(root: &Path, active: bool, lease: bool) -> (SessionControl, Binding) {
        let mut binding = native_binding::tests::descriptor(root);
        let sessions = SessionControl::existing_only(&binding.control_root).unwrap();
        let (namespace, issuance_epoch) = sessions.binding_namespace().unwrap();
        binding.namespace = namespace;
        binding.issuance_epoch = issuance_epoch;
        let recipe = Recipe {
            shell: None,
            cwd: None,
            args: Vec::new(),
        };
        let record = Record {
            owner: binding.principal.clone(),
            logical: binding.logical.clone(),
            display_name: "fixture".into(),
            recipe: recipe.clone(),
            effective_recipe: recipe,
            lifetime: Lifetime::Persistent {},
            epoch: Epoch::new(if active { 2 } else { 1 }).unwrap(),
            revoked: !active,
            name: binding.identity.name.clone(),
            launch: binding.operation.clone(),
            generation: GenerationId::new(binding.identity.created_at_unix_ms).unwrap(),
            fingerprint: binding.spawn_fingerprint.clone(),
            phase: Phase::Running,
            identity: Some(Identity {
                pid: binding.identity.pid,
                endpoint: binding.identity.endpoint.clone(),
            }),
            artifacts_retired: false,
            binding: Some(ImportedBinding {
                binding: binding.clone(),
                lease_reserved: true,
                acknowledged: true,
                activated: active,
                released: false,
            }),
        };
        if lease {
            LeaseGuard::reserve(root, record.generation)
                .unwrap()
                .create(&DaemonLease {
                    version: 1,
                    name: record.name.clone(),
                    generation: record.generation,
                    revision: 1,
                    lifetime: Lifetime::Persistent {},
                    temporary: None,
                    deadline_ms: None,
                    connected: false,
                    stopped: false,
                    remote_revoked: !active,
                    pending: None,
                    last_attachment: None,
                    grant: record.grant(),
                    writer_epoch: Epoch::new(1).unwrap(),
                    native_pid: Some(binding.identity.pid),
                })
                .unwrap();
        }
        let mut tx = sessions.transaction().unwrap();
        tx.state.last_generation = record.generation.get();
        tx.state.sessions.insert(binding.logical.clone(), record);
        tx.save().unwrap();
        drop(tx);
        (sessions, binding)
    }

    #[test]
    fn imported_binding_native_end_never_observes_absence_or_admits_policy() {
        for absent in [false, true] {
            for lease in [false, true] {
                let directory = storage::private_test_directory();
                let (control, binding) = release_fixture(directory.path(), true, lease);
                let target = Target {
                    logical: binding.logical.clone(),
                    generation: GenerationId::new(binding.identity.created_at_unix_ms).unwrap(),
                };
                if absent {
                    let mut tx = control.transaction().unwrap();
                    tx.state.sessions.get_mut(&binding.logical).unwrap().phase = Phase::Absent;
                    tx.save().unwrap();
                }
                let journal = std::fs::read(control.root.join(STATE_FILE)).unwrap();
                let lease_path = directory
                    .path()
                    .join(format!("host-daemon-{}.json", target.generation.get()));
                let before_lease = std::fs::read(&lease_path).ok();
                assert_eq!(
                    control.end(&binding.principal, &target, |_| -> Result<(), ControlError> {
                        panic!("an imported local shell cannot receive native End authority")
                    }),
                    Err(ControlError::Denied)
                );
                assert_eq!(std::fs::read(control.root.join(STATE_FILE)).unwrap(), journal);
                assert_eq!(std::fs::read(&lease_path).ok(), before_lease);
            }
        }
    }

    #[test]
    fn imported_binding_release_after_exact_absence_revokes_present_or_missing_lease_and_is_idempotent(
    ) {
        for active in [false, true] {
            for lease in [false, true] {
                let root = storage::private_test_directory();
                let (sessions, binding) = release_fixture(root.path(), active, lease);
                let receipt = sessions
                    .release_existing_with(
                        &binding,
                        |_| Ok(()),
                        || Ok(false),
                        || panic!("authoritatively absent native generation must not require IPC"),
                    )
                    .unwrap();
                assert_eq!(receipt.phase, BindingPhase::Released);
                let tx = sessions.transaction().unwrap();
                let record = &tx.state.sessions[&binding.logical];
                assert!(record.revoked);
                assert!(record.binding.as_ref().unwrap().released);
                if lease {
                    let lease = LeaseGuard::acquire(root.path(), record.generation)
                        .unwrap()
                        .read()
                        .unwrap();
                    assert!(lease.remote_revoked);
                    assert!(!lease.grant.remote_access);
                }
                drop(tx);
                assert_eq!(
                    sessions
                        .release_existing_with(
                            &binding,
                            |_| Ok(()),
                            || panic!(
                                "released checkpoint must survive later missing native state"
                            ),
                            || panic!("released checkpoint must not contact the daemon")
                        )
                        .unwrap(),
                    receipt
                );
                assert!(sessions.switch_binding(&binding, |_| Ok(())).is_err());
            }
        }
    }

    #[test]
    fn imported_binding_release_unknown_busy_foreign_corrupt_and_failed_revalidation_stay_pending()
    {
        for failure in [
            "unknown",
            "busy",
            "foreign",
            "corrupt",
            "missing-lock-present-lease",
            "missing-live",
            "native-error",
            "final-owner",
        ] {
            let root = storage::private_test_directory();
            let (sessions, mut binding) =
                release_fixture(root.path(), true, failure != "missing-live");
            let directory = storage::private_directory(root.path(), false).unwrap();
            let lock = if failure == "busy" {
                let lock = storage::open_private_file(
                    &directory,
                    Path::new("host-daemon-123.lock"),
                    false,
                )
                .unwrap();
                lock.lock_exclusive().unwrap();
                Some(lock)
            } else {
                None
            };
            if failure == "foreign" {
                binding.principal = PrincipalId::new();
            }
            if failure == "corrupt" {
                storage::replace_private(&directory, Path::new("host-daemon-123.json"), b"broken")
                    .unwrap();
            }
            if failure == "missing-lock-present-lease" {
                std::fs::remove_file(root.path().join("host-daemon-123.lock")).unwrap();
            }
            let mut admissions = 0;
            let result = sessions.release_existing_with(
                &binding,
                |_| {
                    admissions += 1;
                    if failure == "final-owner" && admissions == 3 {
                        Err(ControlError::Denied)
                    } else {
                        Ok(())
                    }
                },
                || {
                    if failure == "unknown" {
                        Err(ControlError::Unknown)
                    } else {
                        Ok(failure == "native-error" || failure == "missing-live")
                    }
                },
                || Err(ControlError::CommittedButNotPublished),
            );
            assert!(result.is_err(), "{failure}");
            let tx = sessions.transaction().unwrap();
            assert!(
                !tx.state
                    .sessions
                    .values()
                    .next()
                    .unwrap()
                    .binding
                    .as_ref()
                    .unwrap()
                    .released,
                "{failure}"
            );
            drop(tx);
            drop(lock);
        }
    }

    #[test]
    fn imported_binding_release_publication_before_native_reservation_requires_absence_and_retains_tombstone(
    ) {
        for observation in [
            Ok(false),
            Ok(true),
            Err(ControlError::Unknown),
            Err(ControlError::Busy),
        ] {
            let root = storage::private_test_directory();
            let (sessions, binding) = release_fixture(root.path(), false, false);
            let mut tx = sessions.transaction().unwrap();
            tx.state.sessions.clear();
            tx.save().unwrap();
            drop(tx);
            let result = sessions.release_existing_with(
                &binding,
                |_| Ok(()),
                || observation,
                || panic!("never contact an unreserved native generation"),
            );
            assert_eq!(result.is_ok(), observation == Ok(false));
            let tx = sessions.transaction().unwrap();
            if observation == Ok(false) {
                let record = &tx.state.sessions[&binding.logical];
                assert!(record.revoked);
                assert!(record.binding.as_ref().unwrap().released);
                assert!(!record.binding.as_ref().unwrap().lease_reserved);
                assert_eq!(record.phase, Phase::Absent);
            } else {
                assert!(tx.state.sessions.is_empty());
            }
        }
    }

    #[test]
    fn imported_binding_release_before_reservation_refuses_foreign_same_name_generation() {
        let root = storage::private_test_directory();
        let (sessions, mut binding) = release_fixture(root.path(), false, false);
        binding.logical = LogicalSessionId::new();
        binding.identity.created_at_unix_ms += 1;
        assert_eq!(
            sessions.release_existing_with(
                &binding,
                |_| Ok(()),
                || Ok(false),
                || panic!("foreign same-name generation must not be contacted"),
            ),
            Err(ControlError::AlreadyExists)
        );
        assert_eq!(sessions.transaction().unwrap().state.sessions.len(), 1);
    }

    #[test]
    fn imported_binding_release_lost_native_reply_uses_only_fresh_authoritative_absence() {
        for observation in [
            Ok(false),
            Ok(true),
            Err(ControlError::Unknown),
            Err(ControlError::Busy),
        ] {
            let root = storage::private_test_directory();
            let (sessions, binding) = release_fixture(root.path(), true, true);
            let mut observations = 0;
            let result = sessions.release_existing_with(
                &binding,
                |_| Ok(()),
                || {
                    observations += 1;
                    if observations == 1 {
                        Ok(true)
                    } else {
                        observation
                    }
                },
                || Err(ControlError::CommittedButNotPublished),
            );
            assert_eq!(result.is_ok(), observation == Ok(false));
            let tx = sessions.transaction().unwrap();
            let record = &tx.state.sessions[&binding.logical];
            assert!(record.revoked);
            assert_eq!(
                record.binding.as_ref().unwrap().released,
                observation == Ok(false)
            );
        }
    }

    #[cfg(unix)]
    fn root() -> PathBuf {
        std::env::current_dir()
            .unwrap()
            .join(format!("target/binding-unit-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn private_inventory_source_uncertainty_preserves_healthy_authorized_entries() {
        fn native_registry(root: &Path, record: &Record) -> File {
            let directory = storage::private_directory(root, false).unwrap();
            let identity = record.native().unwrap();
            let lifetime = storage::open_private_file(
                &directory,
                Path::new(&format!(
                    "lease-{}-{}",
                    identity.pid, identity.created_at_unix_ms
                )),
                true,
            )
            .unwrap();
            fs2::FileExt::try_lock_exclusive(&lifetime).unwrap();
            storage::replace_private(
                &directory,
                Path::new("registry.json"),
                &serde_json::to_vec(&serde_json::json!({"sessions": {
                    (identity.name): {
                        "name": record.name,
                        "pid": identity.pid,
                        "created_at_unix_ms": identity.created_at_unix_ms,
                        "socket": identity.endpoint,
                        "spawn_fingerprint": record.fingerprint,
                        "protocol_version": crate::PROTOCOL_VERSION,
                        "snapshot_schema_version": crate::RECOVERY_SNAPSHOT_SCHEMA_VERSION
                    }
                }}))
                .unwrap(),
            )
            .unwrap();
            lifetime
        }
        let root = storage::private_test_directory();
        let (sessions, mut binding) = release_fixture(root.path(), true, true);
        binding.account = "github:42".into();
        binding.source_authority = native_binding::SourceAuthority::PrivateSessionV1 {
            tag: "0".repeat(64),
        };
        binding.source_installation.clear();
        binding.source_host = binding.host.clone();
        binding.source_key_fingerprint.clear();
        binding.source_receipt_digest.clear();
        binding.source_root = binding.runtime_root.clone();
        binding.source_profile = false;
        binding.source_installation_digest.clear();
        binding.source_ownership_digest.clear();
        binding.source_key_digest.clear();
        binding.source_claim_digest.clear();
        let endpoint = |root: &Path, generation: u128| {
            #[cfg(unix)]
            {
                root.join(format!("{}-{generation}.sock", std::process::id()))
                    .to_string_lossy()
                    .into_owned()
            }
            #[cfg(windows)]
            {
                let _ = root;
                format!(
                    r"\\.\pipe\festerm-sessiond-{}-{generation}",
                    std::process::id()
                )
            }
        };
        binding.identity.endpoint = endpoint(root.path(), binding.identity.created_at_unix_ms);
        binding.validate().unwrap();
        let mut tx = sessions.transaction().unwrap();
        let private = tx.state.sessions.get_mut(&binding.logical).unwrap();
        private.identity.as_mut().unwrap().endpoint = binding.identity.endpoint.clone();
        private.binding.as_mut().unwrap().binding = binding.clone();
        let private_lifetime = native_registry(root.path(), private);
        let mut healthy = private.clone();
        healthy.logical = LogicalSessionId::new();
        healthy.generation = GenerationId::new(124).unwrap();
        healthy.name = native_slot(0);
        healthy.display_name = "healthy authorized label".into();
        healthy.launch = OperationId::new();
        healthy.binding = None;
        healthy.identity.as_mut().unwrap().endpoint = endpoint(&sessions.root, 124);
        LeaseGuard::reserve(&sessions.root, healthy.generation)
            .unwrap()
            .create(&DaemonLease {
                version: 1,
                name: healthy.name.clone(),
                generation: healthy.generation,
                revision: 1,
                lifetime: Lifetime::Persistent {},
                temporary: None,
                deadline_ms: None,
                connected: false,
                stopped: false,
                remote_revoked: false,
                pending: None,
                last_attachment: None,
                grant: healthy.grant(),
                writer_epoch: Epoch::new(1).unwrap(),
                native_pid: Some(binding.identity.pid),
            })
            .unwrap();
        let healthy_lifetime = native_registry(&sessions.root, &healthy);
        let mut foreign = healthy.clone();
        foreign.owner = PrincipalId::new();
        foreign.logical = LogicalSessionId::new();
        foreign.generation = GenerationId::new(125).unwrap();
        foreign.name = native_slot(1);
        foreign.launch = OperationId::new();
        tx.state.last_generation = 125;
        tx.state
            .sessions
            .insert(healthy.logical.clone(), healthy.clone());
        tx.state.sessions.insert(foreign.logical.clone(), foreign);
        tx.save().unwrap();
        drop(tx);
        assert!(detached::host_generation_state(
            root.path(),
            &binding.identity,
            &binding.spawn_fingerprint
        )
        .unwrap());
        assert!(matches!(
            SessionControl::binding_source(&binding),
            Err(ControlError::Unknown)
        ));
        let inventory = sessions.inventory(&binding.principal, |_| Ok(())).unwrap();
        assert_eq!(inventory.sessions.len(), 1);
        assert_eq!(inventory.sessions[0].summary.name, healthy.display_name);
        assert_eq!(inventory.sessions[0].summary.target, healthy.target());
        assert_eq!(inventory.unknown.len(), 1);
        assert_eq!(
            inventory.unknown[0].target,
            Target {
                logical: binding.logical.clone(),
                generation: GenerationId::new(123).unwrap()
            }
        );
        assert_eq!(inventory.unknown[0].grant.state, LogicalState::Unknown);
        for error in [
            ControlError::Denied,
            ControlError::Revoked,
            ControlError::Stale,
            ControlError::Unknown,
            ControlError::Busy,
        ] {
            assert!(matches!(sessions.inventory(&binding.principal, |state| {
                if state.grant.as_ref().is_some_and(|grant| grant.logical == binding.logical) { Err(error) } else { Ok(()) }
            }), Err(actual) if actual == error));
        }
        let mut publications = 0;
        assert!(matches!(
            sessions.inventory(&binding.principal, |state| {
                if state.grant.is_none() {
                    publications += 1;
                }
                if publications == 2 {
                    Err(ControlError::Revoked)
                } else {
                    Ok(())
                }
            }),
            Err(ControlError::Revoked)
        ));
        assert!(matches!(
            sessions.lookup(&binding.principal, &binding.logical, |_| Ok(())),
            Err(ControlError::Unknown)
        ));
        assert!(!sessions.transaction().unwrap().state.sessions[&binding.logical].revoked);
        assert!(
            !LeaseGuard::acquire(root.path(), GenerationId::new(123).unwrap())
                .unwrap()
                .read()
                .unwrap()
                .remote_revoked
        );
        assert_eq!(
            sessions.release_existing_with(
                &binding,
                |_| Ok(()),
                || Err(ControlError::Unknown),
                || panic!("uncertain native state must not invent a release acknowledgement"),
            ),
            Err(ControlError::Unknown)
        );
        let remaining = sessions.inventory(&binding.principal, |_| Ok(())).unwrap();
        assert_eq!(remaining.sessions.len(), 1);
        assert_eq!(remaining.sessions[0].summary.target, healthy.target());
        assert!(
            remaining.unknown.is_empty(),
            "explicitly revoked private record must remain hidden"
        );
        drop((private_lifetime, healthy_lifetime));
    }

    #[test]
    fn imported_binding_namespace_is_stable_and_lost_state_never_reinitializes() {
        #[cfg(windows)]
        let private_root = storage::private_test_directory();
        #[cfg(windows)]
        let root = private_root.path().to_owned();
        #[cfg(unix)]
        let root = root();
        let sessions = SessionControl::existing_only(&root).unwrap();
        assert!(!sessions.has_binding_records().unwrap());
        let namespace = sessions.binding_namespace().unwrap();
        assert_eq!(sessions.binding_namespace().unwrap(), namespace);
        std::fs::remove_file(root.join(STATE_FILE)).unwrap();
        std::fs::remove_file(root.join("host-sessions.lock")).unwrap();
        assert!(sessions.binding_namespace().is_err());
        assert!(!root.join(STATE_FILE).exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn imported_binding_namespace_refuses_foreign_and_corrupt_anchor() {
        #[cfg(windows)]
        let private_root = storage::private_test_directory();
        #[cfg(windows)]
        let root = private_root.path().to_owned();
        #[cfg(unix)]
        let root = root();
        let sessions = SessionControl::existing_only(&root).unwrap();
        sessions.binding_namespace().unwrap();
        let directory = storage::private_directory(&root, false).unwrap();
        for bytes in [
            serde_json::to_vec(&OperationId::new()).unwrap(),
            b"broken".to_vec(),
        ] {
            storage::replace_private(&directory, Path::new("host-binding-namespace.json"), &bytes)
                .unwrap();
            assert!(sessions.has_binding_records().is_err());
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
