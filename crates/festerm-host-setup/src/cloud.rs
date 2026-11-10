//! Foreground account scope and zero-shell cloud provisioning. Optional runtime
//! relay admission reuses only the exact existing protected identity/receipt;
//! the lifecycle journal remains consent authority.

use crate::{
    lifecycle::{Binding, Origin, Phase, Snapshot, Store, Ticket},
    protected_state::Lease,
};
use festerm_secret_store::{SecretBytes, SecretReference, SecretStore};
use festerm_sessiond::private_storage as storage;
use festerm_tunnel_bridge::host_identity::HostIdentity;
use festerm_tunnel_client::{
    host::{
        CreateError, Management, Observation, PreparedCreate, Receipt, Resource, HOST_LABEL,
        HOST_PORT,
    },
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

pub(crate) fn subject(id: u64) -> String {
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
    creation_name: Option<String>,
    submitted: bool,
    receipt: Option<Receipt>,
}

/// Nonportable foreground intent. Only this live handle can initiate sign-in.
/// It carries no token and cannot be deserialized from a configuration file.
pub struct Foreground {
    record: Record,
}

/// Foreground acknowledgement of retained cloud uncertainty, not absence,
/// deletion, creation consent or native-stop evidence. Not deserializable.
pub struct UncertainRecovery {
    record: Record,
    epoch: u64,
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
    #[cfg(test)]
    fixture_user: Option<&'static str>,
}

trait Network {
    #[cfg(feature = "host-runtime")]
    async fn host_token(
        &self,
        identity: &Identity,
        exact: &Receipt,
        cancel: &CancellationToken,
    ) -> Result<zeroize::Zeroizing<String>, AccountError>;

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
    fn prepare_create(
        &self,
        identity: &Identity,
        resource: &Resource,
        name: &str,
    ) -> Result<PreparedCreate, AccountError> {
        Management::prepare_create(identity, resource, name)
    }
    async fn create(
        &self,
        prepared: PreparedCreate,
        cancel: &CancellationToken,
    ) -> Result<Receipt, CreateError>;
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
    #[cfg(feature = "host-runtime")]
    async fn host_token(
        &self,
        identity: &Identity,
        exact: &Receipt,
        cancel: &CancellationToken,
    ) -> Result<zeroize::Zeroizing<String>, AccountError> {
        self.management.host_token(identity, exact, cancel).await
    }

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
        prepared: PreparedCreate,
        cancel: &CancellationToken,
    ) -> Result<Receipt, CreateError> {
        self.management.submit(prepared, cancel).await
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
        Self {
            store,
            #[cfg(test)]
            fixture_user: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn fixture(store: &'a Store) -> Self {
        // Fake network fixtures must not require or authorize a native process token.
        Self {
            store,
            fixture_user: Some("fixture:ordinary-account"),
        }
    }

    fn os_user(&self) -> io::Result<String> {
        #[cfg(test)]
        if let Some(user) = self.fixture_user {
            return Ok(user.into());
        }
        os_user()
    }

    /// Open only the pinned zero-shell endpoint using its stable protected key.
    /// No account prompt, creation, owner enrollment or readiness claim occurs.
    #[cfg(feature = "host-runtime")]
    pub async fn open_relay(
        &self,
        cancel: &CancellationToken,
    ) -> io::Result<festerm_tunnel_bridge::relay::HostConnection> {
        let network = NativeNetwork::new()?;
        let secrets = festerm_secret_store::native_store()
            .map_err(|_| refused("host.secure_store_unavailable"))?;
        let (snapshot, exact, token, identity) = self
            .relay_authorization_with(cancel, &network, &*secrets)
            .await?;
        let connection = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(refused("host.cloud_cancelled")),
            result = tokio::time::timeout(
                std::time::Duration::from_secs(30),
                festerm_tunnel_bridge::relay::host_with_identity(
                    &exact.resource.tunnel_id,
                    &exact.resource.cluster,
                    exact.resource.port,
                    &token,
                    identity,
                )
            ) => result.map_err(|_| refused("host.relay_establishment_timeout"))?
                .map_err(|_| refused("host.relay_establishment_failed"))?,
        };
        if cancel.is_cancelled() || self.store.snapshot()? != snapshot {
            drop(connection);
            return Err(refused("host.service_stale_or_unapproved_operation"));
        }
        Ok(connection)
    }

    #[cfg(feature = "host-runtime")]
    async fn relay_authorization_with(
        &self,
        cancel: &CancellationToken,
        network: &impl Network,
        secrets: &dyn SecretStore,
    ) -> io::Result<(Snapshot, Receipt, zeroize::Zeroizing<String>, HostIdentity)> {
        let _operation = self.operation()?;
        let snapshot = self.store.snapshot()?;
        if snapshot.phase != Phase::Provisioned {
            return Err(refused("host.relay_current_provisioned_consent_required"));
        }
        let mut record = self.load()?;
        let exact = record
            .receipt
            .clone()
            .ok_or_else(|| refused("host.cloud_receipt_missing"))?;
        if snapshot.binding.as_ref() != Some(&binding(&record, &exact)) {
            return Err(refused("host.cloud_binding_changed"));
        }
        let account = self
            .load_identity(&mut record, network, secrets, cancel)
            .await?;
        let token = network
            .host_token(&account, &exact, cancel)
            .await
            .map_err(account_error)?;
        network
            .confirm(&account, cancel)
            .await
            .map_err(account_error)?;
        let identity = self.store.with_locked(|directory, current| {
            if cancel.is_cancelled() || current != &snapshot {
                return Err(refused("host.service_stale_or_unapproved_operation"));
            }
            let identity = HostIdentity::load_from_private_directory(
                directory,
                Path::new(HOST_IDENTITY_FILE),
            )?;
            if identity.host_id() != record.host_id || identity.fingerprint()? != record.fingerprint
            {
                return Err(refused("host.cloud_identity_changed"));
            }
            Ok(identity)
        })?;
        Ok((snapshot, exact, token, identity))
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
            let user = self.os_user()?;
            if let Some(mut record) = read(directory)? {
                if record.account_id != expected_github_subject {
                    return Err(refused("host.cloud_account_changed"));
                }
                // A new foreground ticket may reuse the same disabled identity,
                // resource and protected credential, never another account.
                record.ticket = ticket.clone();
                validate(&record, snapshot, directory, || self.os_user())?;
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
                creation_name: None,
                submitted: false,
                receipt: None,
            };
            crate::protected_state::publish_new(directory, Path::new(RECORD), &encode(&record)?)?;
            Ok(Foreground { record })
        })
    }

    /// Prepare explicit foreground recovery for the exact cancelled ticket,
    /// currently displayed lifecycle epoch and selected verified account.
    pub fn foreground_recovery(
        &self,
        ticket: &Ticket,
        expected_epoch: u64,
        expected_github_subject: u64,
    ) -> io::Result<UncertainRecovery> {
        let _operation = self.operation()?;
        let record = self.load()?;
        if record.account_id != expected_github_subject || &record.ticket != ticket {
            return Err(refused("host.cloud_recovery_scope_changed"));
        }
        let snapshot = self.store.snapshot()?;
        recovery_scope(&record, &snapshot, expected_epoch)?;
        Ok(UncertainRecovery {
            record,
            epoch: expected_epoch,
        })
    }

    /// Call only after explicit foreground acknowledgement that an unbound
    /// cloud resource may still exist. Retains the submitted journal, immutable
    /// reservation and credential reference; never asserts cloud absence.
    pub fn acknowledge_uncertain_submission(
        &self,
        recovery: &UncertainRecovery,
    ) -> io::Result<Phase> {
        let _operation = self.operation()?;
        let record = self.load()?;
        if record != recovery.record {
            return Err(refused("host.cloud_recovery_scope_changed"));
        }
        recovery_scope(&record, &self.store.snapshot()?, recovery.epoch)?;
        self.store
            .release_uncertain_enable(&record.ticket, recovery.epoch)?;
        Ok(Phase::Disabled)
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
            validate(record, snapshot, directory, || self.os_user())?;
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

    /// Explicit foreground provisioning/retry. An authenticated exact absence
    /// may retry only the same unpinned conditional reservation and body.
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
            record.creation_name = Some(record.ticket.approved_settings().friendly_name.clone());
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
            Observation::Absent if record.receipt.is_some() => {
                let result = network.confirm(&identity, cancel).await;
                self.check_cancelled(&record, cancel)?;
                result.map_err(account_error)?;
                return self.missing_resource(&record);
            }
            Observation::Absent => {
                self.require_foreground(&record, foreground, cancel)?;
                let result = network.confirm(&identity, cancel).await;
                self.check_cancelled(&record, cancel)?;
                result.map_err(account_error)?;
                // Earlier journals did not pin the create description. Only
                // proven unsubmitted intent may acquire it; never guess a
                // possibly submitted request's immutable body.
                if record.creation_name.is_none() && !record.submitted {
                    record.creation_name =
                        Some(record.ticket.approved_settings().friendly_name.clone());
                    self.save(&record)?;
                }
                let prepared = network
                    .prepare_create(
                        &identity,
                        &resource,
                        record
                            .creation_name
                            .as_deref()
                            .ok_or_else(|| refused("host.cloud_reservation_missing"))?,
                    )
                    .map_err(account_error)?;
                let was_submitted = record.submitted;
                record.submitted = true;
                // Fence submission under the same lifecycle lease as revoke.
                // Do not hold it across the following network operation.
                self.store.with_locked(|directory, snapshot| {
                    current_ticket(snapshot, &record.ticket, true)?;
                    write(directory, &record)
                })?;
                let result = network.create(prepared, cancel).await;
                if matches!(
                    result,
                    Err(CreateError::NotSent(_) | CreateError::Rejected(_))
                ) {
                    // This attempt provably created nothing. Earlier uncertain
                    // attempts, if any, still retain their reservation.
                    record.submitted = was_submitted;
                    self.save(&record)?;
                }
                if cancel.is_cancelled() {
                    self.store.cancel_enable(&record.ticket)?;
                }
                match result {
                    Ok(_) => {
                        // PUT timestamp precision is not a generation contract.
                        // Read-only, bounded GET completes late receipt capture
                        // even if the initiating foreground token was cancelled.
                        let verification = CancellationToken::new();
                        let observed = network.observe(&identity, &resource, &verification).await;
                        if cancel.is_cancelled() {
                            self.store.cancel_enable(&record.ticket)?;
                        }
                        match observed.map_err(account_error)? {
                            Observation::Present(receipt) => receipt,
                            Observation::Absent => {
                                return Err(refused("host.cloud_created_resource_not_observed"))
                            }
                        }
                    }
                    Err(
                        CreateError::NotSent(error)
                        | CreateError::Rejected(error)
                        | CreateError::Uncertain(error),
                    ) => return Err(account_error(error)),
                    Err(CreateError::Conflict) => {
                        let observed = network.observe(&identity, &resource, cancel).await;
                        if cancel.is_cancelled() {
                            self.store.cancel_enable(&record.ticket)?;
                        }
                        match observed.map_err(account_error)? {
                            Observation::Present(receipt) => receipt,
                            Observation::Absent => {
                                return Err(refused("host.cloud_conditional_conflict_unresolved"))
                            }
                        }
                    }
                }
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

    /// Recover a pending operation without interactive authentication or
    /// another create. Proven unsubmitted reservations need no credential store.
    /// Acknowledged Disabled uncertainty is retained until separate enable
    /// consent; reporting that lifecycle phase is not cloud absence.
    pub async fn reconcile(&self, cancel: &CancellationToken) -> io::Result<Phase> {
        {
            let _operation = self.operation()?;
            let record = self.load()?;
            if !record.submitted {
                return self.resolve_no_submission(&record);
            }
            if retained_unbound(&record, &self.store.snapshot()?) {
                return Ok(Phase::Disabled);
            }
        }
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
            return self.resolve_no_submission(&record);
        }
        if retained_unbound(&record, &self.store.snapshot()?) {
            return Ok(Phase::Disabled);
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
            Observation::Absent if record.receipt.is_some() => {
                let result = network.confirm(&identity, cancel).await;
                self.check_cancelled(&record, cancel)?;
                result.map_err(account_error)?;
                return self.missing_resource(&record);
            }
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

    // Both callers hold the operation lease: no local create can still be
    // in flight when the durable journal proves submission never happened.
    fn resolve_no_submission(&self, record: &Record) -> io::Result<Phase> {
        let snapshot = self.store.snapshot()?;
        if snapshot.phase == Phase::Disabled
            && snapshot.binding.is_none()
            && snapshot.pending_ticket().is_none()
        {
            return Ok(Phase::Disabled);
        }
        self.store.confirm_no_resources(&record.ticket)?;
        Ok(Phase::Disabled)
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

    fn missing_resource(&self, record: &Record) -> io::Result<Phase> {
        let receipt = record
            .receipt
            .as_ref()
            .ok_or_else(|| refused("host.cloud_receipt_missing"))?;
        self.store
            .record_missing_resource(&record.ticket, binding(record, receipt))
    }

    pub async fn health(&self, cancel: &CancellationToken) -> io::Result<Health> {
        let network = NativeNetwork::new()?;
        let secrets = festerm_secret_store::native_store()
            .map_err(|_| refused("host.secure_store_unavailable"))?;
        self.health_with(cancel, &network, &*secrets).await
    }

    /// Read-only adoption of an acknowledged uncertain submission for removal.
    /// Never enables access, requests sign-in, creates a resource or asserts
    /// absence after an uncertain submission. Native stop remains required.
    pub async fn adopt_uncertain_for_cleanup(
        &self,
        cancel: &CancellationToken,
    ) -> io::Result<Phase> {
        let network = NativeNetwork::new()?;
        let secrets =
            festerm_secret_store::native_store().map_err(|_| refused("host.sign_in_required"))?;
        self.adopt_uncertain_for_cleanup_with(cancel, &network, &*secrets)
            .await
    }

    async fn adopt_uncertain_for_cleanup_with(
        &self,
        cancel: &CancellationToken,
        network: &impl Network,
        secrets: &dyn SecretStore,
    ) -> io::Result<Phase> {
        let _operation = self.operation()?;
        let mut record = self.load()?;
        let snapshot = self.store.snapshot()?;
        if snapshot.phase != Phase::Disabled
            || snapshot.binding.is_some()
            || snapshot.pending_ticket().is_some()
            || snapshot.epoch <= record.ticket.epoch()
            || !record.submitted
        {
            return Err(refused("host.cloud_cleanup_adoption_unavailable"));
        }
        let identity = self
            .load_identity(&mut record, network, secrets, cancel)
            .await?;
        let resource = record
            .resource
            .as_ref()
            .ok_or_else(|| refused("host.cloud_reservation_missing"))?;
        let observed = network
            .observe(&identity, resource, cancel)
            .await
            .map_err(account_error)?;
        network
            .confirm(&identity, cancel)
            .await
            .map_err(account_error)?;
        if cancel.is_cancelled() {
            return Err(refused("host.cloud_cancelled"));
        }
        let Observation::Present(receipt) = observed else {
            return Err(refused("host.cloud_creation_outcome_uncertain"));
        };
        pin_receipt(&mut record, receipt.clone())?;
        // Retain authoritative identity before changing the lifecycle. A crash
        // or epoch race retries this same receipt, never new creation consent.
        self.save(&record)?;
        self.store
            .adopt_for_cleanup(&record.ticket, snapshot.epoch, binding(&record, &receipt))
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
            validate(&record, snapshot, directory, || self.os_user())?;
            Ok(record)
        })
    }

    fn save(&self, record: &Record) -> io::Result<()> {
        self.store.with_locked(|directory, snapshot| {
            validate(record, snapshot, directory, || self.os_user())?;
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

fn recovery_scope(record: &Record, snapshot: &Snapshot, epoch: u64) -> io::Result<()> {
    current_ticket(snapshot, &record.ticket, false)?;
    if snapshot.epoch != epoch
        || snapshot.phase != Phase::Reconciling
        || snapshot.epoch <= record.ticket.epoch()
        || snapshot.binding.is_some()
        || record.receipt.is_some()
        || !record.submitted
        || record.resource.is_none()
    {
        return Err(refused("host.cloud_uncertain_recovery_unavailable"));
    }
    Ok(())
}

fn retained_unbound(record: &Record, snapshot: &Snapshot) -> bool {
    record.submitted
        && record.receipt.is_none()
        && snapshot.phase == Phase::Disabled
        && snapshot.binding.is_none()
        && snapshot.pending_ticket().is_none()
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

fn validate(
    record: &Record,
    snapshot: &Snapshot,
    directory: &File,
    os_user: impl FnOnce() -> io::Result<String>,
) -> io::Result<()> {
    if record.version != 1
        || record.installation != snapshot.installation
        || record.origin != snapshot.origin
        || record.os_user != os_user()?
        || record.account_id == 0
        || record.ticket.epoch() > snapshot.epoch
        || record.submitted && record.resource.is_none()
        || record.resource.is_none() && record.creation_name.is_some()
        || record.creation_name.as_ref().is_some_and(|name| {
            name.trim().is_empty() || name.len() > 80 || name.chars().any(char::is_control)
        })
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
        prepare_error: Option<AccountError>,
        create_error: Option<CreateError>,
        cancel_before_send: bool,
        create_without_resource: bool,
        put_created: Option<&'static str>,
        get_created: Option<&'static str>,
        observe_error_after_create: Option<AccountError>,
        observes: usize,
        revoke_observe: bool,
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
        #[cfg(feature = "host-runtime")]
        async fn host_token(
            &self,
            identity: &Identity,
            exact: &Receipt,
            cancel: &CancellationToken,
        ) -> Result<zeroize::Zeroizing<String>, AccountError> {
            if cancel.is_cancelled() {
                return Err(AccountError::Cancelled);
            }
            if exact.resource.account_id != identity.account.id
                || self.state.borrow().created.as_ref() != Some(exact)
            {
                return Err(AccountError::IdentityChanged);
            }
            if self.state.borrow().revoke_observe {
                self.store.revoke().unwrap();
            }
            Ok(zeroize::Zeroizing::new(
                "PRIVATE_FIXTURE_SCOPED_HOST".into(),
            ))
        }

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
        fn prepare_create(
            &self,
            identity: &Identity,
            resource: &Resource,
            name: &str,
        ) -> Result<PreparedCreate, AccountError> {
            if let Some(error) = self.state.borrow().prepare_error {
                return Err(error);
            }
            Management::prepare_create(identity, resource, name)
        }
        async fn create(
            &self,
            prepared: PreparedCreate,
            cancel: &CancellationToken,
        ) -> Result<Receipt, CreateError> {
            let mut state = self.state.borrow_mut();
            if state.cancel_before_send {
                cancel.cancel();
                return Err(CreateError::NotSent(AccountError::Cancelled));
            }
            state.creates += 1;
            let receipt = Receipt {
                resource: prepared.resource().clone(),
                created: state.get_created.unwrap_or("2026-11-09T00:00:00Z").into(),
            };
            if let Some(error @ (CreateError::Rejected(_) | CreateError::NotSent(_))) =
                state.create_error
            {
                return Err(error);
            }
            if !state.create_without_resource {
                state.created = Some(receipt.clone());
            }
            if state.revoke_create {
                self.store.revoke().unwrap();
            }
            if state.cancel_create {
                cancel.cancel();
            }
            if state.lose_reply {
                return Err(CreateError::Uncertain(AccountError::Network));
            }
            if let Some(error) = state.create_error {
                return Err(error);
            }
            let mut receipt = receipt;
            if let Some(created) = state.put_created {
                receipt.created = created.into();
            }
            Ok(receipt)
        }
        async fn observe(
            &self,
            _: &Identity,
            _: &Resource,
            _: &CancellationToken,
        ) -> Result<Observation, AccountError> {
            let mut state = self.state.borrow_mut();
            state.observes += 1;
            if state.revoke_observe {
                self.store.revoke().unwrap();
            }
            if let Some(error) = state.observe_error {
                return Err(error);
            }
            if let Some(error) = state
                .observe_error_after_create
                .filter(|_| state.creates > 0)
            {
                return Err(error);
            }
            Ok(state
                .created
                .clone()
                .map_or(Observation::Absent, Observation::Present))
        }
    }

    fn fixture() -> (storage::PrivateTestDirectory, Store, MemorySecretStore) {
        let root = storage::private_test_directory();
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

    #[test]
    fn host_cloud_fixture_account_is_instance_scoped_and_rejects_another_os_user() {
        let (_root, store, _secrets) = fixture();
        let adapter = CloudHostAdapter::fixture(&store);
        foreground(&adapter);
        let other = CloudHostAdapter {
            store: &store,
            fixture_user: Some("fixture:another-account"),
        };
        assert_eq!(
            other.load().err().unwrap().to_string(),
            "host.cloud_scope_changed"
        );
        assert!(adapter.load().is_ok());
        let native = CloudHostAdapter::new(&store);
        assert!(native.fixture_user.is_none());
        assert!(match (native.os_user(), os_user()) {
            (Ok(actual), Ok(expected)) => actual == expected,
            (Err(actual), Err(expected)) => actual.kind() == expected.kind(),
            _ => false,
        });
    }

    #[test]
    fn host_cloud_record_validation_preserves_native_account_denial() {
        let (_root, store, _secrets) = fixture();
        let adapter = CloudHostAdapter::fixture(&store);
        let foreground = foreground(&adapter);
        store
            .with_locked(|directory, snapshot| {
                let error = validate(&foreground.record, snapshot, directory, || {
                    Err(io::ErrorKind::PermissionDenied.into())
                })
                .unwrap_err();
                assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
                Ok(())
            })
            .unwrap();
    }

    #[cfg(all(feature = "host-runtime", feature = "native-services"))]
    struct ControllerBoundary<'a> {
        network: FixtureNetwork<'a>,
        secrets: MemorySecretStore,
        events: std::cell::RefCell<Vec<&'static str>>,
        fail_install: std::cell::Cell<bool>,
    }

    #[cfg(all(feature = "host-runtime", feature = "native-services"))]
    impl crate::controller::SetupBoundary for ControllerBoundary<'_> {
        fn fixture_cloud<'a>(&self, store: &'a Store) -> CloudHostAdapter<'a> {
            CloudHostAdapter::fixture(store)
        }

        async fn sign_in(
            &self,
            cloud: &CloudHostAdapter<'_>,
            foreground: &Foreground,
            cancel: &CancellationToken,
            prompt: impl FnOnce(DevicePrompt),
        ) -> io::Result<()> {
            self.events.borrow_mut().push("sign-in");
            cloud
                .sign_in_with(foreground, cancel, prompt, &self.network, &self.secrets)
                .await
        }

        async fn provision(
            &self,
            cloud: &CloudHostAdapter<'_>,
            foreground: &Foreground,
            cancel: &CancellationToken,
        ) -> io::Result<()> {
            self.events.borrow_mut().push("provision");
            cloud
                .provision_with(foreground, cancel, &self.network, &self.secrets)
                .await
                .map(|_| ())
        }

        async fn health(
            &self,
            cloud: &CloudHostAdapter<'_>,
            cancel: &CancellationToken,
        ) -> io::Result<Health> {
            self.events.borrow_mut().push("health");
            cloud
                .health_with(cancel, &self.network, &self.secrets)
                .await
        }

        fn owners<'a>(
            &self,
            store: &'a Store,
        ) -> io::Result<crate::enrollment::OwnerEnrollment<'a>> {
            self.events.borrow_mut().push("owners");
            store.owner_enrollment_with_storage(
                crate::enrollment::OwnerStorage::HeadlessProtectedFile,
            )
        }

        fn install(
            &self,
            store: &Store,
            _state: &Path,
            stage: &crate::services::TrustedStage,
            epoch: u64,
        ) -> io::Result<crate::services::Observation> {
            stage.sessiond_path()?;
            let snapshot = store.snapshot()?;
            assert_eq!(snapshot.phase, Phase::Provisioned);
            assert_eq!(snapshot.epoch, epoch);
            let policy = store
                .owner_enrollment_with_storage(
                    crate::enrollment::OwnerStorage::HeadlessProtectedFile,
                )?
                .snapshot()?;
            assert_eq!(
                policy.binding.host.host.as_str(),
                snapshot.binding.as_ref().unwrap().host_id
            );
            self.events.borrow_mut().push("install");
            if self.fail_install.get() {
                return Err(refused("host.fixture_install_failed"));
            }
            Ok(crate::services::Observation {
                process: crate::services::ProcessState::Stopped,
                installation: snapshot.installation,
                origin: snapshot.origin,
                epoch,
            })
        }
    }

    #[cfg(all(feature = "host-runtime", feature = "native-services"))]
    fn controller_boundary(store: &Store, secrets: MemorySecretStore) -> ControllerBoundary<'_> {
        ControllerBoundary {
            network: FixtureNetwork::new(store),
            secrets,
            events: Default::default(),
            fail_install: Default::default(),
        }
    }

    #[cfg(all(feature = "host-runtime", feature = "native-services"))]
    fn owner_consent(key: [u8; 32]) -> crate::controller::OwnerConsent {
        crate::controller::OwnerConsent {
            owner_public_key: key,
            confirmed_owner_fingerprint: crate::enrollment::owner_key_fingerprint(&key).unwrap(),
            permissions: festerm_host_protocol::Capabilities {
                inventory: true,
                attach: true,
                detach: true,
                ensure: true,
                create: true,
                temporary: true,
                ..Default::default()
            },
        }
    }

    #[cfg(all(feature = "host-runtime", feature = "native-services"))]
    #[tokio::test(start_paused = true)]
    async fn host_controller_setup_orders_real_identity_account_policy_and_registration_without_shells(
    ) {
        let (root, store, secrets) = fixture();
        let boundary = controller_boundary(&store, secrets);
        let stage = crate::services::fixture_stage(&root.path().join("stage"));
        let cancel = CancellationToken::new();
        let result = crate::controller::setup_with(
            crate::controller::SetupRequest {
                store: &store,
                state: &root.path().join("host"),
                stage: &stage,
                account: 42,
                cancel: &cancel,
            },
            |_| boundary.events.borrow_mut().push("prompt"),
            &boundary,
        )
        .await
        .unwrap();
        assert_eq!(
            &*boundary.events.borrow(),
            &["sign-in", "prompt", "provision", "owners", "install"]
        );
        let initial = result.owner_approval_prerequisite;
        assert!(initial.keys.is_empty());
        assert!(!initial.remote_access);
        assert_eq!(initial.binding.account, "github:42");
        assert_eq!(
            result.native_registration.process,
            crate::services::ProcessState::Stopped
        );
        assert!(!root.path().join("host/daemon").exists());
        let identity =
            HostIdentity::load(&root.path().join("host").join(HOST_IDENTITY_FILE)).unwrap();
        assert_eq!(initial.binding.host.host.as_str(), identity.host_id());
        assert_eq!(
            initial.binding.host.host_key_fingerprint,
            identity.fingerprint().unwrap()
        );
        let key = festerm_tunnel_bridge::owner_auth::OwnerSigningKey::generate()
            .unwrap()
            .public_key();
        let approved =
            crate::controller::approve_owner_with(&store, owner_consent(key), &boundary).unwrap();
        assert!(approved.remote_access && approved.capabilities.temporary);
        assert!(!approved.capabilities.creation_allowed);
        assert_eq!(approved.binding, initial.binding);
        assert_eq!(approved.keys.len(), 1);
        let repeated =
            crate::controller::approve_owner_with(&store, owner_consent(key), &boundary).unwrap();
        assert_eq!(repeated.keys, approved.keys);
        assert_eq!(repeated.binding, approved.binding);
        boundary.events.borrow_mut().clear();
        let resumed = crate::controller::setup_with(
            crate::controller::SetupRequest {
                store: &store,
                state: &root.path().join("host"),
                stage: &stage,
                account: 42,
                cancel: &cancel,
            },
            |_| panic!("no repeated sign-in"),
            &boundary,
        )
        .await
        .unwrap();
        assert_eq!(&*boundary.events.borrow(), &["health", "owners", "install"]);
        assert_eq!(
            resumed.owner_approval_prerequisite.binding,
            approved.binding
        );
        assert_eq!(resumed.owner_approval_prerequisite.keys.len(), 1);
        assert_eq!(boundary.network.state.borrow().creates, 1);
        assert_eq!(boundary.network.state.borrow().sign_ins, 1);
    }

    #[cfg(all(feature = "host-runtime", feature = "native-services"))]
    #[tokio::test(start_paused = true)]
    async fn host_controller_owner_approval_fences_fingerprint_phase_creation_and_runtime_issuer() {
        let (root, store, secrets) = fixture();
        let boundary = controller_boundary(&store, secrets);
        let key = festerm_tunnel_bridge::owner_auth::OwnerSigningKey::generate()
            .unwrap()
            .public_key();
        assert!(
            crate::controller::approve_owner_with(&store, owner_consent(key), &boundary).is_err()
        );
        assert!(boundary.events.borrow().is_empty());
        let stage = crate::services::fixture_stage(&root.path().join("stage"));
        crate::controller::setup_with(
            crate::controller::SetupRequest {
                store: &store,
                state: &root.path().join("host"),
                stage: &stage,
                account: 42,
                cancel: &CancellationToken::new(),
            },
            |_| {},
            &boundary,
        )
        .await
        .unwrap();
        let owners = store
            .owner_enrollment_with_storage(crate::enrollment::OwnerStorage::HeadlessProtectedFile)
            .unwrap();
        let initial = owners.snapshot().unwrap();
        boundary.events.borrow_mut().clear();
        for refusal in ["fingerprint", "creation", "issuer"] {
            let mut consent = owner_consent(key);
            match refusal {
                "fingerprint" => {
                    consent.confirmed_owner_fingerprint = "wrong-reviewed-fingerprint".into()
                }
                "creation" => consent.permissions.creation_allowed = true,
                "issuer" => {
                    consent.permissions.issuance_epoch =
                        Some(festerm_host_protocol::Epoch::new(1).unwrap())
                }
                _ => unreachable!(),
            }
            assert!(
                crate::controller::approve_owner_with(&store, consent, &boundary).is_err(),
                "{refusal}"
            );
            assert_eq!(owners.snapshot().unwrap(), initial);
            assert!(boundary.events.borrow().is_empty());
        }
        let state = store.snapshot().unwrap();
        assert_eq!(state.phase, Phase::Provisioned);
        let wrong_account = crate::controller::setup_with(
            crate::controller::SetupRequest {
                store: &store,
                state: &root.path().join("host"),
                stage: &stage,
                account: 43,
                cancel: &CancellationToken::new(),
            },
            |_| panic!("no account switch"),
            &boundary,
        )
        .await;
        assert!(wrong_account.is_err());
        assert!(boundary.events.borrow().is_empty());
    }

    #[cfg(all(feature = "host-runtime", feature = "native-services"))]
    #[tokio::test(start_paused = true)]
    async fn host_controller_setup_late_revocation_retains_receipt_without_owner_or_install() {
        for after in ["sign-in", "provision"] {
            let (root, store, secrets) = fixture();
            let boundary = controller_boundary(&store, secrets);
            if after == "sign-in" {
                boundary.network.state.borrow_mut().revoke_sign_in = true;
            } else {
                boundary.network.state.borrow_mut().revoke_create = true;
            }
            let stage = crate::services::fixture_stage(&root.path().join("stage"));
            assert!(crate::controller::setup_with(
                crate::controller::SetupRequest {
                    store: &store,
                    state: &root.path().join("host"),
                    stage: &stage,
                    account: 42,
                    cancel: &CancellationToken::new()
                },
                |_| {},
                &boundary,
            )
            .await
            .is_err());
            assert!(!boundary.events.borrow().contains(&"owners"));
            assert!(!boundary.events.borrow().contains(&"install"));
            assert!(!root.path().join("host/owner-enrollment.json").exists());
            assert!(!root.path().join("host/daemon").exists());
            if after == "provision" {
                assert_eq!(store.snapshot().unwrap().phase, Phase::StopRequested);
                assert!(CloudHostAdapter::fixture(&store)
                    .load()
                    .unwrap()
                    .receipt
                    .is_some());
            } else {
                assert_eq!(store.snapshot().unwrap().phase, Phase::Reconciling);
                assert_eq!(boundary.network.state.borrow().creates, 0);
            }
        }
    }

    #[cfg(all(feature = "host-runtime", feature = "native-services"))]
    #[tokio::test(start_paused = true)]
    async fn host_controller_setup_retries_failed_install_without_duplicate_cloud_or_owner_identity(
    ) {
        let (root, store, secrets) = fixture();
        let boundary = controller_boundary(&store, secrets);
        boundary.fail_install.set(true);
        let stage = crate::services::fixture_stage(&root.path().join("stage"));
        assert!(crate::controller::setup_with(
            crate::controller::SetupRequest {
                store: &store,
                state: &root.path().join("host"),
                stage: &stage,
                account: 42,
                cancel: &CancellationToken::new()
            },
            |_| {},
            &boundary,
        )
        .await
        .is_err());
        let owners = store
            .owner_enrollment_with_storage(crate::enrollment::OwnerStorage::HeadlessProtectedFile)
            .unwrap();
        let initial = owners.snapshot().unwrap();
        assert!(initial.keys.is_empty() && !initial.remote_access);
        boundary.fail_install.set(false);
        boundary.events.borrow_mut().clear();
        let retried = crate::controller::setup_with(
            crate::controller::SetupRequest {
                store: &store,
                state: &root.path().join("host"),
                stage: &stage,
                account: 42,
                cancel: &CancellationToken::new(),
            },
            |_| panic!("no repeated sign-in"),
            &boundary,
        )
        .await
        .unwrap();
        assert_eq!(retried.owner_approval_prerequisite, initial);
        assert_eq!(&*boundary.events.borrow(), &["health", "owners", "install"]);
        assert_eq!(boundary.network.state.borrow().creates, 1);
        assert_eq!(boundary.network.state.borrow().sign_ins, 1);
    }

    #[cfg(all(feature = "host-runtime", feature = "native-services"))]
    #[tokio::test(start_paused = true)]
    async fn host_controller_creation_requires_configuration_and_independent_owner_consent() {
        let (root, store, secrets) = fixture();
        let mut settings = store.snapshot().unwrap().settings;
        settings.allow_creation = true;
        store.configure(settings).unwrap();
        let boundary = controller_boundary(&store, secrets);
        let stage = crate::services::fixture_stage(&root.path().join("stage"));
        crate::controller::setup_with(
            crate::controller::SetupRequest {
                store: &store,
                state: &root.path().join("host"),
                stage: &stage,
                account: 42,
                cancel: &CancellationToken::new(),
            },
            |_| {},
            &boundary,
        )
        .await
        .unwrap();
        let key = festerm_tunnel_bridge::owner_auth::OwnerSigningKey::generate()
            .unwrap()
            .public_key();
        let denied =
            crate::controller::approve_owner_with(&store, owner_consent(key), &boundary).unwrap();
        assert!(denied.capabilities.temporary);
        assert!(!denied.capabilities.creation_allowed);
        let mut explicit = owner_consent(key);
        explicit.permissions.creation_allowed = true;
        explicit.permissions.temporary = false;
        let approved = crate::controller::approve_owner_with(&store, explicit, &boundary).unwrap();
        assert!(approved.capabilities.creation_allowed);
        assert!(!approved.capabilities.temporary);
        assert_eq!(approved.keys.len(), 1);
        assert_eq!(approved.binding, denied.binding);
        assert!(!root.path().join("host/daemon").exists());
        store.revoke().unwrap();
        assert!(
            crate::controller::approve_owner_with(&store, owner_consent(key), &boundary).is_err()
        );
    }

    #[cfg(all(feature = "host-runtime", feature = "native-services"))]
    #[tokio::test(start_paused = true)]
    async fn host_controller_cancelled_or_missing_account_consent_has_no_identity_or_account_effect(
    ) {
        let (root, store, secrets) = fixture();
        let boundary = controller_boundary(&store, secrets);
        let stage = crate::services::fixture_stage(&root.path().join("stage"));
        for account in [0, 42] {
            let cancel = CancellationToken::new();
            if account == 42 {
                cancel.cancel();
            }
            assert!(crate::controller::setup_with(
                crate::controller::SetupRequest {
                    store: &store,
                    state: &root.path().join("host"),
                    stage: &stage,
                    account,
                    cancel: &cancel
                },
                |_| panic!("no prompt without consent"),
                &boundary,
            )
            .await
            .is_err());
            assert!(boundary.events.borrow().is_empty());
            assert_eq!(store.snapshot().unwrap().phase, Phase::Disabled);
            assert!(store.snapshot().unwrap().pending_ticket().is_none());
            assert!(!root.path().join("host").join(HOST_IDENTITY_FILE).exists());
        }
    }

    #[cfg(feature = "host-runtime")]
    #[tokio::test(start_paused = true)]
    async fn host_cloud_foreground_initializes_real_identity_once_for_provision_and_run() {
        let (root, store, secrets) = fixture();
        let path = root.path().join("host").join(HOST_IDENTITY_FILE);
        assert!(!path.exists());
        let adapter = CloudHostAdapter::fixture(&store);
        let approved = foreground(&adapter);
        let created = HostIdentity::load(&path).unwrap();
        let host_id = created.host_id();
        let fingerprint = created.fingerprint().unwrap();
        assert_eq!(approved.record.host_id, host_id);
        assert_eq!(approved.record.fingerprint, fingerprint);
        let repeated = adapter.foreground(&approved.record.ticket, 42).unwrap();
        assert_eq!(repeated.record.host_id, host_id);
        assert_eq!(repeated.record.fingerprint, fingerprint);

        let network = FixtureNetwork::new(&store);
        let cancel = CancellationToken::new();
        adapter
            .sign_in_with(&approved, &cancel, |_| {}, &network, &secrets)
            .await
            .unwrap();
        adapter
            .provision_with(&approved, &cancel, &network, &secrets)
            .await
            .unwrap();
        let (_, receipt, _token, existing) = adapter
            .relay_authorization_with(&cancel, &network, &secrets)
            .await
            .unwrap();
        assert_eq!(existing.host_id(), host_id);
        assert_eq!(existing.fingerprint().unwrap(), fingerprint);
        assert_eq!(receipt.resource.account_id, 42);
        assert_eq!(HostIdentity::load(&path).unwrap().host_id(), host_id);

        std::fs::remove_file(&path).unwrap();
        assert!(adapter
            .relay_authorization_with(&cancel, &network, &secrets)
            .await
            .is_err());
        assert!(adapter.foreground(&approved.record.ticket, 42).is_err());
        assert!(!path.exists());
        assert_eq!(network.state.borrow().creates, 1);
        assert_eq!(network.state.borrow().sign_ins, 1);

        let (other_root, other_store, _other_secrets) = fixture();
        let other_adapter = CloudHostAdapter::fixture(&other_store);
        let other = foreground(&other_adapter);
        let missing = other_root.path().join("host").join(HOST_IDENTITY_FILE);
        std::fs::remove_file(&missing).unwrap();
        assert!(other_adapter.foreground(&other.record.ticket, 42).is_err());
        assert!(!missing.exists());
    }

    #[cfg(feature = "host-runtime")]
    #[tokio::test(start_paused = true)]
    async fn host_cloud_relay_uses_existing_identity_and_scoped_token_without_enrollment() {
        let (_root, store, secrets) = fixture();
        let adapter = CloudHostAdapter::fixture(&store);
        let approval = foreground(&adapter);
        let network = FixtureNetwork::new(&store);
        let cancel = CancellationToken::new();
        adapter
            .sign_in_with(&approval, &cancel, |_| {}, &network, &secrets)
            .await
            .unwrap();
        adapter
            .provision_with(&approval, &cancel, &network, &secrets)
            .await
            .unwrap();
        let before = store.snapshot().unwrap();
        let pinned = adapter.load().unwrap();
        let (snapshot, receipt, token, identity) = adapter
            .relay_authorization_with(&cancel, &network, &secrets)
            .await
            .unwrap();
        assert_eq!(snapshot, before);
        assert_eq!(receipt, pinned.receipt.unwrap());
        assert_eq!(&*token, "PRIVATE_FIXTURE_SCOPED_HOST");
        assert_eq!(identity.host_id(), pinned.host_id);
        assert_eq!(identity.fingerprint().unwrap(), pinned.fingerprint);
        assert_eq!(network.state.borrow().creates, 1);
        assert_eq!(network.state.borrow().sign_ins, 1);
        assert_eq!(store.snapshot().unwrap(), before);
    }

    #[cfg(feature = "host-runtime")]
    #[tokio::test(start_paused = true)]
    async fn host_cloud_relay_refuses_revoke_cancel_replacement_and_absent_identity() {
        for refusal in ["revoke", "cancel", "replacement", "identity", "credential"] {
            let (_root, store, secrets) = fixture();
            let adapter = CloudHostAdapter::fixture(&store);
            let approval = foreground(&adapter);
            let network = FixtureNetwork::new(&store);
            let cancel = CancellationToken::new();
            adapter
                .sign_in_with(&approval, &cancel, |_| {}, &network, &secrets)
                .await
                .unwrap();
            adapter
                .provision_with(&approval, &cancel, &network, &secrets)
                .await
                .unwrap();
            let before = adapter.load().unwrap();
            let before_bytes = store
                .with_locked(|directory, _| {
                    storage::read_private(directory, Path::new(RECORD), 16384)
                })
                .unwrap();
            match refusal {
                "revoke" => network.state.borrow_mut().revoke_observe = true,
                "cancel" => cancel.cancel(),
                "replacement" => {
                    network.state.borrow_mut().created.as_mut().unwrap().created =
                        "another-incarnation".into();
                }
                "identity" => {
                    std::fs::remove_file(_root.path().join("host").join(HOST_IDENTITY_FILE))
                        .unwrap();
                }
                "credential" => {
                    secrets.delete(&reference(&before).unwrap()).unwrap();
                }
                _ => unreachable!(),
            }
            assert!(
                adapter
                    .relay_authorization_with(&cancel, &network, &secrets)
                    .await
                    .is_err(),
                "{refusal}"
            );
            assert_eq!(network.state.borrow().creates, 1);
            assert_eq!(network.state.borrow().sign_ins, 1);
            assert!(
                store
                    .with_locked(|directory, _| storage::read_private(
                        directory,
                        Path::new(RECORD),
                        16384
                    ))
                    .unwrap()
                    == before_bytes
            );
        }
    }

    #[cfg(all(feature = "host-runtime", feature = "native-services"))]
    #[tokio::test(start_paused = true)]
    async fn host_cloud_cleanup_adopts_acknowledged_uncertainty_without_enable_or_create() {
        let (root, store, secrets) = fixture();
        let adapter = CloudHostAdapter::fixture(&store);
        let initial = foreground(&adapter);
        let network = FixtureNetwork::new(&store);
        let cancel = CancellationToken::new();
        adapter
            .sign_in_with(&initial, &cancel, |_| {}, &network, &secrets)
            .await
            .unwrap();
        network.state.borrow_mut().lose_reply = true;
        assert!(adapter
            .provision_with(&initial, &cancel, &network, &secrets)
            .await
            .is_err());
        store.revoke().unwrap();
        let recovery = adapter
            .foreground_recovery(&initial.record.ticket, store.snapshot().unwrap().epoch, 42)
            .unwrap();
        adapter.acknowledge_uncertain_submission(&recovery).unwrap();
        let before = adapter.load().unwrap();
        let epoch = store.snapshot().unwrap().epoch;
        assert_eq!(
            adapter
                .adopt_uncertain_for_cleanup_with(&cancel, &network, &secrets)
                .await
                .unwrap(),
            Phase::StopRequested
        );
        let snapshot = store.snapshot().unwrap();
        assert!(snapshot.epoch > epoch);
        assert!(snapshot.pending_ticket().is_none());
        assert!(!snapshot.settings.allow_creation);
        let pinned = adapter.load().unwrap();
        assert_eq!(pinned.resource, before.resource);
        assert_eq!(pinned.credential, before.credential);
        assert_eq!(pinned.ticket, before.ticket);
        assert_eq!(network.state.borrow().creates, 1);
        assert_eq!(network.state.borrow().sign_ins, 1);
        assert!(store.request_cleanup().is_err());
        let state = root.path().join("host");
        let disabled = crate::controller::fixture_disable(&store, &state).unwrap();
        assert_eq!(disabled.lifecycle.phase, Phase::Disabled);
        assert_eq!(
            disabled.native_process.unwrap().process,
            crate::services::ProcessState::Absent
        );
        store.request_cleanup().unwrap();
        assert_eq!(
            crate::controller::fixture_uninstall(
                &store,
                &state,
                &cancel,
                adapter.cleanup_with(&cancel, &network, &secrets),
            )
            .await
            .unwrap(),
            crate::controller::UninstallResult::NativeRemovedCloudCleanupPending
        );
        assert_eq!(store.snapshot().unwrap().phase, Phase::CleanupPending);
        assert!(!state.join("native-service.json").exists());
        assert_eq!(network.state.borrow().creates, 1);
    }

    #[cfg(all(feature = "host-runtime", feature = "native-services"))]
    #[tokio::test(start_paused = true)]
    async fn host_controller_unpublished_registration_disable_and_uninstall_reach_cloud_cleanup() {
        for cancel_after_publish in [false, true] {
            let (root, store, secrets) = fixture();
            let state = root.path().join("host");
            let adapter = CloudHostAdapter::fixture(&store);
            let initial = foreground(&adapter);
            let network = FixtureNetwork::new(&store);
            let cancel = CancellationToken::new();
            adapter
                .sign_in_with(&initial, &cancel, |_| {}, &network, &secrets)
                .await
                .unwrap();
            network.state.borrow_mut().cancel_create = cancel_after_publish;
            let provisioned = adapter
                .provision_with(&initial, &cancel, &network, &secrets)
                .await;
            assert_eq!(provisioned.is_err(), cancel_after_publish);
            let receipt = adapter.load().unwrap().receipt.unwrap();
            assert!(!state.join("native-service.json").exists());
            let disabled = crate::controller::fixture_disable(&store, &state).unwrap();
            assert_eq!(disabled.lifecycle.phase, Phase::Disabled);
            assert_eq!(
                disabled.native_process.unwrap().process,
                crate::services::ProcessState::Absent
            );
            store.request_cleanup().unwrap();
            let cleanup_cancel = CancellationToken::new();
            assert_eq!(
                crate::controller::fixture_uninstall(
                    &store,
                    &state,
                    &cleanup_cancel,
                    adapter.cleanup_with(&cleanup_cancel, &network, &secrets),
                )
                .await
                .unwrap(),
                crate::controller::UninstallResult::NativeRemovedCloudCleanupPending
            );
            assert_eq!(store.snapshot().unwrap().phase, Phase::CleanupPending);
            assert_eq!(adapter.load().unwrap().receipt.as_ref(), Some(&receipt));
            network.state.borrow_mut().created = None;
            assert_eq!(
                crate::controller::fixture_uninstall(
                    &store,
                    &state,
                    &cleanup_cancel,
                    adapter.cleanup_with(&cleanup_cancel, &network, &secrets),
                )
                .await
                .unwrap(),
                crate::controller::UninstallResult::Removed
            );
            assert_eq!(store.snapshot().unwrap().phase, Phase::Removed);
            assert_eq!(network.state.borrow().creates, 1);
            assert_eq!(network.state.borrow().sign_ins, 1);
            assert!(!state.join("native-service.json").exists());
        }
    }

    #[tokio::test(start_paused = true)]
    async fn host_cloud_cleanup_adoption_retains_unknown_and_epoch_raced_receipts() {
        let (_root, store, secrets) = fixture();
        let adapter = CloudHostAdapter::fixture(&store);
        let initial = foreground(&adapter);
        let network = FixtureNetwork::new(&store);
        let cancel = CancellationToken::new();
        adapter
            .sign_in_with(&initial, &cancel, |_| {}, &network, &secrets)
            .await
            .unwrap();
        {
            let mut state = network.state.borrow_mut();
            state.lose_reply = true;
            state.create_without_resource = true;
        }
        assert!(adapter
            .provision_with(&initial, &cancel, &network, &secrets)
            .await
            .is_err());
        store.revoke().unwrap();
        let recovery = adapter
            .foreground_recovery(&initial.record.ticket, store.snapshot().unwrap().epoch, 42)
            .unwrap();
        adapter.acknowledge_uncertain_submission(&recovery).unwrap();
        let original = adapter.load().unwrap();
        assert!(adapter
            .adopt_uncertain_for_cleanup_with(&cancel, &network, &secrets)
            .await
            .is_err());
        assert!(adapter.load().unwrap() == original);
        assert_eq!(store.snapshot().unwrap().phase, Phase::Disabled);
        {
            let mut state = network.state.borrow_mut();
            state.created = Some(Receipt {
                resource: original.resource.clone().unwrap(),
                created: "2026-11-09T00:00:00Z".into(),
            });
            state.revoke_observe = true;
        }
        assert!(adapter
            .adopt_uncertain_for_cleanup_with(&cancel, &network, &secrets)
            .await
            .is_err());
        assert!(adapter.load().unwrap().receipt.is_some());
        assert!(store.snapshot().unwrap().binding.is_none());
        let pinned = adapter.load().unwrap();
        {
            let mut state = network.state.borrow_mut();
            state.revoke_observe = false;
            state.created.as_mut().unwrap().created = "foreign-incarnation".into();
        }
        assert!(adapter
            .adopt_uncertain_for_cleanup_with(&cancel, &network, &secrets)
            .await
            .is_err());
        assert!(adapter.load().unwrap() == pinned);
        network.state.borrow_mut().created = pinned.receipt.clone();
        assert_eq!(
            adapter
                .adopt_uncertain_for_cleanup_with(&cancel, &network, &secrets)
                .await
                .unwrap(),
            Phase::StopRequested
        );
        assert_eq!(network.state.borrow().creates, 1);
        assert_eq!(network.state.borrow().sign_ins, 1);
    }

    #[tokio::test(start_paused = true)]
    async fn host_cloud_cleanup_adoption_refuses_cancelled_missing_foreign_account_and_resource() {
        for refusal in ["cancelled", "missing-credential", "account", "resource"] {
            let (_root, store, secrets) = fixture();
            let adapter = CloudHostAdapter::fixture(&store);
            let initial = foreground(&adapter);
            let network = FixtureNetwork::new(&store);
            let cancel = CancellationToken::new();
            adapter
                .sign_in_with(&initial, &cancel, |_| {}, &network, &secrets)
                .await
                .unwrap();
            network.state.borrow_mut().lose_reply = true;
            assert!(adapter
                .provision_with(&initial, &cancel, &network, &secrets)
                .await
                .is_err());
            store.revoke().unwrap();
            let recovery = adapter
                .foreground_recovery(&initial.record.ticket, store.snapshot().unwrap().epoch, 42)
                .unwrap();
            adapter.acknowledge_uncertain_submission(&recovery).unwrap();
            let before = adapter.load().unwrap();
            let epoch = store.snapshot().unwrap().epoch;
            match refusal {
                "cancelled" => cancel.cancel(),
                "missing-credential" => {
                    secrets.delete(&reference(&before).unwrap()).unwrap();
                }
                "account" => {
                    network.state.borrow_mut().confirm_error = Some(AccountError::AccountChanged);
                }
                "resource" => {
                    network
                        .state
                        .borrow_mut()
                        .created
                        .as_mut()
                        .unwrap()
                        .resource
                        .account_id = 99;
                }
                _ => unreachable!(),
            }
            assert!(adapter
                .adopt_uncertain_for_cleanup_with(&cancel, &network, &secrets)
                .await
                .is_err());
            assert!(adapter.load().unwrap() == before);
            let snapshot = store.snapshot().unwrap();
            assert_eq!(snapshot.phase, Phase::Disabled);
            assert_eq!(snapshot.epoch, epoch);
            assert!(snapshot.binding.is_none());
            assert_eq!(network.state.borrow().creates, 1);
            assert_eq!(network.state.borrow().sign_ins, 1);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn host_cloud_foreground_sign_in_is_ticket_account_user_scoped_and_cancel_fenced() {
        let (root, store, secrets) = fixture();
        let adapter = CloudHostAdapter::fixture(&store);
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
        let adapter = CloudHostAdapter::fixture(&store);
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
        let recovered = CloudHostAdapter::fixture(&reopened);
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

    #[cfg(all(feature = "host-runtime", feature = "native-services"))]
    #[tokio::test(start_paused = true)]
    async fn host_cloud_health_observes_resource_and_cleanup_retains_exact_generation_blocker() {
        let (root, store, secrets) = fixture();
        let adapter = CloudHostAdapter::fixture(&store);
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
        crate::controller::fixture_disable(&store, &root.path().join("host")).unwrap();
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
            let adapter = CloudHostAdapter::fixture(&store);
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
            let result = adapter
                .provision_with(&foreground, &CancellationToken::new(), &network, &secrets)
                .await;
            if lose_reply {
                assert!(result.is_err());
            } else {
                assert_eq!(result.unwrap(), Phase::StopRequested);
            }
            assert_eq!(network.state.borrow().creates, 1);
        }
    }

    #[cfg(all(feature = "host-runtime", feature = "native-services"))]
    #[tokio::test(start_paused = true)]
    async fn host_cloud_reenable_and_credential_recovery_reuse_only_the_original_identity() {
        let (root, store, secrets) = fixture();
        let adapter = CloudHostAdapter::fixture(&store);
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
        crate::controller::fixture_disable(&store, &root.path().join("host")).unwrap();
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

    #[tokio::test(start_paused = true)]
    async fn host_cloud_local_no_send_and_rejection_resolve_without_widening_consent() {
        for scenario in 0..3 {
            let (_root, store, secrets) = fixture();
            let adapter = CloudHostAdapter::fixture(&store);
            let foreground = foreground(&adapter);
            let network = FixtureNetwork::new(&store);
            let cancel = CancellationToken::new();
            adapter
                .sign_in_with(&foreground, &cancel, |_| {}, &network, &secrets)
                .await
                .unwrap();
            {
                let mut state = network.state.borrow_mut();
                match scenario {
                    0 => state.prepare_error = Some(AccountError::Expired),
                    1 => state.cancel_before_send = true,
                    _ => state.create_error = Some(CreateError::Rejected(AccountError::Response)),
                }
            }
            assert!(adapter
                .provision_with(&foreground, &cancel, &network, &secrets)
                .await
                .is_err());
            let reserved = adapter.load().unwrap();
            assert!(!reserved.submitted);
            assert!(network.state.borrow().created.is_none());
            assert_eq!(
                adapter
                    .reconcile_with(&CancellationToken::new(), &network, &secrets)
                    .await
                    .unwrap(),
                Phase::Disabled
            );
            assert_eq!(store.snapshot().unwrap().phase, Phase::Disabled);
            assert_eq!(network.state.borrow().creates, usize::from(scenario == 2));
            let mut settings = store.snapshot().unwrap().settings;
            settings.friendly_name = "Renewed foreground name".into();
            store.configure(settings).unwrap();
            let next = super::tests::foreground(&adapter);
            {
                let mut state = network.state.borrow_mut();
                state.prepare_error = None;
                state.cancel_before_send = false;
                state.create_error = None;
            }
            assert!(adapter
                .provision_with(&foreground, &CancellationToken::new(), &network, &secrets)
                .await
                .is_err());
            assert_eq!(
                adapter
                    .provision_with(&next, &CancellationToken::new(), &network, &secrets)
                    .await
                    .unwrap(),
                Phase::Provisioned
            );
            let recovered = adapter.load().unwrap();
            assert_eq!(recovered.resource, reserved.resource);
            assert_eq!(recovered.creation_name, reserved.creation_name);
            assert!(!store.snapshot().unwrap().settings.allow_creation);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn host_cloud_retry_requires_authenticated_absence_and_same_immutable_reservation() {
        let (_root, store, secrets) = fixture();
        let adapter = CloudHostAdapter::fixture(&store);
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
        let original = adapter.load().unwrap();
        {
            let mut state = network.state.borrow_mut();
            state.created = None;
            state.lose_reply = false;
            state.observe_error = Some(AccountError::Denied);
        }
        assert!(adapter
            .provision_with(&foreground, &cancel, &network, &secrets)
            .await
            .is_err());
        network.state.borrow_mut().observe_error = None;
        network.state.borrow_mut().confirm_error = Some(AccountError::AccountChanged);
        assert!(adapter
            .provision_with(&foreground, &cancel, &network, &secrets)
            .await
            .is_err());
        assert_eq!(network.state.borrow().creates, 1);
        network.state.borrow_mut().confirm_error = None;
        // A non-foreground reconcile cannot submit, even after an exact 404.
        assert!(adapter
            .reconcile_with(&cancel, &network, &secrets)
            .await
            .is_err());
        assert_eq!(network.state.borrow().creates, 1);
        let mut legacy = original.clone();
        legacy.creation_name = None;
        adapter.save(&legacy).unwrap();
        assert!(adapter
            .provision_with(&foreground, &cancel, &network, &secrets)
            .await
            .is_err());
        assert_eq!(network.state.borrow().creates, 1);
        adapter.save(&original).unwrap();
        network.state.borrow_mut().create_error =
            Some(CreateError::Rejected(AccountError::Response));
        assert!(adapter
            .provision_with(&foreground, &cancel, &network, &secrets)
            .await
            .is_err());
        assert!(adapter.load().unwrap().submitted);
        assert_eq!(network.state.borrow().creates, 2);
        network.state.borrow_mut().create_error = None;
        assert_eq!(
            adapter
                .provision_with(&foreground, &cancel, &network, &secrets)
                .await
                .unwrap(),
            Phase::Provisioned
        );
        let recovered = adapter.load().unwrap();
        assert_eq!(network.state.borrow().creates, 3);
        assert_eq!(recovered.resource, original.resource);
        assert_eq!(recovered.creation_name, original.creation_name);
        assert_eq!(recovered.credential, original.credential);
    }

    #[tokio::test(start_paused = true)]
    async fn host_cloud_conditional_conflict_reconciles_exact_reservation_without_another_create() {
        let (_root, store, secrets) = fixture();
        let adapter = CloudHostAdapter::fixture(&store);
        let foreground = foreground(&adapter);
        let network = FixtureNetwork::new(&store);
        let cancel = CancellationToken::new();
        adapter
            .sign_in_with(&foreground, &cancel, |_| {}, &network, &secrets)
            .await
            .unwrap();
        network.state.borrow_mut().create_error = Some(CreateError::Conflict);
        assert_eq!(
            adapter
                .provision_with(&foreground, &cancel, &network, &secrets)
                .await
                .unwrap(),
            Phase::Provisioned
        );
        assert_eq!(network.state.borrow().creates, 1);
        assert_eq!(
            adapter.load().unwrap().receipt,
            network.state.borrow().created
        );
    }

    #[cfg(all(feature = "host-runtime", feature = "native-services"))]
    #[tokio::test(start_paused = true)]
    async fn host_cloud_pinned_absence_requires_exact_native_stop_before_cleanup() {
        for reconcile in [false, true] {
            let (root, store, secrets) = fixture();
            let adapter = CloudHostAdapter::fixture(&store);
            let initial = foreground(&adapter);
            let network = FixtureNetwork::new(&store);
            let cancel = CancellationToken::new();
            adapter
                .sign_in_with(&initial, &cancel, |_| {}, &network, &secrets)
                .await
                .unwrap();
            adapter
                .provision_with(&initial, &cancel, &network, &secrets)
                .await
                .unwrap();
            let binding = store.snapshot().unwrap().binding.unwrap();
            let receipt = adapter.load().unwrap().receipt;
            crate::controller::fixture_disable(&store, &root.path().join("host")).unwrap();
            network.state.borrow_mut().created = None;
            let next = foreground(&adapter);
            let phase = if reconcile {
                store.revoke().unwrap();
                adapter
                    .reconcile_with(&cancel, &network, &secrets)
                    .await
                    .unwrap()
            } else {
                adapter
                    .provision_with(&next, &cancel, &network, &secrets)
                    .await
                    .unwrap()
            };
            assert_eq!(phase, Phase::StopRequested);
            let stopped = store.snapshot().unwrap();
            assert_eq!(stopped.binding.as_ref(), Some(&binding));
            assert!(stopped.pending_ticket().is_none());
            assert_eq!(adapter.load().unwrap().receipt, receipt);
            assert_eq!(network.state.borrow().creates, 1);
            assert!(store.request_cleanup().is_err());
            assert!(store.confirm_no_resources(&next.record.ticket).is_err());
            let mut wrong = binding.clone();
            wrong.host_key_fingerprint = format!("{}x", wrong.host_key_fingerprint);
            assert!(store.confirm_stopped(stopped.epoch, &wrong).is_err());
            crate::controller::fixture_disable(&store, &root.path().join("host")).unwrap();
            store.request_cleanup().unwrap();
            assert_eq!(
                adapter
                    .cleanup_with(&cancel, &network, &secrets)
                    .await
                    .unwrap(),
                Cleanup::Removed
            );
            assert_eq!(store.snapshot().unwrap().phase, Phase::Removed);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn host_cloud_offline_cancel_acknowledgement_retains_uncertainty_and_observes_late_resource(
    ) {
        let (_root, store, secrets) = fixture();
        let adapter = CloudHostAdapter::fixture(&store);
        let initial = foreground(&adapter);
        let network = FixtureNetwork::new(&store);
        let cancel = CancellationToken::new();
        adapter
            .sign_in_with(&initial, &cancel, |_| {}, &network, &secrets)
            .await
            .unwrap();
        {
            let mut state = network.state.borrow_mut();
            state.create_error = Some(CreateError::Uncertain(AccountError::Network));
            state.create_without_resource = true;
        }
        assert!(adapter
            .provision_with(&initial, &cancel, &network, &secrets)
            .await
            .is_err());
        store.revoke().unwrap();
        let original = adapter.load().unwrap();
        let epoch = store.snapshot().unwrap().epoch;
        assert!(adapter
            .foreground_recovery(&initial.record.ticket, epoch, 99)
            .is_err());
        assert!(adapter
            .foreground_recovery(&initial.record.ticket, epoch - 1, 42)
            .is_err());
        let stale = adapter
            .foreground_recovery(&initial.record.ticket, epoch, 42)
            .unwrap();
        store.revoke().unwrap();
        assert!(adapter.acknowledge_uncertain_submission(&stale).is_err());
        let epoch = store.snapshot().unwrap().epoch;
        let recovery = adapter
            .foreground_recovery(&initial.record.ticket, epoch, 42)
            .unwrap();
        let (_other_root, other_store, _) = fixture();
        let other_adapter = CloudHostAdapter::fixture(&other_store);
        let other_foreground = foreground(&other_adapter);
        assert!(adapter
            .foreground_recovery(&other_foreground.record.ticket, epoch, 42)
            .is_err());
        assert!(other_adapter
            .acknowledge_uncertain_submission(&recovery)
            .is_err());
        assert!(adapter
            .reconcile_with(&cancel, &network, &secrets)
            .await
            .is_err());
        assert_eq!(store.snapshot().unwrap().phase, Phase::Reconciling);
        network.state.borrow_mut().observe_error = Some(AccountError::Network);
        assert!(adapter
            .reconcile_with(&cancel, &network, &secrets)
            .await
            .is_err());
        assert_eq!(store.snapshot().unwrap().phase, Phase::Reconciling);
        assert_eq!(
            adapter.acknowledge_uncertain_submission(&recovery).unwrap(),
            Phase::Disabled
        );
        assert!(adapter.load().unwrap() == original);
        assert!(adapter.load().unwrap().submitted);
        assert!(store.snapshot().unwrap().epoch > epoch);
        let observes = network.state.borrow().observes;
        assert_eq!(
            adapter
                .reconcile_with(&cancel, &network, &secrets)
                .await
                .unwrap(),
            Phase::Disabled
        );
        assert_eq!(network.state.borrow().observes, observes);
        assert!(adapter
            .health_with(&cancel, &network, &secrets)
            .await
            .is_err());
        assert!(store.request_cleanup().is_err());
        assert!(adapter.acknowledge_uncertain_submission(&recovery).is_err());
        {
            let mut state = network.state.borrow_mut();
            state.observe_error = None;
            state.create_error = None;
            state.created = Some(Receipt {
                resource: original.resource.clone().unwrap(),
                created: "2026-11-09T00:00:00Z".into(),
            });
        }
        assert_eq!(
            adapter
                .reconcile_with(&cancel, &network, &secrets)
                .await
                .unwrap(),
            Phase::Disabled
        );
        let next = foreground(&adapter);
        assert!(adapter
            .provision_with(&initial, &cancel, &network, &secrets)
            .await
            .is_err());
        assert_eq!(
            adapter
                .provision_with(&next, &cancel, &network, &secrets)
                .await
                .unwrap(),
            Phase::Provisioned
        );
        let recovered = adapter.load().unwrap();
        assert_eq!(recovered.resource, original.resource);
        assert_eq!(recovered.creation_name, original.creation_name);
        assert_eq!(recovered.credential, original.credential);
        assert_eq!(network.state.borrow().creates, 1);
        assert!(adapter.acknowledge_uncertain_submission(&recovery).is_err());
    }

    #[cfg(all(feature = "host-runtime", feature = "native-services"))]
    #[tokio::test(start_paused = true)]
    async fn host_cloud_uncertain_acknowledgement_refuses_receipts_and_native_stop_states() {
        let (root, store, secrets) = fixture();
        let adapter = CloudHostAdapter::fixture(&store);
        let initial = foreground(&adapter);
        let network = FixtureNetwork::new(&store);
        let cancel = CancellationToken::new();
        adapter
            .sign_in_with(&initial, &cancel, |_| {}, &network, &secrets)
            .await
            .unwrap();
        network.state.borrow_mut().lose_reply = true;
        assert!(adapter
            .provision_with(&initial, &cancel, &network, &secrets)
            .await
            .is_err());
        store.revoke().unwrap();
        let recovery = adapter
            .foreground_recovery(&initial.record.ticket, store.snapshot().unwrap().epoch, 42)
            .unwrap();
        // Interrupted receipt publication may precede the lifecycle binding.
        let mut record = adapter.load().unwrap();
        record.receipt = network.state.borrow().created.clone();
        adapter.save(&record).unwrap();
        assert!(adapter.acknowledge_uncertain_submission(&recovery).is_err());
        assert!(adapter
            .foreground_recovery(&initial.record.ticket, store.snapshot().unwrap().epoch, 42)
            .is_err());
        assert_eq!(
            adapter
                .reconcile_with(&cancel, &network, &secrets)
                .await
                .unwrap(),
            Phase::StopRequested
        );
        assert!(adapter.acknowledge_uncertain_submission(&recovery).is_err());
        assert!(adapter
            .foreground_recovery(&initial.record.ticket, store.snapshot().unwrap().epoch, 42)
            .is_err());
        assert!(store.request_cleanup().is_err());
        crate::controller::fixture_disable(&store, &root.path().join("host")).unwrap();
        assert!(adapter.acknowledge_uncertain_submission(&recovery).is_err());
        assert!(adapter.load().unwrap().receipt.is_some());
    }

    #[tokio::test(start_paused = true)]
    async fn host_cloud_put_success_pins_only_authoritative_get_and_retains_cancelled_receipt() {
        for scenario in 0..3 {
            let (_root, store, secrets) = fixture();
            let adapter = CloudHostAdapter::fixture(&store);
            let initial = foreground(&adapter);
            let network = FixtureNetwork::new(&store);
            let cancel = CancellationToken::new();
            adapter
                .sign_in_with(&initial, &cancel, |_| {}, &network, &secrets)
                .await
                .unwrap();
            {
                let mut state = network.state.borrow_mut();
                state.put_created = Some("2026-11-09T00:00:00.123Z");
                state.get_created = Some("2026-11-09T00:00:00.1234567Z");
                state.cancel_create = scenario == 1;
                if scenario == 2 {
                    state.observe_error_after_create = Some(AccountError::Network);
                }
            }
            let result = adapter
                .provision_with(&initial, &cancel, &network, &secrets)
                .await;
            if scenario == 2 {
                assert!(result.is_err());
                assert!(adapter.load().unwrap().receipt.is_none());
                assert!(store.snapshot().unwrap().binding.is_none());
                assert!(adapter.load().unwrap().submitted);
                store.revoke().unwrap();
                let recovery = adapter
                    .foreground_recovery(
                        &initial.record.ticket,
                        store.snapshot().unwrap().epoch,
                        42,
                    )
                    .unwrap();
                adapter.acknowledge_uncertain_submission(&recovery).unwrap();
                network.state.borrow_mut().observe_error_after_create = None;
                let next = foreground(&adapter);
                assert_eq!(
                    adapter
                        .provision_with(&next, &CancellationToken::new(), &network, &secrets)
                        .await
                        .unwrap(),
                    Phase::Provisioned
                );
            } else if scenario == 1 {
                assert!(result.is_err());
                assert_eq!(store.snapshot().unwrap().phase, Phase::StopRequested);
            } else {
                assert_eq!(result.unwrap(), Phase::Provisioned);
            }
            let record = adapter.load().unwrap();
            assert_eq!(
                record.receipt.unwrap().created,
                "2026-11-09T00:00:00.1234567Z"
            );
            assert_eq!(
                store.snapshot().unwrap().binding.unwrap().cloud_incarnation,
                "2026-11-09T00:00:00.1234567Z"
            );
            assert_eq!(network.state.borrow().creates, 1);
        }
    }
}
