use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use tunnels::{
    contracts::{Tunnel, TunnelConnectionMode, TunnelEndpoint},
    management::{
        new_tunnel_management, Authorization, HttpError, TunnelLocator, TunnelRequestOptions,
    },
};
use zeroize::{Zeroize, Zeroizing};

use crate::{http_client, identity::Identity, valid_token, Error, Https};

pub const BRIDGE_PORT_LABEL: &str = "festerm-bridge-v1";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HostBinding {
    pub tunnel_id: String,
    pub cluster: String,
    pub host_id: String,
    pub public_key: String,
    pub port: u16,
}

pub struct ResolvedTunnel {
    pub binding: HostBinding,
    pub endpoint: TunnelEndpoint,
    pub connect_token: Zeroizing<String>,
}

pub fn locator(name: &str) -> Result<TunnelLocator, Error> {
    fn identifier(value: &str) -> bool {
        !value.is_empty()
            && value.len() <= 100
            && value
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            && !value.starts_with('-')
            && !value.ends_with('-')
    }
    if let Some((id, cluster)) = name.split_once('.') {
        if identifier(id) && identifier(cluster) {
            return Ok(TunnelLocator::ID {
                id: id.into(),
                cluster: cluster.into(),
            });
        }
    } else if identifier(name) {
        return Ok(TunnelLocator::Name(name.into()));
    }
    Err(Error::Tunnel)
}

pub async fn resolve(
    identity: &Identity,
    name: &str,
    cancel: &CancellationToken,
) -> Result<ResolvedTunnel, Error> {
    let locator = locator(name)?;
    let identity_http = Https::new()?;
    identity.confirm_account(&identity_http, cancel).await?;
    let mut builder = new_tunnel_management("fesTerm-native-tunnel/1");
    builder.client(http_client()?);
    builder.authorization(Authorization::Github(identity.access_token().into()));
    let management: tunnels::management::TunnelManagementClient = builder.into();
    let options = TunnelRequestOptions {
        include_ports: true,
        token_scopes: vec!["connect".into()],
        ..Default::default()
    };
    let tunnel = tokio::select! {
        _ = cancel.cancelled() => return Err(Error::Cancelled),
        value = management.get_tunnel(&locator, &options) => value.map_err(management_error)?,
    };
    let resolved = select(tunnel)?;
    identity.confirm_account(&identity_http, cancel).await?;
    if cancel.is_cancelled() {
        return Err(Error::Cancelled);
    }
    Ok(resolved)
}

fn management_client(
    identity: &Identity,
) -> Result<tunnels::management::TunnelManagementClient, Error> {
    let mut builder = new_tunnel_management("fesTerm-native-tunnel/1");
    builder.client(http_client()?);
    builder.authorization(Authorization::Github(identity.access_token().into()));
    Ok(builder.into())
}

/// Request body for a lifetime extension: only the fields the service accepts
/// on update, with the validity window set to `days` from now. Server-owned
/// state (tokens, status, endpoints, ports, timestamps) is dropped.
pub fn extension_update(mut tunnel: Tunnel, days: u32) -> Tunnel {
    wipe_tokens(&mut tunnel);
    tunnel.access_tokens = None;
    tunnel.status = None;
    tunnel.endpoints = Vec::new();
    tunnel.ports = Vec::new();
    tunnel.created = None;
    tunnel.expiration = None;
    tunnel.custom_expiration = Some(days.saturating_mul(86_400));
    tunnel
}

/// Pushes the tunnel's service-side expiry `days` forward. Only the tunnel's
/// owner can do this; callers treat failure as non-fatal.
pub async fn extend_lifetime(
    identity: &Identity,
    name: &str,
    days: u32,
    cancel: &CancellationToken,
) -> Result<(), Error> {
    let locator = locator(name)?;
    let management = management_client(identity)?;
    let options = TunnelRequestOptions::default();
    let tunnel = tokio::select! {
        _ = cancel.cancelled() => return Err(Error::Cancelled),
        value = management.get_tunnel(&locator, &options) => value.map_err(management_error)?,
    };
    let update = extension_update(tunnel, days);
    tokio::select! {
        _ = cancel.cancelled() => Err(Error::Cancelled),
        value = management.update_tunnel(&update, &options) => {
            value.map(|_| ()).map_err(management_error)
        }
    }
}

