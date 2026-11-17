//! Account/route-bound normal client bootstrap. Keys prove possession, never
//! enrolled-owner authority. Every new channel performs a fresh server proof.
use super::*;
use crate::ordinary_auth::{OrdinaryDeviceKey, OrdinaryHostInfo, OrdinaryRoute};
use festerm_tunnel_client::{identity::Identity, service};
use std::sync::Arc;

mod ordinary_keys;
#[cfg(all(test, feature = "testing"))]
#[path = "ordinary/ordinary_tests.rs"]
mod tests;
pub(super) use ordinary_keys::{KeyBinding, NativeKey};
pub use ordinary_keys::{KeyError, OrdinaryKeyIndex};
pub(super) const ORIGINAL_PURPOSE: &str = "fesTerm ordinary durable originals v1";

/// Captured bound-account lifetime, separate from a discovery chooser request.
#[derive(Clone)]
pub struct OrdinaryAccountGuard {
    pub(super) cancel: CancellationToken,
    current: Arc<dyn Fn() -> Result<(), ContractError> + Send + Sync>,
}
impl OrdinaryAccountGuard {
    pub fn new(
        cancel: CancellationToken,
        current: Arc<dyn Fn() -> Result<(), ContractError> + Send + Sync>,
    ) -> Self {
        Self { cancel, current }
    }
    pub fn check(&self) -> Result<(), ContractError> {
        if self.cancel.is_cancelled() {
            return Err(ContractError::Stale);
        }
        let result = (self.current)();
        if result.is_err() {
            self.cancel.cancel();
        }
        result
    }
}

// A cancelled chooser request must fence its pending native work without
// invalidating already-running tabs bound to the same account.
fn key_guard_for_request(
    account: &OrdinaryAccountGuard,
    request: &CancellationToken,
) -> OrdinaryAccountGuard {
    let account = account.clone();
    OrdinaryAccountGuard::new(request.clone(), Arc::new(move || account.check()))
}

struct Inner {
    info: OrdinaryHostInfo,
    binding: Binding,
    endpoint: TunnelEndpoint,
    token: Zeroizing<String>,
    key: NativeKey,
    guard: OrdinaryAccountGuard,
    #[cfg(feature = "testing")]
    fixture_only: bool,
}
/// Authenticated metadata and native possession snapshot, not a reusable grant.
#[derive(Clone)]
pub struct OrdinaryContext(Arc<Inner>);
impl OrdinaryContext {
    pub fn binding(&self) -> &Binding {
        &self.0.binding
    }
    pub fn host_info(&self) -> &OrdinaryHostInfo {
        &self.0.info
    }
    pub fn port(&self) -> u16 {
        self.0.info.route.port
    }
    pub(super) fn reference(&self) -> SecretReference {
        self.0.key.reference.duplicate_for_transport()
    }
    pub(super) fn key_index(&self) -> &OrdinaryKeyIndex {
        &self.0.key.index
    }
    pub(super) fn account_guard(&self) -> OrdinaryAccountGuard {
        self.0.guard.clone()
    }
    pub(super) fn vault(&self) -> Arc<dyn festerm_secret_store::SecretStore> {
        self.0.key.vault.clone()
    }

    pub(super) async fn connect(
        self,
        permit: ConnectionPermit,
        role: StreamRole,
        cancel: &CancellationToken,
    ) -> Result<(relay::ClientConnection, ConnectionPermit, Binding), Failure> {
        #[cfg(feature = "testing")]
        if self.0.fixture_only {
            return Err(ContractError::Denied.into());
        }
        self.0.guard.check()?;
        let stream = connect(
            &self.0.endpoint,
            self.port(),
            &self.0.token,
            cancel,
            &self.0.guard,
        )
        .await
        .map_err(|error| error.at_attach_stage_for(role, super::AttachStage::Relay))?;
        let expected = self.0.info.clone();
        let key = self.0.key.key.clone();
        let guard = self.0.guard.clone();
        let check = guard.clone();
        let route = expected.route.clone();
        let proof_route = route.clone();
        let (stream, permit, info) = crate::host_runtime::authenticate_ordinary_client_using_guard(
            stream,
            permit,
            route,
            role,
            cancel.clone(),
            move |connection, challenge| {
                guard.check()?;
                if !same_host(&challenge.host_info, &expected)
                    || challenge.host_info.policy_epoch.get() < expected.policy_epoch.get()
                {
                    return Err(ContractError::Stale);
                }
                key.prove(connection, &proof_route, challenge, role)
            },
            move || check.check(),
        )
        .await
        .map_err(|error| {
            super::Failure::Control(error)
                .at_attach_stage_for(role, super::AttachStage::Authentication)
        })?;
        self.0.guard.check()?;
        if !same_host(&info, &self.0.info)
            || info.policy_epoch.get() < self.0.info.policy_epoch.get()
        {
            return Err(Failure::BindingMismatch);
        }
        Ok((stream, permit, self.0.binding.clone()))
    }
}

