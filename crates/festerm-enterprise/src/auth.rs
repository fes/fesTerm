use std::fmt;
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use festerm_secret_store::SecretBytes;
use oauth2::basic::BasicClient;
use oauth2::{
    AuthUrl, ClientId, CsrfToken, PkceCodeChallenge, PkceCodeVerifier, RedirectUrl, Scope, TokenUrl,
};
use serde::Deserialize;
use tokio::sync::Notify;
use url::{form_urlencoded, Url};
use zeroize::{Zeroize, Zeroizing};

use crate::http::{HttpClient, HttpError, HttpErrorKind};

const LOOPBACK_PATH: &str = "/";
const CALLBACK_ACCEPT_POLL: Duration = Duration::from_millis(20);
const CALLBACK_IO_POLL: Duration = Duration::from_millis(20);
const CALLBACK_IO_TIMEOUT_CAP: Duration = Duration::from_millis(200);
const MAX_CALLBACK_REQUEST_BYTES: usize = 8 * 1024;
const MAX_CALLBACK_RESPONSE_BYTES: usize = 1024;
const MAX_TOKEN_RESPONSE_BYTES: usize = 32 * 1024;
const MAX_TOKEN_ERROR_BYTES: usize = 16 * 1024;
pub(crate) const DEV_CENTER_SCOPE: &str = "https://devcenter.azure.com/.default";

#[derive(Clone)]
pub struct OperationControl {
    deadline: Instant,
    state: Arc<OperationState>,
}

struct OperationState {
    cancelled: AtomicBool,
    notify: Notify,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OperationControlError {
    ZeroTimeout,
    DeadlineOverflow,
}

impl fmt::Display for OperationControlError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroTimeout => formatter.write_str("timeout must be greater than zero"),
            Self::DeadlineOverflow => {
                formatter.write_str("timeout is too large to represent safely")
            }
        }
    }
}

impl std::error::Error for OperationControlError {}

impl OperationControl {
    pub fn with_timeout(timeout: Duration) -> Result<Self, OperationControlError> {
        if timeout.is_zero() {
            return Err(OperationControlError::ZeroTimeout);
        }
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or(OperationControlError::DeadlineOverflow)?;
        Ok(Self::with_deadline(deadline))
    }

    #[must_use]
    pub fn with_deadline(deadline: Instant) -> Self {
        Self {
            deadline,
            state: Arc::new(OperationState {
                cancelled: AtomicBool::new(false),
                notify: Notify::new(),
            }),
        }
    }

    #[must_use]
    pub const fn deadline(&self) -> Instant {
        self.deadline
    }

    pub fn cancel(&self) {
        if !self.state.cancelled.swap(true, Ordering::AcqRel) {
            self.state.notify.notify_waiters();
        }
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.state.cancelled.load(Ordering::Acquire)
    }

