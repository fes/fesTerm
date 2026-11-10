//! Foreground account scope and zero-shell cloud provisioning. The lifecycle
//! journal remains consent authority; this adapter never starts an endpoint.

use crate::{
    lifecycle::{Binding, Origin, Phase, Snapshot, Store, Ticket},
    protected_state::Lease,
};
use festerm_secret_store::{SecretBytes, SecretReference, SecretStore};
use festerm_sessiond::private_storage as storage;
use festerm_tunnel_bridge::host_identity::HostIdentity;
use festerm_tunnel_client::{
    host::{Management, Observation, Receipt, Resource, HOST_LABEL, HOST_PORT},
    identity::{self, DevicePrompt, Identity},
    Error as AccountError, Https,
};
use serde::{Deserialize, Serialize};
use std::{fs::File, io, path::Path};
use tokio_util::sync::CancellationToken;

const RECORD: &str = "host-cloud.json";
const LOCK: &str = "host-cloud.lock";
pub const HOST_IDENTITY_FILE: &str = "host-identity";

fn refused(stage: &'static str) -> io::Error {
    io::Error::other(stage)
}

fn account_error(error: AccountError) -> io::Error {
    refused(match error {
        AccountError::Cancelled => "host.cloud_cancelled",
        AccountError::Expired | AccountError::Store => "host.sign_in_required",
        AccountError::Denied => "host.cloud_denied",
        AccountError::AccountChanged => "host.cloud_account_changed",
        AccountError::IdentityChanged => "host.cloud_identity_changed",
        AccountError::Network => "host.cloud_offline_or_unknown",
        AccountError::RateLimited => "host.cloud_rate_limited",
        _ => "host.cloud_response_refused",
    })
}

fn os_user() -> io::Result<String> {
    #[cfg(unix)]
    return festerm_unix_security::current_user_id().map(|id| format!("uid:{id}"));
    #[cfg(windows)]
    return festerm_windows_security::current_user_service_context().map(|(sid, _)| sid);
    #[cfg(not(any(unix, windows)))]
    Err(refused("host.cloud_platform_unavailable"))
}

fn subject(id: u64) -> String {
    format!("github:{id}")
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Record {
    version: u16,
    installation: String,
    origin: Origin,
    os_user: String,
    ticket: Ticket,
    account_id: u64,
    credential: String,
    authenticated: bool,
    host_id: String,
    fingerprint: String,
    resource: Option<Resource>,
    submitted: bool,
    receipt: Option<Receipt>,
}

/// Nonportable foreground intent. Only this live handle can initiate sign-in.
/// It carries no token and cannot be deserialized from a configuration file.
pub struct Foreground {
    record: Record,
}

/// Cloud presence is not endpoint readiness. No variant claims Ready until
/// service/channel/principal checks are composed by their separate owners.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Health {
    PresentEndpointUnqualified { receipt: Receipt },
    Absent,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Cleanup {
    /// Exact authenticated absence was observed; the tombstone was committed.
    Removed,
    /// The official API has no documented generation-atomic delete contract.
    /// No DELETE was sent and the protected receipt remains pending.
    GenerationConditionalDeleteUnavailable,
}

/// Desktop and headless share this adapter. Construction has no side effects.
pub struct CloudHostAdapter<'a> {
    store: &'a Store,
}

trait Network {
    async fn sign_in(
        &self,
        account: u64,
        cancel: &CancellationToken,
        prompt: impl FnOnce(DevicePrompt),
    ) -> Result<Identity, AccountError>;
    async fn confirm(
        &self,
        identity: &Identity,
        cancel: &CancellationToken,
    ) -> Result<(), AccountError>;
    async fn refresh(
        &self,
        identity: &Identity,
        cancel: &CancellationToken,
    ) -> Result<Identity, AccountError>;
    async fn recommend(
        &self,
        identity: &Identity,
        cancel: &CancellationToken,
    ) -> Result<String, AccountError>;
    async fn create(
        &self,
        identity: &Identity,
        resource: &Resource,
        name: &str,
        cancel: &CancellationToken,
    ) -> Result<Receipt, AccountError>;
    async fn observe(
        &self,
        identity: &Identity,
        resource: &Resource,
        cancel: &CancellationToken,
    ) -> Result<Observation, AccountError>;
}

struct NativeNetwork {
    identity: Https,
    management: Management,
}
impl NativeNetwork {
    fn new() -> io::Result<Self> {
        Ok(Self {
            identity: Https::new().map_err(account_error)?,
            management: Management::new().map_err(account_error)?,
        })
    }
}
impl Network for NativeNetwork {
    async fn sign_in(
        &self,
        account: u64,
        cancel: &CancellationToken,
        prompt: impl FnOnce(DevicePrompt),
    ) -> Result<Identity, AccountError> {
        identity::sign_in(
            &self.identity,
            identity::registered_client_id().ok_or(AccountError::Registration)?,
            Some(account),
            cancel,
            prompt,
        )
        .await
    }
    async fn confirm(
        &self,
        identity: &Identity,
        cancel: &CancellationToken,
    ) -> Result<(), AccountError> {
        identity.confirm_account(&self.identity, cancel).await
    }
    async fn refresh(
        &self,
        identity: &Identity,
        cancel: &CancellationToken,
    ) -> Result<Identity, AccountError> {
        identity
            .refresh(
                &self.identity,
                identity::registered_client_id().ok_or(AccountError::Registration)?,
                cancel,
            )
            .await
    }
    async fn recommend(
        &self,
        identity: &Identity,
        cancel: &CancellationToken,
    ) -> Result<String, AccountError> {
        self.management.recommend_cluster(identity, cancel).await
    }
    async fn create(
        &self,
        identity: &Identity,
        resource: &Resource,
        name: &str,
        cancel: &CancellationToken,
    ) -> Result<Receipt, AccountError> {
        self.management
            .create(identity, resource, name, cancel)
            .await
    }
    async fn observe(
        &self,
        identity: &Identity,
        resource: &Resource,
        cancel: &CancellationToken,
    ) -> Result<Observation, AccountError> {
        self.management.observe(identity, resource, cancel).await
    }
}

