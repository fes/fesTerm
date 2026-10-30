#![forbid(unsafe_code)]
//! Native, host-independent identity and service discovery. No subprocesses,
//! browser implementation, native FFI, daemon IPC, or global account slot.

pub mod identity;
pub mod service;
#[cfg(test)]
mod tests;

use std::{fmt, future::Future, pin::Pin};
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

pub const RESPONSE_LIMIT: usize = 64 * 1024;
pub const TOKEN_LIMIT: usize = 16 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    Registration,
    Cancelled,
    Expired,
    Denied,
    Network,
    Response,
    AccountChanged,
    Tunnel,
    Ambiguous,
    Store,
    Busy,
    RateLimited,
}

impl fmt::Display for Error {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.write_str(match self {
            Self::Registration => "GitHub sign-in is not configured in this build.",
            Self::Cancelled => "Cancelled.",
            Self::Expired => "Sign-in expired. Try again.",
            Self::Denied => "Access denied. Check the account and tunnel.",
            Self::Network => "Connection failed. Try again.",
            Self::Response => "Unexpected service response.",
            Self::AccountChanged => "Account changed. Sign in again.",
            Self::Tunnel => "No fesTerm bridge is available for this tunnel.",
            Self::Ambiguous => "This tunnel has multiple hosts or ports.",
            Self::Store => "Secure storage unavailable. Sign in again.",
            Self::Busy => "Another sign-in is finishing. Try again.",
            Self::RateLimited => "GitHub limit reached. Try later.",
        })
    }
}
impl std::error::Error for Error {}

pub(crate) fn valid_token(token: &str) -> bool {
    !token.is_empty()
        && token.len() <= TOKEN_LIMIT
        && token.bytes().all(|byte| byte.is_ascii_graphic())
}
pub(crate) fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, Error> {
    if bytes.len() > RESPONSE_LIMIT {
        return Err(Error::Response);
    }
    serde_json::from_slice(bytes).map_err(|_| Error::Response)
}

pub(crate) fn http_client() -> Result<reqwest::Client, Error> {
    reqwest::Client::builder()
        .https_only(true)
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(20))
        .user_agent("fesTerm-native-tunnel/1")
        .build()
        .map_err(|_| Error::Network)
}

pub(crate) async fn bounded_response(
    mut response: reqwest::Response,
    cancel: &CancellationToken,
) -> Result<Zeroizing<Vec<u8>>, Error> {
    if cancel.is_cancelled() {
        return Err(Error::Cancelled);
    }
    if response.status().as_u16() == 429 {
        return Err(Error::RateLimited);
    }
    if !response.status().is_success() && response.status().as_u16() != 400 {
        return Err(if matches!(response.status().as_u16(), 401 | 403) {
            Error::Denied
        } else {
            Error::Network
        });
    }
    if response
        .content_length()
        .is_some_and(|len| len > RESPONSE_LIMIT as u64)
    {
        return Err(Error::Response);
    }
    let mut bytes = Zeroizing::new(Vec::new());
    loop {
        let chunk = tokio::select! {
            _ = cancel.cancelled() => return Err(Error::Cancelled),
            value = response.chunk() => value.map_err(|_| Error::Network)?,
        };
        let Some(chunk) = chunk else { return Ok(bytes) };
        if chunk.len() > RESPONSE_LIMIT - bytes.len() {
            return Err(Error::Response);
        }
        bytes.extend_from_slice(&chunk);
    }
}

pub type ResponseFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Zeroizing<Vec<u8>>, Error>> + Send + 'a>>;

/// A test/host seam, not a configurable network authority. Production requests
/// always use the fixed GitHub HTTPS endpoints in `Https`.
pub trait IdentityTransport: Send + Sync {
    fn request<'a>(
        &'a self,
        operation: IdentityRequest<'a>,
        cancel: &'a CancellationToken,
    ) -> ResponseFuture<'a>;
}

pub enum IdentityRequest<'a> {
    Device {
        client_id: &'a str,
    },
    Poll {
        client_id: &'a str,
        device_code: &'a str,
    },
    Account {
        access_token: &'a str,
    },
    Refresh {
        client_id: &'a str,
        refresh_token: &'a str,
    },
}

pub struct Https(reqwest::Client);
impl Https {
    pub fn new() -> Result<Self, Error> {
        http_client().map(Self)
    }
}
impl Https {
    fn build_request(
        &self,
        operation: IdentityRequest<'_>,
    ) -> Result<(bool, reqwest::RequestBuilder), Error> {
        let oauth = !matches!(&operation, IdentityRequest::Account { .. });
        let request = match operation {
            IdentityRequest::Account { access_token } => {
                if !valid_token(access_token) {
                    return Err(Error::Response);
                }
                let value = Zeroizing::new(format!("Bearer {access_token}"));
                let mut authorization =
                    reqwest::header::HeaderValue::from_str(&value).map_err(|_| Error::Response)?;
                authorization.set_sensitive(true);
                self.0
                    .get("https://api.github.com/user")
                    .header(reqwest::header::AUTHORIZATION, authorization)
                    .header("Accept", "application/vnd.github+json")
                    .header("X-GitHub-Api-Version", "2022-11-28")
            }

            operation => {
                let mut form = url::form_urlencoded::Serializer::new(String::new());
                let endpoint = match operation {
                    IdentityRequest::Device { client_id } => {
                        form.append_pair("client_id", client_id);
                        form.append_pair("scope", "read:user");
                        "https://github.com/login/device/code"
                    }
                    IdentityRequest::Poll {
                        client_id,
                        device_code,
                    } => {
                        form.append_pair("client_id", client_id);
                        form.append_pair("device_code", device_code);
                        form.append_pair(
                            "grant_type",
                            "urn:ietf:params:oauth:grant-type:device_code",
                        );
                        "https://github.com/login/oauth/access_token"
                    }
                    IdentityRequest::Refresh {
                        client_id,
                        refresh_token,
                    } => {
                        form.append_pair("client_id", client_id);
                        form.append_pair("grant_type", "refresh_token");
                        form.append_pair("refresh_token", refresh_token);
                        "https://github.com/login/oauth/access_token"
                    }
                    IdentityRequest::Account { .. } => unreachable!(),
                };
                self.0
                    .post(endpoint)
                    .header("Accept", "application/json")
                    .header("Content-Type", "application/x-www-form-urlencoded")
                    .body(form.finish())
            }
        };
        Ok((oauth, request))
    }
}
impl IdentityTransport for Https {
    fn request<'a>(
        &'a self,
        operation: IdentityRequest<'a>,
        cancel: &'a CancellationToken,
    ) -> ResponseFuture<'a> {
        Box::pin(async move {
            let stage = match &operation {
                IdentityRequest::Device { .. } => "device-grant",
                IdentityRequest::Poll { .. } => "device-token",
                IdentityRequest::Account { .. } => "github-account",
                IdentityRequest::Refresh { .. } => "github-refresh",
            };
            let mut http_status = None;
            let result = async {
                let (oauth, request) = self.build_request(operation)?;
                let response = tokio::select! {
                    _ = cancel.cancelled() => return Err(Error::Cancelled),
                    result = request.send() => result.map_err(|_| Error::Network)?,
                };
                http_status = Some(response.status().as_u16());
                if !oauth && !response.status().is_success() {
                    return Err(Error::Denied);
                }
                bounded_response(response, cancel).await
            }
            .await;
            result.inspect_err(|error| {
                if *error != Error::Cancelled {
                    tracing::warn!(stage, http_status = ?http_status, error = ?error, "Native GitHub request failed");
                }
            })
        })
    }
}