    pub(crate) async fn cancelled(&self) {
        if self.is_cancelled() {
            return;
        }
        self.state.notify.notified().await;
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EntraTenantId(String);

impl EntraTenantId {
    pub fn parse(value: &str) -> Result<Self, AuthError> {
        parse_guid(value)
            .map(Self)
            .map_err(|_| AuthError::new(AuthErrorKind::InvalidTenantId))
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicClientId(String);

impl PublicClientId {
    pub fn parse(value: &str) -> Result<Self, AuthError> {
        parse_guid(value)
            .map(Self)
            .map_err(|_| AuthError::new(AuthErrorKind::InvalidClientId))
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone)]
struct AccessTokenContext {
    tenant_id: EntraTenantId,
    client_id: PublicClientId,
    scope: &'static str,
}

#[derive(Default)]
struct SensitiveText(String);

impl SensitiveText {
    fn expose(&self) -> &str {
        &self.0
    }

    fn into_inner(mut self) -> String {
        std::mem::take(&mut self.0)
    }
}

impl fmt::Debug for SensitiveText {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SensitiveText(<redacted>)")
    }
}

impl Drop for SensitiveText {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl<'de> Deserialize<'de> for SensitiveText {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        String::deserialize(deserializer).map(Self)
    }
}

pub struct AccessToken {
    bytes: SecretBytes,
    context: AccessTokenContext,
    expires_at: Instant,
}

impl AccessToken {
    fn from_parts(
        secret: String,
        context: AccessTokenContext,
        expires_in_seconds: u64,
    ) -> Result<Self, AuthError> {
        if secret.is_empty() || secret.len() > 16 * 1024 || expires_in_seconds == 0 {
            let mut secret = secret;
            secret.zeroize();
            return Err(AuthError::new(AuthErrorKind::MalformedTokenResponse));
        }
        let Some(expires_at) = Instant::now().checked_add(Duration::from_secs(expires_in_seconds))
        else {
            let mut secret = secret;
            secret.zeroize();
            return Err(AuthError::new(AuthErrorKind::MalformedTokenResponse));
        };
        Ok(Self {
            bytes: SecretBytes::from_secret_string(secret),
            context,
            expires_at,
        })
    }

    #[cfg(test)]
    pub(crate) fn from_test_parts(
        secret: String,
        tenant_id: EntraTenantId,
        client_id: PublicClientId,
        expires_at: Instant,
    ) -> Result<Self, AuthError> {
        if secret.is_empty() || secret.len() > 16 * 1024 {
            let mut secret = secret;
            secret.zeroize();
            return Err(AuthError::new(AuthErrorKind::MalformedTokenResponse));
        }
        Ok(Self {
            bytes: SecretBytes::from_secret_string(secret),
            context: AccessTokenContext {
                tenant_id,
                client_id,
                scope: DEV_CENTER_SCOPE,
            },
            expires_at,
        })
    }

    pub fn with_bearer_str<T>(&self, operation: impl FnOnce(&str) -> T) -> T {
        self.bytes.with_bytes(|bytes| {
            let value =
                std::str::from_utf8(bytes).expect("access tokens originate from UTF-8 JSON");
            operation(value)
        })
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    pub(crate) fn ensure_usable(&self) -> Result<(), AccessTokenStateError> {
        debug_assert!(!self.context.tenant_id.as_str().is_empty());
        debug_assert!(!self.context.client_id.as_str().is_empty());
        if Instant::now() >= self.expires_at {
            return Err(AccessTokenStateError::Expired);
        }
        Ok(())
    }

    pub(crate) const fn scope(&self) -> &'static str {
        self.context.scope
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AccessTokenStateError {
    Expired,
}

pub struct AuthConfiguration {
    tenant_id: EntraTenantId,
    client_id: PublicClientId,
}

impl AuthConfiguration {
    #[must_use]
    pub fn new(tenant_id: EntraTenantId, client_id: PublicClientId) -> Self {
        Self {
            tenant_id,
            client_id,
        }
    }

    fn authorize_url(&self) -> String {
        format!(
            "https://login.microsoftonline.com/{}/oauth2/v2.0/authorize",
            self.tenant_id.as_str()
        )
    }

    fn token_url(&self) -> String {
        format!(
            "https://login.microsoftonline.com/{}/oauth2/v2.0/token",
            self.tenant_id.as_str()
        )
    }

    fn token_context(&self) -> AccessTokenContext {
        AccessTokenContext {
            tenant_id: self.tenant_id.clone(),
            client_id: self.client_id.clone(),
            scope: DEV_CENTER_SCOPE,
        }
    }
}

pub struct AuthorizationSession {
    auth_url: Url,
    token_url: String,
    token_context: AccessTokenContext,
    redirect_uri: String,
    expected_host: String,
    state: CsrfToken,
    pkce_verifier: PkceCodeVerifier,
    listener: TcpListener,
    #[cfg(test)]
    port: u16,
    http_client: HttpClient,
    control: OperationControl,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthErrorKind {
    InvalidTenantId,
    InvalidClientId,
    BrowserLaunchFailed,
    Cancelled,
    TimedOut,
    LoopbackBindFailed,
    CallbackMalformed,
    CallbackRejected,
    AuthorizationDenied,
    PolicyDenied,
    ClaimsChallengeDenied,
    Network,
    RedirectRejected,
    TokenExchangeRejected,
    TokenResponseTooLarge,
    MalformedTokenResponse,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AuthError {
    kind: AuthErrorKind,
}

impl AuthError {
    const fn new(kind: AuthErrorKind) -> Self {
        Self { kind }
    }

    #[must_use]
    pub const fn kind(self) -> AuthErrorKind {
        self.kind
    }
}

impl fmt::Display for AuthError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind {
            AuthErrorKind::InvalidTenantId => {
                formatter.write_str("tenant ID must be a canonical GUID")
            }
            AuthErrorKind::InvalidClientId => {
                formatter.write_str("client ID must be a canonical GUID")
            }
            AuthErrorKind::BrowserLaunchFailed => {
                formatter.write_str("failed to open the system browser")
            }
            AuthErrorKind::Cancelled => formatter.write_str("operation cancelled"),
            AuthErrorKind::TimedOut => formatter.write_str("operation timed out"),
            AuthErrorKind::LoopbackBindFailed => {
                formatter.write_str("failed to bind a loopback callback listener")
            }
            AuthErrorKind::CallbackMalformed => {
                formatter.write_str("loopback callback request was malformed")
            }
            AuthErrorKind::CallbackRejected => {
                formatter.write_str("loopback callback request was rejected")
            }
            AuthErrorKind::AuthorizationDenied => formatter.write_str("sign-in was denied"),
            AuthErrorKind::PolicyDenied => formatter.write_str(
                "sign-in was blocked by tenant policy; broker and device-compliance proof are not implemented",
            ),
            AuthErrorKind::ClaimsChallengeDenied => {
                formatter.write_str("sign-in requires an unsupported claims challenge")
            }
            AuthErrorKind::Network => formatter.write_str("network request failed"),
            AuthErrorKind::RedirectRejected => {
                formatter.write_str("redirected responses are not accepted")
            }
            AuthErrorKind::TokenExchangeRejected => {
                formatter.write_str("the authorization code exchange was rejected")
            }
            AuthErrorKind::TokenResponseTooLarge => {
                formatter.write_str("the token endpoint response exceeded configured bounds")
            }
            AuthErrorKind::MalformedTokenResponse => {
                formatter.write_str("the token endpoint response was malformed")
            }
        }
    }
}

impl std::error::Error for AuthError {}

pub fn begin_authorization(
    configuration: AuthConfiguration,
    control: OperationControl,
) -> Result<AuthorizationSession, AuthError> {
    let authority = ProductionAuthority::from_configuration(&configuration);
    begin_authorization_inner(configuration, control, authority)
}

impl AuthorizationSession {
    #[must_use]
    pub fn authorization_url(&self) -> &Url {
        &self.auth_url
    }

    pub fn open_browser_and_complete(self) -> Result<AccessToken, AuthError> {
        webbrowser::open(self.auth_url.as_str())
            .map_err(|_| AuthError::new(AuthErrorKind::BrowserLaunchFailed))?;
        self.complete()
    }

    pub fn complete(self) -> Result<AccessToken, AuthError> {
        let callback = wait_for_callback(
            &self.listener,
            &self.expected_host,
            self.state.secret(),
            &self.control,
        )?;
        exchange_code(
            &self.http_client,
            &self.control,
            TokenExchangeInput {
                token_url: &self.token_url,
                token_context: &self.token_context,
                redirect_uri: &self.redirect_uri,
                code: &callback.code,
                code_verifier: self.pkce_verifier.secret(),
            },
        )
    }
}

fn begin_authorization_inner(
    configuration: AuthConfiguration,
    control: OperationControl,
    authority: AuthorityEndpoints,
) -> Result<AuthorizationSession, AuthError> {
    if control.is_cancelled() {
        return Err(AuthError::new(AuthErrorKind::Cancelled));
    }
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .map_err(|_| AuthError::new(AuthErrorKind::LoopbackBindFailed))?;
    listener
        .set_nonblocking(true)
        .map_err(|_| AuthError::new(AuthErrorKind::LoopbackBindFailed))?;
    let port = listener
        .local_addr()
        .map_err(|_| AuthError::new(AuthErrorKind::LoopbackBindFailed))?
        .port();
    let redirect_uri = format!("http://localhost:{port}{LOOPBACK_PATH}");
    let expected_host = format!("localhost:{port}");

    let client = BasicClient::new(ClientId::new(configuration.client_id.as_str().to_owned()))
        .set_auth_uri(
            AuthUrl::new(authority.authorize_url.clone())
                .map_err(|_| AuthError::new(AuthErrorKind::Network))?,
        )
        .set_token_uri(
            TokenUrl::new(authority.token_url.clone())
                .map_err(|_| AuthError::new(AuthErrorKind::Network))?,
        )
        .set_redirect_uri(
            RedirectUrl::new(redirect_uri.clone())
                .map_err(|_| AuthError::new(AuthErrorKind::LoopbackBindFailed))?,
        );
    let (pkce_challenge, pkce_verifier) = PkceCodeChallenge::new_random_sha256();
    let (auth_url, state) = client
        .authorize_url(CsrfToken::new_random)
        .add_scope(Scope::new(DEV_CENTER_SCOPE.to_owned()))
        .set_pkce_challenge(pkce_challenge)
        .url();

    let http_client = {
        #[cfg(test)]
        {
            if authority.allow_http() {
                HttpClient::new_for_test()
            } else {
                HttpClient::new()
            }
        }
        #[cfg(not(test))]
        {
            HttpClient::new()
        }
    }
    .map_err(map_http_error)?;

    Ok(AuthorizationSession {
        auth_url,
        token_url: authority.token_url,
        token_context: configuration.token_context(),
        redirect_uri,
        expected_host,
        state,
        pkce_verifier,
        listener,
        #[cfg(test)]
        port,
        http_client,
        control,
    })
}

struct CallbackResult {
    code: SensitiveText,
}

fn wait_for_callback(
    listener: &TcpListener,
    expected_host: &str,
    expected_state: &str,
    control: &OperationControl,
) -> Result<CallbackResult, AuthError> {
    loop {
        if control.is_cancelled() {
            return Err(AuthError::new(AuthErrorKind::Cancelled));
        }
        if Instant::now() >= control.deadline() {
            return Err(AuthError::new(AuthErrorKind::TimedOut));
        }
        match listener.accept() {
            Ok((mut stream, _)) => {
                let result =
                    handle_callback_request(&mut stream, expected_host, expected_state, control);
                let response = match result {
                    Ok(callback) => {
                        write_loopback_response(
                            &mut stream,
                            b"200 OK",
                            b"Authentication request received. Return to the app.\n",
                            control,
                        )?;
                        return Ok(callback);
                    }
                    Err(error) => {
                        let body = match error.kind() {
                            AuthErrorKind::AuthorizationDenied => {
                                b"Sign-in was denied. You can close this window.\n".as_slice()
                            }
                            AuthErrorKind::PolicyDenied | AuthErrorKind::ClaimsChallengeDenied => {
                                b"Sign-in requires unsupported policy or claims handling. You can close this window.\n".as_slice()
                            }
                            _ => b"The callback request was rejected. You can close this window.\n".as_slice(),
                        };
                        let _ =
                            write_loopback_response(&mut stream, b"400 Bad Request", body, control);
                        error
                    }
                };
                let _ = stream.shutdown(Shutdown::Both);
                return Err(response);
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(CALLBACK_ACCEPT_POLL);
            }
            Err(_) => return Err(AuthError::new(AuthErrorKind::CallbackMalformed)),
        }
    }
}

fn handle_callback_request(
    stream: &mut TcpStream,
    expected_host: &str,
    expected_state: &str,
    control: &OperationControl,
) -> Result<CallbackResult, AuthError> {
    let request = read_bounded_http_request(stream, control)?;
    let header_end = request
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| AuthError::new(AuthErrorKind::CallbackMalformed))?;
    let body = &request[header_end + 4..];
    if !body.is_empty() {
        return Err(AuthError::new(AuthErrorKind::CallbackRejected));
    }

    let headers = std::str::from_utf8(&request[..header_end])
        .map_err(|_| AuthError::new(AuthErrorKind::CallbackMalformed))?;
    let mut lines = headers.split("\r\n");
    let request_line = lines
        .next()
        .ok_or_else(|| AuthError::new(AuthErrorKind::CallbackMalformed))?;
    let mut request_parts = request_line.split_whitespace();
    let method = request_parts
        .next()
        .ok_or_else(|| AuthError::new(AuthErrorKind::CallbackMalformed))?;
    let target = request_parts
        .next()
        .ok_or_else(|| AuthError::new(AuthErrorKind::CallbackMalformed))?;
    let version = request_parts
        .next()
        .ok_or_else(|| AuthError::new(AuthErrorKind::CallbackMalformed))?;
    if request_parts.next().is_some()
        || method != "GET"
        || !matches!(version, "HTTP/1.1" | "HTTP/1.0")
    {
        return Err(AuthError::new(AuthErrorKind::CallbackRejected));
    }
    validate_origin_form_target(target)?;

    let mut host = None;
    let mut content_length = None;
    let mut saw_transfer_encoding = false;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            return Err(AuthError::new(AuthErrorKind::CallbackMalformed));
        };
        let trimmed = value.trim();
        if name.eq_ignore_ascii_case("host") {
            if host.replace(trimmed.to_owned()).is_some() {
                return Err(AuthError::new(AuthErrorKind::CallbackRejected));
            }
        } else if name.eq_ignore_ascii_case("content-length") {
            if content_length.replace(trimmed.to_owned()).is_some() {
                return Err(AuthError::new(AuthErrorKind::CallbackRejected));
            }
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            saw_transfer_encoding = true;
        }
    }
    let host = host.ok_or_else(|| AuthError::new(AuthErrorKind::CallbackRejected))?;
    if !host.eq_ignore_ascii_case(expected_host) {
        return Err(AuthError::new(AuthErrorKind::CallbackRejected));
    }
    if saw_transfer_encoding {
        return Err(AuthError::new(AuthErrorKind::CallbackRejected));
    }
    if content_length.is_some_and(|value| value != "0") {
        return Err(AuthError::new(AuthErrorKind::CallbackRejected));
    }

    let parsed = Url::parse(&format!("http://{expected_host}{target}"))
        .map_err(|_| AuthError::new(AuthErrorKind::CallbackMalformed))?;
    if parsed.path() != LOOPBACK_PATH {
        return Err(AuthError::new(AuthErrorKind::CallbackRejected));
    }

    let mut code = None;
    let mut error = None;
    let mut state = None;
    for (name, value) in parsed.query_pairs() {
        match name.as_ref() {
            "code" => {
                if code.replace(SensitiveText(value.into_owned())).is_some() {
                    return Err(AuthError::new(AuthErrorKind::CallbackRejected));
                }
            }
            "error" => {
                if error.replace(value.into_owned()).is_some() {
                    return Err(AuthError::new(AuthErrorKind::CallbackRejected));
                }
            }
            "state" if state.replace(value.into_owned()).is_some() => {
                return Err(AuthError::new(AuthErrorKind::CallbackRejected));
            }
            "state" => {}
            _ => {}
        }
    }

    let state = state.ok_or_else(|| AuthError::new(AuthErrorKind::CallbackRejected))?;
    if state != expected_state {
        return Err(AuthError::new(AuthErrorKind::CallbackRejected));
    }
    if error.is_some() && code.is_some() {
        return Err(AuthError::new(AuthErrorKind::CallbackRejected));
    }
    if let Some(error) = error {
        return Err(map_authorization_error(&error));
    }
    let code = code.ok_or_else(|| AuthError::new(AuthErrorKind::CallbackRejected))?;
    if code.expose().is_empty() || code.expose().len() > 4096 {
        return Err(AuthError::new(AuthErrorKind::CallbackRejected));
    }
    if control.is_cancelled() {
        return Err(AuthError::new(AuthErrorKind::Cancelled));
    }
    if Instant::now() >= control.deadline() {
        return Err(AuthError::new(AuthErrorKind::TimedOut));
    }
    Ok(CallbackResult { code })
}

fn validate_origin_form_target(target: &str) -> Result<(), AuthError> {
    if !target.starts_with('/')
        || target.starts_with("//")
        || target.contains('#')
        || target.contains("://")
    {
        return Err(AuthError::new(AuthErrorKind::CallbackRejected));
    }
    Ok(())
}

fn read_bounded_http_request(
    stream: &mut TcpStream,
    control: &OperationControl,
) -> Result<Zeroizing<Vec<u8>>, AuthError> {
    let mut request = Zeroizing::new(Vec::new());
    let mut buffer = [0_u8; 1024];
    loop {
        if control.is_cancelled() {
            return Err(AuthError::new(AuthErrorKind::Cancelled));
        }
        let timeout = current_io_timeout(control)?;
        stream
            .set_read_timeout(Some(timeout))
            .map_err(|_| AuthError::new(AuthErrorKind::Network))?;
        match stream.read(&mut buffer) {
            Ok(0) => return Err(AuthError::new(AuthErrorKind::CallbackMalformed)),
            Ok(count) => {
                request.extend_from_slice(&buffer[..count]);
                if request.len() > MAX_CALLBACK_REQUEST_BYTES {
                    return Err(AuthError::new(AuthErrorKind::CallbackRejected));
                }
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    return Ok(request);
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                thread::sleep(CALLBACK_IO_POLL);
            }
            Err(_) => return Err(AuthError::new(AuthErrorKind::CallbackMalformed)),
        }
    }
}

fn write_loopback_response(
    stream: &mut TcpStream,
    status: &[u8],
    body: &[u8],
    control: &OperationControl,
) -> Result<(), AuthError> {
    if body.len() > MAX_CALLBACK_RESPONSE_BYTES {
        return Err(AuthError::new(AuthErrorKind::Network));
    }
    let mut response = Vec::with_capacity(status.len() + body.len() + 128);
    response.extend_from_slice(b"HTTP/1.1 ");
    response.extend_from_slice(status);
    response.extend_from_slice(b"\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: ");
    response.extend_from_slice(body.len().to_string().as_bytes());
    response.extend_from_slice(b"\r\nConnection: close\r\n\r\n");
    response.extend_from_slice(body);

    let mut written = 0;
    while written < response.len() {
        if control.is_cancelled() {
            return Err(AuthError::new(AuthErrorKind::Cancelled));
        }
        let timeout = current_io_timeout(control)?;
        stream
            .set_write_timeout(Some(timeout))
            .map_err(|_| AuthError::new(AuthErrorKind::Network))?;
        match stream.write(&response[written..]) {
            Ok(0) => return Err(AuthError::new(AuthErrorKind::Network)),
            Ok(count) => written += count,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                thread::sleep(CALLBACK_IO_POLL);
            }
            Err(_) => return Err(AuthError::new(AuthErrorKind::Network)),
        }
    }
    stream
        .flush()
        .map_err(|_| AuthError::new(AuthErrorKind::Network))
}

fn current_io_timeout(control: &OperationControl) -> Result<Duration, AuthError> {
    if control.is_cancelled() {
        return Err(AuthError::new(AuthErrorKind::Cancelled));
    }
    let remaining = control.deadline().saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(AuthError::new(AuthErrorKind::TimedOut));
    }
    Ok(remaining.min(CALLBACK_IO_TIMEOUT_CAP))
}

