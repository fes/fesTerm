//! Zero-shell HOST cloud resources. Account credentials are management authority,
//! never the enrolled-owner proof used by the inner HOST protocol.

use crate::{http_client, identity::Identity, service::wipe_tokens, Error};
use reqwest::{header, Method, Request};
use serde::{Deserialize, Serialize};
use std::{future::Future, pin::Pin};
use tokio_util::sync::CancellationToken;
use tunnels::{
    contracts::{
        ClusterRecommendationResponse, Tunnel, TunnelAccessControl, TunnelAccessControlEntry,
        TunnelAccessControlEntryType, TunnelPort, PROD_DNS_NAME, PROVIDERS_GITHUB,
        TUNNEL_PROTOCOL_TCP,
    },
    management::Authorization,
};
use zeroize::Zeroizing;

pub const HOST_LABEL: &str = "festerm-host-v1";
pub const HOST_PORT: u16 = 2222;

/// Public metadata, not permission to create a resource. The setup controller
/// durably reserves these exact values before submitting a request.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Resource {
    pub tunnel_id: String,
    pub cluster: String,
    pub account_id: u64,
    pub ownership_label: String,
    pub port: u16,
    pub labels: Vec<String>,
}

impl Resource {
    pub fn validate(&self) -> Result<(), Error> {
        if self.account_id == 0
            || self.port != HOST_PORT
            || self.labels != [HOST_LABEL.to_owned(), self.ownership_label.clone()]
            || !identifier(&self.tunnel_id, 49)
            || !identifier(&self.cluster, 24)
            || !self
                .ownership_label
                .strip_prefix("festerm-owned-")
                .is_some_and(|id| {
                    id.len() == 32
                        && id
                            .bytes()
                            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
                })
        {
            return Err(Error::Response);
        }
        Ok(())
    }

    pub fn qualified_id(&self) -> String {
        format!("{}.{}", self.tunnel_id, self.cluster)
    }
}

