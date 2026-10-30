use std::{fmt, time::Duration};

use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use zeroize::{Zeroize, Zeroizing};

use crate::{valid_token, Error, IdentityRequest, IdentityTransport, RESPONSE_LIMIT};

pub const VERIFICATION_URI: &str = "https://github.com/login/device";
const STORE_PREFIX: &[u8] = b"FESTERM_GITHUB_IDENTITY_V1\0";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Account {
    pub id: u64,
    pub login: String,
}
impl Account {
    pub fn validate(&self) -> Result<(), Error> {
        if self.id == 0
            || self.login.is_empty()
            || self.login.len() > 39
            || !self
                .login
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err(Error::Response);
        }
        Ok(())
    }
}

/// Owned OAuth/refresh credentials. Only the native vault may persist this
/// encoding; it is deliberately not interchangeable with a scoped connect token.
pub struct Identity {
    pub account: Account,
    access_token: String,
    refresh_token: Option<String>,
    expires_at: Option<u64>,
    refresh_expires_at: Option<u64>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredIdentity {
    account: Account,
    access_token: String,
    refresh_token: Option<String>,
    expires_at: Option<u64>,
    refresh_expires_at: Option<u64>,
}
impl Drop for StoredIdentity {
    fn drop(&mut self) {
        self.access_token.zeroize();
        if let Some(token) = self.refresh_token.as_mut() {
            token.zeroize();
        }
    }
}
impl Drop for Identity {
    fn drop(&mut self) {
        self.access_token.zeroize();
        if let Some(token) = self.refresh_token.as_mut() {
            token.zeroize();
        }
    }
}
impl fmt::Debug for Identity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("GitHubIdentity(<private credential>)")
    }
}
impl Identity {
    pub fn encode_private(&self) -> Result<Zeroizing<Vec<u8>>, Error> {
        let mut bytes = Zeroizing::new(STORE_PREFIX.to_vec());
        let record = StoredIdentity {
            account: self.account.clone(),
            access_token: self.access_token.clone(),
            refresh_token: self.refresh_token.clone(),
            expires_at: self.expires_at,
            refresh_expires_at: self.refresh_expires_at,
        };
        serde_json::to_writer(&mut *bytes, &record).map_err(|_| Error::Response)?;
        if bytes.len() > RESPONSE_LIMIT {
            return Err(Error::Response);
        }
        Ok(bytes)
    }
    pub fn decode_private(bytes: &[u8], expected_account: u64) -> Result<Self, Error> {
        if bytes.len() > RESPONSE_LIMIT {
            return Err(Error::Response);
        }
        let bytes = bytes.strip_prefix(STORE_PREFIX).ok_or(Error::Response)?;
        let mut record: StoredIdentity = crate::decode(bytes)?;
        let identity = Self {
            account: record.account.clone(),
            access_token: std::mem::take(&mut record.access_token),
            refresh_token: record.refresh_token.take(),
            expires_at: record.expires_at,
            refresh_expires_at: record.refresh_expires_at,
        };
        identity.account.validate()?;
        if identity.account.id != expected_account {
            return Err(Error::AccountChanged);
        }
        if !valid_token(&identity.access_token)
            || identity
                .refresh_token
                .as_deref()
                .is_some_and(|t| !valid_token(t))
        {
            return Err(Error::Response);
        }
        Ok(identity)
    }
    pub(crate) fn access_token(&self) -> &str {
        &self.access_token
    }

    pub fn needs_sign_in(&self) -> bool {
        self.expires_at
            .is_some_and(|expires| now_seconds() >= expires)
    }

    pub fn can_refresh(&self) -> bool {
        self.refresh_token.is_some()
            && self
                .refresh_expires_at
                .is_none_or(|expires| now_seconds() < expires)
    }