struct TokenExchangeInput<'a> {
    token_url: &'a str,
    token_context: &'a AccessTokenContext,
    redirect_uri: &'a str,
    code: &'a SensitiveText,
    code_verifier: &'a str,
}

fn exchange_code(
    client: &HttpClient,
    control: &OperationControl,
    input: TokenExchangeInput<'_>,
) -> Result<AccessToken, AuthError> {
    let body = form_urlencoded::Serializer::new(String::new())
        .append_pair("grant_type", "authorization_code")
        .append_pair("client_id", input.token_context.client_id.as_str())
        .append_pair("redirect_uri", input.redirect_uri)
        .append_pair("code", input.code.expose())
        .append_pair("code_verifier", input.code_verifier)
        .finish();
    let body = Zeroizing::new(body);

    let response = client.post_form(
        input.token_url,
        &body,
        control,
        MAX_TOKEN_RESPONSE_BYTES,
        MAX_TOKEN_ERROR_BYTES,
    );

    let response = match response {
        Ok(value) => value,
        Err(error) if error.kind() == HttpErrorKind::ResponseTooLarge => {
            return Err(AuthError::new(AuthErrorKind::TokenResponseTooLarge));
        }
        Err(error) if error.kind() == HttpErrorKind::Redirected => {
            return Err(AuthError::new(AuthErrorKind::RedirectRejected));
        }
        Err(error) => return Err(map_http_error(error)),
    };

    let status = response.status();
    let bytes = response.into_body();
    if !status.is_success() {
        let parsed = serde_json::from_slice::<TokenErrorResponse>(&bytes);
        return Err(match parsed {
            Ok(error) => map_token_endpoint_error(&error.error, error.claims.is_some()),
            Err(_) => AuthError::new(AuthErrorKind::TokenExchangeRejected),
        });
    }

    let parsed = serde_json::from_slice::<TokenSuccessResponse>(&bytes);
    let mut parsed = parsed.map_err(|_| AuthError::new(AuthErrorKind::MalformedTokenResponse))?;
    if !parsed.token_type.eq_ignore_ascii_case("Bearer") {
        return Err(AuthError::new(AuthErrorKind::MalformedTokenResponse));
    }
    let access_token = std::mem::take(&mut parsed.access_token).into_inner();
    AccessToken::from_parts(access_token, input.token_context.clone(), parsed.expires_in)
}

