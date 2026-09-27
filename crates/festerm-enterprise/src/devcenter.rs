use std::collections::HashSet;
use std::fmt;

use reqwest::StatusCode;
use serde::Deserialize;
use url::Url;
use zeroize::Zeroize;

use crate::auth::{AccessToken, AccessTokenStateError, OperationControl, DEV_CENTER_SCOPE};
use crate::http::{
    HttpClient, HttpError, HttpErrorKind, HttpResponse, DEFAULT_MAX_ERROR_BYTES,
    DEFAULT_MAX_RESPONSE_BYTES,
};

pub const API_VERSION: &str = "2025-02-01";
const MAX_PAGE_COUNT: usize = 32;
const MAX_ITEMS_PER_PAGE: usize = 256;
const MAX_TOTAL_ITEMS: usize = 2048;
const MAX_NAME_LEN: usize = 63;
const MAX_URI_LEN: usize = 4096;
const MAX_DESCRIPTION_LEN: usize = 4096;
const MAX_DISPLAY_LEN: usize = 512;
const MAX_LOCATION_LEN: usize = 128;
const MAX_POOL_LEN: usize = 128;
const MAX_STATE_LEN: usize = 128;
const MAX_USER_LEN: usize = 128;
const MAX_REMOTE_URI_LEN: usize = 4096;

pub struct DevCenterClient {
    endpoint: DevCenterUri,
    token: AccessToken,
    http_client: HttpClient,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DevCenterUri(Url);

impl DevCenterUri {
    pub fn parse(value: &str) -> Result<Self, DevCenterError> {
        if value.len() > MAX_URI_LEN {
            return Err(DevCenterError::new(DevCenterErrorKind::InvalidDevCenterUri));
        }
        let url = Url::parse(value)
            .map_err(|_| DevCenterError::new(DevCenterErrorKind::InvalidDevCenterUri))?;
        validate_dev_center_url(&url)?;
        Ok(Self(url))
    }