async fn connect(
    endpoint: &TunnelEndpoint,
    port: u16,
    token: &str,
    cancel: &CancellationToken,
    guard: &OrdinaryAccountGuard,
) -> Result<relay::ClientConnection, Failure> {
    guard.check()?;
    tokio::select! {
        biased;
        _=cancel.cancelled()=>Err(Error::Cancelled.into()),
        _=guard.cancel.cancelled()=>Err(Error::Cancelled.into()),
        value=tokio::time::timeout(Duration::from_secs(15),relay::connect_ordinary_endpoint(endpoint,port,token)) => value.map_err(|_|Failure::Control(Error::Deadline))?.map_err(|error|Failure::Control(error.into())),
    }
}

fn route(resolved: &service::ResolvedTunnel) -> Result<OrdinaryRoute, Failure> {
    let binding = &resolved.binding;
    let endpoint = &resolved.endpoint;
    if binding.host_id != endpoint.host_id
        || endpoint.host_public_keys.as_slice() != [binding.public_key.as_str()]
        || resolved.connect_token.is_empty()
        || resolved.connect_token.len() > 16 * 1024
    {
        return Err(Failure::BindingMismatch);
    }
    let host = crate::owner_auth::PinnedHost {
        host: binding.host_id.clone().try_into()?,
        host_key_fingerprint: relay::host_fingerprint(&binding.public_key)
            .map_err(|_| Failure::BindingMismatch)?,
    };
    let route = OrdinaryRoute {
        tunnel_id: binding.tunnel_id.clone(),
        cluster: binding.cluster.clone(),
        endpoint_id: endpoint.id.clone().ok_or(Failure::BindingMismatch)?,
        port: binding.port,
        host,
    };
    route.validate()?;
    Ok(route)
}