fn map_authorization_error(error: &str) -> AuthError {
    match error {
        "access_denied" => AuthError::new(AuthErrorKind::AuthorizationDenied),
        "interaction_required" | "temporarily_unavailable" => {
            AuthError::new(AuthErrorKind::PolicyDenied)
        }
        _ => AuthError::new(AuthErrorKind::AuthorizationDenied),
    }
}

fn map_token_endpoint_error(error: &str, has_claims: bool) -> AuthError {
    if has_claims {
        return AuthError::new(AuthErrorKind::ClaimsChallengeDenied);
    }
    match error {
        "interaction_required" | "insufficient_claims" => {
            AuthError::new(AuthErrorKind::PolicyDenied)
        }
        "access_denied" => AuthError::new(AuthErrorKind::AuthorizationDenied),
        _ => AuthError::new(AuthErrorKind::TokenExchangeRejected),
    }
}

fn map_http_error(error: HttpError) -> AuthError {
    match error.kind() {
        HttpErrorKind::Cancelled => AuthError::new(AuthErrorKind::Cancelled),
        HttpErrorKind::TimedOut => AuthError::new(AuthErrorKind::TimedOut),
        HttpErrorKind::Redirected => AuthError::new(AuthErrorKind::RedirectRejected),
        HttpErrorKind::ResponseTooLarge => AuthError::new(AuthErrorKind::TokenResponseTooLarge),
        HttpErrorKind::Network => AuthError::new(AuthErrorKind::Network),
    }
}