fn identifier(value: &str, max: usize) -> bool {
    (3..=max).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !value.starts_with('-')
        && !value.ends_with('-')
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub resource: Resource,
    pub created: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Observation {
    Present(Receipt),
    /// Only an authenticated exact-cluster, exact-ID HTTP 404 yields this.
    Absent,
}

/// Built and validated before the setup journal records a possible submission.
/// The request owns its short-lived sensitive authorization header.
pub struct PreparedCreate {
    request: Request,
    resource: Resource,
}

impl PreparedCreate {
    pub fn resource(&self) -> &Resource {
        &self.resource
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CreateError {
    /// The transport future was never polled.
    NotSent(Error),
    /// A complete service response definitively rejected this create.
    Rejected(Error),
    /// A conditional conflict requires observation, never ID freshening.
    Conflict,
    /// The request may have been accepted; retain its durable reservation.
    Uncertain(Error),
}

struct Reply {
    status: u16,
    bytes: Zeroizing<Vec<u8>>,
}

type ReplyFuture<'a> = Pin<Box<dyn Future<Output = Result<Reply, Error>> + Send + 'a>>;
trait Transport: Send + Sync {
    fn send(&self, request: Request) -> ReplyFuture<'_>;
}

struct Http(reqwest::Client);
impl Transport for Http {
    fn send(&self, request: Request) -> ReplyFuture<'_> {
        Box::pin(async move {
            let mut response = self.0.execute(request).await.map_err(|_| Error::Network)?;
            let status = response.status().as_u16();
            if response
                .content_length()
                .is_some_and(|n| n > crate::RESPONSE_LIMIT as u64)
            {
                return Err(Error::Response);
            }
            let mut bytes = Zeroizing::new(Vec::new());
            while let Some(chunk) = response.chunk().await.map_err(|_| Error::Network)? {
                if chunk.len() > crate::RESPONSE_LIMIT - bytes.len() {
                    return Err(Error::Response);
                }
                bytes.extend_from_slice(&chunk);
            }
            Ok(Reply { status, bytes })
        })
    }
}

/// Fixed official HTTPS authority, no executable path or network override.
pub struct Management(Http);
impl Management {
    pub fn new() -> Result<Self, Error> {
        Ok(Self(Http(http_client()?)))
    }

    pub async fn recommend_cluster(
        &self,
        identity: &Identity,
        cancel: &CancellationToken,
    ) -> Result<String, Error> {
        recommend(&self.0, identity, cancel).await
    }

    pub fn prepare_create(
        identity: &Identity,
        resource: &Resource,
        description: &str,
    ) -> Result<PreparedCreate, Error> {
        prepare_create(identity, resource, description)
    }

    pub async fn submit(
        &self,
        prepared: PreparedCreate,
        cancel: &CancellationToken,
    ) -> Result<Receipt, CreateError> {
        submit(&self.0, prepared, cancel).await
    }

    pub async fn observe(
        &self,
        identity: &Identity,
        resource: &Resource,
        cancel: &CancellationToken,
    ) -> Result<Observation, Error> {
        observe(&self.0, identity, resource, cancel).await
    }

    /// Obtain only a short-lived host token for the exact pinned cloud receipt.
    /// This grants relay connectivity, never enrolled-owner or shell authority.
    pub async fn host_token(
        &self,
        identity: &Identity,
        exact: &Receipt,
        cancel: &CancellationToken,
    ) -> Result<Zeroizing<String>, Error> {
        host_token(&self.0, identity, exact, cancel).await
    }
}

fn request(
    identity: &Identity,
    method: Method,
    cluster: Option<&str>,
    path: &str,
) -> Result<Request, Error> {
    if identity.needs_sign_in() {
        return Err(Error::Expired);
    }
    let host = match cluster {
        Some(cluster) if identifier(cluster, 24) => {
            format!(
                "{cluster}.{}",
                PROD_DNS_NAME
                    .strip_prefix("global.")
                    .ok_or(Error::Response)?
            )
        }
        None => PROD_DNS_NAME.into(),
        _ => return Err(Error::Response),
    };
    let mut url = url::Url::parse(&format!("https://{host}{path}")).map_err(|_| Error::Response)?;
    url.query_pairs_mut()
        .append_pair("api-version", "2023-09-27-preview");
    let mut request = Request::new(method, url);
    let auth = Authorization::Github(identity.access_token().into());
    let value = Zeroizing::new(auth.as_header().ok_or(Error::Response)?);
    let mut value = header::HeaderValue::from_str(&value).map_err(|_| Error::Response)?;
    value.set_sensitive(true);
    request.headers_mut().insert(header::AUTHORIZATION, value);
    request.headers_mut().insert(
        header::USER_AGENT,
        header::HeaderValue::from_static("fesTerm-native-tunnel/1"),
    );
    Ok(request)
}

async fn send(
    http: &impl Transport,
    request: Request,
    cancel: &CancellationToken,
) -> Result<Reply, Error> {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(Error::Cancelled),
        reply = http.send(request) => {
            let reply = reply?;
            if reply.bytes.len() > crate::RESPONSE_LIMIT { return Err(Error::Response); }
            match reply.status {
                200..=299 | 404 => Ok(reply),
                401 | 403 => Err(Error::Denied),
                429 => Err(Error::RateLimited),
                409 | 412 => Err(Error::IdentityChanged),
                _ => Err(Error::Network),
            }
        }
    }
}

async fn recommend(
    http: &impl Transport,
    identity: &Identity,
    cancel: &CancellationToken,
) -> Result<String, Error> {
    let reply = match send(
        http,
        request(identity, Method::GET, None, "/clusters/recommendations")?,
        cancel,
    )
    .await
    {
        // Match the Microsoft SDK's placement-only retry: provider authorization
        // can be rejected before this public endpoint reaches its controller.
        Err(Error::Denied) => {
            let mut anonymous = request(identity, Method::GET, None, "/clusters/recommendations")?;
            anonymous.headers_mut().remove(header::AUTHORIZATION);
            send(http, anonymous, cancel).await?
        }
        result => result?,
    };
    if reply.status != 200 {
        return Err(Error::Response);
    }
    let response: ClusterRecommendationResponse = crate::decode(&reply.bytes)?;
    response
        .recommended_cluster_id
        .filter(|s| identifier(s, 24))
        .ok_or(Error::Response)
}

fn resource_request(
    identity: &Identity,
    method: Method,
    resource: &Resource,
) -> Result<Request, Error> {
    resource.validate()?;
    if resource.account_id != identity.account.id {
        return Err(Error::AccountChanged);
    }
    let mut request = request(
        identity,
        method,
        Some(&resource.cluster),
        &format!("/tunnels/{}", resource.tunnel_id),
    )?;
    request
        .url_mut()
        .query_pairs_mut()
        .append_pair("includePorts", "true");
    Ok(request)
}

fn prepare_create(
    identity: &Identity,
    resource: &Resource,
    description: &str,
) -> Result<PreparedCreate, Error> {
    if description.trim().is_empty()
        || description.len() > 80
        || description.chars().any(char::is_control)
    {
        return Err(Error::Response);
    }
    let mut request = resource_request(identity, Method::PUT, resource)?;
    // The official TypeScript SDK's createTunnel uses this precondition.
    // Never use create-or-update or generate another ID after a lost reply.
    request
        .headers_mut()
        .insert(header::IF_NONE_MATCH, header::HeaderValue::from_static("*"));
    request.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("application/json"),
    );
    let tunnel = Tunnel {
        tunnel_id: Some(resource.tunnel_id.clone()),
        cluster_id: Some(resource.cluster.clone()),
        description: Some(description.into()),
        labels: resource.labels.clone(),
        access_control: Some(TunnelAccessControl {
            entries: vec![TunnelAccessControlEntry {
                kind: TunnelAccessControlEntryType::Users,
                provider: Some(PROVIDERS_GITHUB.into()),
                subjects: vec![resource.account_id.to_string()],
                scopes: vec!["connect".into()],
                is_inherited: false,
                is_deny: false,
                is_inverse: false,
                organization: None,
                expiration: None,
            }],
        }),
        ports: vec![TunnelPort {
            port_number: resource.port,
            protocol: Some(TUNNEL_PROTOCOL_TCP.into()),
            labels: vec![HOST_LABEL.into()],
            ..Default::default()
        }],
        ..Default::default()
    };
    *request.body_mut() = Some(
        serde_json::to_vec(&tunnel)
            .map_err(|_| Error::Response)?
            .into(),
    );
    Ok(PreparedCreate {
        request,
        resource: resource.clone(),
    })
}

