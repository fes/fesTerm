//! Prepared provider credentials and fresh protected ordinary policy.
//! No owner verifier or enrolled key is required for ordinary remoting.
use crate::{
    cloud::{CloudHostAdapter, PreparedOrdinaryObserver},
    enrollment::{CurrentOwnerPolicy, CurrentOwnerPolicySource, EnrollmentSnapshot, OwnerStorage},
    lifecycle::{OrdinaryAccess, Phase, Snapshot, Store},
    protected_state::Lease,
};
use festerm_host_protocol::{Capabilities, ContractError, Epoch, HostId, Validate};
use festerm_sessiond::private_storage as storage;
use festerm_tunnel_bridge::{
    host_control::{AuthorityKind, PolicyGuard, PolicyView, ProtectedPolicySource},
    ordinary_auth::{OrdinaryHostInfo, OrdinaryPolicy, OrdinaryRoute, ORDINARY_VERSION},
    owner_auth::SessionPolicy,
};
use sha2::{Digest, Sha256};
use std::{io, path::Path, sync::Arc};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ReadinessEvent {
    OwnerDigestChanged,
    CloudDigestChanged,
    IdentityNotReadyBefore,
    IdentityNotReadyAfter,
    RenewalOk,
    RenewalError,
}
type ReadinessDiagnostic = Arc<dyn Fn(ReadinessEvent, Option<&io::Error>) + Send + Sync>;

struct Prepared {
    expected: Snapshot,
    owner_digest: [u8; 32],
    route: OrdinaryRoute,
    observer: PreparedOrdinaryObserver,
    refresh_reported: std::sync::atomic::AtomicU8,
}

#[derive(Debug)]
struct ReadinessRefreshRequired(ReadinessEvent);
impl std::fmt::Display for ReadinessRefreshRequired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("host.ordinary_readiness_refresh_required")
    }
}
impl std::error::Error for ReadinessRefreshRequired {}
impl Prepared {
    fn require_refresh(&self, diagnostic: Option<&ReadinessDiagnostic>, reason: ReadinessEvent) {
        report_readiness_once(&self.refresh_reported, diagnostic, reason);
    }
}

fn report_readiness_once(
    reported: &std::sync::atomic::AtomicU8,
    diagnostic: Option<&ReadinessDiagnostic>,
    reason: ReadinessEvent,
) {
    let bit = match reason {
        ReadinessEvent::OwnerDigestChanged => 1,
        ReadinessEvent::CloudDigestChanged => 2,
        ReadinessEvent::IdentityNotReadyBefore => 4,
        ReadinessEvent::IdentityNotReadyAfter => 8,
        ReadinessEvent::RenewalOk | ReadinessEvent::RenewalError => return,
    };
    if reported.fetch_or(bit, std::sync::atomic::Ordering::Relaxed) & bit == 0 {
        // Caller has already released its protected policy guard. At most
        // one event per closed reason and prepared Arc, never per tick.
        if let Some(diagnostic) = diagnostic {
            diagnostic(reason, None);
        }
        eprintln!("host.ordinary_readiness_refresh_required: disable then enable remote access to reload protected policy/account readiness; existing shells are retained");
    }
}

pub struct CurrentHostPolicySource {
    store: Arc<Store>,
    owner: CurrentOwnerPolicySource,
    storage: OwnerStorage,
    ordinary: std::sync::RwLock<Option<Arc<Prepared>>>,
    diagnostic: Option<ReadinessDiagnostic>,
}

pub struct CurrentHostPolicy(CurrentPolicy);

enum CurrentPolicy {
    Owner(CurrentOwnerPolicy),
    Ordinary {
        _lease: Lease,
        policy: Box<OrdinaryPolicy>,
    },
}

impl PolicyGuard for CurrentHostPolicy {
    fn view(&self) -> PolicyView<'_> {
        match &self.0 {
            CurrentPolicy::Owner(p) => PolicyView::Owner(p.verifier()),
            CurrentPolicy::Ordinary { policy, .. } => PolicyView::Ordinary(policy),
        }
    }
}