fn parse_guid(value: &str) -> Result<String, ()> {
    const GROUPS: [usize; 5] = [8, 4, 4, 4, 12];
    let value = value.trim();
    if value.len() != 36 {
        return Err(());
    }
    let mut offset = 0;
    for (index, group_len) in GROUPS.iter().enumerate() {
        let end = offset + group_len;
        if !value[offset..end]
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(());
        }
        offset = end;
        if index != GROUPS.len() - 1 {
            if value.as_bytes().get(offset) != Some(&b'-') {
                return Err(());
            }
            offset += 1;
        }
    }
    Ok(value.to_ascii_lowercase())
}

#[derive(Deserialize)]
struct TokenSuccessResponse {
    access_token: SensitiveText,
    token_type: String,
    expires_in: u64,
}

impl Drop for TokenSuccessResponse {
    fn drop(&mut self) {
        self.token_type.zeroize();
    }
}

#[derive(Deserialize)]
struct TokenErrorResponse {
    error: String,
    #[serde(default)]
    claims: Option<String>,
}

impl Drop for TokenErrorResponse {
    fn drop(&mut self) {
        self.error.zeroize();
        if let Some(claims) = self.claims.as_mut() {
            claims.zeroize();
        }
    }
}

struct AuthorityEndpoints {
    authorize_url: String,
    token_url: String,
    #[cfg(test)]
    allow_http: bool,
}

