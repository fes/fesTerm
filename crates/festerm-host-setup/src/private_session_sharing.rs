//! Explicit private local generation sharing into the current Desktop host.
//! Consent and recovery use the existing protected SessionControl records.
use crate::{
    enrollment::{OwnerEnrollment, PrivateSessionPolicy},
    lifecycle::{Origin, Phase, Snapshot, Store},
};
use festerm_host_protocol::{Epoch, LogicalSessionId, OperationId};
use festerm_sessiond::{
    host_control::{ControlError, SessionControl},
    native_binding::{self, Binding, Receipt, SourceAuthority},
    private_storage as storage, LocalSessionTarget,
};
use std::{io, path::Path};

#[derive(Clone, Debug)]
pub struct Consent {
    pub expected: Snapshot,
    pub owner_revision: Epoch,
    pub operation: OperationId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    Unsupported,
    Denied,
    Busy,
    Stale,
    Invalid,
    Conflict,
    Revoked,
    Unknown,
    OutcomeUncertain,
    /// Cancellation can follow partial publication. Look up/release the original operation.
    Cancelled,
}

impl From<ControlError> for Error {
    fn from(error: ControlError) -> Self {
        match error {
            ControlError::Busy => Self::Busy,
            ControlError::Stale => Self::Stale,
            ControlError::Revoked => Self::Revoked,
            ControlError::Invalid => Self::Invalid,
            ControlError::RecipeConflict
            | ControlError::OperationConflict
            | ControlError::AlreadyExists => Self::Conflict,
            ControlError::CommittedButNotPublished => Self::OutcomeUncertain,
            ControlError::Cancelled => Self::Cancelled,
            ControlError::Unknown | ControlError::Storage | ControlError::Ended => Self::Unknown,
            ControlError::Denied => Self::Denied,
        }
    }
}

fn authority_error(error: io::Error) -> Error {
    if error.kind() == io::ErrorKind::WouldBlock {
        Error::Busy
    } else {
        Error::Denied
    }
}

fn native_error(error: io::Error) -> Error {
    match error.kind() {
        io::ErrorKind::WouldBlock => Error::Busy,
        io::ErrorKind::Unsupported => Error::Unsupported,
        _ => Error::Unknown,
    }
}

fn control_error(error: Error) -> ControlError {
    match error {
        Error::Busy => ControlError::Busy,
        Error::Stale => ControlError::Stale,
        Error::Cancelled => ControlError::Cancelled,
        Error::Revoked => ControlError::Revoked,
        Error::Invalid => ControlError::Invalid,
        Error::Conflict => ControlError::OperationConflict,
        Error::OutcomeUncertain => ControlError::CommittedButNotPublished,
        Error::Unknown | Error::Unsupported => ControlError::Unknown,
        Error::Denied => ControlError::Denied,
    }
}

pub struct Sharing<'a> {
    owners: OwnerEnrollment<'a>,
    sessions: SessionControl,
    root: std::path::PathBuf,
}

impl<'a> Sharing<'a> {
    /// Existing NativeVault/Desktop authority only; never setup or enrollment.
    pub fn current(store: &'a Store, state: &Path) -> Result<Self, Error> {
        Self::open(
            store,
            state,
            store.owner_enrollment().map_err(authority_error)?,
        )
    }