    /// The caller serializes refresh for this profile and atomically publishes a
    /// fresh native-store reference after account validation. A successful
    /// service exchange consumes the old pair even if later persistence fails.
    pub async fn refresh(
        &self,
        transport: &impl IdentityTransport,
        client_id: &str,
        cancel: &CancellationToken,
    ) -> Result<Self, Error> {
        if !valid_client_id(client_id) {
            return Err(Error::Registration);
        }
        if !self.can_refresh() {
            return Err(Error::Expired);
        }
        let bytes = tokio::select! {
            _ = cancel.cancelled() => return Err(Error::Cancelled),
            value = transport.request(IdentityRequest::Refresh {
                client_id, refresh_token: self.refresh_token.as_deref().ok_or(Error::Expired)?,
            }, cancel) => value?,
        };
        let mut poll: Poll = crate::decode(&bytes)?;
        if poll.error.is_some()
            || poll.token_type.as_deref() != Some("bearer")
            || !minimal_scope(poll.scope.as_deref())
        {
            return Err(Error::Denied);
        }
        let access = Zeroizing::new(poll.access_token.take().ok_or(Error::Response)?);
        let refresh = Zeroizing::new(poll.refresh_token.take().ok_or(Error::Response)?);
        if !valid_token(&access) || !valid_token(&refresh) {
            return Err(Error::Response);
        }
        let account = account(transport, &access, cancel).await?;
        if account.id != self.account.id {
            return Err(Error::AccountChanged);
        }
        Ok(Self {
            account,
            access_token: access.to_string(),
            refresh_token: Some(refresh.to_string()),
            expires_at: expiry(poll.expires_in)?,
            refresh_expires_at: expiry(poll.refresh_token_expires_in)?,
        })
    }

    pub async fn confirm_account(
        &self,
        transport: &impl IdentityTransport,
        cancel: &CancellationToken,
    ) -> Result<(), Error> {
        if self.needs_sign_in() {
            return Err(Error::Expired);
        }
        if account(transport, &self.access_token, cancel).await?.id != self.account.id {
            return Err(Error::AccountChanged);
        }
        Ok(())
    }
}

pub struct DevicePrompt {
    pub user_code: String,
}
impl Drop for DevicePrompt {
    fn drop(&mut self) {
        self.user_code.zeroize();
    }
}
impl fmt::Debug for DevicePrompt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("GitHubDevicePrompt(<private code>)")
    }
}

#[derive(Deserialize)]
struct Grant {
    device_code: String,
    user_code: String,
    verification_uri: String,
    expires_in: u64,
    #[serde(default = "default_poll_interval")]
    interval: u64,
}
fn default_poll_interval() -> u64 {
    5
}
impl Drop for Grant {
    fn drop(&mut self) {
        self.device_code.zeroize();
        self.user_code.zeroize();
    }
}

#[derive(Deserialize)]
struct Poll {
    access_token: Option<String>,
    refresh_token: Option<String>,
    token_type: Option<String>,
    error: Option<String>,
    expires_in: Option<u64>,
    refresh_token_expires_in: Option<u64>,
    scope: Option<String>,
}
impl Drop for Poll {
    fn drop(&mut self) {
        if let Some(token) = self.access_token.as_mut() {
            token.zeroize();
        }
        if let Some(token) = self.refresh_token.as_mut() {
            token.zeroize();
        }
    }
}

pub fn registered_client_id() -> Option<&'static str> {
    option_env!("FESTERM_GITHUB_CLIENT_ID").filter(|id| valid_client_id(id))
}
fn valid_client_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 128 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.')
}