struct ProductionAuthority;

impl ProductionAuthority {
    fn from_configuration(configuration: &AuthConfiguration) -> AuthorityEndpoints {
        AuthorityEndpoints {
            authorize_url: configuration.authorize_url(),
            token_url: configuration.token_url(),
            #[cfg(test)]
            allow_http: false,
        }
    }
}

impl AuthorityEndpoints {
    #[cfg(test)]
    fn allow_http(&self) -> bool {
        self.allow_http
    }
}

#[cfg(test)]
impl AuthorityEndpoints {
    fn test(authorize_url: String, token_url: String) -> Self {
        Self {
            authorize_url,
            token_url,
            allow_http: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::net::SocketAddr;
    use std::sync::mpsc;
    use std::thread;

    use static_assertions::assert_not_impl_any;

    assert_not_impl_any!(AccessToken: Clone, std::fmt::Debug, serde::Serialize);
    assert_not_impl_any!(AuthorizationSession: std::fmt::Debug);

    #[derive(Clone)]
    struct TestServer {
        address: SocketAddr,
    }

    impl TestServer {
        fn url(&self, path: &str) -> String {
            format!("http://{}{}", self.address, path)
        }
    }

    fn spawn_server(
        responder: impl Fn(String, HashMap<String, String>, Vec<u8>) -> (u16, &'static str, Vec<u8>)
            + Send
            + Sync
            + 'static,
    ) -> (TestServer, mpsc::Sender<()>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let responder = Arc::new(responder);
        let responder_clone = Arc::clone(&responder);
        let (stop_tx, stop_rx) = mpsc::channel();
        thread::spawn(move || loop {
            if stop_rx.try_recv().is_ok() {
                break;
            }
            match listener.accept() {
                Ok((mut stream, _)) => {
                    stream.set_nonblocking(false).unwrap();
                    stream
                        .set_read_timeout(Some(Duration::from_secs(2)))
                        .unwrap();
                    stream
                        .set_write_timeout(Some(Duration::from_secs(2)))
                        .unwrap();
                    let request = read_fixture_request(&mut stream).unwrap();
                    let (path, headers, body) = parse_fixture_request(&request);
                    let (status, content_type, body_bytes) = responder_clone(path, headers, body);
                    let status_text = match status {
                        200 => "OK",
                        400 => "Bad Request",
                        401 => "Unauthorized",
                        403 => "Forbidden",
                        _ => "Status",
                    };
                    write!(
                        stream,
                        "HTTP/1.1 {status} {status_text}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body_bytes.len()
                    )
                    .unwrap();
                    stream.write_all(&body_bytes).unwrap();
                    stream.flush().unwrap();
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(_) => break,
            }
        });
        (TestServer { address }, stop_tx)
    }

    fn read_fixture_request(stream: &mut TcpStream) -> io::Result<String> {
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 1024];
        let deadline = Instant::now() + Duration::from_millis(750);
        loop {
            let count = match stream.read(&mut buffer) {
                Ok(count) => count,
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) && Instant::now() < deadline =>
                {
                    thread::sleep(Duration::from_millis(10));
                    continue;
                }
                Err(error) => return Err(error),
            };
            if count == 0 {
                break;
            }
            bytes.extend_from_slice(&buffer[..count]);
            if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        Ok(String::from_utf8(bytes).unwrap())
    }

    fn parse_fixture_request(request: &str) -> (String, HashMap<String, String>, Vec<u8>) {
        let mut parts = request.split("\r\n\r\n");
        let headers = parts.next().unwrap();
        let body = parts.next().unwrap_or_default().as_bytes().to_vec();
        let mut lines = headers.split("\r\n");
        let request_line = lines.next().unwrap();
        let path = request_line.split_whitespace().nth(1).unwrap().to_owned();
        let mut parsed_headers = HashMap::new();
        for line in lines {
            if let Some((name, value)) = line.split_once(':') {
                parsed_headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
            }
        }
        (path, parsed_headers, body)
    }

    fn start_authorization(server: &TestServer) -> AuthorizationSession {
        let configuration = AuthConfiguration::new(
            EntraTenantId::parse("11111111-1111-1111-1111-111111111111").unwrap(),
            PublicClientId::parse("22222222-2222-2222-2222-222222222222").unwrap(),
        );
        let control = OperationControl::with_timeout(Duration::from_secs(5)).unwrap();
        begin_authorization_inner(
            configuration,
            control,
            AuthorityEndpoints::test(server.url("/authorize"), server.url("/token")),
        )
        .unwrap()
    }

    fn callback_response(port: u16, request: String) -> String {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        stream.flush().unwrap();
        read_fixture_request(&mut stream).unwrap()
    }

    #[test]
    fn auth_flow_completes_after_loopback_callback_and_cleans_up_listener() {
        let (server, stop) = spawn_server(|path, _headers, body| {
            assert_eq!(path, "/token");
            let form: HashMap<String, String> =
                form_urlencoded::parse(&body).into_owned().collect();
            assert_eq!(
                form.get("grant_type"),
                Some(&"authorization_code".to_owned())
            );
            assert_eq!(form.get("code"), Some(&"authcode".to_owned()));
            (
                200,
                "application/json",
                br#"{"token_type":"Bearer","access_token":"******","expires_in":3600}"#.to_vec(),
            )
        });
        let session = start_authorization(&server);
        let port = session.port;
        let url = session.authorization_url().clone();
        let state = url
            .query_pairs()
            .find(|(name, _)| name == "state")
            .map(|(_, value)| value.into_owned())
            .unwrap();
        let client = thread::spawn(move || {
            callback_response(
                port,
                format!(
                    "GET /?code=authcode&state={state} HTTP/1.1\r\nHost: localhost:{port}\r\nConnection: close\r\n\r\n"
                ),
            )
        });
        let token = session.complete().unwrap();
        assert!(!token.is_empty());
        let response = client.join().unwrap();
        assert!(response.contains("Authentication request received. Return to the app."));
        assert!(!response.contains("Authentication complete"));
        TcpListener::bind(("127.0.0.1", port)).expect("listener must be cleaned up");
        stop.send(()).unwrap();
    }

    #[test]
    fn auth_flow_rejects_absolute_form_targets_and_duplicate_headers() {
        let (server, stop) = spawn_server(|_, _, _| unreachable!());
        let session = start_authorization(&server);
        let port = session.port;
        let response = thread::spawn(move || {
            callback_response(
                port,
                format!(
                    "GET http://example.invalid/?code=x&state=s HTTP/1.1\r\nHost: localhost:{port}\r\nHost: localhost:{port}\r\nConnection: close\r\n\r\n"
                ),
            )
        });
        let error = session.complete().err().expect("session should fail");
        assert_eq!(error.kind(), AuthErrorKind::CallbackRejected);
        assert!(response.join().unwrap().contains("400 Bad Request"));
        stop.send(()).unwrap();
    }

    #[test]
    fn auth_flow_rejects_duplicate_state_and_unexpected_body_framing() {
        let (server, stop) = spawn_server(|_, _, _| unreachable!());
        let session = start_authorization(&server);
        let port = session.port;
        let state = session
            .authorization_url()
            .query_pairs()
            .find(|(name, _)| name == "state")
            .map(|(_, value)| value.into_owned())
            .unwrap();
        let response = thread::spawn(move || {
            callback_response(
                port,
                format!(
                    "GET /?code=one&state={state}&state={state} HTTP/1.1\r\nHost: localhost:{port}\r\nContent-Length: 1\r\nConnection: close\r\n\r\nX"
                ),
            )
        });
        let error = session.complete().err().expect("session should fail");
        assert_eq!(error.kind(), AuthErrorKind::CallbackRejected);
        assert!(response.join().unwrap().contains("400 Bad Request"));
        stop.send(()).unwrap();
    }

    #[test]
    fn auth_flow_can_be_cancelled_after_an_idle_socket_is_accepted() {
        let (server, stop) = spawn_server(|_, _, _| unreachable!());
        let configuration = AuthConfiguration::new(
            EntraTenantId::parse("11111111-1111-1111-1111-111111111111").unwrap(),
            PublicClientId::parse("22222222-2222-2222-2222-222222222222").unwrap(),
        );
        let control = OperationControl::with_timeout(Duration::from_secs(5)).unwrap();
        let cancel = control.clone();
        let session = begin_authorization_inner(
            configuration,
            control,
            AuthorityEndpoints::test(server.url("/authorize"), server.url("/token")),
        )
        .unwrap();
        let port = session.port;
        let stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let idle = thread::spawn(move || {
            let _stream = stream;
            thread::sleep(Duration::from_millis(300));
        });
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(100));
            cancel.cancel();
        });
        let error = session.complete().err().expect("session should fail");
        assert_eq!(error.kind(), AuthErrorKind::Cancelled);
        idle.join().unwrap();
        TcpListener::bind(("127.0.0.1", port)).expect("listener must be cleaned up");
        stop.send(()).unwrap();
    }