async fn submit(
    http: &impl Transport,
    prepared: PreparedCreate,
    cancel: &CancellationToken,
) -> Result<Receipt, CreateError> {
    let PreparedCreate { request, resource } = prepared;
    let mut polled = false;
    let transport = async {
        polled = true;
        http.send(request).await
    };
    let result = tokio::select! {
        biased;
        _ = cancel.cancelled() => None,
        reply = transport => Some(reply),
    };
    let reply = match result {
        None if !polled => return Err(CreateError::NotSent(Error::Cancelled)),
        None => return Err(CreateError::Uncertain(Error::Cancelled)),
        Some(Err(error)) => return Err(CreateError::Uncertain(error)),
        Some(Ok(reply)) => reply,
    };
    if reply.bytes.len() > crate::RESPONSE_LIMIT {
        return Err(CreateError::Uncertain(Error::Response));
    }
    match reply.status {
        200..=299 => receipt(&reply.bytes, &resource).map_err(CreateError::Uncertain),
        400 => Err(CreateError::Rejected(Error::Response)),
        401 | 403 => Err(CreateError::Rejected(Error::Denied)),
        409 | 412 => Err(CreateError::Conflict),
        _ => Err(CreateError::Uncertain(Error::Network)),
    }
}

async fn observe(
    http: &impl Transport,
    identity: &Identity,
    resource: &Resource,
    cancel: &CancellationToken,
) -> Result<Observation, Error> {
    let request = resource_request(identity, Method::GET, resource)?;
    let reply = send(http, request, cancel).await?;
    if reply.status == 404 {
        return Ok(Observation::Absent);
    }
    receipt(&reply.bytes, resource).map(Observation::Present)
}