/// A tunnel owned by the signed-in account that hosts a fesTerm bridge.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TunnelSummary {
    /// `id.cluster` locator, accepted as a profile's tunnel name.
    pub locator: String,
    pub name: Option<String>,
    pub description: Option<String>,
    pub active: bool,
}

/// Lists the account's tunnels that carry a fesTerm bridge port, active hosts
/// first.
pub async fn list(
    identity: &Identity,
    cancel: &CancellationToken,
) -> Result<Vec<TunnelSummary>, Error> {
    let identity_http = Https::new()?;
    identity.confirm_account(&identity_http, cancel).await?;
    let management = management_client(identity)?;
    let options = TunnelRequestOptions {
        include_ports: true,
        ..Default::default()
    };
    let tunnels = tokio::select! {
        _ = cancel.cancelled() => return Err(Error::Cancelled),
        value = management.list_all_tunnels(&options) => value.map_err(management_error)?,
    };
    identity.confirm_account(&identity_http, cancel).await?;
    Ok(summarize(tunnels))
}

pub fn summarize(tunnels: Vec<Tunnel>) -> Vec<TunnelSummary> {
    let mut summaries: Vec<TunnelSummary> = tunnels
        .into_iter()
        .filter(|tunnel| {
            tunnel.ports.iter().any(|port| {
                port.labels.iter().any(|label| label == BRIDGE_PORT_LABEL)
                    || port.description.as_deref() == Some(BRIDGE_PORT_LABEL)
            })
        })
        .filter_map(|tunnel| {
            let locator = format!("{}.{}", tunnel.tunnel_id?, tunnel.cluster_id?);
            self::locator(&locator).ok()?;
            let hosts = tunnel
                .status
                .as_ref()
                .and_then(|status| status.host_connection_count.as_ref())
                .is_some_and(|count| count.get_count() > 0);
            Some(TunnelSummary {
                locator,
                name: tunnel.name,
                description: tunnel.description,
                active: hosts || !tunnel.endpoints.is_empty(),
            })
        })
        .collect();
    summaries.sort_by_key(|summary| !summary.active);
    summaries
}

pub fn select(mut tunnel: Tunnel) -> Result<ResolvedTunnel, Error> {
    let token = tunnel
        .access_tokens
        .as_mut()
        .and_then(|tokens| tokens.remove("connect"))
        .map(Zeroizing::new);
    wipe_tokens(&mut tunnel);
    let token = token.ok_or_else(|| {
        tracing::warn!(
            stage = "tunnel-connect-token",
            "Native Dev Tunnel response rejected"
        );
        Error::Denied
    })?;
    select_with_token(tunnel, token)
}

/// Qualification/advanced scoped-token path, independent of OAuth registration.
/// The ordinary application calls `resolve` with its native identity broker.
pub async fn resolve_scoped(
    name: &str,
    token: Zeroizing<String>,
    cancel: &CancellationToken,
) -> Result<ResolvedTunnel, Error> {
    if !valid_token(&token) {
        return Err(Error::Response);
    }
    let locator = locator(name)?;
    let mut builder = new_tunnel_management("fesTerm-native-tunnel/1");
    builder.client(http_client()?);
    builder.authorization(Authorization::Tunnel(token.to_string()));
    let management: tunnels::management::TunnelManagementClient = builder.into();
    let options = TunnelRequestOptions {
        include_ports: true,
        ..Default::default()
    };
    let tunnel = tokio::select! {
        _ = cancel.cancelled() => return Err(Error::Cancelled),
        value = management.get_tunnel(&locator, &options) => value.map_err(management_error)?,
    };
    select_with_token(tunnel, token)
}

