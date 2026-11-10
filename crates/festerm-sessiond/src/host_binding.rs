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

impl SessionControl {
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
                let recipe = Recipe {
                    shell: None,
                    cwd: None,
                    args: Vec::new(),
                };
                let record = Record {
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
                };
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
        mut admit: impl FnMut(&Binding) -> Result<G, ControlError>,
    ) -> Result<Receipt, ControlError> {
        if binding.control_root != self.root {
            return Err(ControlError::Denied);
        }
        {
            let mut tx = self.transaction()?;
            let _guard = admit(binding)?;
            if tx.state.namespace.as_ref() != Some(&binding.namespace) {
                return Err(ControlError::Stale);
            }
            let record = tx.state.sessions.get_mut(&binding.logical);
            if let Some(record) = record {
                let imported = record.binding.as_ref().ok_or(ControlError::Denied)?;
                if imported.binding != *binding {
                    return Err(ControlError::Denied);
                }
                if imported.lease_reserved {
                    let guard = LeaseGuard::acquire(&binding.runtime_root, record.generation)
                        .map_err(storage_error)?;
                    let mut lease = guard.read().map_err(storage_error)?;
                    if !record.lease_matches(&lease) || lease.stopped {
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
                    tx.save()?;
                }
            }
        }
        let receipt = native_binding::release(binding).map_err(uncertain)?;
        if receipt.binding != *binding || receipt.phase != BindingPhase::Released {
            return Err(ControlError::Unknown);
        }
        let mut tx = self.transaction()?;
        let _guard = admit(binding)?;
        let Some(record) = tx.state.sessions.get_mut(&binding.logical) else {
            return Ok(receipt);
        };
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

    fn root() -> PathBuf {
        std::env::current_dir()
            .unwrap()
            .join(format!("target/binding-unit-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn imported_binding_namespace_is_stable_and_lost_state_never_reinitializes() {
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