impl CurrentHostPolicySource {
    /// Provider credentials and initial readiness precede accepting clients.
    /// Each protected policy use separately rechecks the native policy anchor.
    pub async fn prepare(
        store: Arc<Store>,
        storage_kind: OwnerStorage,
        expected: &Snapshot,
        cancel: &CancellationToken,
    ) -> io::Result<Self> {
        if store.snapshot()? != *expected || cancel.is_cancelled() {
            return Err(io::Error::other("host.ordinary_scope_changed"));
        }
        let expected = expected.clone();
        let owner = CurrentOwnerPolicySource::new(store.clone(), storage_kind);
        let ordinary = if expected.settings.ordinary_access == OrdinaryAccess::ProviderOwner {
            let enrollment = store
                .owner_enrollment_with_storage(storage_kind)?
                .snapshot()?;
            let observer = CloudHostAdapter::prepare_ordinary_observer(
                store.clone(),
                expected.clone(),
                None,
                None,
                cancel,
            )
            .await?;
            Some(Self::prepared(&store, expected, enrollment, observer)?)
        } else {
            None
        };
        Ok(Self {
            store,
            owner,
            storage: storage_kind,
            ordinary: std::sync::RwLock::new(ordinary.map(Arc::new)),
            diagnostic: None,
        })
    }

    pub(crate) fn with_diagnostic(mut self, diagnostic: ReadinessDiagnostic) -> Self {
        self.diagnostic = Some(diagnostic);
        self
    }

    /// Prepare only the immutable worker scope, outside wire admission. The
    /// protected lease and Arc compare make publication one fenced operation.
    pub async fn refresh(
        self: &Arc<Self>,
        expected: &Snapshot,
        cancel: &CancellationToken,
    ) -> io::Result<()> {
        let previous = self
            .ordinary
            .read()
            .map_err(|_| io::Error::other("host.ordinary_context_unavailable"))?
            .clone();
        let Some(previous) = previous else {
            if self.store.snapshot()? != *expected
                || cancel.is_cancelled()
                || expected.settings.ordinary_access == OrdinaryAccess::ProviderOwner
            {
                return Err(io::Error::other("host.ordinary_scope_changed"));
            }
            return Ok(());
        };
        if cancel.is_cancelled() {
            return Err(crate::cloud::cancelled_account_work());
        }
        if previous.expected != *expected {
            return Err(io::Error::other("host.ordinary_scope_changed"));
        }
        // Reject changed native owner policy without blocking the relay loop.
        let source = self.clone();
        let current = expected.clone();
        let old = previous.clone();
        crate::cloud::owned_blocking(move || {
            drop(Self::project_ordinary_current(
                &source.store.owner_enrollment_with_storage(source.storage)?,
                &current,
                old.owner_digest,
                old.observer.record_digest,
                old.observer.resource(),
            )?);
            Ok(())
        })
        .await?;
        let observer = CloudHostAdapter::prepare_ordinary_observer(
            self.store.clone(),
            expected.clone(),
            Some(previous.observer.record_digest),
            Some(previous.owner_digest),
            cancel,
        )
        .await?;
        let source = self.clone();
        let current = expected.clone();
        let publication_cancel = cancel.clone();
        crate::cloud::owned_blocking(move || {
            let enrollment = source
                .store
                .owner_enrollment_with_storage(source.storage)?
                .snapshot()?;
            let next = Self::prepared(&source.store, current.clone(), enrollment, observer)?;
            if next.owner_digest != previous.owner_digest
                || next.route != previous.route
                || !next.observer.same_scope(&previous.observer)
                || !next.observer.is_ready()
            {
                return Err(io::Error::other("host.ordinary_policy_changed"));
            }
            let enrollment = source.store.owner_enrollment_with_storage(source.storage)?;
            let _guard = Self::project_ordinary_current(
                &enrollment,
                &current,
                previous.owner_digest,
                previous.observer.record_digest,
                previous.observer.resource(),
            )?;
            publish_same_arc(&source.ordinary, &previous, next, &publication_cancel)
        })
        .await
    }

