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

struct Prepared {
    expected: Snapshot,
    owner_digest: [u8; 32],
    route: OrdinaryRoute,
    observer: PreparedOrdinaryObserver,
    refresh_reported: std::sync::atomic::AtomicBool,
}

#[derive(Debug)]
struct ReadinessRefreshRequired;
impl std::fmt::Display for ReadinessRefreshRequired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("host.ordinary_readiness_refresh_required")
    }
}
impl std::error::Error for ReadinessRefreshRequired {}
impl Prepared {
    fn require_refresh(&self) {
        if !self
            .refresh_reported
            .swap(true, std::sync::atomic::Ordering::Relaxed)
        {
            eprintln!("host.ordinary_readiness_refresh_required: disable then enable remote access to reload protected policy/account readiness; existing shells are retained");
        }
    }
}

pub struct CurrentHostPolicySource {
    store: Arc<Store>,
    owner: CurrentOwnerPolicySource,
    storage: OwnerStorage,
    ordinary: std::sync::RwLock<Option<Arc<Prepared>>>,
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
        cancel: &CancellationToken,
    ) -> io::Result<Self> {
        let expected = store.snapshot()?;
        let owner = CurrentOwnerPolicySource::new(store.clone(), storage_kind);
        let ordinary = if expected.settings.ordinary_access == OrdinaryAccess::ProviderOwner {
            let enrollment = store
                .owner_enrollment_with_storage(storage_kind)?
                .snapshot()?;
            let observer = CloudHostAdapter::new(&store)
                .prepare_ordinary_observer(cancel)
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
        })
    }

    /// Called between relay attempts, outside all wire admission deadlines.
    pub async fn refresh(&self, cancel: &CancellationToken) -> io::Result<()> {
        let prepared = Self::prepare(self.store.clone(), self.storage_kind(), cancel).await?;
        let next = prepared
            .ordinary
            .into_inner()
            .map_err(|_| io::Error::other("host.ordinary_context_unavailable"))?;
        *self
            .ordinary
            .write()
            .map_err(|_| io::Error::other("host.ordinary_context_unavailable"))? = next;
        Ok(())
    }
    fn storage_kind(&self) -> OwnerStorage {
        self.storage
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
            refresh_reported: false.into(),
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
            if owner != owner_digest || cloud != cloud_digest {
                return Err(io::Error::other(ReadinessRefreshRequired));
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
                } else if e
                    .get_ref()
                    .is_some_and(|cause| cause.is::<ReadinessRefreshRequired>())
                {
                    // Report only after the failed projection dropped its
                    // protected lifecycle lease; diagnostic I/O owns no lock.
                    prepared.require_refresh();
                    ContractError::Stale
                } else {
                    ContractError::Denied
                }
            })
    }
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
                prepared.require_refresh();
                return Err(ContractError::Stale);
            }
            prepared.observer.observe(cancel).await.map_err(|_| {
                if !prepared.observer.is_ready() {
                    prepared.require_refresh();
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
    fn provider_owner_policy_uses_verified_zero_admin_key_binding_without_legacy_permissions() {
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
        let ticket = store.request_enable_if_current(&before, &settings).unwrap();
        store
            .record_provisioned(&ticket, before.binding.unwrap())
            .unwrap();
        expected = store.snapshot().unwrap();
        let verified = owners.snapshot().unwrap();
        let policy = CurrentHostPolicySource::policy_for(&expected, &verified, &resource).unwrap();
        assert!(policy.session.capabilities.inventory && policy.session.capabilities.attach);
        assert!(
            !policy.session.capabilities.creation_allowed
                && !policy.session.capabilities.explicit_end
        );
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
