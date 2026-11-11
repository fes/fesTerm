//! App-local HOST originals. The private receipt anchors one native-vault
//! payload; neither ordinary profiles nor filesystem payloads are authority.
use super::*;
use approvals::{Error as StorageError, ReceiptIndex};
use base64::{engine::general_purpose::STANDARD_NO_PAD, Engine};
use festerm_host_protocol::{AttachmentId, IssuedOperation, LogicalSessionId, Validate};
use festerm_secret_store::{native_store, SecretBytes, SecretStore, MAX_SECRET_BYTES};
use festerm_tunnel_client::host_control::{
    IssuedControlOperation, ProtectedOriginalCheckpoint, ProtectedOriginalReopener,
};
use serde::{Deserialize, Serialize};
use std::path::Path;

const DOMAIN: &str = "fesTerm protected HOST client originals v1";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Blob {
    domain: String,
    reference: String,
    owner: OwnerBinding,
    key: String,
    endpoint: String,
    actions: Vec<Stored>,
    last_update: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Stored {
    id: String,
    review: String,
    body: Body,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "purpose", deny_unknown_fields)]
enum Body {
    Creation {
        ensure: bool,
        creation: Creation,
        issued: IssuedOperation,
        ticket: String,
        confirmed: Option<Target>,
    },
    End {
        operation: OperationId,
        target: Target,
    },
}

/// Selected only by the application sandbox and fresh protected context.
/// Native calls belong on its existing bounded foreground/background worker.
pub struct OriginalStore {
    index: ReceiptIndex,
    owner: OwnerBinding,
    key: SecretReference,
    endpoint: String,
    vault: Arc<dyn SecretStore>,
}

pub enum RecoveredIntent {
    Creation(Arc<CreationIntent>),
    End(Arc<EndIntent>),
}

pub struct RecoveredOriginal {
    pub review: String,
    pub intent: RecoveredIntent,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OriginalId {
    Creation(AttachmentId),
    End(OperationId),
}

impl OriginalId {
    fn key(&self) -> String {
        match self {
            Self::Creation(id) => id.to_string(),
            Self::End(id) => id.to_string(),
        }
    }

    fn matches_purpose(&self, body: &Body) -> bool {
        matches!(
            (self, body),
            (Self::Creation(_), Body::Creation { .. }) | (Self::End(_), Body::End { .. })
        )
    }
}

pub struct ReviewedForget {
    store: Arc<OriginalStore>,
    id: OriginalId,
    snapshot: Option<Stored>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ForgetOutcome {
    Removed,
    AlreadyMissing,
}

pub struct ForgottenOriginal {
    store: Arc<OriginalStore>,
    id: OriginalId,
    outcome: ForgetOutcome,
}

impl ForgottenOriginal {
    pub fn id(&self) -> &OriginalId {
        &self.id
    }

    pub fn outcome(&self) -> ForgetOutcome {
        self.outcome
    }

    pub fn matches_context(&self, context: &ApprovedContext) -> bool {
        self.store
            .matches(&context.approved, &context.reference, &context.binding)
    }
}

impl ReviewedForget {
    pub fn id(&self) -> &OriginalId {
        &self.id
    }

    pub fn matches_context(&self, context: &ApprovedContext) -> bool {
        self.store
            .matches(&context.approved, &context.reference, &context.binding)
    }

    pub fn logical(&self) -> Option<&LogicalSessionId> {
        self.snapshot.as_ref().map(|original| match &original.body {
            Body::Creation { creation, .. } => &creation.logical,
            Body::End { target, .. } => &target.logical,
        })
    }

    pub fn target(&self) -> Option<&Target> {
        self.snapshot
            .as_ref()
            .and_then(|original| match &original.body {
                Body::Creation { confirmed, .. } => confirmed.as_ref(),
                Body::End { target, .. } => Some(target),
            })
    }