    /// Controller-owned maintenance; admission performs no credential refresh.
    /// A short expired interval remains fail-closed until a ready replacement
    /// is published. Success retains the relay, runtime and aggregate budgets.
    pub async fn maintain(
        self: &Arc<Self>,
        expected: &Snapshot,
        cancel: &CancellationToken,
    ) -> io::Result<()> {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(1));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => return Ok(()),
                _ = tick.tick() => {}
            }
            let expired = self
                .ordinary
                .read()
                .map_err(|_| io::Error::other("host.ordinary_context_unavailable"))?
                .as_ref()
                .is_some_and(|prepared| !prepared.observer.is_ready());
            if expired {
                // Never drop a renewal with an owned native task in flight.
                // The controller cancels and drains this maintenance future.
                let refreshed = self.refresh(expected, cancel).await;
                if let Some(diagnostic) = &self.diagnostic {
                    diagnostic(
                        if refreshed.is_ok() {
                            ReadinessEvent::RenewalOk
                        } else {
                            ReadinessEvent::RenewalError
                        },
                        refreshed.as_ref().err(),
                    );
                }
                refreshed.map_err(crate::cloud::terminal_ordinary_renewal)?;
            }
        }
    }

    fn prepared(
        store: &Store,
        expected: Snapshot,
        enrollment: EnrollmentSnapshot,
        observer: PreparedOrdinaryObserver,
    ) -> io::Result<Prepared> {
        let policy = Self::policy_for(&expected, &enrollment, observer.resource())?;
        let owner_digest = store.with_locked(|directory, current| {
            if current != &expected {
                return Err(io::Error::other("host.ordinary_scope_changed"));
            }
            let bytes =
                storage::read_private(directory, Path::new("owner-enrollment.json"), 16 * 1024)?;
            let record: EnrollmentSnapshot = serde_json::from_slice(&bytes)
                .map_err(|_| io::Error::other("host.ordinary_policy_changed"))?;
            if record != enrollment {
                return Err(io::Error::other("host.ordinary_policy_changed"));
            }
            Ok(Sha256::digest(bytes).into())
        })?;
        Ok(Prepared {
            expected,
            owner_digest,
            route: policy.host_info.route,
            observer,
            refresh_reported: 0.into(),
        })
    }

    fn policy_for(
        expected: &Snapshot,
        enrollment: &EnrollmentSnapshot,
        resource: &festerm_tunnel_client::host::Resource,
    ) -> io::Result<OrdinaryPolicy> {
        if expected.phase != Phase::Provisioned
            || expected.settings.ordinary_access != OrdinaryAccess::ProviderOwner
            || resource.create_access != festerm_tunnel_client::host::CreateAccess::OwnerDefault
            || enrollment.binding.installation != expected.installation
        {
            return Err(io::Error::other("host.ordinary_scope_changed"));
        }
        let route = OrdinaryRoute {
            tunnel_id: resource.tunnel_id.clone(),
            cluster: resource.cluster.clone(),
            endpoint_id: format!("{}-relay", enrollment.binding.host.host),
            port: resource.port,
            host: enrollment.binding.host.clone(),
        };
        route.validate().map_err(io::Error::from)?;
        let policy = OrdinaryPolicy {
            host_info: OrdinaryHostInfo {
                version: ORDINARY_VERSION,
                route,
                installation: HostId::try_from(expected.installation.clone())
                    .map_err(io::Error::from)?,
                principal: enrollment.binding.principal.clone(),
                policy_epoch: enrollment.revision,
            },
            session: SessionPolicy {
                host: enrollment.binding.host.clone(),
                principal: enrollment.binding.principal.clone(),
                host_epoch: Epoch::new(
                    expected
                        .epoch
                        .checked_add(1)
                        .ok_or_else(|| io::Error::other("host.epoch_exhausted"))?,
                )
                .map_err(io::Error::from)?,
                grant_epoch: enrollment.revision,
                remote_access: true,
                capabilities: Capabilities {
                    inventory: true,
                    attach: true,
                    detach: true,
                    profiles: true,
                    ensure_profile: true,
                    ensure: true,
                    create: expected.settings.allow_creation,
                    creation_allowed: expected.settings.allow_creation,
                    explicit_end: expected.settings.allow_session_termination,
                    ..Capabilities::default()
                },
            },
            files: Some(festerm_tunnel_bridge::file_protocol::HostPolicy {
                scope: festerm_tunnel_bridge::file_protocol::HostScope {
                    host: enrollment.binding.host.host.clone(),
                    installation: HostId::try_from(expected.installation.clone())
                        .map_err(io::Error::from)?,
                    epoch: expected.epoch.to_string(),
                },
                filesystem: expected.settings.filesystem.clone(),
            }),
            denied_keys: enrollment
                .keys
                .iter()
                .filter(|k| k.revoked)
                .map(|k| k.public_key)
                .collect(),
        };
        policy.validate().map_err(io::Error::from)?;
        Ok(policy)
    }

    fn project_ordinary_current(
        enrollment: &crate::enrollment::OwnerEnrollment<'_>,
        expected: &Snapshot,
        owner_digest: [u8; 32],
        cloud_digest: [u8; 32],
        resource: &festerm_tunnel_client::host::Resource,
    ) -> io::Result<CurrentHostPolicy> {
        enrollment.with_current_snapshot_noninteractive(|directory, current, fresh, lease| {
            if current != expected
                || current.phase != Phase::Provisioned
                || current.settings.ordinary_access != OrdinaryAccess::ProviderOwner
            {
                return Err(io::Error::other("host.ordinary_policy_changed"));
            }
            let owner: [u8; 32] = Sha256::digest(storage::read_private(
                directory,
                Path::new("owner-enrollment.json"),
                16 * 1024,
            )?)
            .into();
            let cloud: [u8; 32] = Sha256::digest(storage::read_private(
                directory,
                Path::new("host-cloud.json"),
                16 * 1024,
            )?)
            .into();
            if owner != owner_digest {
                return Err(io::Error::other(ReadinessRefreshRequired(
                    ReadinessEvent::OwnerDigestChanged,
                )));
            }
            if cloud != cloud_digest {
                return Err(io::Error::other(ReadinessRefreshRequired(
                    ReadinessEvent::CloudDigestChanged,
                )));
            }
            Ok(CurrentHostPolicy(CurrentPolicy::Ordinary {
                _lease: lease,
                policy: Box::new(Self::policy_for(current, fresh, resource)?),
            }))
        })
    }

    fn current_ordinary(&self) -> Result<CurrentHostPolicy, ContractError> {
        let prepared = self
            .ordinary
            .read()
            .map_err(|_| ContractError::Denied)?
            .clone()
            .ok_or(ContractError::Denied)?;
        self.store
            .owner_enrollment_with_storage(self.storage)
            .and_then(|enrollment| {
                Self::project_ordinary_current(
                    &enrollment,
                    &prepared.expected,
                    prepared.owner_digest,
                    prepared.observer.record_digest,
                    prepared.observer.resource(),
                )
            })
            .map_err(|e| {
                if e.kind() == io::ErrorKind::WouldBlock {
                    ContractError::Busy
                } else if let Some(cause) = e
                    .get_ref()
                    .and_then(|cause| cause.downcast_ref::<ReadinessRefreshRequired>())
                {
                    // Projection returned, dropping its protected policy lease.
                    prepared.require_refresh(self.diagnostic.as_ref(), cause.0);
                    ContractError::Stale
                } else {
                    ContractError::Denied
                }
            })
    }
}