impl<'a> CloudHostAdapter<'a> {
    pub fn new(store: &'a Store) -> Self {
        Self { store }
    }

    fn operation(&self) -> io::Result<Lease> {
        self.store.with_locked(|directory, _| {
            let file = match storage::open_private_file(directory, Path::new(LOCK), false) {
                Ok(file) => file,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    match storage::open_private_file(directory, Path::new(LOCK), true) {
                        Ok(file) => file,
                        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                            storage::open_private_file(directory, Path::new(LOCK), false)?
                        }
                        Err(error) => return Err(error),
                    }
                }
                Err(error) => return Err(error),
            };
            Lease::exclusive(file)
        })
    }

    /// Call only from explicit foreground account approval for the exact
    /// request_enable ticket/settings shown to this OS user. Login/display name
    /// is deliberately not an account identifier.
    pub fn foreground(
        &self,
        ticket: &Ticket,
        expected_github_subject: u64,
    ) -> io::Result<Foreground> {
        let _operation = self.operation()?;
        if expected_github_subject == 0 {
            return Err(refused("host.cloud_account_required"));
        }
        self.store.with_locked(|directory, snapshot| {
            current_ticket(snapshot, ticket, true)?;
            let user = os_user()?;
            if let Some(mut record) = read(directory)? {
                if record.account_id != expected_github_subject {
                    return Err(refused("host.cloud_account_changed"));
                }
                // A new foreground ticket may reuse the same disabled identity,
                // resource and protected credential, never another account.
                record.ticket = ticket.clone();
                validate(&record, snapshot, directory)?;
                write(directory, &record)?;
                return Ok(Foreground { record });
            }
            if snapshot.binding.is_some() {
                return Err(refused(
                    "host.cloud_receipt_missing_reconciliation_required",
                ));
            }
            let identity = HostIdentity::load_or_create_from_private_directory(
                directory,
                Path::new(HOST_IDENTITY_FILE),
            )?;
            let record = Record {
                version: 1,
                installation: snapshot.installation.clone(),
                origin: snapshot.origin,
                os_user: user,
                ticket: ticket.clone(),
                account_id: expected_github_subject,
                credential: SecretReference::generate().to_persisted_string(),
                authenticated: false,
                host_id: identity.host_id(),
                fingerprint: identity.fingerprint()?,
                resource: None,
                submitted: false,
                receipt: None,
            };
            crate::protected_state::publish_new(directory, Path::new(RECORD), &encode(&record)?)?;
            Ok(Foreground { record })
        })
    }

    /// The prompt is short-lived foreground display only; never log it or put
    /// it in arguments, environment, portable config or a persisted UI model.
    pub async fn sign_in(
        &self,
        foreground: &Foreground,
        cancel: &CancellationToken,
        prompt: impl FnOnce(DevicePrompt),
    ) -> io::Result<()> {
        let network = NativeNetwork::new()?;
        let secrets = festerm_secret_store::native_store()
            .map_err(|_| refused("host.secure_store_unavailable"))?;
        self.sign_in_with(foreground, cancel, prompt, &network, &*secrets)
            .await
    }

    async fn sign_in_with(
        &self,
        foreground: &Foreground,
        cancel: &CancellationToken,
        prompt: impl FnOnce(DevicePrompt),
        network: &impl Network,
        secrets: &dyn SecretStore,
    ) -> io::Result<()> {
        let _operation = self.operation()?;
        let mut record = self.load()?;
        self.require_foreground(&record, foreground, cancel)?;
        // Existing authority must be opened/refreshed, not silently replaced.
        if record.authenticated {
            let result = self
                .load_identity(&mut record, network, secrets, cancel)
                .await;
            self.check_cancelled(&record, cancel)?;
            match result {
                Ok(_) => return self.require_foreground(&record, foreground, cancel),
                Err(error)
                    if matches!(
                        error.to_string().as_str(),
                        "host.sign_in_required" | "host.cloud_denied"
                    ) => {}
                Err(error) => return Err(error),
            }
        }
        let reference = reference(&record)?;
        let existing = match secrets.get(&reference) {
            Ok(secret) => {
                let identity = secret
                    .with_bytes(|bytes| {
                        Identity::decode_private(
                            bytes,
                            record.account_id,
                            identity::SERVICE_CLIENT_ID,
                        )
                    })
                    .map_err(account_error)?;
                if !identity.needs_sign_in() {
                    let result = network.confirm(&identity, cancel).await;
                    self.check_cancelled(&record, cancel)?;
                    match result {
                        Ok(()) => {
                            self.require_foreground(&record, foreground, cancel)?;
                            record.authenticated = true;
                            return self.save(&record);
                        }
                        Err(AccountError::Denied | AccountError::Expired) => {}
                        Err(error) => return Err(account_error(error)),
                    }
                }
                true
            }
            Err(festerm_secret_store::SecretStoreError::Missing) => false,
            Err(_) => return Err(refused("host.secure_store_unavailable")),
        };
        let result = network.sign_in(record.account_id, cancel, prompt).await;
        self.check_cancelled(&record, cancel)?;
        let identity = result.map_err(account_error)?;
        if identity.account.id != record.account_id {
            return Err(refused("host.cloud_account_changed"));
        }
        self.require_foreground(&record, foreground, cancel)?;
        let encoded = identity.encode_private().map_err(account_error)?;
        // The reference was committed before network I/O; no orphan on crash.
        // create_reserved also refuses a concurrently occupied reference.
        self.store.with_locked(|directory, snapshot| {
            current_ticket(snapshot, &record.ticket, true)?;
            let secret = SecretBytes::copy_from_slice(&encoded);
            if existing {
                secrets.update(&reference, &secret)
            } else {
                secrets.create_reserved(&reference, &secret)
            }
            .map_err(|_| refused("host.secure_store_unavailable"))?;
            record.authenticated = true;
            write(directory, &record)
        })
    }

    fn require_foreground(
        &self,
        record: &Record,
        foreground: &Foreground,
        cancel: &CancellationToken,
    ) -> io::Result<()> {
        self.require_scope(record, foreground)?;
        self.check_cancelled(record, cancel)?;
        self.store.with_locked(|directory, snapshot| {
            validate(record, snapshot, directory)?;
            current_ticket(snapshot, &record.ticket, true)
        })
    }

    fn require_scope(&self, record: &Record, foreground: &Foreground) -> io::Result<()> {
        if record.installation != foreground.record.installation
            || record.ticket != foreground.record.ticket
            || record.account_id != foreground.record.account_id
            || record.credential != foreground.record.credential
        {
            return Err(refused("host.cloud_foreground_scope_changed"));
        }
        Ok(())
    }

    fn check_cancelled(&self, record: &Record, cancel: &CancellationToken) -> io::Result<()> {
        if cancel.is_cancelled() {
            self.store.cancel_enable(&record.ticket)?;
            return Err(refused("host.cloud_cancelled"));
        }
        Ok(())
    }

    /// Submit at most once per durable reservation. Repeated/lost replies only
    /// observe the same ID/cluster/label; they never create a second tunnel.
    pub async fn provision(
        &self,
        foreground: &Foreground,
        cancel: &CancellationToken,
    ) -> io::Result<Phase> {
        let network = NativeNetwork::new()?;
        let secrets = festerm_secret_store::native_store()
            .map_err(|_| refused("host.secure_store_unavailable"))?;
        self.provision_with(foreground, cancel, &network, &*secrets)
            .await
    }

    async fn provision_with(
        &self,
        foreground: &Foreground,
        cancel: &CancellationToken,
        network: &impl Network,
        secrets: &dyn SecretStore,
    ) -> io::Result<Phase> {
        let _operation = self.operation()?;
        let mut record = self.load()?;
        self.require_scope(&record, foreground)?;
        // A cancelled pending request may be observed for late cleanup, but
        // can never reach the submission branch.
        if !record.submitted {
            self.require_foreground(&record, foreground, cancel)?;
        }
        let result = self
            .load_identity(&mut record, network, secrets, cancel)
            .await;
        self.check_cancelled(&record, cancel)?;
        let identity = result?;
        if record.resource.is_none() {
            let result = network.recommend(&identity, cancel).await;
            self.check_cancelled(&record, cancel)?;
            let cluster = result.map_err(account_error)?;
            self.require_foreground(&record, foreground, cancel)?;
            let ownership_label = format!("festerm-owned-{}", uuid::Uuid::new_v4().simple());
            let resource = Resource {
                tunnel_id: format!("festerm-{}", uuid::Uuid::new_v4().simple()),
                cluster,
                account_id: record.account_id,
                labels: vec![HOST_LABEL.into(), ownership_label.clone()],
                ownership_label,
                port: HOST_PORT,
            };
            resource.validate().map_err(account_error)?;
            record.resource = Some(resource);
            self.save(&record)?;
        }
        let resource = record
            .resource
            .as_ref()
            .ok_or_else(|| refused("host.cloud_reservation_missing"))?
            .clone();
        let result = network.observe(&identity, &resource, cancel).await;
        self.check_cancelled(&record, cancel)?;
        let observed = result.map_err(account_error)?;
        let receipt = match observed {
            Observation::Present(receipt) => {
                // A collision before submission can never be adopted.
                if !record.submitted {
                    return Err(refused("host.cloud_resource_collision"));
                }
                receipt
            }
            Observation::Absent if record.submitted => {
                return Err(refused("host.cloud_creation_outcome_uncertain"))
            }
            Observation::Absent => {
                self.require_foreground(&record, foreground, cancel)?;
                record.submitted = true;
                // Fence submission under the same lifecycle lease as revoke.
                // Do not hold it across the following network operation.
                self.store.with_locked(|directory, snapshot| {
                    current_ticket(snapshot, &record.ticket, true)?;
                    write(directory, &record)
                })?;
                let result = network
                    .create(
                        &identity,
                        &resource,
                        &record.ticket.approved_settings().friendly_name,
                        cancel,
                    )
                    .await;
                if cancel.is_cancelled() {
                    self.store.cancel_enable(&record.ticket)?;
                }
                result.map_err(account_error)?
            }
        };
        pin_receipt(&mut record, receipt)?;
        // Commit late success BEFORE any cancellable post-network verification.
        // Revocation cannot erase the resource receipt or restore enable intent.
        self.save(&record)?;
        self.publish(&record)?;
        let result = network.confirm(&identity, cancel).await;
        self.check_cancelled(&record, cancel)?;
        result.map_err(account_error)?;
        Ok(self.store.snapshot()?.phase)
    }

    /// Recover a submitted pending operation without initiating interactive
    /// authentication or creating another resource.
    pub async fn reconcile(&self, cancel: &CancellationToken) -> io::Result<Phase> {
        let network = NativeNetwork::new()?;
        let secrets = festerm_secret_store::native_store()
            .map_err(|_| refused("host.secure_store_unavailable"))?;
        self.reconcile_with(cancel, &network, &*secrets).await
    }

    async fn reconcile_with(
        &self,
        cancel: &CancellationToken,
        network: &impl Network,
        secrets: &dyn SecretStore,
    ) -> io::Result<Phase> {
        let _operation = self.operation()?;
        let mut record = self.load()?;
        if !record.submitted {
            return Err(refused("host.cloud_no_submission"));
        }
        let identity = self
            .load_identity(&mut record, network, secrets, cancel)
            .await?;
        let resource = record
            .resource
            .as_ref()
            .ok_or_else(|| refused("host.cloud_reservation_missing"))?;
        let receipt = match network
            .observe(&identity, resource, cancel)
            .await
            .map_err(account_error)?
        {
            Observation::Present(receipt) => receipt,
            Observation::Absent => return Err(refused("host.cloud_creation_outcome_uncertain")),
        };
        pin_receipt(&mut record, receipt)?;
        self.save(&record)?;
        self.publish(&record)?;
        network
            .confirm(&identity, cancel)
            .await
            .map_err(account_error)?;
        Ok(self.store.snapshot()?.phase)
    }

    fn publish(&self, record: &Record) -> io::Result<Phase> {
        let receipt = record
            .receipt
            .as_ref()
            .ok_or_else(|| refused("host.cloud_receipt_missing"))?;
        let binding = binding(record, receipt);
        let snapshot = self.store.snapshot()?;
        if snapshot.binding.as_ref() == Some(&binding) && snapshot.pending_ticket().is_none() {
            return Ok(snapshot.phase);
        }
        self.store.record_provisioned(&record.ticket, binding)
    }

    pub async fn health(&self, cancel: &CancellationToken) -> io::Result<Health> {
        let network = NativeNetwork::new()?;
        let secrets = festerm_secret_store::native_store()
            .map_err(|_| refused("host.secure_store_unavailable"))?;
        self.health_with(cancel, &network, &*secrets).await
    }

    async fn health_with(
        &self,
        cancel: &CancellationToken,
        network: &impl Network,
        secrets: &dyn SecretStore,
    ) -> io::Result<Health> {
        let _operation = self.operation()?;
        let mut record = self.load()?;
        let identity = self
            .load_identity(&mut record, network, secrets, cancel)
            .await?;
        let exact = record
            .receipt
            .as_ref()
            .ok_or_else(|| refused("host.cloud_receipt_missing"))?;
        let observation = network
            .observe(&identity, &exact.resource, cancel)
            .await
            .map_err(account_error)?;
        network
            .confirm(&identity, cancel)
            .await
            .map_err(account_error)?;
        match observation {
            Observation::Present(receipt) if &receipt == exact => {
                Ok(Health::PresentEndpointUnqualified { receipt })
            }
            Observation::Present(_) => Err(refused("host.cloud_generation_changed")),
            Observation::Absent => Ok(Health::Absent),
        }
    }

    /// Cloud cleanup is distinct from native stop. The caller must first
    /// obtain exact native stop evidence and request_cleanup in the lifecycle.
    pub async fn cleanup(&self, cancel: &CancellationToken) -> io::Result<Cleanup> {
        let network = NativeNetwork::new()?;
        let secrets = festerm_secret_store::native_store()
            .map_err(|_| refused("host.secure_store_unavailable"))?;
        self.cleanup_with(cancel, &network, &*secrets).await
    }

    async fn cleanup_with(
        &self,
        cancel: &CancellationToken,
        network: &impl Network,
        secrets: &dyn SecretStore,
    ) -> io::Result<Cleanup> {
        let _operation = self.operation()?;
        let mut record = self.load()?;
        let snapshot = self.store.snapshot()?;
        if snapshot.phase != Phase::CleanupPending {
            return Err(refused("host.cleanup_not_requested"));
        }
        let exact = record
            .receipt
            .clone()
            .ok_or_else(|| refused("host.cloud_receipt_missing"))?;
        if snapshot.binding.as_ref() != Some(&binding(&record, &exact)) {
            return Err(refused("host.cloud_binding_changed"));
        }
        let identity = self
            .load_identity(&mut record, network, secrets, cancel)
            .await?;
        let observed = network
            .observe(&identity, &exact.resource, cancel)
            .await
            .map_err(account_error)?;
        network
            .confirm(&identity, cancel)
            .await
            .map_err(account_error)?;
        match observed {
            Observation::Present(receipt) if receipt == exact => {
                Ok(Cleanup::GenerationConditionalDeleteUnavailable)
            }
            Observation::Present(_) => Err(refused("host.cloud_generation_changed")),
            Observation::Absent => {
                self.store
                    .confirm_removed(snapshot.epoch, &binding(&record, &exact))?;
                Ok(Cleanup::Removed)
            }
        }
    }

    async fn load_identity(
        &self,
        record: &mut Record,
        network: &impl Network,
        secrets: &dyn SecretStore,
        cancel: &CancellationToken,
    ) -> io::Result<Identity> {
        if cancel.is_cancelled() {
            return Err(refused("host.cloud_cancelled"));
        }
        if !record.authenticated {
            return Err(refused("host.sign_in_required"));
        }
        let reference = reference(record)?;
        let secret = secrets
            .get(&reference)
            .map_err(|_| refused("host.sign_in_required"))?;
        let mut identity = secret
            .with_bytes(|bytes| {
                Identity::decode_private(bytes, record.account_id, identity::SERVICE_CLIENT_ID)
            })
            .map_err(account_error)?;
        if identity.needs_sign_in() {
            identity = network
                .refresh(&identity, cancel)
                .await
                .map_err(account_error)?;
            if identity.account.id != record.account_id {
                return Err(refused("host.cloud_account_changed"));
            }
            // Same reserved reference; a failed refresh/persistence never
            // authenticates absence or falls back to plaintext credentials.
            let bytes = identity.encode_private().map_err(account_error)?;
            secrets
                .update(&reference, &SecretBytes::copy_from_slice(&bytes))
                .map_err(|_| refused("host.secure_store_unavailable"))?;
        }
        network
            .confirm(&identity, cancel)
            .await
            .map_err(account_error)?;
        Ok(identity)
    }

    fn load(&self) -> io::Result<Record> {
        self.store.with_locked(|directory, snapshot| {
            let record = read(directory)?.ok_or_else(|| refused("host.cloud_not_configured"))?;
            validate(&record, snapshot, directory)?;
            Ok(record)
        })
    }

    fn save(&self, record: &Record) -> io::Result<()> {
        self.store.with_locked(|directory, snapshot| {
            validate(record, snapshot, directory)?;
            write(directory, record)
        })
    }
}