    fn as_url(&self) -> &Url {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectName(String);

impl ProjectName {
    pub fn parse(value: &str) -> Result<Self, DevCenterError> {
        validate_resource_name(value, DevCenterErrorKind::InvalidProjectName)?;
        Ok(Self(value.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DevBoxName(String);

impl DevBoxName {
    pub fn parse(value: &str) -> Result<Self, DevCenterError> {
        validate_resource_name(value, DevCenterErrorKind::InvalidDevBoxName)?;
        Ok(Self(value.to_owned()))
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Project {
    pub name: String,
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub uri: String,
    pub max_dev_boxes_per_user: Option<u32>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AbilitySet {
    pub admin: Vec<String>,
    pub developer: Vec<String>,
}

impl AbilitySet {
    #[must_use]
    pub fn contains_developer(&self, required: &str) -> bool {
        self.developer.iter().any(|ability| ability == required)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectAbilities {
    pub project_name: String,
    pub abilities: AbilitySet,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DevBox {
    pub name: String,
    pub project_name: String,
    pub provisioning_state: Option<String>,
    pub pool_name: Option<String>,
    pub location: Option<String>,
    pub os_type: Option<String>,
    pub user: Option<String>,
    pub uri: String,
}

pub struct SensitiveUri(Box<str>);

impl SensitiveUri {
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SensitiveUri {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SensitiveUri(<redacted>)")
    }
}

impl Drop for SensitiveUri {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

pub struct RemoteConnection {
    pub cloud_pc_connection_url: Option<SensitiveUri>,
    pub rdp_connection_url: Option<SensitiveUri>,
    pub web_url: Option<SensitiveUri>,
}

impl fmt::Debug for RemoteConnection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RemoteConnection(<redacted>)")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DevCenterErrorKind {
    InvalidDevCenterUri,
    InvalidProjectName,
    InvalidDevBoxName,
    Cancelled,
    TimedOut,
    Network,
    UnexpectedStatus(u16),
    RedirectRejected,
    Unauthorized,
    TokenExpired,
    PolicyDenied,
    Forbidden,
    PaginationOriginMismatch,
    PaginationCycle,
    ResponseTooLarge,
    MalformedResponse,
    ResponseBoundsExceeded,
    AbilityDenied,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DevCenterError {
    kind: DevCenterErrorKind,
}

impl DevCenterError {
    const fn new(kind: DevCenterErrorKind) -> Self {
        Self { kind }
    }

    #[must_use]
    pub const fn kind(self) -> DevCenterErrorKind {
        self.kind
    }
}

impl fmt::Display for DevCenterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind {
            DevCenterErrorKind::InvalidDevCenterUri => formatter.write_str("dev center URI must be an HTTPS https://*.devcenter.azure.com endpoint with no credentials, port, query, fragment, or unexpected base path"),
            DevCenterErrorKind::InvalidProjectName => formatter.write_str("project names must match ^[a-zA-Z0-9][a-zA-Z0-9-_.]{2,62}$"),
            DevCenterErrorKind::InvalidDevBoxName => formatter.write_str("dev box names must match ^[a-zA-Z0-9][a-zA-Z0-9-_.]{2,62}$"),
            DevCenterErrorKind::Cancelled => formatter.write_str("operation cancelled"),
            DevCenterErrorKind::TimedOut => formatter.write_str("operation timed out"),
            DevCenterErrorKind::Network => formatter.write_str("network request failed"),
            DevCenterErrorKind::UnexpectedStatus(status) => write!(formatter, "Dev Center request returned unexpected HTTP status {status}"),
            DevCenterErrorKind::RedirectRejected => formatter.write_str("redirected responses are not accepted"),
            DevCenterErrorKind::Unauthorized => formatter.write_str("request rejected because the access token is invalid"),
            DevCenterErrorKind::TokenExpired => formatter.write_str("request was rejected locally because the transient access token has expired"),
            DevCenterErrorKind::PolicyDenied => formatter.write_str("request requires unsupported tenant policy or claims handling"),
            DevCenterErrorKind::Forbidden => formatter.write_str("request rejected by the Dev Center service"),
            DevCenterErrorKind::PaginationOriginMismatch => formatter.write_str("pagination nextLink crossed the authenticated origin or used an invalid URI"),
            DevCenterErrorKind::PaginationCycle => formatter.write_str("pagination nextLink cycle detected"),
            DevCenterErrorKind::ResponseTooLarge => formatter.write_str("response exceeded configured byte bounds"),
            DevCenterErrorKind::MalformedResponse => formatter.write_str("response body was malformed"),
            DevCenterErrorKind::ResponseBoundsExceeded => formatter.write_str("response exceeded configured page or item bounds"),
            DevCenterErrorKind::AbilityDenied => formatter.write_str("project abilities do not allow the requested operation"),
        }
    }
}

impl std::error::Error for DevCenterError {}

impl DevCenterClient {
    pub fn new(endpoint: DevCenterUri, token: AccessToken) -> Result<Self, DevCenterError> {
        let http_client = HttpClient::new().map_err(map_http_error)?;
        Ok(Self {
            endpoint,
            token,
            http_client,
        })
    }

    #[cfg(test)]
    fn new_for_test(endpoint: DevCenterUri, token: AccessToken) -> Result<Self, DevCenterError> {
        let http_client = HttpClient::new_for_test().map_err(map_http_error)?;
        Ok(Self {
            endpoint,
            token,
            http_client,
        })
    }

    pub fn list_projects(
        &self,
        control: &OperationControl,
    ) -> Result<Vec<Project>, DevCenterError> {
        let first = self
            .endpoint
            .join_path("projects", &[("api-version", API_VERSION)]);
        self.fetch_paged::<WireProjectPage, WireProject, Project>(&first, control, |wire| {
            wire.try_into_project()
        })
    }

    pub fn get_project_abilities(
        &self,
        project_name: &ProjectName,
        control: &OperationControl,
    ) -> Result<ProjectAbilities, DevCenterError> {
        let path = format!("projects/{}/users/me/abilities", project_name.as_str());
        let url = self
            .endpoint
            .join_path(&path, &[("api-version", API_VERSION)]);
        let bytes = self.get_response_bytes(&url, control)?.into_bytes();
        let wire: WireProjectAbilities = serde_json::from_slice(&bytes)
            .map_err(|_| DevCenterError::new(DevCenterErrorKind::MalformedResponse))?;
        Ok(ProjectAbilities {
            project_name: project_name.as_str().to_owned(),
            abilities: wire.try_into_abilities()?,
        })
    }

    pub fn list_owned_dev_boxes(
        &self,
        project_name: &ProjectName,
        control: &OperationControl,
    ) -> Result<Vec<DevBox>, DevCenterError> {
        let abilities = self.get_project_abilities(project_name, control)?;
        if !abilities.abilities.contains_developer("ReadDevBoxes") {
            return Err(DevCenterError::new(DevCenterErrorKind::AbilityDenied));
        }
        let path = format!("projects/{}/users/me/devboxes", project_name.as_str());
        let url = self
            .endpoint
            .join_path(&path, &[("api-version", API_VERSION)]);
        self.fetch_paged::<WireDevBoxPage, WireDevBox, DevBox>(&url, control, |wire| {
            wire.try_into_dev_box()
        })
    }

    pub fn get_remote_connection(
        &self,
        project_name: &ProjectName,
        dev_box_name: &DevBoxName,
        control: &OperationControl,
    ) -> Result<RemoteConnection, DevCenterError> {
        let abilities = self.get_project_abilities(project_name, control)?;
        if !abilities
            .abilities
            .contains_developer("ReadRemoteConnections")
        {
            return Err(DevCenterError::new(DevCenterErrorKind::AbilityDenied));
        }
        let path = format!(
            "projects/{}/users/me/devboxes/{}/remoteConnection",
            project_name.as_str(),
            dev_box_name.as_str()
        );
        let url = self
            .endpoint
            .join_path(&path, &[("api-version", API_VERSION)]);
        let bytes = self.get_response_bytes(&url, control)?.into_bytes();
        let wire: WireRemoteConnection = serde_json::from_slice(&bytes)
            .map_err(|_| DevCenterError::new(DevCenterErrorKind::MalformedResponse))?;
        wire.try_into_remote_connection()
    }

    fn fetch_paged<P, W, T>(
        &self,
        first_url: &str,
        control: &OperationControl,
        mut convert: impl FnMut(W) -> Result<T, DevCenterError>,
    ) -> Result<Vec<T>, DevCenterError>
    where
        P: PagedResponse<Item = W> + for<'de> Deserialize<'de>,
    {
        let mut next_url = Some(first_url.to_owned());
        let mut seen = HashSet::new();
        let mut page_count = 0_usize;
        let mut items = Vec::new();
        while let Some(url) = next_url {
            if !seen.insert(url.clone()) {
                return Err(DevCenterError::new(DevCenterErrorKind::PaginationCycle));
            }
            page_count += 1;
            if page_count > MAX_PAGE_COUNT {
                return Err(DevCenterError::new(
                    DevCenterErrorKind::ResponseBoundsExceeded,
                ));
            }
            let bytes = self.get_response_bytes(&url, control)?.into_bytes();
            let page: P = serde_json::from_slice(&bytes)
                .map_err(|_| DevCenterError::new(DevCenterErrorKind::MalformedResponse))?;
            if page.items().len() > MAX_ITEMS_PER_PAGE {
                return Err(DevCenterError::new(
                    DevCenterErrorKind::ResponseBoundsExceeded,
                ));
            }
            let next_link = page.next_link().map(str::to_owned);
            for wire in page.into_items() {
                items.push(convert(wire)?);
                if items.len() > MAX_TOTAL_ITEMS {
                    return Err(DevCenterError::new(
                        DevCenterErrorKind::ResponseBoundsExceeded,
                    ));
                }
            }
            next_url = match next_link {
                Some(next) => Some(self.validate_next_link(&next)?),
                None => None,
            };
        }
        Ok(items)
    }

    fn get_response_bytes(
        &self,
        url: &str,
        control: &OperationControl,
    ) -> Result<HttpResponse, DevCenterError> {
        if self.token.scope() != DEV_CENTER_SCOPE {
            return Err(DevCenterError::new(DevCenterErrorKind::AbilityDenied));
        }
        self.token.ensure_usable().map_err(map_token_state_error)?;
        self.token.with_bearer_str(|token| {
            let response = self.http_client.get(
                url,
                token,
                control,
                DEFAULT_MAX_RESPONSE_BYTES,
                DEFAULT_MAX_ERROR_BYTES,
            );
            let response = match response {
                Ok(response) => response,
                Err(error) => return Err(map_http_error(error)),
            };
            if response.status() == StatusCode::OK {
                return Ok(response);
            }
            Err(map_status_error(
                response.status(),
                response.claims_challenge(),
            ))
        })
    }

    fn validate_next_link(&self, value: &str) -> Result<String, DevCenterError> {
        if value.len() > MAX_URI_LEN {
            return Err(DevCenterError::new(
                DevCenterErrorKind::PaginationOriginMismatch,
            ));
        }
        let next = Url::parse(value)
            .map_err(|_| DevCenterError::new(DevCenterErrorKind::PaginationOriginMismatch))?;
        if next.scheme() != "https" && next.scheme() != "http" {
            return Err(DevCenterError::new(
                DevCenterErrorKind::PaginationOriginMismatch,
            ));
        }
        if !next.username().is_empty() || next.password().is_some() || next.fragment().is_some() {
            return Err(DevCenterError::new(
                DevCenterErrorKind::PaginationOriginMismatch,
            ));
        }
        let current = self.endpoint.as_url();
        if next.scheme() != current.scheme()
            || next.host_str() != current.host_str()
            || next.port_or_known_default() != current.port_or_known_default()
        {
            return Err(DevCenterError::new(
                DevCenterErrorKind::PaginationOriginMismatch,
            ));
        }
        Ok(next.to_string())
    }
}

impl DevCenterUri {
    fn join_path(&self, path: &str, query: &[(&str, &str)]) -> String {
        let mut url = self.0.clone();
        url.set_path(path);
        let mut pairs = url.query_pairs_mut();
        pairs.clear();
        for (key, value) in query {
            pairs.append_pair(key, value);
        }
        drop(pairs);
        url.to_string()
    }
}

fn validate_dev_center_url(url: &Url) -> Result<(), DevCenterError> {
    if url.scheme() != "https" {
        return Err(DevCenterError::new(DevCenterErrorKind::InvalidDevCenterUri));
    }
    if !url.username().is_empty() || url.password().is_some() || url.fragment().is_some() {
        return Err(DevCenterError::new(DevCenterErrorKind::InvalidDevCenterUri));
    }
    if url.port().is_some() || url.query().is_some() {
        return Err(DevCenterError::new(DevCenterErrorKind::InvalidDevCenterUri));
    }
    if url.path() != "/" && !url.path().is_empty() {
        return Err(DevCenterError::new(DevCenterErrorKind::InvalidDevCenterUri));
    }
    let Some(host) = url.host_str() else {
        return Err(DevCenterError::new(DevCenterErrorKind::InvalidDevCenterUri));
    };
    if host == "devcenter.azure.com"
        || !host.ends_with(".devcenter.azure.com")
        || host.contains('/')
    {
        return Err(DevCenterError::new(DevCenterErrorKind::InvalidDevCenterUri));
    }
    Ok(())
}

fn validate_resource_name(value: &str, kind: DevCenterErrorKind) -> Result<(), DevCenterError> {
    let bytes = value.as_bytes();
    if !(3..=63).contains(&bytes.len()) {
        return Err(DevCenterError::new(kind));
    }
    if !bytes[0].is_ascii_alphanumeric() {
        return Err(DevCenterError::new(kind));
    }
    if !bytes
        .iter()
        .all(|byte| matches!(byte, b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.'))
    {
        return Err(DevCenterError::new(kind));
    }
    Ok(())
}

fn bounded_optional_string(
    value: Option<String>,
    max: usize,
) -> Result<Option<String>, DevCenterError> {
    match value {
        Some(value) => Ok(Some(bounded_string(value, max, true)?)),
        None => Ok(None),
    }
}

fn bounded_string(value: String, max: usize, allow_empty: bool) -> Result<String, DevCenterError> {
    if value.len() > max || (!allow_empty && value.is_empty()) {
        return Err(DevCenterError::new(
            DevCenterErrorKind::ResponseBoundsExceeded,
        ));
    }
    Ok(value)
}

fn bounded_ability_vec(values: Vec<String>) -> Result<Vec<String>, DevCenterError> {
    if values.len() > 64 {
        return Err(DevCenterError::new(
            DevCenterErrorKind::ResponseBoundsExceeded,
        ));
    }
    values
        .into_iter()
        .map(|value| bounded_string(value, MAX_NAME_LEN * 2, false))
        .collect()
}

fn bounded_uri(value: String) -> Result<String, DevCenterError> {
    let uri = bounded_string(value, MAX_URI_LEN, false)?;
    Url::parse(&uri).map_err(|_| DevCenterError::new(DevCenterErrorKind::MalformedResponse))?;
    Ok(uri)
}

fn bounded_sensitive_uri(value: Option<String>) -> Result<Option<SensitiveUri>, DevCenterError> {
    match value {
        Some(value) => {
            let uri = bounded_string(value, MAX_REMOTE_URI_LEN, false)?;
            Url::parse(&uri)
                .map_err(|_| DevCenterError::new(DevCenterErrorKind::MalformedResponse))?;
            Ok(Some(SensitiveUri(uri.into_boxed_str())))
        }
        None => Ok(None),
    }
}

fn map_http_error(error: HttpError) -> DevCenterError {
    match error.kind() {
        HttpErrorKind::Cancelled => DevCenterError::new(DevCenterErrorKind::Cancelled),
        HttpErrorKind::TimedOut => DevCenterError::new(DevCenterErrorKind::TimedOut),
        HttpErrorKind::Redirected => DevCenterError::new(DevCenterErrorKind::RedirectRejected),
        HttpErrorKind::ResponseTooLarge => {
            DevCenterError::new(DevCenterErrorKind::ResponseTooLarge)
        }
        HttpErrorKind::Network => DevCenterError::new(DevCenterErrorKind::Network),
    }
}

fn map_status_error(status: StatusCode, claims_challenge: bool) -> DevCenterError {
    if claims_challenge {
        return DevCenterError::new(DevCenterErrorKind::PolicyDenied);
    }
    match status {
        StatusCode::UNAUTHORIZED => DevCenterError::new(DevCenterErrorKind::Unauthorized),
        StatusCode::FORBIDDEN => DevCenterError::new(DevCenterErrorKind::Forbidden),
        _ if status.is_redirection() => DevCenterError::new(DevCenterErrorKind::RedirectRejected),
        _ => DevCenterError::new(DevCenterErrorKind::UnexpectedStatus(status.as_u16())),
    }
}

fn map_token_state_error(error: AccessTokenStateError) -> DevCenterError {
    match error {
        AccessTokenStateError::Expired => DevCenterError::new(DevCenterErrorKind::TokenExpired),
    }
}

trait PagedResponse {
    type Item;

    fn items(&self) -> &[Self::Item];
    fn next_link(&self) -> Option<&str>;
    fn into_items(self) -> Vec<Self::Item>;
}

#[derive(Deserialize)]
struct WireProjectPage {
    value: Vec<WireProject>,
    #[serde(default, rename = "nextLink")]
    next_link: Option<String>,
}

impl PagedResponse for WireProjectPage {
    type Item = WireProject;
    fn items(&self) -> &[Self::Item] {
        &self.value
    }
    fn next_link(&self) -> Option<&str> {
        self.next_link.as_deref()
    }
    fn into_items(self) -> Vec<Self::Item> {
        self.value
    }
}

#[derive(Deserialize)]
struct WireProject {
    name: String,
    #[serde(default, rename = "displayName")]
    display_name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    uri: String,
    #[serde(default, rename = "maxDevBoxesPerUser")]
    max_dev_boxes_per_user: Option<u32>,
}

impl WireProject {
    fn try_into_project(self) -> Result<Project, DevCenterError> {
        validate_resource_name(&self.name, DevCenterErrorKind::MalformedResponse)?;
        Ok(Project {
            name: self.name,
            display_name: bounded_optional_string(self.display_name, MAX_DISPLAY_LEN)?,
            description: bounded_optional_string(self.description, MAX_DESCRIPTION_LEN)?,
            uri: bounded_uri(self.uri)?,
            max_dev_boxes_per_user: self.max_dev_boxes_per_user,
        })
    }
}

#[derive(Deserialize)]
struct WireProjectAbilities {
    #[serde(default, rename = "abilitiesAsAdmin")]
    abilities_as_admin: Vec<String>,
    #[serde(default, rename = "abilitiesAsDeveloper")]
    abilities_as_developer: Vec<String>,
}

impl WireProjectAbilities {
    fn try_into_abilities(self) -> Result<AbilitySet, DevCenterError> {
        Ok(AbilitySet {
            admin: bounded_ability_vec(self.abilities_as_admin)?,
            developer: bounded_ability_vec(self.abilities_as_developer)?,
        })
    }
}

#[derive(Deserialize)]
struct WireDevBoxPage {
    value: Vec<WireDevBox>,
    #[serde(default, rename = "nextLink")]
    next_link: Option<String>,
}

impl PagedResponse for WireDevBoxPage {
    type Item = WireDevBox;
    fn items(&self) -> &[Self::Item] {
        &self.value
    }
    fn next_link(&self) -> Option<&str> {
        self.next_link.as_deref()
    }
    fn into_items(self) -> Vec<Self::Item> {
        self.value
    }
}

#[derive(Deserialize)]
struct WireDevBox {
    name: String,
    #[serde(rename = "projectName")]
    project_name: String,
    #[serde(default, rename = "provisioningState")]
    provisioning_state: Option<String>,
    #[serde(default, rename = "poolName")]
    pool_name: Option<String>,
    #[serde(default)]
    location: Option<String>,
    #[serde(default, rename = "osType")]
    os_type: Option<String>,
    #[serde(default)]
    user: Option<String>,
    uri: String,
}

impl WireDevBox {
    fn try_into_dev_box(self) -> Result<DevBox, DevCenterError> {
        validate_resource_name(&self.name, DevCenterErrorKind::MalformedResponse)?;
        validate_resource_name(&self.project_name, DevCenterErrorKind::MalformedResponse)?;
        Ok(DevBox {
            name: self.name,
            project_name: self.project_name,
            provisioning_state: bounded_optional_string(self.provisioning_state, MAX_STATE_LEN)?,
            pool_name: bounded_optional_string(self.pool_name, MAX_POOL_LEN)?,
            location: bounded_optional_string(self.location, MAX_LOCATION_LEN)?,
            os_type: bounded_optional_string(self.os_type, MAX_STATE_LEN)?,
            user: bounded_optional_string(self.user, MAX_USER_LEN)?,
            uri: bounded_uri(self.uri)?,
        })
    }
}

#[derive(Deserialize)]
struct WireRemoteConnection {
    #[serde(default, rename = "cloudPcConnectionUrl")]
    cloud_pc_connection_url: Option<String>,
    #[serde(default, rename = "rdpConnectionUrl")]
    rdp_connection_url: Option<String>,
    #[serde(default, rename = "webUrl")]
    web_url: Option<String>,
}

impl WireRemoteConnection {
    fn try_into_remote_connection(self) -> Result<RemoteConnection, DevCenterError> {
        Ok(RemoteConnection {
            cloud_pc_connection_url: bounded_sensitive_uri(self.cloud_pc_connection_url)?,
            rdp_connection_url: bounded_sensitive_uri(self.rdp_connection_url)?,
            web_url: bounded_sensitive_uri(self.web_url)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EntraTenantId, PublicClientId};
    use static_assertions::assert_not_impl_any;
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpListener, TcpStream};
    use std::sync::{mpsc, Arc, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    assert_not_impl_any!(SensitiveUri: Clone, serde::Serialize);
    assert_not_impl_any!(RemoteConnection: Clone, serde::Serialize);

    #[derive(Clone)]
    struct TestServer {
        address: SocketAddr,
    }

    fn spawn_server(
        responder: impl Fn(String) -> (u16, &'static str, Vec<u8>, Vec<(&'static str, String)>)
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
                    let request = read_request(&mut stream).unwrap();
                    let path = request.split_whitespace().nth(1).unwrap().to_owned();
                    let (status, content_type, body, extra_headers) = responder_clone(path);
                    let status_text = match status {
                        200 => "OK",
                        302 => "Found",
                        401 => "Unauthorized",
                        403 => "Forbidden",
                        _ => "Status",
                    };
                    write!(stream, "HTTP/1.1 {status} {status_text}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n", body.len()).unwrap();
                    for (name, value) in extra_headers {
                        write!(stream, "{name}: {value}\r\n").unwrap();
                    }
                    write!(stream, "\r\n").unwrap();
                    stream.write_all(&body).unwrap();
                    stream.flush().unwrap();
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(10))
                }
                Err(_) => break,
            }
        });
        (TestServer { address }, stop_tx)
    }

    fn read_request(stream: &mut TcpStream) -> std::io::Result<String> {
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 1024];
        loop {
            let count = stream.read(&mut buffer)?;
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

    fn test_token(expires_at: Instant) -> AccessToken {
        AccessToken::from_test_parts(
            "token-value".to_owned(),
            EntraTenantId::parse("11111111-1111-1111-1111-111111111111").unwrap(),
            PublicClientId::parse("22222222-2222-2222-2222-222222222222").unwrap(),
            expires_at,
        )
        .unwrap()
    }

    fn test_client(server: &TestServer, expires_at: Instant) -> DevCenterClient {
        let endpoint = DevCenterUri(Url::parse(&format!("http://{}/", server.address)).unwrap());
        DevCenterClient::new_for_test(endpoint, test_token(expires_at)).unwrap()
    }

    #[test]
    fn dev_center_does_not_accept_partial_inventory_or_hide_http_failures() {
        for status in [206, 404, 429, 503] {
            let (server, stop) = spawn_server(move |_| {
                (
                    status,
                    "application/json",
                    br#"{"value":[]}"#.to_vec(),
                    vec![],
                )
            });
            let client = test_client(&server, Instant::now() + Duration::from_secs(60));
            let control = OperationControl::with_timeout(Duration::from_secs(5)).unwrap();
            let result = client.list_projects(&control);
            stop.send(()).unwrap();
            let error = result.unwrap_err();
            assert_eq!(error.kind(), DevCenterErrorKind::UnexpectedStatus(status));
            assert!(error.to_string().contains(&status.to_string()));
        }
    }

    #[test]
    fn dev_center_lists_projects_and_dev_boxes_across_pages() {
        let base_holder = Arc::new(Mutex::new(String::new()));
        let base_for_closure = Arc::clone(&base_holder);
        let (server, stop) = spawn_server(move |path| {
            match path.as_str() {
            "/projects?api-version=2025-02-01" => (
                200,
                "application/json",
                format!("{{\"value\":[{{\"name\":\"Alpha01\",\"displayName\":\"\",\"description\":\"\",\"uri\":\"{base}/projects/Alpha01\"}}],\"nextLink\":\"{base}/projects?page=2\"}}", base = base_for_closure.lock().unwrap().clone()).into_bytes(),
                vec![],
            ),
            "/projects?page=2" => (
                200,
                "application/json",
                format!("{{\"value\":[{{\"name\":\"Beta02\",\"uri\":\"{base}/projects/Beta02\"}}]}}", base = base_for_closure.lock().unwrap().clone()).into_bytes(),
                vec![],
            ),
            "/projects/Alpha01/users/me/abilities?api-version=2025-02-01" => (
                200,
                "application/json",
                br#"{"abilitiesAsAdmin":[],"abilitiesAsDeveloper":["ReadDevBoxes"]}"#.to_vec(),
                vec![],
            ),
            "/projects/Alpha01/users/me/devboxes?api-version=2025-02-01" => (
                200,
                "application/json",
                format!("{{\"value\":[{{\"name\":\"Box001\",\"projectName\":\"Alpha01\",\"uri\":\"{base}/projects/Alpha01/users/me/devboxes/Box001\",\"provisioningState\":\"Succeeded\"}}],\"nextLink\":\"{base}/projects/Alpha01/users/me/devboxes?page=2\"}}", base = base_for_closure.lock().unwrap().clone()).into_bytes(),
                vec![],
            ),
            "/projects/Alpha01/users/me/devboxes?page=2" => (
                200,
                "application/json",
                format!("{{\"value\":[{{\"name\":\"Box002\",\"projectName\":\"Alpha01\",\"uri\":\"{base}/projects/Alpha01/users/me/devboxes/Box002\"}}]}}", base = base_for_closure.lock().unwrap().clone()).into_bytes(),
                vec![],
            ),
            _ => (404, "text/plain", b"missing".to_vec(), vec![]),
        }
        });
        *base_holder.lock().unwrap() = format!("http://{}", server.address);
        let client = test_client(&server, Instant::now() + Duration::from_secs(60));
        let control = OperationControl::with_timeout(Duration::from_secs(5)).unwrap();

        let projects = client.list_projects(&control).unwrap();
        assert_eq!(projects[0].display_name.as_deref(), Some(""));
        assert_eq!(projects[0].description.as_deref(), Some(""));
        let dev_boxes = client
            .list_owned_dev_boxes(&ProjectName::parse("Alpha01").unwrap(), &control)
            .unwrap();
        assert_eq!(dev_boxes.len(), 2);
        stop.send(()).unwrap();
    }

    #[test]
    fn dev_center_rejects_cross_origin_next_link() {
        let (server, stop) = spawn_server(|path| match path.as_str() {
            "/projects?api-version=2025-02-01" => (
                200,
                "application/json",
                br#"{"value":[],"nextLink":"https://evil.example/projects?page=2"}"#.to_vec(),
                vec![],
            ),
            _ => unreachable!(),
        });
        let client = test_client(&server, Instant::now() + Duration::from_secs(60));
        let control = OperationControl::with_timeout(Duration::from_secs(5)).unwrap();
        let error = client.list_projects(&control).unwrap_err();
        assert_eq!(error.kind(), DevCenterErrorKind::PaginationOriginMismatch);
        stop.send(()).unwrap();
    }

    #[test]
    fn dev_center_rejects_pagination_cycles() {
        let base_holder = Arc::new(Mutex::new(String::new()));
        let base_for_closure = Arc::clone(&base_holder);
        let (server, stop) = spawn_server(move |path| {
            let base = base_for_closure.lock().unwrap().clone();
            match path.as_str() {
                "/projects?api-version=2025-02-01" => (
                    200,
                    "application/json",
                    format!("{{\"value\":[],\"nextLink\":\"{base}/projects?page=2\"}}")
                        .into_bytes(),
                    vec![],
                ),
                "/projects?page=2" => (
                    200,
                    "application/json",
                    format!(
                        "{{\"value\":[],\"nextLink\":\"{base}/projects?api-version=2025-02-01\"}}"
                    )
                    .into_bytes(),
                    vec![],
                ),
                _ => unreachable!(),
            }
        });
        *base_holder.lock().unwrap() = format!("http://{}", server.address);
        let client = test_client(&server, Instant::now() + Duration::from_secs(60));
        let control = OperationControl::with_timeout(Duration::from_secs(5)).unwrap();
        let error = client.list_projects(&control).unwrap_err();
        assert_eq!(error.kind(), DevCenterErrorKind::PaginationCycle);
        stop.send(()).unwrap();
    }

    #[test]
    fn dev_center_rejects_http_redirects() {
        let (server, stop) = spawn_server(|path| match path.as_str() {
            "/projects?api-version=2025-02-01" => (
                302,
                "text/plain",
                Vec::new(),
                vec![("Location", "http://example.invalid/next".to_owned())],
            ),
            _ => unreachable!(),
        });
        let client = test_client(&server, Instant::now() + Duration::from_secs(60));
        let control = OperationControl::with_timeout(Duration::from_secs(5)).unwrap();
        let error = client.list_projects(&control).unwrap_err();
        assert_eq!(error.kind(), DevCenterErrorKind::RedirectRejected);
        stop.send(()).unwrap();
    }

    #[test]
    fn dev_center_reports_claims_challenge_from_www_authenticate() {
        let (server, stop) = spawn_server(|path| match path.as_str() {
            "/projects?api-version=2025-02-01" => (
                401,
                "application/json",
                br#"{}"#.to_vec(),
                vec![(
                    "WWW-Authenticate",
                    "Bearer error=\"insufficient_claims\", claims=\"opaque\"".to_owned(),
                )],
            ),
            _ => unreachable!(),
        });
        let client = test_client(&server, Instant::now() + Duration::from_secs(60));
        let control = OperationControl::with_timeout(Duration::from_secs(5)).unwrap();
        let error = client.list_projects(&control).unwrap_err();
        assert_eq!(error.kind(), DevCenterErrorKind::PolicyDenied);
        stop.send(()).unwrap();
    }

    #[test]
    fn dev_center_rejects_expired_tokens_before_network() {
        let (server, stop) = spawn_server(|_| unreachable!());
        let client = test_client(&server, Instant::now() - Duration::from_secs(1));
        let control = OperationControl::with_timeout(Duration::from_secs(5)).unwrap();
        let error = client.list_projects(&control).unwrap_err();
        assert_eq!(error.kind(), DevCenterErrorKind::TokenExpired);
        stop.send(()).unwrap();
    }

    #[test]
    fn dev_center_allows_listing_without_remote_connection_ability_but_blocks_link_fetch() {
        let base_holder = Arc::new(Mutex::new(String::new()));
        let base_for_closure = Arc::clone(&base_holder);
        let (server, stop) = spawn_server(move |path| {
            match path.as_str() {
            "/projects/Alpha01/users/me/abilities?api-version=2025-02-01" => (
                200,
                "application/json",
                br#"{"abilitiesAsAdmin":[],"abilitiesAsDeveloper":["ReadDevBoxes"]}"#.to_vec(),
                vec![],
            ),
            "/projects/Alpha01/users/me/devboxes?api-version=2025-02-01" => (
                200,
                "application/json",
                format!("{{\"value\":[{{\"name\":\"Box001\",\"projectName\":\"Alpha01\",\"uri\":\"{base}/projects/Alpha01/users/me/devboxes/Box001\"}}]}}", base = base_for_closure.lock().unwrap().clone()).into_bytes(),
                vec![],
            ),
            _ => unreachable!(),
        }
        });
        *base_holder.lock().unwrap() = format!("http://{}", server.address);
        let client = test_client(&server, Instant::now() + Duration::from_secs(60));
        let control = OperationControl::with_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(
            client
                .list_owned_dev_boxes(&ProjectName::parse("Alpha01").unwrap(), &control)
                .unwrap()
                .len(),
            1
        );
        let error = client
            .get_remote_connection(
                &ProjectName::parse("Alpha01").unwrap(),
                &DevBoxName::parse("Box001").unwrap(),
                &control,
            )
            .unwrap_err();
        assert_eq!(error.kind(), DevCenterErrorKind::AbilityDenied);
        stop.send(()).unwrap();
    }

    #[test]
    fn dev_center_rejects_excessive_item_counts() {
        let items = (0..=MAX_ITEMS_PER_PAGE)
            .map(|index| format!("{{\"name\":\"P{index:0>3}\",\"uri\":\"http://127.0.0.1/projects/P{index:0>3}\"}}"))
            .collect::<Vec<_>>()
            .join(",");
        let body = format!("{{\"value\":[{items}]}}").into_bytes();
        let (server, stop) = spawn_server(move |path| match path.as_str() {
            "/projects?api-version=2025-02-01" => (200, "application/json", body.clone(), vec![]),
            _ => unreachable!(),
        });
        let client = test_client(&server, Instant::now() + Duration::from_secs(60));
        let control = OperationControl::with_timeout(Duration::from_secs(5)).unwrap();
        let error = client.list_projects(&control).unwrap_err();
        assert_eq!(error.kind(), DevCenterErrorKind::ResponseBoundsExceeded);
        stop.send(()).unwrap();
    }

    #[test]
    fn dev_center_can_be_cancelled() {
        let (server, stop) = spawn_server(|path| match path.as_str() {
            "/projects?api-version=2025-02-01" => {
                thread::sleep(Duration::from_millis(100));
                (200, "application/json", br#"{"value":[]}"#.to_vec(), vec![])
            }
            _ => unreachable!(),
        });
        let client = test_client(&server, Instant::now() + Duration::from_secs(60));
        let control = OperationControl::with_timeout(Duration::from_secs(5)).unwrap();
        control.cancel();
        let error = client.list_projects(&control).unwrap_err();
        assert_eq!(error.kind(), DevCenterErrorKind::Cancelled);
        stop.send(()).unwrap();
    }

    #[test]
    fn dev_center_parses_remote_connections_without_exposing_debug_urls() {
        let (server, stop) = spawn_server(|path| {
            match path.as_str() {
            "/projects/Alpha01/users/me/abilities?api-version=2025-02-01" => (
                200,
                "application/json",
                br#"{"abilitiesAsAdmin":[],"abilitiesAsDeveloper":["ReadRemoteConnections"]}"#.to_vec(),
                vec![],
            ),
            "/projects/Alpha01/users/me/devboxes/Box001/remoteConnection?api-version=2025-02-01" => (
                200,
                "application/json",
                br#"{"webUrl":"https://example.invalid/web","rdpConnectionUrl":"https://example.invalid/rdp"}"#.to_vec(),
                vec![],
            ),
            _ => unreachable!(),
        }
        });
        let client = test_client(&server, Instant::now() + Duration::from_secs(60));
        let control = OperationControl::with_timeout(Duration::from_secs(5)).unwrap();
        let remote = client
            .get_remote_connection(
                &ProjectName::parse("Alpha01").unwrap(),
                &DevBoxName::parse("Box001").unwrap(),
                &control,
            )
            .unwrap();
        assert_eq!(
            remote.web_url.as_ref().map(SensitiveUri::expose),
            Some("https://example.invalid/web")
        );
        assert!(format!("{remote:?}").contains("<redacted>"));
        stop.send(()).unwrap();
    }

    #[test]
    fn validation_rejects_invalid_dev_center_uri_and_names() {
        assert!(DevCenterUri::parse("https://1234-name.region.devcenter.azure.com").is_ok());
        assert_eq!(
            DevCenterUri::parse("http://1234-name.region.devcenter.azure.com")
                .unwrap_err()
                .kind(),
            DevCenterErrorKind::InvalidDevCenterUri
        );
        assert_eq!(
            ProjectName::parse("no").unwrap_err().kind(),
            DevCenterErrorKind::InvalidProjectName
        );
        assert_eq!(
            DevBoxName::parse("bad/name").unwrap_err().kind(),
            DevCenterErrorKind::InvalidDevBoxName
        );
        assert_eq!(
            DevCenterUri::parse(&format!("https://{}", "a".repeat(MAX_URI_LEN)))
                .unwrap_err()
                .kind(),
            DevCenterErrorKind::InvalidDevCenterUri
        );
    }
}