    pub fn forget(
        self,
        context: &ApprovedContext,
        cancel: &CancellationToken,
    ) -> Result<ForgottenOriginal, Failure> {
        if !self.matches_context(context) {
            return Err(Failure::BindingMismatch);
        }
        if cancel.is_cancelled() {
            return Err(Error::Cancelled.into());
        }
        let outcome = self.store.access(false, |blob| {
            if cancel.is_cancelled() {
                return Ok((None, false));
            }
            let Some(index) = blob
                .actions
                .iter()
                .position(|item| item.id == self.id.key())
            else {
                return Ok((Some(ForgetOutcome::AlreadyMissing), false));
            };
            let current = &blob.actions[index];
            if !self.id.matches_purpose(&current.body) {
                return Err(StorageError::BindingMismatch);
            }
            let Some(snapshot) = &self.snapshot else {
                return Err(StorageError::BindingMismatch);
            };
            let current =
                Zeroizing::new(serde_json::to_vec(current).map_err(|_| StorageError::Storage)?);
            let snapshot =
                Zeroizing::new(serde_json::to_vec(snapshot).map_err(|_| StorageError::Storage)?);
            if *current != *snapshot {
                return Err(StorageError::BindingMismatch);
            }
            blob.actions.remove(index);
            Ok((Some(ForgetOutcome::Removed), true))
        })?;
        let outcome = outcome.ok_or_else(|| Failure::from(Error::Cancelled))?;
        if cancel.is_cancelled() {
            return Err(Error::Cancelled.into());
        }
        Ok(ForgottenOriginal {
            store: self.store,
            id: self.id,
            outcome,
        })
    }
}

pub(super) struct OriginalHandle {
    store: Arc<OriginalStore>,
    id: String,
    review: String,
}

fn payload_failure(error: festerm_secret_store::SecretStoreError) -> StorageError {
    use festerm_secret_store::SecretStoreError::{LockedOrUnavailable, Unsupported};
    match error {
        LockedOrUnavailable | Unsupported => StorageError::OwnerKey(error.into()),
        _ => StorageError::Storage,
    }
}

fn failed(error: StorageError) -> Failure {
    match error {
        StorageError::OwnerKey(error) => Failure::OwnerKey(error),
        StorageError::BindingMismatch => Failure::BindingMismatch,
        _ => Failure::OriginalStorage,
    }
}

impl OriginalStore {
    pub fn review_forget(
        self: &Arc<Self>,
        context: &ApprovedContext,
        id: OriginalId,
        cancel: &CancellationToken,
    ) -> Result<ReviewedForget, Failure> {
        if !self.matches(&context.approved, &context.reference, &context.binding) {
            return Err(Failure::BindingMismatch);
        }
        if cancel.is_cancelled() {
            return Err(Error::Cancelled.into());
        }
        let snapshot = self.access(false, |blob| {
            let original = blob.actions.iter().find(|item| item.id == id.key());
            if original.is_some_and(|item| !id.matches_purpose(&item.body)) {
                return Err(StorageError::BindingMismatch);
            }
            Ok((original.cloned(), false))
        })?;
        if cancel.is_cancelled() {
            return Err(Error::Cancelled.into());
        }
        Ok(ReviewedForget {
            store: self.clone(),
            id,
            snapshot,
        })
    }

    pub fn recover(
        root: &Path,
        context: &ApprovedContext,
    ) -> Result<(Arc<Self>, Vec<RecoveredOriginal>), Failure> {
        let vault = native_store().map_err(|error| Failure::OwnerKey(error.into()))?;
        Self::recover_with(root, context, Arc::from(vault))
    }

    fn recover_with(
        root: &Path,
        context: &ApprovedContext,
        vault: Arc<dyn SecretStore>,
    ) -> Result<(Arc<Self>, Vec<RecoveredOriginal>), Failure> {
        Self::recover_index(
            ReceiptIndex::in_app_sandbox(root).map_err(failed)?,
            context,
            vault,
        )
    }

    /// Explicit foreground recovery using the native-created application
    /// capability; subsequent accesses retain that same pinned namespace.
    #[cfg(unix)]
    pub fn recover_in_native_app_sandbox(
        root: Arc<festerm_secret_store::NativeAppPrivateRoot>,
        context: &ApprovedContext,
    ) -> Result<(Arc<Self>, Vec<RecoveredOriginal>), Failure> {
        let vault = native_store().map_err(|error| Failure::OwnerKey(error.into()))?;
        Self::recover_index(
            ReceiptIndex::in_native_app_sandbox(root),
            context,
            Arc::from(vault),
        )
    }

