//! Offline fakes for the network seams. Enabled by the `testing` feature so the
//! host-side loopback tests (real daemon, in-memory relay) can reuse them. Never
//! contacts GitHub or the Dev Tunnels service.
use std::{
    io,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Mutex,
    },
    time::{SystemTime, UNIX_EPOCH},
};

use festerm_mobile_profiles::{AccountProvider, ProfileStore};
use festerm_secret_store::SecretBytes;
use festerm_tunnel_bridge::mobile::BoxStream;
use festerm_tunnel_client::{
    identity::{Identity, SERVICE_CLIENT_ID},
    service::{HostBinding, ResolvedTunnel, TunnelSummary},
    Error as ClientError, IdentityRequest, IdentityTransport, ResponseFuture,
};
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

use crate::{BoxFuture, BridgeConnector, TunnelDirectory};

pub const CLIENT_ID: &str = SERVICE_CLIENT_ID;
/// A well-formed Ed25519 host key; the fake relay does not verify it.
pub const HOST_KEY: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl";

/// A different valid key, for host-change tests.
pub const OTHER_HOST_KEY: &str =
    "AAAAC3NzaC1lZDI1NTE5AAAAIFV/zCi2rPZFRDl1HIYPLnfDgwg/Lu4Fg9xeUMPx9Mpe";

const GRANT: &str = r#"{"device_code":"PRIVATE_DEVICE","user_code":"ABCD-1234","verification_uri":"https://github.com/login/device","expires_in":900,"interval":5}"#;
const TOKEN: &str = r#"{"access_token":"PRIVATE_REFRESHED","refresh_token":"PRIVATE_REFRESH2","token_type":"bearer","scope":"","expires_in":3600,"refresh_token_expires_in":7200}"#;

/// A scripted GitHub: `user_id` answers `/user`; the device flow succeeds on the
/// first poll after the protocol's minimum 5 s interval.
pub struct FakeGithub {
    pub user_id: u64,
    pub login: &'static str,
    pub refreshes: AtomicUsize,
}
impl FakeGithub {
    #[must_use]
    pub fn new(user_id: u64, login: &'static str) -> Self {
        Self {
            user_id,
            login,
            refreshes: AtomicUsize::new(0),
        }
    }
}
impl IdentityTransport for FakeGithub {
    fn request<'a>(
        &'a self,
        operation: IdentityRequest<'a>,
        cancel: &'a CancellationToken,
    ) -> ResponseFuture<'a> {
        Box::pin(async move {
            if cancel.is_cancelled() {
                return Err(ClientError::Cancelled);
            }
            let body = match operation {
                IdentityRequest::Device { .. } => GRANT.to_owned(),
                IdentityRequest::Poll { .. } => TOKEN.to_owned(),
                IdentityRequest::Refresh { .. } => {
                    self.refreshes.fetch_add(1, Ordering::Relaxed);
                    TOKEN.to_owned()
                }
                IdentityRequest::Account { .. } => {
                    format!(r#"{{"id":{},"login":"{}"}}"#, self.user_id, self.login)
                }
            };
            Ok(Zeroizing::new(body.into_bytes()))
        })
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |time| time.as_secs())
}

/// Stores a signed-in account directly (no 5 s device flow). `expired` makes
/// its access token already stale so the next use must refresh.
pub fn seed_account(
    store: &Mutex<ProfileStore>,
    user_id: u64,
    login: &str,
    expired: bool,
) -> String {
    let expires_at = if expired { now() - 60 } else { now() + 3600 };
    let mut bytes = b"FESTERM_GITHUB_IDENTITY_V2\0".to_vec();
    bytes.extend(
        format!(
            r#"{{"account":{{"id":{user_id},"login":"{login}"}},"client_id":"{CLIENT_ID}","access_token":"PRIVATE_ACCESS","refresh_token":"PRIVATE_REFRESH","expires_at":{expires_at},"refresh_expires_at":{}}}"#,
            now() + 7200
        )
        .into_bytes(),
    );
    assert!(Identity::decode_private(&bytes, user_id, CLIENT_ID).is_ok());
    let mut store = store.lock().unwrap();
    let secret = store
        .store_secret(&SecretBytes::copy_from_slice(&bytes))
        .unwrap();
    store
        .upsert_account(AccountProvider::Github, user_id, login, secret)
        .unwrap()
}

/// Serves one fixed tunnel whose host key can be swapped to simulate a change.
pub struct FixedDirectory {
    pub host_key: Mutex<String>,
}
impl FixedDirectory {
    #[must_use]
    pub fn new() -> Self {
        Self {
            host_key: Mutex::new(HOST_KEY.into()),
        }
    }
    #[must_use]
    pub fn binding(&self) -> HostBinding {
        HostBinding {
            tunnel_id: "devbox".into(),
            cluster: "euw".into(),
            host_id: "host-1".into(),
            public_key: self.host_key.lock().unwrap().clone(),
            port: 4000,
        }
    }
}
impl FixedDirectory {
    /// The binding in the shape a profile stores as its approved host.
    #[must_use]
    pub fn trusted_host(&self) -> festerm_mobile_profiles::TrustedTunnelHost {
        let b = self.binding();
        festerm_mobile_profiles::TrustedTunnelHost {
            tunnel_id: b.tunnel_id,
            cluster: b.cluster,
            host_id: b.host_id,
            public_key: b.public_key,
            port: b.port,
        }
    }
}
impl Default for FixedDirectory {
    fn default() -> Self {
        Self::new()
    }
}
impl TunnelDirectory for FixedDirectory {
    fn list<'a>(
        &'a self,
        _: &'a Identity,
        _: &'a CancellationToken,
    ) -> BoxFuture<'a, Result<Vec<TunnelSummary>, ClientError>> {
        Box::pin(async move {
            Ok(vec![TunnelSummary {
                locator: "devbox.euw".into(),
                name: Some("devbox".into()),
                description: None,
                active: true,
            }])
        })
    }
    fn resolve<'a>(
        &'a self,
        _: &'a Identity,
        name: &'a str,
        _: &'a CancellationToken,
    ) -> BoxFuture<'a, Result<ResolvedTunnel, ClientError>> {
        Box::pin(async move {
            if name != "devbox.euw" && name != "devbox" {
                return Err(ClientError::TunnelNotFound);
            }
            let endpoint = serde_json::from_str(
                r#"{"connectionMode":"TunnelRelay","hostId":"host-1","hostPublicKeys":[]}"#,
            )
            .map_err(|_| ClientError::Response)?;
            Ok(ResolvedTunnel {
                binding: self.binding(),
                endpoint,
                connect_token: Zeroizing::new("PRIVATE_CONNECT".into()),
            })
        })
    }
}

/// Hands out one in-memory stream per connection from `open`.
pub struct StreamConnector<F: Fn() -> BoxStream + Send + Sync>(pub F);
impl<F: Fn() -> BoxStream + Send + Sync> BridgeConnector for StreamConnector<F> {
    fn connect<'a>(&'a self, _: &'a ResolvedTunnel) -> BoxFuture<'a, io::Result<BoxStream>> {
        Box::pin(async move { Ok((self.0)()) })
    }
}