fn current_ticket(snapshot: &Snapshot, ticket: &Ticket, active: bool) -> io::Result<()> {
    if snapshot.pending_ticket() != Some(ticket)
        || (active
            && (snapshot.phase != Phase::EnableRequested || snapshot.epoch != ticket.epoch()))
        || &snapshot.settings != ticket.approved_settings()
    {
        return Err(refused("host.cloud_foreground_consent_expired"));
    }
    Ok(())
}

fn binding(record: &Record, receipt: &Receipt) -> Binding {
    Binding {
        host_id: record.host_id.clone(),
        account: subject(record.account_id),
        qualified_tunnel: receipt.resource.qualified_id(),
        cloud_incarnation: receipt.created.clone(),
        host_key_fingerprint: record.fingerprint.clone(),
    }
}

fn pin_receipt(record: &mut Record, receipt: Receipt) -> io::Result<()> {
    if record.resource.as_ref() != Some(&receipt.resource)
        || receipt.created.is_empty()
        || record.receipt.as_ref().is_some_and(|old| old != &receipt)
    {
        return Err(refused("host.cloud_generation_changed"));
    }
    record.receipt = Some(receipt);
    Ok(())
}

fn validate(record: &Record, snapshot: &Snapshot, directory: &File) -> io::Result<()> {
    if record.version != 1
        || record.installation != snapshot.installation
        || record.origin != snapshot.origin
        || record.os_user != os_user()?
        || record.account_id == 0
        || record.ticket.epoch() > snapshot.epoch
        || record.submitted && record.resource.is_none()
        || record.receipt.is_some() && !record.submitted
    {
        return Err(refused("host.cloud_scope_changed"));
    }
    reference(record)?;
    let identity =
        HostIdentity::load_from_private_directory(directory, Path::new(HOST_IDENTITY_FILE))?;
    if identity.host_id() != record.host_id || identity.fingerprint()? != record.fingerprint {
        return Err(refused("host.cloud_host_identity_changed"));
    }
    if let Some(resource) = &record.resource {
        resource.validate().map_err(account_error)?;
        if resource.account_id != record.account_id {
            return Err(refused("host.cloud_account_changed"));
        }
    }
    if let Some(receipt) = &record.receipt {
        if record.resource.as_ref() != Some(&receipt.resource)
            || receipt.created.is_empty()
            || snapshot
                .binding
                .as_ref()
                .is_some_and(|old| old != &binding(record, receipt))
        {
            return Err(refused("host.cloud_binding_changed"));
        }
    }
    if let Some(ticket) = snapshot.pending_ticket() {
        if ticket != &record.ticket {
            return Err(refused("host.cloud_foreground_consent_expired"));
        }
    }
    Ok(())
}