    fn recover_index(
        index: ReceiptIndex,
        context: &ApprovedContext,
        vault: Arc<dyn SecretStore>,
    ) -> Result<(Arc<Self>, Vec<RecoveredOriginal>), Failure> {
        let store = Arc::new(Self {
            index,
            owner: context.approved.clone(),
            key: context.reference.duplicate_for_transport(),
            endpoint: context.binding.endpoint().into(),
            vault,
        });
        let actions = store.access(true, |blob| Ok((blob.actions.clone(), false)))?;
        let mut recovered = Vec::new();
        for action in actions {
            let handle = store.handle(action.id, action.review.clone());
            let intent = match action.body {
                Body::Creation {
                    ensure,
                    creation,
                    issued,
                    confirmed,
                    ..
                } => RecoveredIntent::Creation(Arc::new(CreationIntent::from_original(
                    context,
                    if ensure {
                        CreationMode::Ensure
                    } else {
                        CreationMode::Create
                    },
                    creation,
                    handle,
                    issued,
                    confirmed,
                )?)),
                Body::End { operation, target } => RecoveredIntent::End(Arc::new(
                    EndIntent::from_original(context, target, operation, handle),
                )),
            };
            recovered.push(RecoveredOriginal {
                review: action.review,
                intent,
            });
        }
        Ok((store, recovered))
    }

    pub(super) fn matches(
        &self,
        owner: &OwnerBinding,
        key: &SecretReference,
        binding: &Binding,
    ) -> bool {
        self.owner == *owner
            && self.key == *key
            && self.owner.host.host == *binding.host()
            && self.owner.account == binding.account()
            && self.endpoint == binding.endpoint()
    }

    pub(super) fn handle(self: &Arc<Self>, id: String, review: String) -> Arc<OriginalHandle> {
        Arc::new(OriginalHandle {
            store: self.clone(),
            id,
            review,
        })
    }

    fn access<T>(
        &self,
        initialize: bool,
        action: impl FnOnce(&mut Blob) -> Result<(T, bool), StorageError>,
    ) -> Result<T, Failure> {
        self.index
            .with_originals(
                self.vault.as_ref(),
                &self.owner,
                &self.key,
                initialize,
                |reference, newly_reserved, commit| {
                    let mut blob = if newly_reserved {
                        let blob = Blob {
                            domain: DOMAIN.into(),
                            reference: reference.to_persisted_string(),
                            owner: self.owner.clone(),
                            key: self.key.to_persisted_string(),
                            endpoint: self.endpoint.clone(),
                            actions: Vec::new(),
                            last_update: None,
                        };
                        let bytes = Zeroizing::new(
                            serde_json::to_vec(&blob).map_err(|_| StorageError::Storage)?,
                        );
                        self.vault
                            .create_reserved(reference, &SecretBytes::copy_from_slice(&bytes))
                            .map_err(payload_failure)?;
                        blob
                    } else {
                        // A reserved/ready anchor never generates a missing payload.
                        let bytes = self.vault.get(reference).map_err(payload_failure)?;
                        if bytes.len() > MAX_SECRET_BYTES {
                            return Err(StorageError::Storage);
                        }
                        bytes
                            .with_bytes(|bytes| serde_json::from_slice::<Blob>(bytes))
                            .map_err(|_| StorageError::Storage)?
                    };
                    if blob.domain != DOMAIN
                        || blob.reference != reference.to_persisted_string()
                        || blob.owner != self.owner
                        || blob.key != self.key.to_persisted_string()
                        || blob.endpoint != self.endpoint
                        || blob.actions.len() > 32
                    {
                        return Err(StorageError::BindingMismatch);
                    }
                    for (index, original) in blob.actions.iter().enumerate() {
                        if original.review.len() > 16 * 1024
                            || blob.actions[..index]
                                .iter()
                                .any(|other| other.id == original.id)
                        {
                            return Err(StorageError::Storage);
                        }
                        match &original.body {
                            Body::Creation {
                                creation,
                                ticket,
                                confirmed,
                                ..
                            } => {
                                creation.validate().map_err(|_| StorageError::Storage)?;
                                if original.id != creation.attachment.to_string()
                                    || ticket.len() > festerm_host_protocol::CONTROL_FRAME_BYTES * 2
                                    || confirmed
                                        .as_ref()
                                        .is_some_and(|target| target.logical != creation.logical)
                                {
                                    return Err(StorageError::Storage);
                                }
                            }
                            Body::End { operation, .. } if original.id != operation.to_string() => {
                                return Err(StorageError::Storage);
                            }
                            Body::End { .. } => {}
                        }
                    }
                    let creations = blob
                        .actions
                        .iter()
                        .filter(|action| matches!(action.body, Body::Creation { .. }))
                        .count();
                    if creations > 16 || blob.actions.len() - creations > 16 {
                        return Err(StorageError::Storage);
                    }
                    if let Some(pending) = commit.pending() {
                        if blob.last_update.as_deref() != Some(pending) {
                            return Err(StorageError::Incomplete);
                        }
                        // The native payload proves which exact pending update
                        // committed; it is not inferred from filesystem absence.
                        commit.mark(None)?;
                    }
                    let (result, changed) = action(&mut blob)?;
                    if changed {
                        let nonce = OperationId::new().to_string();
                        blob.last_update = Some(nonce.clone());
                        let bytes = Zeroizing::new(
                            serde_json::to_vec(&blob).map_err(|_| StorageError::Storage)?,
                        );
                        if bytes.len() > MAX_SECRET_BYTES {
                            return Err(StorageError::Storage);
                        }
                        commit.mark(Some(nonce))?;
                        self.vault
                            .update(reference, &SecretBytes::copy_from_slice(&bytes))
                            .map_err(payload_failure)?;
                        commit.mark(None)?;
                    }
                    Ok(result)
                },
            )
            .map_err(failed)
    }
}

impl OriginalHandle {
    pub(super) fn agrees(&self, store: &OriginalStore, review: &str) -> bool {
        self.store.index.same_storage(&store.index)
            && self.review == review
            && self.store.owner == store.owner
            && self.store.key == store.key
            && self.store.endpoint == store.endpoint
    }