    #[test]
    fn auth_flow_times_out_on_slow_partial_headers_and_cleans_up_listener() {
        let (server, stop) = spawn_server(|_, _, _| unreachable!());
        let configuration = AuthConfiguration::new(
            EntraTenantId::parse("11111111-1111-1111-1111-111111111111").unwrap(),
            PublicClientId::parse("22222222-2222-2222-2222-222222222222").unwrap(),
        );
        let control = OperationControl::with_timeout(Duration::from_secs(5)).unwrap();
        let mut session = begin_authorization_inner(
            configuration,
            control,
            AuthorityEndpoints::test(server.url("/authorize"), server.url("/token")),
        )
        .unwrap();
        let port = session.port;
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream.write_all(b"GET /?code=auth").unwrap();
        stream.flush().unwrap();
        // Measure the pending read, not HTTP-client construction or thread scheduling.
        session.control = OperationControl::with_timeout(Duration::from_millis(150)).unwrap();
        let slow = thread::spawn(move || {
            let _stream = stream;
            thread::sleep(Duration::from_secs(1));
        });
        let error = session.complete().err().expect("session should fail");
        assert_eq!(error.kind(), AuthErrorKind::TimedOut);
        slow.join().unwrap();
        TcpListener::bind(("127.0.0.1", port)).expect("listener must be cleaned up");
        stop.send(()).unwrap();
    }