fn management_error(error: HttpError) -> Error {
    let (error, status) = match error {
        HttpError::ConnectionError(_) => (Error::Network, None),
        HttpError::AuthorizationError(_) => (Error::Response, None),
        HttpError::ResponseError(response) => {
            let status = response.status_code.as_u16();
            let error = match status {
                401 | 403 => Error::Denied,
                404 => Error::TunnelNotFound,
                429 => Error::RateLimited,
                500..=599 => Error::Network,
                _ => Error::Response,
            };
            (error, Some(status))
        }
    };
    tracing::warn!(
        stage = "tunnel-lookup-http",
        http_status = ?status,
        error = ?error,
        "Native Dev Tunnel lookup failed"
    );
    error
}

fn wipe_tokens(tunnel: &mut Tunnel) {
    if let Some(tokens) = tunnel.access_tokens.as_mut() {
        for token in tokens.values_mut() {
            token.zeroize();
        }
    }
    for port in &mut tunnel.ports {
        if let Some(tokens) = port.access_tokens.as_mut() {
            for token in tokens.values_mut() {
                token.zeroize();
            }
        }
    }
}

fn select_with_token(
    mut tunnel: Tunnel,
    token: Zeroizing<String>,
) -> Result<ResolvedTunnel, Error> {
    wipe_tokens(&mut tunnel);
    if !valid_token(&token) {
        return Err(Error::Response);
    }
    if tunnel.endpoints.len() > 1 || tunnel.ports.len() > 1 {
        return Err(Error::Ambiguous);
    }
    let port = tunnel
        .ports
        .first()
        .filter(|port| {
            port.labels.iter().any(|label| label == BRIDGE_PORT_LABEL)
                || port.description.as_deref() == Some(BRIDGE_PORT_LABEL)
        })
        .ok_or(Error::Tunnel)?
        .port_number;
    let endpoint = tunnel.endpoints.pop().ok_or(Error::Tunnel)?;
    if port == 0
        || !matches!(endpoint.connection_mode, TunnelConnectionMode::TunnelRelay)
        || endpoint.host_id.is_empty()
        || endpoint.host_id.len() > 128
        || endpoint.host_public_keys.len() != 1
        || endpoint.host_public_keys[0].is_empty()
        || endpoint.host_public_keys[0].len() > 4096
    {
        return Err(Error::Tunnel);
    }
    let tunnel_id = tunnel.tunnel_id.take().ok_or(Error::Tunnel)?;
    let cluster = tunnel.cluster_id.take().ok_or(Error::Tunnel)?;
    locator(&format!("{tunnel_id}.{cluster}"))?;
    Ok(ResolvedTunnel {
        binding: HostBinding {
            tunnel_id,
            cluster,
            host_id: endpoint.host_id.clone(),
            public_key: endpoint.host_public_keys[0].clone(),
            port,
        },
        endpoint,
        connect_token: token,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listed(json: &str) -> Vec<Tunnel> {
        serde_json::from_str(json).expect("tunnel fixture")
    }

    #[test]
    fn listing_keeps_bridge_tunnels_and_puts_active_hosts_first() {
        let tunnels = listed(
            r#"[
              {"tunnelId":"idle1","clusterId":"use","name":"idle","ports":[{"portNumber":22,"labels":["festerm-bridge-v1"]}]},
              {"tunnelId":"other","clusterId":"use","ports":[{"portNumber":80,"labels":["web"]}]},
              {"tunnelId":"live1","clusterId":"euw","description":"laptop","status":{"hostConnectionCount":1},"ports":[{"portNumber":22,"labels":["festerm-bridge-v1"]}]}
            ]"#,
        );
        let summaries = summarize(tunnels);
        assert_eq!(
            summaries
                .iter()
                .map(|s| (s.locator.as_str(), s.active))
                .collect::<Vec<_>>(),
            [("live1.euw", true), ("idle1.use", false)]
        );
        assert_eq!(summaries[0].description.as_deref(), Some("laptop"));
    }

    #[test]
    fn lifetime_extension_sends_only_client_owned_fields() {
        let tunnel = listed(
            r#"[{"tunnelId":"t1","clusterId":"use","name":"mine","accessTokens":{"connect":"secret"},
              "status":{"hostConnectionCount":1},"expiration":"2030-01-01T00:00:00Z",
              "ports":[{"portNumber":22}],"endpoints":[]}]"#,
        )
        .remove(0);
        let update = extension_update(tunnel, 30);
        assert_eq!(update.custom_expiration, Some(30 * 86_400));
        assert!(update.access_tokens.is_none());
        assert!(update.status.is_none());
        assert!(update.ports.is_empty());
        assert!(update.expiration.is_none());
        assert_eq!(update.name.as_deref(), Some("mine"));
    }

    #[test]
    fn native_service_lookup_distinguishes_denial_missing_tunnels_and_service_failures() {
        for (status, expected) in [
            (307, Error::Response),
            (400, Error::Response),
            (401, Error::Denied),
            (403, Error::Denied),
            (404, Error::TunnelNotFound),
            (429, Error::RateLimited),
            (500, Error::Network),
            (503, Error::Network),
        ] {
            let error = HttpError::ResponseError(tunnels::management::ResponseError {
                url: "https://global.rel.tunnels.api.visualstudio.com/tunnels/owned"
                    .parse()
                    .unwrap(),
                status_code: reqwest::StatusCode::from_u16(status).unwrap(),
                data: Some("PRIVATE_SERVICE_DETAILS".into()),
                request_id: Some("PRIVATE_REQUEST_ID".into()),
            });
            assert_eq!(management_error(error), expected);
        }
        assert_eq!(
            management_error(HttpError::AuthorizationError("PRIVATE_RESPONSE".into())),
            Error::Response
        );
        let connection = reqwest::Client::new()
            .get("invalid url")
            .build()
            .unwrap_err();
        assert_eq!(
            management_error(HttpError::ConnectionError(connection)),
            Error::Network
        );
    }

    fn tunnel() -> Tunnel {
        serde_json::from_value(serde_json::json!({
            "tunnelId":"owned","clusterId":"usw3","accessTokens":{"connect":"PRIVATE_CONNECT"},
            "ports":[{"portNumber":47461,"labels":[BRIDGE_PORT_LABEL]}],
            "endpoints":[{"connectionMode":"TunnelRelay","hostId":"owned-host",
                "hostPublicKeys":["owned-pin"],"clientRelayUri":"wss://usw3.rel.tunnels.api.visualstudio.com/owned"}]
        })).unwrap()
    }
    #[test]
    fn native_service_selection_refuses_ordinary_tunnels_ambiguity_and_missing_pins() {
        let selected = select(tunnel()).unwrap();
        assert_eq!(selected.binding.port, 47461);
        assert_eq!(selected.binding.host_id, "owned-host");
        assert!(selected.connect_token.as_str() == "PRIVATE_CONNECT");
        let mut described = tunnel();
        described.ports[0].labels.clear();
        described.ports[0].description = Some(BRIDGE_PORT_LABEL.into());
        assert!(select(described).is_ok());
        let mut ordinary = tunnel();
        ordinary.ports[0].labels.clear();
        assert!(matches!(select(ordinary), Err(Error::Tunnel)));
        let mut ambiguous = tunnel();
        ambiguous.endpoints.push(ambiguous.endpoints[0].clone());
        assert!(matches!(select(ambiguous), Err(Error::Ambiguous)));
        let mut ports = tunnel();
        ports.ports.push(ports.ports[0].clone());
        ports.ports[1].labels.clear();
        assert!(matches!(select(ports), Err(Error::Ambiguous)));
        let mut unpinned = tunnel();
        unpinned.endpoints[0].host_public_keys.clear();
        assert!(matches!(select(unpinned), Err(Error::Tunnel)));
        let mut host_only = tunnel();
        host_only.access_tokens = Some([("host".into(), "PRIVATE_HOST".into())].into());
        assert!(matches!(select(host_only), Err(Error::Denied)));
        for invalid in [
            "file:///owned",
            "https://example.test",
            "owned.usw3.extra",
            ".usw3",
            "owned.",
            "../owned",
            "owned\n",
        ] {
            assert!(locator(invalid).is_err());
        }
    }
}