pub async fn sign_in(
    transport: &impl IdentityTransport,
    client_id: &str,
    expected_account: Option<u64>,
    cancel: &CancellationToken,
    prompt: impl FnOnce(DevicePrompt),
) -> Result<Identity, Error> {
    if !valid_client_id(client_id) {
        return Err(Error::Registration);
    }
    if cancel.is_cancelled() {
        return Err(Error::Cancelled);
    }
    let bytes = transport
        .request(IdentityRequest::Device { client_id }, cancel)
        .await?;
    #[derive(Deserialize)]
    struct Failure {
        error: Option<String>,
    }
    let failure: Failure = crate::decode(&bytes)?;
    if let Some(error) = failure.error.as_deref() {
        return Err(oauth_error(error));
    }
    let grant: Grant = crate::decode(&bytes)?;
    if !valid_token(&grant.device_code)
        || grant.user_code.is_empty()
        || grant.user_code.len() > 32
        || !grant
            .user_code
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'-')
        || grant.verification_uri != VERIFICATION_URI
        || !(1..=900).contains(&grant.expires_in)
        || !(1..=60).contains(&grant.interval)
    {
        return Err(Error::Response);
    }
    drop(bytes);
    prompt(DevicePrompt {
        user_code: grant.user_code.clone(),
    });
    let expires = tokio::time::Instant::now() + Duration::from_secs(grant.expires_in);
    let mut interval = grant.interval.max(5);
    loop {
        tokio::select! {
            _ = cancel.cancelled() => return Err(Error::Cancelled),
            _ = tokio::time::sleep_until(expires) => return Err(Error::Expired),
            _ = tokio::time::sleep(Duration::from_secs(interval)) => {}
        }
        let bytes = tokio::select! {
            _ = cancel.cancelled() => return Err(Error::Cancelled),
            _ = tokio::time::sleep_until(expires) => return Err(Error::Expired),
            bytes = transport.request(IdentityRequest::Poll { client_id, device_code: &grant.device_code }, cancel) => bytes?,
        };
        let mut poll: Poll = crate::decode(&bytes)?;
        match poll.error.as_deref() {
            Some("authorization_pending") => continue,
            Some("slow_down") if interval <= 55 => {
                interval += 5;
                continue;
            }
            Some(error) => return Err(oauth_error(error)),
            None => {}
        }
        if poll.token_type.as_deref() != Some("bearer") || !minimal_scope(poll.scope.as_deref()) {
            return Err(Error::Response);
        }
        let access_token = Zeroizing::new(poll.access_token.take().ok_or(Error::Response)?);
        if !valid_token(&access_token)
            || poll
                .refresh_token
                .as_deref()
                .is_some_and(|t| !valid_token(t))
        {
            return Err(Error::Response);
        }
        let account = account(transport, &access_token, cancel).await?;
        if expected_account.is_some_and(|id| id != account.id) {
            return Err(Error::AccountChanged);
        }
        return Ok(Identity {
            account,
            access_token: access_token.to_string(),
            refresh_token: poll.refresh_token.take(),
            expires_at: expiry(poll.expires_in)?,
            refresh_expires_at: expiry(poll.refresh_token_expires_in)?,
        });
    }
}

fn now_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |time| time.as_secs())
}
fn minimal_scope(scope: Option<&str>) -> bool {
    scope.is_none_or(|scope| {
        scope
            .split([',', ' '])
            .all(|scope| scope.is_empty() || scope == "read:user")
    })
}
fn oauth_error(error: &str) -> Error {
    match error {
        "access_denied" | "incorrect_device_code" => Error::Denied,
        "expired_token" => Error::Expired,
        "device_flow_disabled" | "incorrect_client_credentials" | "unsupported_grant_type" => {
            Error::Registration
        }
        "rate_limit_exceeded" => Error::RateLimited,
        _ => Error::Response,
    }
}
fn expiry(seconds: Option<u64>) -> Result<Option<u64>, Error> {
    seconds
        .map(|seconds| now_seconds().checked_add(seconds).ok_or(Error::Response))
        .transpose()
}