    pub(super) fn capture_creation(
        &self,
        mode: CreationMode,
        creation: &Creation,
        ticket: &IssuedControlOperation,
        request: &Request,
    ) -> Result<(), Failure> {
        let checkpoint = ticket.protected_checkpoint(request)?;
        let encoded = STANDARD_NO_PAD.encode(checkpoint.copy_for_protected_store().as_slice());
        self.capture(Body::Creation {
            ensure: mode == CreationMode::Ensure,
            creation: creation.clone(),
            issued: ticket.operation().clone(),
            ticket: encoded,
            confirmed: None,
        })
    }

    pub(super) fn capture_end(
        &self,
        operation: &OperationId,
        target: &Target,
    ) -> Result<(), Failure> {
        self.capture(Body::End {
            operation: operation.clone(),
            target: target.clone(),
        })
    }

    fn capture(&self, body: Body) -> Result<(), Failure> {
        self.store.access(false, |blob| {
            if let Some(original) = blob.actions.iter().find(|action| action.id == self.id) {
                let same = match (&original.body, &body) {
                    (
                        Body::Creation {
                            ensure: a,
                            creation: b,
                            issued: c,
                            ticket: d,
                            ..
                        },
                        Body::Creation {
                            ensure: e,
                            creation: f,
                            issued: g,
                            ticket: h,
                            ..
                        },
                    ) => a == e && b == f && c == g && d == h,
                    (
                        Body::End {
                            operation: a,
                            target: b,
                        },
                        Body::End {
                            operation: c,
                            target: d,
                        },
                    ) => a == c && b == d,
                    _ => false,
                };
                if !same || original.review != self.review {
                    return Err(StorageError::BindingMismatch);
                }
                return Ok(((), false));
            }
            let creation = matches!(body, Body::Creation { .. });
            if self.review.len() > 16 * 1024
                || blob
                    .actions
                    .iter()
                    .filter(|action| matches!(action.body, Body::Creation { .. }) == creation)
                    .count()
                    >= 16
            {
                return Err(StorageError::Storage);
            }
            blob.actions.push(Stored {
                id: self.id.clone(),
                review: self.review.clone(),
                body,
            });
            Ok(((), true))
        })
    }

    pub(super) fn confirmed_creation(&self, target: &Target) -> Result<(), Failure> {
        self.store.access(false, |blob| {
            let original = blob
                .actions
                .iter_mut()
                .find(|action| action.id == self.id)
                .ok_or(StorageError::Storage)?;
            let Body::Creation {
                creation,
                confirmed,
                ..
            } = &mut original.body
            else {
                return Err(StorageError::BindingMismatch);
            };
            if creation.logical != target.logical
                || confirmed.as_ref().is_some_and(|old| old != target)
            {
                return Err(StorageError::BindingMismatch);
            }
            *confirmed = Some(target.clone());
            Ok(((), true))
        })
    }