fn reference(record: &Record) -> io::Result<SecretReference> {
    SecretReference::parse(&record.credential)
        .map_err(|_| refused("host.cloud_credential_reference_invalid"))
}
fn read(directory: &File) -> io::Result<Option<Record>> {
    match storage::read_private(directory, Path::new(RECORD), 16 * 1024) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|_| refused("host.cloud_journal_invalid")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}
fn encode(record: &Record) -> io::Result<Vec<u8>> {
    serde_json::to_vec(record).map_err(|_| refused("host.cloud_journal_invalid"))
}
fn write(directory: &File, record: &Record) -> io::Result<()> {
    storage::replace_private(directory, Path::new(RECORD), &encode(record)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use festerm_secret_store::{MemorySecretStore, SecretStoreError};
    use festerm_tunnel_client::{IdentityRequest, IdentityTransport, ResponseFuture};
    use std::{cell::RefCell, collections::VecDeque, sync::Mutex};

    struct IdentityFixture(Mutex<VecDeque<&'static str>>);
    impl IdentityTransport for IdentityFixture {
        fn request<'b>(
            &'b self,
            request: IdentityRequest<'b>,
            _: &'b CancellationToken,
        ) -> ResponseFuture<'b> {
            Box::pin(async move {
                match request {
                    IdentityRequest::Device { client_id }
                    | IdentityRequest::Poll { client_id, .. } => {
                        assert_eq!(client_id, identity::SERVICE_CLIENT_ID)
                    }
                    IdentityRequest::Account { access_token } => {
                        assert_eq!(access_token, "PRIVATE_FIXTURE_ACCESS")
                    }
                    IdentityRequest::Refresh { .. } => return Err(AccountError::Expired),
                }
                Ok(self
                    .0
                    .lock()
                    .unwrap()
                    .pop_front()
                    .unwrap()
                    .as_bytes()
                    .to_vec()
                    .into())
            })
        }
    }

    #[derive(Default)]
    struct State {
        created: Option<Receipt>,
        creates: usize,
        sign_ins: usize,
        lose_reply: bool,
        revoke_create: bool,
        cancel_create: bool,
        revoke_sign_in: bool,
        observe_error: Option<AccountError>,
        confirm_error: Option<AccountError>,
    }
    struct FixtureNetwork<'a> {
        store: &'a Store,
        state: RefCell<State>,
    }
    impl<'a> FixtureNetwork<'a> {
        fn new(store: &'a Store) -> Self {
            Self {
                store,
                state: RefCell::new(State::default()),
            }
        }
    }
    impl Network for FixtureNetwork<'_> {
        async fn sign_in(
            &self,
            account: u64,
            cancel: &CancellationToken,
            prompt: impl FnOnce(DevicePrompt),
        ) -> Result<Identity, AccountError> {
            self.state.borrow_mut().sign_ins += 1;
            let http = IdentityFixture(Mutex::new([
                r#"{"device_code":"PRIVATE_FIXTURE_DEVICE","user_code":"ABCD-1234","verification_uri":"https://github.com/login/device","expires_in":900,"interval":5}"#,
                r#"{"access_token":"PRIVATE_FIXTURE_ACCESS","token_type":"bearer","scope":""}"#,
                r#"{"id":42,"login":"fixture-display-name"}"#,
            ].into()));
            let identity = identity::sign_in(
                &http,
                identity::SERVICE_CLIENT_ID,
                Some(account),
                cancel,
                prompt,
            )
            .await?;
            if self.state.borrow().revoke_sign_in {
                self.store.revoke().unwrap();
            }
            Ok(identity)
        }
        async fn confirm(
            &self,
            identity: &Identity,
            cancel: &CancellationToken,
        ) -> Result<(), AccountError> {
            if let Some(error) = self.state.borrow().confirm_error {
                return Err(error);
            }
            let http = IdentityFixture(Mutex::new(
                [r#"{"id":42,"login":"renamed-display-only"}"#].into(),
            ));
            identity.confirm_account(&http, cancel).await
        }
        async fn refresh(
            &self,
            _: &Identity,
            _: &CancellationToken,
        ) -> Result<Identity, AccountError> {
            Err(AccountError::Expired)
        }
        async fn recommend(
            &self,
            _: &Identity,
            _: &CancellationToken,
        ) -> Result<String, AccountError> {
            Ok("usw2".into())
        }
        async fn create(
            &self,
            _: &Identity,
            resource: &Resource,
            _: &str,
            cancel: &CancellationToken,
        ) -> Result<Receipt, AccountError> {
            let mut state = self.state.borrow_mut();
            state.creates += 1;
            let receipt = Receipt {
                resource: resource.clone(),
                created: "2026-11-09T00:00:00Z".into(),
            };
            state.created = Some(receipt.clone());
            if state.revoke_create {
                self.store.revoke().unwrap();
            }
            if state.cancel_create {
                cancel.cancel();
            }
            if state.lose_reply {
                return Err(AccountError::Network);
            }
            Ok(receipt)
        }
        async fn observe(
            &self,
            _: &Identity,
            _: &Resource,
            _: &CancellationToken,
        ) -> Result<Observation, AccountError> {
            let state = self.state.borrow();
            if let Some(error) = state.observe_error {
                return Err(error);
            }
            Ok(state
                .created
                .clone()
                .map_or(Observation::Absent, Observation::Present))
        }
    }

    fn fixture() -> (tempfile::TempDir, Store, MemorySecretStore) {
        let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
        let store = Store::create(
            &root.path().join("host"),
            Origin::Headless,
            crate::lifecycle::Settings {
                friendly_name: "Fixture HOST".into(),
                allow_creation: false,
                startup: crate::lifecycle::Startup::Manual,
            },
        )
        .unwrap();
        (root, store, MemorySecretStore::new())
    }
    fn foreground(adapter: &CloudHostAdapter<'_>) -> Foreground {
        let settings = adapter.store.snapshot().unwrap().settings;
        let ticket = adapter.store.request_enable(&settings).unwrap();
        adapter.foreground(&ticket, 42).unwrap()
    }

    #[tokio::test(start_paused = true)]
    async fn host_cloud_foreground_sign_in_is_ticket_account_user_scoped_and_cancel_fenced() {
        let (root, store, secrets) = fixture();
        let adapter = CloudHostAdapter::new(&store);
        let foreground = foreground(&adapter);
        assert!(adapter.foreground(&foreground.record.ticket, 99).is_err());
        let network = FixtureNetwork::new(&store);
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        network.state.borrow_mut().revoke_sign_in = true;
        assert!(adapter
            .sign_in_with(
                &foreground,
                &CancellationToken::new(),
                |prompt| {
                    assert_eq!(prompt.user_code, "ABCD-1234");
                },
                &network,
                &secrets
            )
            .await
            .is_err());
        assert_eq!(network.state.borrow().sign_ins, 1);
        assert!(adapter
            .sign_in_with(&foreground, &cancelled, |_| panic!(), &network, &secrets)
            .await
            .is_err());
        assert_eq!(network.state.borrow().sign_ins, 1);
        assert!(matches!(
            secrets.get(&reference(&foreground.record).unwrap()),
            Err(SecretStoreError::Missing)
        ));
        assert!(!adapter.load().unwrap().authenticated);
        assert!(adapter
            .provision_with(&foreground, &CancellationToken::new(), &network, &secrets)
            .await
            .is_err());
        assert_eq!(network.state.borrow().creates, 0);
        assert_eq!(store.snapshot().unwrap().phase, Phase::Reconciling);
        assert!(!store.snapshot().unwrap().settings.allow_creation);
        let text = std::fs::read_to_string(root.path().join("host").join(RECORD)).unwrap();
        assert!(!text.contains("PRIVATE") && !text.contains("ABCD-1234"));
        assert!(Store::open(&root.path().join("host"), Origin::Desktop).is_err());
        store
            .with_locked(|directory, _| {
                let mut record = read(directory)?.unwrap();
                record.os_user = "someone-else".into();
                write(directory, &record)
            })
            .unwrap();
        assert!(adapter.load().is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn host_cloud_lost_reply_recovers_exact_late_resource_without_duplicate_or_reenable() {
        let (root, store, secrets) = fixture();
        let adapter = CloudHostAdapter::new(&store);
        let foreground = foreground(&adapter);
        let network = FixtureNetwork::new(&store);
        let cancel = CancellationToken::new();
        adapter
            .sign_in_with(&foreground, &cancel, |_| {}, &network, &secrets)
            .await
            .unwrap();
        network.state.borrow_mut().lose_reply = true;
        assert!(adapter
            .provision_with(&foreground, &cancel, &network, &secrets)
            .await
            .is_err());
        let record = adapter.load().unwrap();
        assert!(record.submitted && record.receipt.is_none());
        store.revoke().unwrap();
        let reopened = Store::open(&root.path().join("host"), Origin::Headless).unwrap();
        let recovered = CloudHostAdapter::new(&reopened);
        assert_eq!(
            recovered
                .reconcile_with(&cancel, &network, &secrets)
                .await
                .unwrap(),
            Phase::StopRequested
        );
        assert_eq!(network.state.borrow().creates, 1);
        let binding = reopened.snapshot().unwrap().binding.unwrap();
        assert_eq!(binding.account, "github:42");
        assert_eq!(
            binding.qualified_tunnel,
            record.resource.unwrap().qualified_id()
        );
        assert!(!binding.qualified_tunnel.contains("fixture-display"));
        assert_eq!(reopened.snapshot().unwrap().phase, Phase::StopRequested);
        assert_eq!(
            recovered
                .provision_with(&foreground, &cancel, &network, &secrets)
                .await
                .unwrap(),
            Phase::StopRequested
        );
        assert_eq!(network.state.borrow().creates, 1);
    }

    #[tokio::test(start_paused = true)]
    async fn host_cloud_health_observes_resource_and_cleanup_retains_exact_generation_blocker() {
        let (_root, store, secrets) = fixture();
        let adapter = CloudHostAdapter::new(&store);
        let foreground = foreground(&adapter);
        let network = FixtureNetwork::new(&store);
        let cancel = CancellationToken::new();
        adapter
            .sign_in_with(&foreground, &cancel, |_| {}, &network, &secrets)
            .await
            .unwrap();
        assert_eq!(
            adapter
                .provision_with(&foreground, &cancel, &network, &secrets)
                .await
                .unwrap(),
            Phase::Provisioned
        );
        assert!(matches!(
            adapter
                .health_with(&cancel, &network, &secrets)
                .await
                .unwrap(),
            Health::PresentEndpointUnqualified { .. }
        ));
        for error in [
            AccountError::Denied,
            AccountError::Network,
            AccountError::RateLimited,
        ] {
            network.state.borrow_mut().observe_error = Some(error);
            assert!(adapter
                .health_with(&cancel, &network, &secrets)
                .await
                .is_err());
        }

        network.state.borrow_mut().observe_error = None;
        network.state.borrow_mut().confirm_error = Some(AccountError::AccountChanged);
        assert!(adapter
            .health_with(&cancel, &network, &secrets)
            .await
            .is_err());
        network.state.borrow_mut().confirm_error = None;
        store.revoke().unwrap();
        let snapshot = store.snapshot().unwrap();
        let exact = snapshot.binding.unwrap();
        store.confirm_stopped(snapshot.epoch, &exact).unwrap();
        store.request_cleanup().unwrap();
        assert_eq!(
            adapter
                .cleanup_with(&cancel, &network, &secrets)
                .await
                .unwrap(),
            Cleanup::GenerationConditionalDeleteUnavailable
        );
        assert_eq!(store.snapshot().unwrap().phase, Phase::CleanupPending);
        network.state.borrow_mut().created.as_mut().unwrap().created =
            "2026-11-10T00:00:00Z".into();
        assert!(adapter
            .cleanup_with(&cancel, &network, &secrets)
            .await
            .is_err());
        assert!(adapter
            .health_with(&cancel, &network, &secrets)
            .await
            .is_err());
        network.state.borrow_mut().created = None;
        network.state.borrow_mut().observe_error = Some(AccountError::Denied);
        assert!(adapter
            .cleanup_with(&cancel, &network, &secrets)
            .await
            .is_err());
        assert_eq!(store.snapshot().unwrap().phase, Phase::CleanupPending);
        network.state.borrow_mut().observe_error = None;
        assert_eq!(
            adapter
                .cleanup_with(&cancel, &network, &secrets)
                .await
                .unwrap(),
            Cleanup::Removed
        );
        assert_eq!(store.snapshot().unwrap().phase, Phase::Removed);
    }

    #[tokio::test(start_paused = true)]
    async fn host_cloud_cancelled_submission_journals_late_success_and_never_resubmits_uncertainty()
    {
        for lose_reply in [false, true] {
            let (_root, store, secrets) = fixture();
            let adapter = CloudHostAdapter::new(&store);
            let foreground = foreground(&adapter);
            let network = FixtureNetwork::new(&store);
            let cancel = CancellationToken::new();
            adapter
                .sign_in_with(&foreground, &cancel, |_| {}, &network, &secrets)
                .await
                .unwrap();
            network.state.borrow_mut().cancel_create = true;
            network.state.borrow_mut().lose_reply = lose_reply;
            assert!(adapter
                .provision_with(&foreground, &cancel, &network, &secrets)
                .await
                .is_err());
            let record = adapter.load().unwrap();
            assert!(record.submitted);
            assert_eq!(record.receipt.is_some(), !lose_reply);
            assert_eq!(
                store.snapshot().unwrap().phase,
                if lose_reply {
                    Phase::Reconciling
                } else {
                    Phase::StopRequested
                }
            );
            network.state.borrow_mut().created = None;
            assert!(adapter
                .provision_with(&foreground, &CancellationToken::new(), &network, &secrets)
                .await
                .is_err());
            assert_eq!(network.state.borrow().creates, 1);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn host_cloud_reenable_and_credential_recovery_reuse_only_the_original_identity() {
        let (_root, store, secrets) = fixture();
        let adapter = CloudHostAdapter::new(&store);
        let foreground = foreground(&adapter);
        let network = FixtureNetwork::new(&store);
        let cancel = CancellationToken::new();
        adapter
            .sign_in_with(&foreground, &cancel, |_| {}, &network, &secrets)
            .await
            .unwrap();
        // Simulate interruption after reserved credential publication, before
        // the nonsecret journal's authenticated flag was committed.
        let mut record = adapter.load().unwrap();
        record.authenticated = false;
        adapter.save(&record).unwrap();
        adapter
            .sign_in_with(
                &foreground,
                &cancel,
                |_| panic!("must recover existing credential"),
                &network,
                &secrets,
            )
            .await
            .unwrap();
        assert_eq!(network.state.borrow().sign_ins, 1);
        let credential_ref = reference(&adapter.load().unwrap()).unwrap();
        let secret = secrets.get(&credential_ref).unwrap();
        let expired = secret.with_bytes(|bytes| {
            let prefix = b"FESTERM_GITHUB_IDENTITY_V2\0";
            let mut value: serde_json::Value =
                serde_json::from_slice(bytes.strip_prefix(prefix).unwrap()).unwrap();
            value["expires_at"] = 1.into();
            [prefix.as_slice(), &serde_json::to_vec(&value).unwrap()].concat()
        });
        secrets
            .update(&credential_ref, &SecretBytes::copy_from_slice(&expired))
            .unwrap();
        assert!(adapter
            .provision_with(&foreground, &cancel, &network, &secrets)
            .await
            .is_err());
        assert_eq!(network.state.borrow().creates, 0);
        adapter
            .sign_in_with(&foreground, &cancel, |_| {}, &network, &secrets)
            .await
            .unwrap();
        assert_eq!(network.state.borrow().sign_ins, 2);
        adapter
            .provision_with(&foreground, &cancel, &network, &secrets)
            .await
            .unwrap();
        let original = store.snapshot().unwrap().binding.unwrap();
        store.revoke().unwrap();
        let snapshot = store.snapshot().unwrap();
        store.confirm_stopped(snapshot.epoch, &original).unwrap();
        let next = super::tests::foreground(&adapter);
        assert!(adapter
            .sign_in_with(&foreground, &cancel, |_| panic!(), &network, &secrets)
            .await
            .is_err());
        adapter
            .sign_in_with(&next, &cancel, |_| panic!(), &network, &secrets)
            .await
            .unwrap();
        assert_eq!(
            adapter
                .provision_with(&next, &cancel, &network, &secrets)
                .await
                .unwrap(),
            Phase::Provisioned
        );
        assert_eq!(store.snapshot().unwrap().binding.unwrap(), original);
        assert_eq!(network.state.borrow().creates, 1);
        assert_eq!(network.state.borrow().sign_ins, 2);
        // Missing/expired vault credentials never become an empty cloud result.
        let record = adapter.load().unwrap();
        secrets.delete(&reference(&record).unwrap()).unwrap();
        assert!(adapter
            .health_with(&cancel, &network, &secrets)
            .await
            .is_err());
    }
}