async fn host_token(
    http: &impl Transport,
    identity: &Identity,
    exact: &Receipt,
    cancel: &CancellationToken,
) -> Result<Zeroizing<String>, Error> {
    let mut request = resource_request(identity, Method::GET, &exact.resource)?;
    request
        .url_mut()
        .query_pairs_mut()
        .append_pair("tokenScopes", "host");
    let reply = send(http, request, cancel).await?;
    if reply.status == 404 {
        return Err(Error::TunnelNotFound);
    }
    if receipt(&reply.bytes, &exact.resource)? != *exact {
        return Err(Error::IdentityChanged);
    }
    let mut tunnel: Tunnel = crate::decode(&reply.bytes)?;
    let token = tunnel
        .access_tokens
        .as_mut()
        .and_then(|tokens| tokens.remove("host"))
        .map(Zeroizing::new);
    wipe_tokens(&mut tunnel);
    token
        .filter(|token| crate::valid_token(token))
        .ok_or(Error::Response)
}

fn receipt(bytes: &[u8], resource: &Resource) -> Result<Receipt, Error> {
    let mut tunnel: Tunnel = crate::decode(bytes)?;
    wipe_tokens(&mut tunnel);
    let same_acl = |acl: &TunnelAccessControl| {
        !acl.entries.is_empty()
            && acl.entries.iter().all(|entry| {
                matches!(entry.kind, TunnelAccessControlEntryType::Users)
                    && entry.provider.as_deref() == Some(PROVIDERS_GITHUB)
                    && entry.subjects == [resource.account_id.to_string()]
                    && !entry.is_inverse
                    && !entry.is_deny
                    && entry.organization.is_none()
                    && entry.expiration.is_none()
            })
    };
    let labels = &resource.labels;
    if tunnel.tunnel_id.as_deref() != Some(&resource.tunnel_id)
        || tunnel.cluster_id.as_deref() != Some(&resource.cluster)
        || tunnel.labels.len() != 2
        || !labels.iter().all(|l| tunnel.labels.contains(l))
        || !tunnel.access_control.as_ref().is_some_and(same_acl)
        || tunnel.ports.len() != 1
        || !tunnel.ports.iter().all(|port| {
            port.port_number == resource.port
                && port.protocol.as_deref() == Some(TUNNEL_PROTOCOL_TCP)
                && port.labels == [HOST_LABEL]
                && port.access_control.as_ref().is_none_or(same_acl)
                && port
                    .tunnel_id
                    .as_ref()
                    .is_none_or(|id| id == &resource.tunnel_id)
                && port
                    .cluster_id
                    .as_ref()
                    .is_none_or(|id| id == &resource.cluster)
        })
    {
        return Err(Error::IdentityChanged);
    }
    Ok(Receipt {
        resource: resource.clone(),
        created: tunnel.created.ok_or(Error::Response)?.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::VecDeque, sync::Mutex};

    struct Fake {
        replies: Mutex<VecDeque<(u16, serde_json::Value)>>,
        requests: Mutex<Vec<Request>>,
    }
    impl Fake {
        fn new(replies: Vec<(u16, serde_json::Value)>) -> Self {
            Self {
                replies: Mutex::new(replies.into()),
                requests: Mutex::new(Vec::new()),
            }
        }
    }
    impl Transport for Fake {
        fn send(&self, request: Request) -> ReplyFuture<'_> {
            Box::pin(async move {
                self.requests.lock().unwrap().push(request);
                let (status, body) = self.replies.lock().unwrap().pop_front().unwrap();
                Ok(Reply {
                    status,
                    bytes: Zeroizing::new(serde_json::to_vec(&body).unwrap()),
                })
            })
        }
    }
    fn identity() -> Identity {
        identity_with_expiry(None)
    }
    fn identity_with_expiry(expires_at: Option<u64>) -> Identity {
        let body = serde_json::json!({
            "account": {"id": 42, "login": "fixture"},
            "client_id": crate::identity::SERVICE_CLIENT_ID,
            "access_token": "PRIVATE_SYNTHETIC_GITHUB_ACCESS",
            "refresh_token": null, "expires_at": expires_at, "refresh_expires_at": null,
        });
        let bytes = [
            b"FESTERM_GITHUB_IDENTITY_V2\0".as_slice(),
            &serde_json::to_vec(&body).unwrap(),
        ]
        .concat();
        Identity::decode_private(&bytes, 42, crate::identity::SERVICE_CLIENT_ID).unwrap()
    }
    fn resource() -> Resource {
        Resource {
            tunnel_id: "festerm-11111111111141118111111111111111".into(),
            cluster: "usw2".into(),
            account_id: 42,
            ownership_label: "festerm-owned-22222222222242228222222222222222".into(),
            port: HOST_PORT,
            labels: vec![
                HOST_LABEL.into(),
                "festerm-owned-22222222222242228222222222222222".into(),
            ],
        }
    }
    fn response() -> serde_json::Value {
        let resource = resource();
        serde_json::json!({
            "tunnelId": resource.tunnel_id, "clusterId": resource.cluster,
            "labels": [HOST_LABEL, resource.ownership_label],
            "created": "2026-11-09T00:00:00Z",
            "accessControl": {"entries": [{
                "type": "Users", "provider": "github", "subjects": ["42"], "scopes": ["connect"],
            }]},
            "ports": [{"portNumber": HOST_PORT, "protocol": "tcp", "labels": [HOST_LABEL]}],
        })
    }

    #[tokio::test]
    async fn host_cloud_placement_retries_denied_authorization_once_anonymously() {
        for denied in [401, 403] {
            let http = Fake::new(vec![
                (denied, serde_json::json!({})),
                (
                    200,
                    serde_json::json!({"recommendedClusterId":"usw2","isFallback":false,"recommendations":[]}),
                ),
            ]);
            assert_eq!(
                recommend(&http, &identity(), &CancellationToken::new())
                    .await
                    .unwrap(),
                "usw2"
            );
            let requests = http.requests.lock().unwrap();
            assert_eq!(requests.len(), 2);
            assert_eq!(requests[0].method(), Method::GET);
            assert_eq!(requests[1].method(), Method::GET);
            assert_eq!(requests[0].url(), requests[1].url());
            assert_eq!(requests[1].url().as_str(), "https://global.rel.tunnels.api.visualstudio.com/clusters/recommendations?api-version=2023-09-27-preview");
            assert!(requests[0].headers()[header::AUTHORIZATION].is_sensitive());
            assert!(!requests[1].headers().contains_key(header::AUTHORIZATION));
            let mut original_headers = requests[0].headers().clone();
            original_headers.remove(header::AUTHORIZATION);
            assert_eq!(&original_headers, requests[1].headers());
        }
    }

    #[tokio::test]
    async fn host_cloud_placement_does_not_retry_other_failures_or_repeat_anonymous_denial() {
        for (status, expected) in [
            (404, Error::Response),
            (429, Error::RateLimited),
            (500, Error::Network),
            (409, Error::IdentityChanged),
            (200, Error::Response),
        ] {
            let http = Fake::new(vec![(status, serde_json::json!({}))]);
            assert_eq!(
                recommend(&http, &identity(), &CancellationToken::new()).await,
                Err(expected)
            );
            assert_eq!(http.requests.lock().unwrap().len(), 1);
        }
        for status in [401, 403] {
            let http = Fake::new(vec![
                (401, serde_json::json!({})),
                (status, serde_json::json!({})),
            ]);
            assert_eq!(
                recommend(&http, &identity(), &CancellationToken::new()).await,
                Err(Error::Denied)
            );
            assert_eq!(http.requests.lock().unwrap().len(), 2);
        }
        let http = Fake::new(vec![]);
        assert_eq!(
            recommend(
                &http,
                &identity_with_expiry(Some(1)),
                &CancellationToken::new()
            )
            .await,
            Err(Error::Expired)
        );
        assert!(http.requests.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn host_cloud_placement_cancellation_prevents_anonymous_retry() {
        struct CancelOnReply {
            cancel: CancellationToken,
            requests: Mutex<Vec<Request>>,
        }
        impl Transport for CancelOnReply {
            fn send(&self, request: Request) -> ReplyFuture<'_> {
                Box::pin(async move {
                    self.requests.lock().unwrap().push(request);
                    self.cancel.cancel();
                    Ok(Reply {
                        status: 401,
                        bytes: Zeroizing::new(Vec::new()),
                    })
                })
            }
        }
        let cancel = CancellationToken::new();
        let http = CancelOnReply {
            cancel: cancel.clone(),
            requests: Mutex::new(Vec::new()),
        };
        assert_eq!(
            recommend(&http, &identity(), &cancel).await,
            Err(Error::Cancelled)
        );
        assert_eq!(http.requests.lock().unwrap().len(), 1);
        let http = Fake::new(vec![]);
        assert_eq!(
            recommend(&http, &identity(), &cancel).await,
            Err(Error::Cancelled)
        );
        assert!(http.requests.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn host_cloud_resource_operations_never_retry_denied_authorization_anonymously() {
        let identity = identity();
        let exact = receipt(&serde_json::to_vec(&response()).unwrap(), &resource()).unwrap();
        let cancel = CancellationToken::new();
        for status in [401, 403] {
            for operation in 0..3 {
                let http = Fake::new(vec![(status, serde_json::json!({}))]);
                match operation {
                    0 => {
                        let prepared =
                            prepare_create(&identity, &resource(), "Fixture host").unwrap();
                        assert_eq!(
                            submit(&http, prepared, &cancel).await,
                            Err(CreateError::Rejected(Error::Denied))
                        );
                    }
                    1 => assert_eq!(
                        observe(&http, &identity, &resource(), &cancel).await,
                        Err(Error::Denied)
                    ),
                    _ => assert_eq!(
                        host_token(&http, &identity, &exact, &cancel).await,
                        Err(Error::Denied)
                    ),
                }
                let requests = http.requests.lock().unwrap();
                assert_eq!(requests.len(), 1);
                assert!(requests[0].headers()[header::AUTHORIZATION].is_sensitive());
            }
        }
    }

    #[tokio::test]
    async fn host_cloud_scoped_host_token_requires_exact_receipt_and_never_uses_management_token() {
        let identity = identity();
        let exact = receipt(&serde_json::to_vec(&response()).unwrap(), &resource()).unwrap();
        let mut body = response();
        body["accessTokens"] =
            serde_json::json!({"host":"PRIVATE_SCOPED_HOST","manage":"PRIVATE_UNUSED"});
        let http = Fake::new(vec![(200, body)]);
        let token = host_token(&http, &identity, &exact, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(token.as_str(), "PRIVATE_SCOPED_HOST");
        let requests = http.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method(), Method::GET);
        assert!(requests[0]
            .url()
            .query_pairs()
            .any(|(key, value)| key == "tokenScopes" && value == "host"));
        assert!(!requests[0].url().as_str().contains("PRIVATE"));
        assert!(requests[0].headers()[header::AUTHORIZATION].is_sensitive());
    }

    #[tokio::test]
    async fn host_cloud_scoped_host_token_refuses_replacement_account_absence_and_cancellation() {
        let identity = identity();
        let exact = receipt(&serde_json::to_vec(&response()).unwrap(), &resource()).unwrap();
        for (field, value) in [
            ("created", serde_json::json!("2026-11-10T00:00:00Z")),
            ("tunnelId", serde_json::json!("foreign-tunnel")),
            (
                "accessTokens",
                serde_json::json!({"manage":"PRIVATE_UNUSED"}),
            ),
            ("accessTokens", serde_json::json!({"host":"not a token"})),
        ] {
            let mut body = response();
            body["accessTokens"] = serde_json::json!({"host":"PRIVATE_SCOPED_HOST"});
            body[field] = value;
            let http = Fake::new(vec![(200, body)]);
            assert!(
                host_token(&http, &identity, &exact, &CancellationToken::new())
                    .await
                    .is_err()
            );
        }
        let http = Fake::new(vec![(404, serde_json::json!({}))]);
        assert!(matches!(
            host_token(&http, &identity, &exact, &CancellationToken::new()).await,
            Err(Error::TunnelNotFound)
        ));
        let mut foreign = exact.clone();
        foreign.resource.account_id = 43;
        let http = Fake::new(vec![]);
        assert!(matches!(
            host_token(&http, &identity, &foreign, &CancellationToken::new()).await,
            Err(Error::AccountChanged)
        ));
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert!(matches!(
            host_token(&http, &identity, &exact, &cancel).await,
            Err(Error::Cancelled)
        ));
        assert!(http.requests.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn host_cloud_requests_use_fixed_authority_private_owner_acl_and_zero_shell_port() {
        let mut put_response = response();
        put_response["created"] = "2026-11-09T00:00:00.123Z".into();
        let mut get_response = response();
        get_response["created"] = "2026-11-09T00:00:00.123456700+00:00".into();
        let http = Fake::new(vec![
            (
                200,
                serde_json::json!({"recommendedClusterId":"usw2","isFallback":false,"recommendations":[]}),
            ),
            (201, put_response),
            (200, get_response),
        ]);
        let cancel = CancellationToken::new();
        let identity = identity();
        assert_eq!(recommend(&http, &identity, &cancel).await.unwrap(), "usw2");
        let prepared = prepare_create(&identity, &resource(), "Fixture host").unwrap();
        let put_receipt = submit(&http, prepared, &cancel).await.unwrap();
        let Observation::Present(get_receipt) = observe(&http, &identity, &resource(), &cancel)
            .await
            .unwrap()
        else {
            panic!("exact resource missing");
        };
        assert_eq!(put_receipt.created, "2026-11-09T00:00:00.123Z");
        assert_eq!(get_receipt.created, "2026-11-09T00:00:00.1234567Z");
        assert_ne!(get_receipt, put_receipt);
        let requests = http.requests.lock().unwrap();
        assert_eq!(requests[0].url().as_str(), "https://global.rel.tunnels.api.visualstudio.com/clusters/recommendations?api-version=2023-09-27-preview");
        assert_eq!(requests[1].method(), Method::PUT);
        assert_eq!(
            requests[1].url().host_str(),
            Some("usw2.rel.tunnels.api.visualstudio.com")
        );
        assert_eq!(requests[1].headers()[header::IF_NONE_MATCH], "*");
        assert!(requests
            .iter()
            .all(|request| request.headers()[header::AUTHORIZATION].is_sensitive()));
        assert!(requests
            .iter()
            .all(|request| !request.url().as_str().contains("PRIVATE")
                && !request.url().as_str().contains("tokenScopes")));
        let body: serde_json::Value =
            serde_json::from_slice(requests[1].body().unwrap().as_bytes().unwrap()).unwrap();
        assert_eq!(
            body["accessControl"]["entries"][0]["subjects"],
            serde_json::json!(["42"])
        );
        assert_eq!(
            body["accessControl"]["entries"][0]["scopes"],
            serde_json::json!(["connect"])
        );
        assert_eq!(body["ports"].as_array().unwrap().len(), 1);
        assert_eq!(body["ports"][0]["portNumber"], HOST_PORT);
        assert!(body.get("accessTokens").is_none());
        assert!(body.get("endpoints").is_none());
        assert!(body.get("name").is_none());
    }

    #[tokio::test]
    async fn host_cloud_response_refuses_foreign_resources_and_non_authoritative_absence() {
        let cancel = CancellationToken::new();
        let identity = identity();
        for field in [
            "labels",
            "clusterId",
            "tunnelId",
            "accessControl",
            "ports",
            "created",
        ] {
            let mut body = response();
            match field {
                "labels" => body[field] = serde_json::json!([HOST_LABEL, "someone-elses-label"]),
                "clusterId" => body[field] = "euw1".into(),
                "tunnelId" => body[field] = "some-other-tunnel".into(),
                "accessControl" => body[field]["entries"][0]["type"] = "Anonymous".into(),
                "ports" => body[field][0]["portNumber"] = 22.into(),
                "created" => body
                    .as_object_mut()
                    .unwrap()
                    .remove("created")
                    .map(|_| ())
                    .unwrap(),
                _ => unreachable!(),
            }
            let http = Fake::new(vec![(200, body)]);
            assert!(
                observe(&http, &identity, &resource(), &cancel)
                    .await
                    .is_err(),
                "{field}"
            );
        }
        for status in [401, 403, 429, 500, 503] {
            let http = Fake::new(vec![(
                status,
                serde_json::json!({"message":"untrusted PRIVATE body"}),
            )]);
            assert!(observe(&http, &identity, &resource(), &cancel)
                .await
                .is_err());
        }
        let http = Fake::new(vec![(404, serde_json::json!({}))]);
        assert_eq!(
            observe(&http, &identity, &resource(), &cancel)
                .await
                .unwrap(),
            Observation::Absent
        );
        let http = Fake::new(vec![]);
        let mut foreign = resource();
        foreign.account_id = 99;
        assert_eq!(
            observe(&http, &identity, &foreign, &cancel).await,
            Err(Error::AccountChanged)
        );
        assert!(http.requests.lock().unwrap().is_empty());
        cancel.cancel();
        assert_eq!(
            observe(&http, &identity, &resource(), &cancel).await,
            Err(Error::Cancelled)
        );
        assert!(http.requests.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn host_cloud_create_distinguishes_preflight_not_sent_rejection_conflict_and_uncertainty()
    {
        let resource = resource();
        assert!(matches!(
            prepare_create(&identity_with_expiry(Some(1)), &resource, "Fixture"),
            Err(Error::Expired)
        ));
        let http = Fake::new(vec![]);
        let cancel = CancellationToken::new();
        let prepared = prepare_create(&identity(), &resource, "Fixture").unwrap();
        cancel.cancel();
        assert_eq!(
            submit(&http, prepared, &cancel).await,
            Err(CreateError::NotSent(Error::Cancelled))
        );
        assert!(http.requests.lock().unwrap().is_empty());

        struct CancelOnPoll(CancellationToken);
        impl Transport for CancelOnPoll {
            fn send(&self, _: Request) -> ReplyFuture<'_> {
                Box::pin(async {
                    self.0.cancel();
                    std::future::pending().await
                })
            }
        }
        let cancel = CancellationToken::new();
        let http = CancelOnPoll(cancel.clone());
        let prepared = prepare_create(&identity(), &resource, "Fixture").unwrap();
        assert_eq!(
            submit(&http, prepared, &cancel).await,
            Err(CreateError::Uncertain(Error::Cancelled))
        );

        for (status, expected) in [
            (400, CreateError::Rejected(Error::Response)),
            (401, CreateError::Rejected(Error::Denied)),
            (403, CreateError::Rejected(Error::Denied)),
            (409, CreateError::Conflict),
            (412, CreateError::Conflict),
            (500, CreateError::Uncertain(Error::Network)),
            (201, CreateError::Uncertain(Error::IdentityChanged)),
        ] {
            let mut replies = vec![(status, serde_json::json!({}))];
            if expected == CreateError::Conflict {
                replies.push((200, response()));
            } else if status == 400 {
                replies.extend([(404, serde_json::json!({})), (201, response())]);
            }
            let http = Fake::new(replies);
            let prepared = prepare_create(&identity(), &resource, "Fixture").unwrap();
            let cancel = CancellationToken::new();
            assert_eq!(submit(&http, prepared, &cancel).await, Err(expected));
            if expected == CreateError::Conflict {
                assert!(matches!(
                    observe(&http, &identity(), &resource, &cancel)
                        .await
                        .unwrap(),
                    Observation::Present(_)
                ));
            } else if status == 400 {
                assert_eq!(
                    observe(&http, &identity(), &resource, &cancel)
                        .await
                        .unwrap(),
                    Observation::Absent
                );
                let retry = prepare_create(&identity(), &resource, "Fixture").unwrap();
                submit(&http, retry, &cancel).await.unwrap();
            }
            let requests = http.requests.lock().unwrap();
            assert_eq!(
                requests.len(),
                if status == 400 {
                    3
                } else if expected == CreateError::Conflict {
                    2
                } else {
                    1
                }
            );
            assert_eq!(requests[0].headers()[header::IF_NONE_MATCH], "*");
            assert!(requests[0].url().path().ends_with(&resource.tunnel_id));
            if status == 400 {
                assert_eq!(requests[0].url(), requests[2].url());
                assert_eq!(
                    requests[0].body().unwrap().as_bytes(),
                    requests[2].body().unwrap().as_bytes()
                );
                assert_eq!(requests[2].headers()[header::IF_NONE_MATCH], "*");
            }
        }
    }
}