/// Resolve via the provider's authenticated management API, prepare the exact
/// native account/route key before SSH, then return genuine HostInfo/inventory.
pub async fn discover(
    identity: &Identity,
    locator: &str,
    index: OrdinaryKeyIndex,
    budgets: &Budgets,
    account_guard: OrdinaryAccountGuard,
    request_cancel: &CancellationToken,
) -> Result<(OrdinaryContext, Inventory), Failure> {
    account_guard.check()?;
    identity
        .account
        .validate()
        .map_err(|_| Failure::BindingMismatch)?;
    let permit = ConnectionPermit::reserve(budgets)?;
    let resolved = tokio::select! {
        biased;
        _=request_cancel.cancelled()=>return Err(Error::Cancelled.into()),
        _=account_guard.cancel.cancelled()=>return Err(Error::Cancelled.into()),
        value=tokio::time::timeout(Duration::from_secs(30),service::resolve_host(identity,locator,request_cancel))=>value.map_err(|_|Failure::Control(Error::Deadline))?.map_err(|e| match e.original_error() {
            festerm_tunnel_client::Error::Expired|festerm_tunnel_client::Error::AccountChanged=>Failure::ProviderIdentityExpired,
            festerm_tunnel_client::Error::Cancelled=>Failure::Control(Error::Cancelled),
            _=>Failure::ProviderUnavailable,
        })?,
    };
    account_guard.check()?;
    let route = route(&resolved)?;
    let key_binding = KeyBinding::new(identity.account.id, route.clone())?;
    let key_guard = key_guard_for_request(&account_guard, request_cancel);
    let (key, permit) = tokio::select! {
        biased;
        _ = account_guard.cancel.cancelled() => return Err(Error::Cancelled.into()),
        value = open_native_owner_key(permit, request_cancel, move || index.prepare(&key_binding, &key_guard)) => value?,
    };
    account_guard.check()?;
    let stream = connect(
        &resolved.endpoint,
        route.port,
        &resolved.connect_token,
        request_cancel,
        &account_guard,
    )
    .await?;
    let signing = key.key.clone();
    let proof_route = route.clone();
    let proof_guard = account_guard.clone();
    let publish_guard = account_guard.clone();
    let authenticated = crate::host_runtime::authenticate_ordinary_client_using_guard(
        stream,
        permit,
        route,
        StreamRole::Control,
        request_cancel.clone(),
        move |connection, challenge| {
            proof_guard.check()?;
            signing.prove(connection, &proof_route, challenge, StreamRole::Control)
        },
        move || publish_guard.check(),
    );
    let (stream, permit, info) = tokio::select! {
        biased;
        _ = account_guard.cancel.cancelled() => return Err(Error::Cancelled.into()),
        value = authenticated => value?,
    };
    account_guard.check()?;
    let binding = Binding::new(
        info.route.host.host.clone(),
        format!("github:{}", identity.account.id),
        format!(
            "{}.{}:{}",
            info.route.tunnel_id, info.route.cluster, info.route.port
        ),
    )?;
    let mut client = ControlClient::on_authenticated_stream(stream, binding.clone(), permit);
    let inventory = tokio::select! {
        biased;
        _ = account_guard.cancel.cancelled() => return Err(Error::Cancelled.into()),
        value = inventory_authenticated(&mut client, OperationId::new(), request_cancel) => value?,
    };
    account_guard.check()?;
    if request_cancel.is_cancelled() {
        return Err(Error::Cancelled.into());
    }
    Ok((
        OrdinaryContext(Arc::new(Inner {
            info,
            binding,
            endpoint: resolved.endpoint,
            token: resolved.connect_token,
            key,
            guard: account_guard,
            #[cfg(feature = "testing")]
            fixture_only: false,
        })),
        inventory,
    ))
}

fn same_host(a: &OrdinaryHostInfo, b: &OrdinaryHostInfo) -> bool {
    a.version == b.version
        && a.route == b.route
        && a.installation == b.installation
        && a.principal == b.principal
}

/// UI composition fixture only: private storage + memory vault, and transport
/// explicitly denied. This cannot provide production or provider admission.
#[cfg(feature = "testing")]
pub(crate) fn testing_context(
    index: OrdinaryKeyIndex,
    account_id: u64,
    info: OrdinaryHostInfo,
    guard: OrdinaryAccountGuard,
) -> Result<OrdinaryContext, Failure> {
    info.validate()?;
    guard.check()?;
    let key_binding = KeyBinding::new(account_id, info.route.clone())?;
    let vault = Arc::new(festerm_secret_store::MemorySecretStore::new());
    let key = index
        .prepare_with(vault, &key_binding, &guard)
        .map_err(Failure::OrdinaryKey)?;
    let endpoint = TunnelEndpoint {
        id: Some(info.route.endpoint_id.clone()),
        host_id: info.route.host.host.to_string(),
        connection_mode: tunnels::contracts::TunnelConnectionMode::TunnelRelay,
        host_public_keys: Vec::new(),
        port_uri_format: None,
        tunnel_uri: None,
        port_ssh_command_format: None,
        tunnel_ssh_command: None,
        ssh_gateway_public_key: None,
        local_network_tunnel_endpoint: Default::default(),
        tunnel_relay_tunnel_endpoint: Default::default(),
    };
    let binding = Binding::new(
        info.route.host.host.clone(),
        format!("github:{account_id}"),
        format!(
            "{}.{}:{}",
            info.route.tunnel_id, info.route.cluster, info.route.port
        ),
    )?;
    Ok(OrdinaryContext(Arc::new(Inner {
        info,
        binding,
        endpoint,
        token: Zeroizing::new("fixture-token-not-a-provider-credential".into()),
        key,
        guard,
        fixture_only: true,
    })))
}