// Called only while the exact current protected policy lease remains held.
fn publish_same_arc<T>(
    slot: &std::sync::RwLock<Option<Arc<T>>>,
    previous: &Arc<T>,
    next: T,
    cancel: &CancellationToken,
) -> io::Result<()> {
    let mut current = slot
        .write()
        .map_err(|_| io::Error::other("host.ordinary_context_unavailable"))?;
    if cancel.is_cancelled() {
        return Err(crate::cloud::cancelled_account_work());
    }
    if current
        .as_ref()
        .is_none_or(|value| !Arc::ptr_eq(value, previous))
    {
        return Err(io::Error::other("host.ordinary_scope_changed"));
    }
    *current = Some(Arc::new(next));
    Ok(())
}

impl ProtectedPolicySource for CurrentHostPolicySource {
    type Guard = CurrentHostPolicy;
    fn current_policy(&self) -> Result<Self::Guard, ContractError> {
        self.owner
            .current_policy()
            .map(|policy| CurrentHostPolicy(CurrentPolicy::Owner(policy)))
    }
    fn current_policy_for(&self, kind: AuthorityKind) -> Result<Self::Guard, ContractError> {
        match kind {
            AuthorityKind::EnrolledOwner => self.current_policy(),
            AuthorityKind::Ordinary => self.current_ordinary(),
        }
    }
    fn observe_ordinary<'a>(
        &'a self,
        route: &'a OrdinaryRoute,
        cancel: &'a CancellationToken,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), ContractError>> + Send + 'a>>
    {
        Box::pin(async move {
            let prepared = self
                .ordinary
                .read()
                .map_err(|_| ContractError::Denied)?
                .clone()
                .ok_or(ContractError::Denied)?;
            if &prepared.route != route {
                return Err(ContractError::Denied);
            }
            if !prepared.observer.is_ready() {
                prepared.require_refresh(
                    self.diagnostic.as_ref(),
                    ReadinessEvent::IdentityNotReadyBefore,
                );
                return Err(ContractError::Stale);
            }
            prepared.observer.observe(cancel).await.map_err(|_| {
                if !prepared.observer.is_ready() {
                    prepared.require_refresh(
                        self.diagnostic.as_ref(),
                        ReadinessEvent::IdentityNotReadyAfter,
                    );
                    ContractError::Stale
                } else {
                    ContractError::Denied
                }
            })?;
            if cancel.is_cancelled() {
                return Err(ContractError::Denied);
            }
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lifecycle::{OrdinaryAccess, Origin};
    use festerm_tunnel_client::host::{
        CreateAccess, CreateProtocol, Resource, HOST_LABEL, HOST_PORT,
    };

    #[test]
    fn readiness_diagnostic_is_bounded_per_static_reason_without_tick_noise() {
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured = events.clone();
        let diagnostic: ReadinessDiagnostic = Arc::new(move |event, error| {
            assert!(error.is_none());
            captured.lock().unwrap().push(event);
        });
        let reported = std::sync::atomic::AtomicU8::new(0);
        for _ in 0..3 {
            for reason in [
                ReadinessEvent::OwnerDigestChanged,
                ReadinessEvent::CloudDigestChanged,
                ReadinessEvent::IdentityNotReadyBefore,
                ReadinessEvent::IdentityNotReadyAfter,
            ] {
                report_readiness_once(&reported, Some(&diagnostic), reason);
            }
        }
        assert_eq!(events.lock().unwrap().len(), 4);
    }

    #[test]
    fn prepared_publication_refuses_same_value_different_arc_or_cancelled_work() {
        let previous = Arc::new(1_u8);
        let slot = std::sync::RwLock::new(Some(previous.clone()));
        let cancel = CancellationToken::new();
        publish_same_arc(&slot, &previous, 2, &cancel).unwrap();
        assert_eq!(**slot.read().unwrap().as_ref().unwrap(), 2);
        assert!(publish_same_arc(&slot, &previous, 3, &cancel).is_err());
        let current = slot.read().unwrap().clone().unwrap();
        *slot.write().unwrap() = Some(Arc::new(2));
        assert!(publish_same_arc(&slot, &current, 3, &cancel).is_err());
        let current = slot.read().unwrap().clone().unwrap();
        cancel.cancel();
        assert!(publish_same_arc(&slot, &current, 3, &cancel).is_err());
        assert_eq!(**slot.read().unwrap().as_ref().unwrap(), 2);
    }

    #[test]
    fn provider_owner_policy_uses_verified_zero_admin_key_binding_without_legacy_permissions() {
        for (creation, termination) in [(false, false), (false, true), (true, false), (true, true)]
        {
            let (_root, store, vault) = crate::enrollment::tests::setup_origin(Origin::Desktop);
            let owners = crate::enrollment::tests::fixture_owners(&store, vault);
            let original = owners
                .initialize(Path::new("host-identity"), "github:1234".into())
                .unwrap();
            assert!(original.keys.is_empty() && !original.remote_access);
            let mut expected = store.snapshot().unwrap();
            let resource = Resource {
                tunnel_id: "owned-fixture".into(),
                cluster: "usw3".into(),
                account_id: 1234,
                ownership_label: format!("festerm-owned-{}", "a".repeat(32)),
                port: HOST_PORT,
                labels: vec![
                    HOST_LABEL.into(),
                    format!("festerm-owned-{}", "a".repeat(32)),
                ],
                create_protocol: CreateProtocol::default(),
                create_access: CreateAccess::OwnerDefault,
            };
            // Missing/legacy policy never acquires permission merely from identity.
            assert!(CurrentHostPolicySource::policy_for(&expected, &original, &resource).is_err());
            store.revoke().unwrap();
            let stopped = store.snapshot().unwrap();
            store
                .confirm_stopped(stopped.epoch, stopped.binding.as_ref().unwrap())
                .unwrap();
            let before = store.snapshot().unwrap();
            let mut settings = before.settings.clone();
            settings.ordinary_access = OrdinaryAccess::ProviderOwner;
            settings.allow_creation = creation;
            settings.allow_session_termination = termination;
            let ticket = store.request_enable_if_current(&before, &settings).unwrap();
            store
                .record_provisioned(&ticket, before.binding.unwrap())
                .unwrap();
            expected = store.snapshot().unwrap();
            let verified = owners.snapshot().unwrap();
            let policy =
                CurrentHostPolicySource::policy_for(&expected, &verified, &resource).unwrap();
            assert!(policy.session.capabilities.inventory && policy.session.capabilities.attach);
            assert_eq!(policy.session.capabilities.creation_allowed, creation);
            assert_eq!(policy.session.capabilities.explicit_end, termination);
            assert!(!policy.session.capabilities.temporary);
            assert_eq!(policy.host_info.principal, original.binding.principal);
            let files = policy.files.as_ref().unwrap();
            assert!(files.filesystem.is_none());
            assert_eq!(files.scope.host, policy.host_info.route.host.host);
            assert_eq!(files.scope.installation, policy.host_info.installation);
            assert_ne!(
                expected.epoch,
                verified.revision.get(),
                "fixture must distinguish lifecycle and enrollment epochs"
            );
            assert_eq!(files.scope.epoch, expected.epoch.to_string());
            assert!(owners.snapshot().unwrap().keys.is_empty());
            let mut legacy = resource.clone();
            legacy.create_access = CreateAccess::LegacyGithubUsers;
            assert!(CurrentHostPolicySource::policy_for(&expected, &verified, &legacy).is_err());
            store.revoke().unwrap();
            assert!(CurrentHostPolicySource::policy_for(
                &store.snapshot().unwrap(),
                &verified,
                &resource
            )
            .is_err());
        }
    }
}

#[cfg(test)]
mod anchor_tests {
    use super::*;
    use crate::{
        enrollment::tests::{fixture_owners, setup_origin},
        lifecycle::Origin,
    };
    use festerm_tunnel_bridge::owner_auth::OwnerSigningKey;
    use festerm_tunnel_client::host::{
        CreateAccess, CreateProtocol, Resource, HOST_LABEL, HOST_PORT,
    };

    fn enable_provider(store: &Store) {
        store.revoke().unwrap();
        let stopped = store.snapshot().unwrap();
        store
            .confirm_stopped(stopped.epoch, stopped.binding.as_ref().unwrap())
            .unwrap();
        let before = store.snapshot().unwrap();
        let mut settings = before.settings.clone();
        settings.ordinary_access = OrdinaryAccess::ProviderOwner;
        let ticket = store.request_enable_if_current(&before, &settings).unwrap();
        store
            .record_provisioned(&ticket, before.binding.unwrap())
            .unwrap();
    }
    fn resource() -> Resource {
        let label = format!("festerm-owned-{}", "a".repeat(32));
        Resource {
            tunnel_id: "owned-fixture".into(),
            cluster: "usw3".into(),
            account_id: 1234,
            ownership_label: label.clone(),
            port: HOST_PORT,
            labels: vec![HOST_LABEL.into(), label],
            create_protocol: CreateProtocol::default(),
            create_access: CreateAccess::OwnerDefault,
        }
    }

    #[test]
    fn ordinary_projection_rechecks_native_anchor_after_unobserved_or_observed_policy_rollback() {
        for observed in [false, true] {
            let (_root, store, vault) = setup_origin(Origin::Desktop);
            let owners = fixture_owners(&store, vault.clone());
            owners
                .initialize(Path::new("host-identity"), "github:1234".into())
                .unwrap();
            enable_provider(&store);
            let initial = owners.snapshot().unwrap();
            let key = OwnerSigningKey::from_seed(&[71; 32]);
            let a = owners
                .enroll(
                    initial.revision,
                    key.public_key(),
                    &crate::enrollment::owner_key_fingerprint(&key.public_key()).unwrap(),
                )
                .unwrap();
            let expected = store.snapshot().unwrap();
            let (bytes_a, cloud_digest) = store
                .with_locked(|directory, _| {
                    storage::write_new_private(
                        directory,
                        Path::new("host-cloud.json"),
                        b"public fixture receipt",
                    )?;
                    Ok((
                        storage::read_private(
                            directory,
                            Path::new("owner-enrollment.json"),
                            16 * 1024,
                        )?,
                        Sha256::digest(b"public fixture receipt").into(),
                    ))
                })
                .unwrap();
            let owner_digest = Sha256::digest(&bytes_a).into();
            let resource = resource();
            // This is the real protected projection seam; provider observation
            // is a separate wire fixture, not authenticated by this test.
            let guard = CurrentHostPolicySource::project_ordinary_current(
                &owners,
                &expected,
                owner_digest,
                cloud_digest,
                &resource,
            )
            .unwrap();
            assert!(store
                .snapshot()
                .is_err_and(|error| error.kind() == io::ErrorKind::WouldBlock));
            assert!(owners
                .revoke(a.revision, key.public_key())
                .is_err_and(|error| error.kind() == io::ErrorKind::WouldBlock));
            drop(guard);
            for (owner, cloud, reason) in [
                ([0; 32], cloud_digest, ReadinessEvent::OwnerDigestChanged),
                (owner_digest, [0; 32], ReadinessEvent::CloudDigestChanged),
            ] {
                let error = CurrentHostPolicySource::project_ordinary_current(
                    &owners, &expected, owner, cloud, &resource,
                )
                .err()
                .unwrap();
                assert_eq!(
                    error
                        .get_ref()
                        .unwrap()
                        .downcast_ref::<ReadinessRefreshRequired>()
                        .unwrap()
                        .0,
                    reason
                );
            }
            let b = owners.revoke(a.revision, key.public_key()).unwrap();
            let bytes_b = store
                .with_locked(|directory, _| {
                    storage::read_private(directory, Path::new("owner-enrollment.json"), 16 * 1024)
                })
                .unwrap();
            if observed {
                assert!(CurrentHostPolicySource::project_ordinary_current(
                    &owners,
                    &expected,
                    owner_digest,
                    cloud_digest,
                    &resource
                )
                .is_err());
            }
            store
                .with_locked(|directory, _| {
                    storage::replace_private(
                        directory,
                        Path::new("owner-enrollment.json"),
                        &bytes_a,
                    )
                })
                .unwrap();
            assert!(
                CurrentHostPolicySource::project_ordinary_current(
                    &owners,
                    &expected,
                    owner_digest,
                    cloud_digest,
                    &resource
                )
                .is_err(),
                "native B + disk A must never restore prepared A"
            );
            store
                .with_locked(|directory, _| {
                    storage::replace_private(
                        directory,
                        Path::new("owner-enrollment.json"),
                        &bytes_b,
                    )
                })
                .unwrap();
            assert_eq!(
                owners
                    .current_snapshot_noninteractive()
                    .unwrap()
                    .enrollment(),
                &b
            );
            assert!(CurrentHostPolicySource::project_ordinary_current(
                &owners,
                &expected,
                owner_digest,
                cloud_digest,
                &resource
            )
            .is_err());
            enable_provider(&store);
            owners.snapshot().unwrap();
            let fresh = store.snapshot().unwrap();
            let fresh_digest = store
                .with_locked(|directory, _| {
                    Ok(Sha256::digest(storage::read_private(
                        directory,
                        Path::new("owner-enrollment.json"),
                        16 * 1024,
                    )?)
                    .into())
                })
                .unwrap();
            let guard = CurrentHostPolicySource::project_ordinary_current(
                &owners,
                &fresh,
                fresh_digest,
                cloud_digest,
                &resource,
            )
            .unwrap();
            assert!(
                matches!(guard.view(),PolicyView::Ordinary(policy)if policy.denied_keys.contains(&key.public_key()))
            );
            drop(guard);
            vault
                .locked
                .store(true, std::sync::atomic::Ordering::Release);
            assert!(CurrentHostPolicySource::project_ordinary_current(
                &owners,
                &fresh,
                fresh_digest,
                cloud_digest,
                &resource
            )
            .is_err());
        }
    }
}