async fn account(
    transport: &impl IdentityTransport,
    token: &str,
    cancel: &CancellationToken,
) -> Result<Account, Error> {
    #[derive(Deserialize)]
    struct User {
        id: u64,
        login: String,
    }
    let bytes = transport
        .request(
            IdentityRequest::Account {
                access_token: token,
            },
            cancel,
        )
        .await?;
    let user: User = crate::decode(&bytes)?;
    let account = Account {
        id: user.id,
        login: user.login,
    };
    account.validate()?;
    Ok(account)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{IdentityRequest, ResponseFuture};
    use std::{collections::VecDeque, sync::Mutex};

    static_assertions::assert_not_impl_any!(Identity: Serialize, Clone);

    struct Fake {
        responses: Mutex<VecDeque<&'static str>>,
        requests: Mutex<Vec<(&'static str, tokio::time::Instant)>>,
    }
    impl Fake {
        fn new(responses: &[&'static str]) -> Self {
            Self {
                responses: Mutex::new(responses.iter().copied().collect()),
                requests: Mutex::new(Vec::new()),
            }
        }
    }
    impl IdentityTransport for Fake {
        fn request<'a>(
            &'a self,
            request: IdentityRequest<'a>,
            cancel: &'a CancellationToken,
        ) -> ResponseFuture<'a> {
            Box::pin(async move {
                if cancel.is_cancelled() {
                    return Err(Error::Cancelled);
                }
                let kind = match request {
                    IdentityRequest::Device { client_id } => {
                        assert_eq!(client_id, "ownedApp");
                        "device"
                    }
                    IdentityRequest::Poll {
                        client_id,
                        device_code,
                    } => {
                        assert_eq!(client_id, "ownedApp");
                        assert!(device_code == "PRIVATE_DEVICE");
                        "poll"
                    }
                    IdentityRequest::Account { access_token } => {
                        assert!(access_token.starts_with("PRIVATE_"));
                        "account"
                    }
                    IdentityRequest::Refresh {
                        client_id,
                        refresh_token,
                    } => {
                        assert_eq!(client_id, "ownedApp");
                        assert!(refresh_token == "PRIVATE_REFRESH");
                        "refresh"
                    }
                };
                self.requests
                    .lock()
                    .unwrap()
                    .push((kind, tokio::time::Instant::now()));
                let body = self
                    .responses
                    .lock()
                    .unwrap()
                    .pop_front()
                    .ok_or(Error::Response)?;
                Ok(Zeroizing::new(body.as_bytes().to_vec()))
            })
        }
    }
    const GRANT: &str = r#"{"device_code":"PRIVATE_DEVICE","user_code":"ABCD-1234","verification_uri":"https://github.com/login/device","expires_in":900,"interval":5}"#;
    const TOKEN: &str = r#"{"access_token":"PRIVATE_ACCESS","refresh_token":"PRIVATE_REFRESH","token_type":"bearer","scope":"read:user","expires_in":3600,"refresh_token_expires_in":7200}"#;
    const USER: &str = r#"{"id":42,"login":"owned-account","name":"ignored"}"#;

    #[tokio::test(start_paused = true)]
    async fn native_device_flow_respects_interval_slowdown_and_private_identity_binding() {
        let http = Fake::new(&[
            GRANT,
            r#"{"error":"authorization_pending"}"#,
            r#"{"error":"slow_down"}"#,
            TOKEN,
            USER,
        ]);
        let identity = sign_in(
            &http,
            "ownedApp",
            Some(42),
            &CancellationToken::new(),
            |prompt| {
                assert!(prompt.user_code == "ABCD-1234");
                assert!(!format!("{prompt:?}").contains("ABCD"));
            },
        )
        .await
        .unwrap();
        let requests = http.requests.lock().unwrap();
        assert_eq!(
            requests.iter().map(|request| request.0).collect::<Vec<_>>(),
            ["device", "poll", "poll", "poll", "account"]
        );
        assert_eq!(
            requests[1].1.duration_since(requests[0].1),
            Duration::from_secs(5)
        );
        assert_eq!(
            requests[2].1.duration_since(requests[1].1),
            Duration::from_secs(5)
        );
        assert_eq!(
            requests[3].1.duration_since(requests[2].1),
            Duration::from_secs(10)
        );
        assert!(!format!("{identity:?}").contains("PRIVATE"));
        let encoded = identity.encode_private().unwrap();
        assert!(Identity::decode_private(&encoded, 42).is_ok());
        assert!(matches!(
            Identity::decode_private(&encoded, 99),
            Err(Error::AccountChanged)
        ));
        assert!(Identity::decode_private(b"ordinary-connect-token", 42).is_err());
        assert!(!identity.needs_sign_in());
        assert!(identity.can_refresh());
    }

    #[tokio::test(start_paused = true)]
    async fn native_device_flow_denial_expiry_cancel_and_changed_account_refuse() {
        let short = Fake::new(&[
            r#"{"device_code":"PRIVATE_DEVICE","user_code":"ABCD-1234","verification_uri":"https://github.com/login/device","expires_in":1}"#,
        ]);
        assert_eq!(
            sign_in(&short, "ownedApp", None, &CancellationToken::new(), |_| {})
                .await
                .unwrap_err(),
            Error::Expired
        );
        assert_eq!(short.requests.lock().unwrap().len(), 1);
        for (error, expected) in [
            (r#"{"error":"access_denied"}"#, Error::Denied),
            (r#"{"error":"expired_token"}"#, Error::Expired),
            (r#"{"error":"device_flow_disabled"}"#, Error::Registration),
            (r#"{"error":"PRIVATE_RAW_ERROR"}"#, Error::Response),
        ] {
            let http = Fake::new(&[GRANT, error]);
            let result = sign_in(&http, "ownedApp", None, &CancellationToken::new(), |_| {}).await;
            assert_eq!(result.unwrap_err(), expected);
            assert!(!expected.to_string().contains("PRIVATE"));
        }
        let http = Fake::new(&[GRANT, TOKEN, USER]);
        assert_eq!(
            sign_in(
                &http,
                "ownedApp",
                Some(99),
                &CancellationToken::new(),
                |_| {}
            )
            .await
            .unwrap_err(),
            Error::AccountChanged
        );
        let cancel = CancellationToken::new();
        let http = Fake::new(&[GRANT]);
        assert_eq!(
            sign_in(&http, "ownedApp", None, &cancel, |_| cancel.cancel())
                .await
                .unwrap_err(),
            Error::Cancelled
        );
        assert_eq!(http.requests.lock().unwrap().len(), 1);
        let http = Fake::new(&[]);
        assert_eq!(
            sign_in(&http, "", None, &CancellationToken::new(), |_| {})
                .await
                .unwrap_err(),
            Error::Registration
        );
        assert!(http.requests.lock().unwrap().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn native_refresh_returns_a_new_pair_only_after_account_revalidation() {
        let http = Fake::new(&[GRANT, TOKEN, USER]);
        let original = sign_in(&http, "ownedApp", None, &CancellationToken::new(), |_| {})
            .await
            .unwrap();
        let refresh = Fake::new(&[
            r#"{"access_token":"PRIVATE_NEW_ACCESS","refresh_token":"PRIVATE_NEW_REFRESH","token_type":"bearer","scope":"read:user","expires_in":7200}"#,
            USER,
        ]);
        let renewed = original
            .refresh(&refresh, "ownedApp", &CancellationToken::new())
            .await
            .unwrap();
        assert!(renewed.access_token == "PRIVATE_NEW_ACCESS");
        assert!(original.access_token == "PRIVATE_ACCESS");
        assert_eq!(
            refresh
                .requests
                .lock()
                .unwrap()
                .iter()
                .map(|request| request.0)
                .collect::<Vec<_>>(),
            ["refresh", "account"]
        );
        let wrong = Fake::new(&[TOKEN, r#"{"id":99,"login":"other-account"}"#]);
        assert_eq!(
            original
                .refresh(&wrong, "ownedApp", &CancellationToken::new())
                .await
                .unwrap_err(),
            Error::AccountChanged
        );
    }

    #[tokio::test(start_paused = true)]
    async fn native_device_flow_refuses_foreign_browser_uris_and_broad_scopes() {
        let foreign = Fake::new(&[
            r#"{"device_code":"PRIVATE_DEVICE","user_code":"ABCD-1234","verification_uri":"https://evil.invalid","expires_in":900,"interval":5}"#,
        ]);
        assert_eq!(
            sign_in(
                &foreign,
                "ownedApp",
                None,
                &CancellationToken::new(),
                |_| panic!("no browser action")
            )
            .await
            .unwrap_err(),
            Error::Response
        );
        let broad = Fake::new(&[
            GRANT,
            r#"{"access_token":"PRIVATE_ACCESS","token_type":"bearer","scope":"repo"}"#,
        ]);
        assert_eq!(
            sign_in(&broad, "ownedApp", None, &CancellationToken::new(), |_| {})
                .await
                .unwrap_err(),
            Error::Response
        );
        assert_eq!(broad.requests.lock().unwrap().len(), 2);
    }
}