    #[test]
    fn auth_flow_rejects_policy_denial_callback() {
        let (server, stop) = spawn_server(|_, _, _| unreachable!());
        let session = start_authorization(&server);
        let port = session.port;
        let state = session
            .authorization_url()
            .query_pairs()
            .find(|(name, _)| name == "state")
            .map(|(_, value)| value.into_owned())
            .unwrap();
        thread::spawn(move || {
            let _ = callback_response(
                port,
                format!(
                    "GET /?error=interaction_required&state={state} HTTP/1.1\r\nHost: localhost:{port}\r\nConnection: close\r\n\r\n"
                ),
            );
        });
        let error = session.complete().err().expect("session should fail");
        assert_eq!(error.kind(), AuthErrorKind::PolicyDenied);
        stop.send(()).unwrap();
    }

    #[test]
    fn token_exchange_rejects_oversized_error_body() {
        let oversized = vec![b'a'; MAX_TOKEN_ERROR_BYTES + 1];
        let (server, stop) =
            spawn_server(move |_, _, _| (400, "application/json", oversized.clone()));
        let session = start_authorization(&server);
        let port = session.port;
        let state = session
            .authorization_url()
            .query_pairs()
            .find(|(name, _)| name == "state")
            .map(|(_, value)| value.into_owned())
            .unwrap();
        thread::spawn(move || {
            let _ = callback_response(
                port,
                format!(
                    "GET /?code=authcode&state={state} HTTP/1.1\r\nHost: localhost:{port}\r\nConnection: close\r\n\r\n"
                ),
            );
        });
        let error = session.complete().err().expect("session should fail");
        assert_eq!(error.kind(), AuthErrorKind::TokenResponseTooLarge);
        stop.send(()).unwrap();
    }

    #[test]
    fn token_exchange_rejects_claims_challenge_errors() {
        let (server, stop) = spawn_server(|_, _, _| {
            (
                400,
                "application/json",
                br#"{"error":"insufficient_claims","claims":"opaque-claims"}"#.to_vec(),
            )
        });
        let session = start_authorization(&server);
        let port = session.port;
        let state = session
            .authorization_url()
            .query_pairs()
            .find(|(name, _)| name == "state")
            .map(|(_, value)| value.into_owned())
            .unwrap();
        thread::spawn(move || {
            let _ = callback_response(
                port,
                format!(
                    "GET /?code=authcode&state={state} HTTP/1.1\r\nHost: localhost:{port}\r\nConnection: close\r\n\r\n"
                ),
            );
        });
        let error = session.complete().err().expect("session should fail");
        assert_eq!(error.kind(), AuthErrorKind::ClaimsChallengeDenied);
        stop.send(()).unwrap();
    }

    #[test]
    fn token_exchange_rejects_expired_token_responses_locally() {
        let (server, stop) = spawn_server(|_, _, _| {
            (
                200,
                "application/json",
                br#"{"token_type":"Bearer","access_token":"******","expires_in":0}"#.to_vec(),
            )
        });
        let session = start_authorization(&server);
        let port = session.port;
        let state = session
            .authorization_url()
            .query_pairs()
            .find(|(name, _)| name == "state")
            .map(|(_, value)| value.into_owned())
            .unwrap();
        thread::spawn(move || {
            let _ = callback_response(
                port,
                format!(
                    "GET /?code=authcode&state={state} HTTP/1.1\r\nHost: localhost:{port}\r\nConnection: close\r\n\r\n"
                ),
            );
        });
        let error = session.complete().err().expect("session should fail");
        assert_eq!(error.kind(), AuthErrorKind::MalformedTokenResponse);
        stop.send(()).unwrap();
    }

    #[test]
    fn operation_control_rejects_zero_and_overflowing_timeouts() {
        assert_eq!(
            OperationControl::with_timeout(Duration::ZERO)
                .err()
                .expect("zero timeout should fail"),
            OperationControlError::ZeroTimeout
        );
        assert_eq!(
            OperationControl::with_timeout(Duration::MAX)
                .err()
                .expect("overflow timeout should fail"),
            OperationControlError::DeadlineOverflow
        );
    }

    #[test]
    fn tenant_and_client_ids_accept_guid_text_and_canonicalize_case() {
        assert!(EntraTenantId::parse("11111111-1111-1111-1111-111111111111").is_ok());
        assert!(PublicClientId::parse("22222222-2222-2222-2222-222222222222").is_ok());
        assert_eq!(
            EntraTenantId::parse("AAAAAAAA-BBBB-CCCC-DDDD-EEEEEEEEEEEE")
                .unwrap()
                .as_str(),
            "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"
        );
        assert_eq!(
            EntraTenantId::parse("11111111-1111-1111-1111-11111111111Z")
                .unwrap_err()
                .kind(),
            AuthErrorKind::InvalidTenantId
        );
        assert_eq!(
            PublicClientId::parse("22222222-2222-2222-2222-22222222222?")
                .unwrap_err()
                .kind(),
            AuthErrorKind::InvalidClientId
        );
    }
}