    pub(super) fn confirmed_end(
        &self,
        operation: &OperationId,
        target: &Target,
    ) -> Result<(), Failure> {
        self.store.access(false, |blob| {
            if !blob.actions.iter().any(|action| action.id == self.id
                && matches!(&action.body, Body::End { operation: op, target: old } if op == operation && old == target))
            {
                return Err(StorageError::BindingMismatch);
            }
            blob.actions.retain(|action| action.id != self.id
                && !matches!(&action.body, Body::Creation { confirmed: Some(old), .. } if old == target));
            Ok(((), true))
        })
    }
}

impl ProtectedOriginalCheckpoint for OriginalHandle {
    type Error = Failure;

    fn reopen(
        &self,
        factory: ProtectedOriginalReopener<'_>,
    ) -> Result<IssuedControlOperation, Failure> {
        self.store.access(false, |blob| {
            let original = blob
                .actions
                .iter()
                .find(|action| action.id == self.id)
                .ok_or(StorageError::Storage)?;
            if original.review != self.review {
                return Err(StorageError::BindingMismatch);
            }

            let Body::Creation { ticket, issued, .. } = &original.body else {
                return Err(StorageError::BindingMismatch);
            };
            let bytes = Zeroizing::new(
                STANDARD_NO_PAD
                    .decode(ticket)
                    .map_err(|_| StorageError::Storage)?,
            );
            let reopened = factory
                .from_native_payload(&bytes)
                .map_err(|_| StorageError::BindingMismatch)?;
            if reopened.operation() != issued {
                return Err(StorageError::BindingMismatch);
            }
            Ok((reopened, false))
        })
    }
}

#[cfg(all(test, unix))]
#[path = "originals_tests.rs"]
mod tests;

#[cfg(all(feature = "testing", unix))]
pub mod testing {
    use super::*;

    /// Composes existing receipt/checkpoint paths with a synthetic memory vault.
    /// Never selects a native platform store or decodes caller-issued authority.
    pub fn memory_fixture(
        root: &Path,
        context: ApprovedContext,
    ) -> Result<
        (
            approvals::Receipt,
            festerm_tunnel_client::service::ResolvedTunnel,
            Arc<OriginalStore>,
        ),
        Failure,
    > {
        memory_fixture_with_store(
            root,
            context,
            Arc::new(festerm_secret_store::MemorySecretStore::new()),
        )
    }

    pub fn memory_fixture_with_store(
        root: &Path,
        mut context: ApprovedContext,
        vault: Arc<festerm_secret_store::MemorySecretStore>,
    ) -> Result<
        (
            approvals::Receipt,
            festerm_tunnel_client::service::ResolvedTunnel,
            Arc<OriginalStore>,
        ),
        Failure,
    > {
        let reviewed = approvals::ReviewedOwner::confirm(
            &serde_json::to_string(&context.approved).map_err(|_| Failure::OriginalStorage)?,
            &context.approved.host.host,
            &context.approved.account,
            &context.approved.host.host_key_fingerprint,
        )
        .map_err(failed)?;
        let receipt = ReceiptIndex::in_app_sandbox(root)
            .map_err(failed)?
            .prepare_with(vault.as_ref(), reviewed)
            .map_err(failed)?;
        context.reference = receipt.reference();
        let resolved = festerm_tunnel_client::service::ResolvedTunnel {
            binding: festerm_tunnel_client::service::HostBinding {
                tunnel_id: "fixture".into(),
                cluster: "test".into(),
                host_id: context.endpoint.host_id.clone(),
                public_key: context.endpoint.host_public_keys[0].clone(),
                port: context.port,
            },
            endpoint: context.endpoint.clone(),
            connect_token: context.token.clone(),
        };
        let (store, originals) = OriginalStore::recover_with(root, &context, vault)?;
        if !originals.is_empty() {
            return Err(Failure::OriginalStorage);
        }
        Ok((receipt, resolved, store))
    }

    pub fn reopen(
        store: &OriginalStore,
        context: &ApprovedContext,
    ) -> Result<(Arc<OriginalStore>, Vec<RecoveredOriginal>), Failure> {
        OriginalStore::recover_index(store.index.clone(), context, store.vault.clone())
    }
}