    pub(crate) fn open(
        store: &'a Store,
        state: &Path,
        owners: OwnerEnrollment<'a>,
    ) -> Result<Self, Error> {
        let selected = storage::private_directory(state, false).map_err(authority_error)?;
        store
            .with_locked(|directory, _| {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    let actual = directory.metadata()?;
                    let proposed = selected.metadata()?;
                    if (actual.dev(), actual.ino()) != (proposed.dev(), proposed.ino()) {
                        return Err(io::Error::other("sharing.foreign_state"));
                    }
                }
                #[cfg(windows)]
                if !festerm_windows_security::same_file_identity(directory, &selected)? {
                    return Err(io::Error::other("sharing.foreign_state"));
                }
                Ok(())
            })
            .map_err(authority_error)?;
        if store.snapshot().map_err(authority_error)?.origin != Origin::Desktop {
            return Err(Error::Denied);
        }
        let root = state.join("daemon");
        Ok(Self {
            sessions: SessionControl::retained_namespace(&root).map_err(Error::from)?,
            owners,
            root,
        })
    }

    fn policy(&self) -> Result<PrivateSessionPolicy, Error> {
        self.owners
            .private_session_policy(self.root.parent().ok_or(Error::Invalid)?)
            .map_err(authority_error)
    }

    fn consent(&self, consent: &Consent) -> Result<PrivateSessionPolicy, Error> {
        let policy = self.policy()?;
        if policy.installation != consent.expected
            || policy.enrollment.revision != consent.owner_revision
        {
            return Err(Error::Stale);
        }
        active(&policy)?;
        Ok(policy)
    }

    fn admission(&self, binding: &Binding, reduction: bool) -> Result<PrivateSessionPolicy, Error> {
        binding.validate().map_err(|_| Error::Invalid)?;
        if !reduction {
            native_binding::verify_private_source(binding).map_err(native_error)?;
        }
        let policy = self.policy()?;
        let owner = &policy.enrollment;
        if binding.source_authority.is_legacy()
            || binding.control_root != self.root
            || binding.installation != policy.installation.installation
            || binding.host != owner.binding.host.host
            || binding.host_key_fingerprint != owner.binding.host.host_key_fingerprint
            || binding.principal != owner.binding.principal
            || binding.account != owner.binding.account
        {
            return Err(Error::Denied);
        }
        let epoch = policy
            .installation
            .epoch
            .checked_add(1)
            .ok_or(Error::Invalid)?;
        if owner.revision.get() < binding.owner_epoch.get() || epoch < binding.host_epoch.get() {
            return Err(Error::Stale);
        }
        if !reduction {
            active(&policy)?;
            if owner.revision != binding.owner_epoch || epoch != binding.host_epoch.get() {
                return Err(Error::Stale);
            }
        }
        Ok(policy)
    }

    /// Phase is a protected journal observation, never admission or native readiness.
    pub fn lookup(&self, operation: &OperationId) -> Result<Option<Receipt>, Error> {
        drop(self.policy()?);
        let directory = match storage::private_directory(&self.root, false) {
            Ok(directory) => directory,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(authority_error(error)),
        };
        match storage::read_private(&directory, Path::new("host-sessions.json"), 4 * 1024 * 1024) {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return match storage::read_private(
                    &directory,
                    Path::new("host-binding-namespace.json"),
                    256,
                ) {
                    Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
                    _ => Err(Error::Unknown),
                };
            }
            Err(error) => return Err(authority_error(error)),
        }
        let found = self
            .sessions
            .private_binding(operation)
            .map_err(Error::from)?;
        if let Some(receipt) = &found {
            let _guard = self.admission(&receipt.binding, true)?;
        } else {
            drop(self.policy()?);
        }
        Ok(found)
    }

    pub fn bind(
        &self,
        consent: &Consent,
        target: &LocalSessionTarget,
        mut is_current: impl FnMut() -> bool,
    ) -> Result<Receipt, Error> {
        if !is_current() {
            return Err(Error::Cancelled);
        }
        {
            let _guard = self.consent(consent)?;
            if !is_current() {
                return Err(Error::Cancelled);
            }
            SessionControl::existing_only(&self.root).map_err(Error::from)?;
        }
        let binding = if let Some(previous) = self.lookup(&consent.operation)? {
            if previous.binding.runtime_root != target.runtime_root()
                || previous.binding.identity != *target.identity()
            {
                return Err(Error::Conflict);
            }
            previous.binding
        } else {
            let fingerprint = native_binding::private_fingerprint(target).map_err(native_error)?;
            let (namespace, issuance_epoch) = {
                let _guard = self.consent(consent)?;
                if !is_current() {
                    return Err(Error::Cancelled);
                }
                self.sessions.binding_namespace().map_err(Error::from)?
            };
            let policy = self.consent(consent)?;
            let owner = &policy.enrollment;
            let mut binding = Binding {
                operation: consent.operation.clone(),
                host: owner.binding.host.host.clone(),
                installation: policy.installation.installation.clone(),
                account: owner.binding.account.clone(),
                host_key_fingerprint: owner.binding.host.host_key_fingerprint.clone(),
                principal: owner.binding.principal.clone(),
                owner_epoch: owner.revision,
                host_epoch: Epoch::new(
                    policy
                        .installation
                        .epoch
                        .checked_add(1)
                        .ok_or(Error::Invalid)?,
                )
                .map_err(|_| Error::Invalid)?,
                issuance_epoch,
                namespace,
                control_root: self.root.clone(),
                logical: LogicalSessionId::new(),
                source_authority: SourceAuthority::PrivateSessionV1 { tag: String::new() },
                source_installation: String::new(),
                source_host: owner.binding.host.host.clone(),
                source_key_fingerprint: String::new(),
                source_receipt_digest: String::new(),
                source_root: target.runtime_root().into(),
                source_profile: false,
                source_installation_digest: String::new(),
                source_ownership_digest: String::new(),
                source_key_digest: String::new(),
                source_claim_digest: String::new(),
                runtime_root: target.runtime_root().into(),
                identity: target.identity().clone(),
                spawn_fingerprint: fingerprint,
            };
            drop(policy);
            if !is_current() {
                return Err(Error::Cancelled);
            }
            native_binding::authenticate_private_source(&mut binding).map_err(native_error)?;
            binding
        };
        self.sessions
            .bind_existing(&binding, |binding| {
                if !is_current() {
                    return Err(ControlError::Cancelled);
                }
                let policy = self.consent(consent).map_err(control_error)?;
                drop(policy);
                self.admission(binding, false).map_err(control_error)
            })
            .map_err(Error::from)
    }

    fn original(&self, supplied: &Receipt) -> Result<Receipt, Error> {
        let original = self
            .lookup(&supplied.binding.operation)?
            .ok_or(Error::Unknown)?;
        if supplied.version != 1 || supplied.binding != original.binding {
            return Err(Error::Denied);
        }
        Ok(original)
    }

    pub fn switch(
        &self,
        receipt: &Receipt,
        mut is_current: impl FnMut() -> bool,
    ) -> Result<Receipt, Error> {
        let original = self.original(receipt)?;
        self.sessions
            .switch_binding(&original.binding, |binding| {
                if !is_current() {
                    return Err(ControlError::Cancelled);
                }
                self.admission(binding, false).map_err(control_error)
            })
            .map_err(Error::from)
    }

    /// Exact irreversible reduction; later owner revocation does not block cleanup.
    pub fn release(&self, receipt: &Receipt) -> Result<Receipt, Error> {
        let original = self.original(receipt)?;
        self.sessions
            .release_existing(&original.binding, |binding| {
                self.admission(binding, true).map_err(control_error)
            })
            .map_err(Error::from)
    }

    pub fn rollback(&self, receipt: &Receipt) -> Result<Receipt, Error> {
        self.release(receipt)
    }
}

#[cfg(test)]
#[path = "private_session_sharing_tests.rs"]
mod tests;

fn active(policy: &PrivateSessionPolicy) -> Result<(), Error> {
    let owner = &policy.enrollment;
    if policy.installation.phase != Phase::Provisioned
        || !owner.remote_access
        || !owner.keys.iter().any(|key| !key.revoked)
        || !owner.capabilities.inventory
        || !owner.capabilities.attach
        || !owner.capabilities.detach
    {
        return Err(Error::Revoked);
    }
    Ok(())
}
