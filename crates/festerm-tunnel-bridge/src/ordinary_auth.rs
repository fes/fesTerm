//! Ordinary remoting wire/proof primitives. A verified proof is NOT authority:
//! the runtime must separately require a current provider-admitted route/policy.
use crate::{
    host_runtime::StreamRole,
    owner_auth::{hex, unhex, validate_owner_public_key, PinnedHost},
    relay::ClientConnection,
};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use festerm_host_protocol::{ContractError, Epoch, HostId, PrincipalId, Validate};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

pub const SSH_USERNAME: &str = "festerm-ordinary-v1";
pub const ORDINARY_VERSION: u16 = 1;
pub const PROOF_LIFETIME: Duration = Duration::from_secs(5);
const DOMAIN: &[u8] = b"fesTerm HOST provider ordinary SSH channel proof v1\0";

/// Public routing metadata, never a provider authorization assertion.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrdinaryRoute {
    pub tunnel_id: String,
    pub cluster: String,
    pub endpoint_id: String,
    pub port: u16,
    pub host: PinnedHost,
}

impl Validate for OrdinaryRoute {
    fn validate(&self) -> Result<(), ContractError> {
        self.host.validate()?;
        for (value, limit) in [
            (&self.tunnel_id, 100),
            (&self.cluster, 100),
            (&self.endpoint_id, 160),
        ] {
            if value.is_empty()
                || value.len() > limit
                || !value
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            {
                return Err(ContractError::Malformed);
            }
        }
        if self.port == 0 || self.endpoint_id != format!("{}-relay", self.host.host) {
            return Err(ContractError::Malformed);
        }
        Ok(())
    }
}

/// Bound automatically only after service resolution and actual SSH pin checks.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrdinaryHostInfo {
    pub version: u16,
    pub route: OrdinaryRoute,
    pub installation: HostId,
    pub principal: PrincipalId,
    pub policy_epoch: Epoch,
}

impl Validate for OrdinaryHostInfo {
    fn validate(&self) -> Result<(), ContractError> {
        if self.version != ORDINARY_VERSION {
            return Err(ContractError::UnsupportedVersion);
        }
        self.route.validate()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrdinaryChallenge {
    pub host_info: OrdinaryHostInfo,
    pub nonce: String,
    pub channel_binding: String,
    pub lifetime_ms: u16,
}

impl Validate for OrdinaryChallenge {
    fn validate(&self) -> Result<(), ContractError> {
        self.host_info.validate()?;
        unhex::<32>(&self.nonce)?;
        unhex::<32>(&self.channel_binding)?;
        if self.lifetime_ms == 0 || u128::from(self.lifetime_ms) > PROOF_LIFETIME.as_millis() {
            return Err(ContractError::Malformed);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrdinaryProof {
    pub public_key: String,
    pub role: StreamRole,
    pub signature: String,
}

impl Validate for OrdinaryProof {
    fn validate(&self) -> Result<(), ContractError> {
        validate_owner_public_key(&unhex::<32>(&self.public_key)?)?;
        unhex::<64>(&self.signature)?;
        Ok(())
    }
}

impl OrdinaryChallenge {
    fn message(&self, key: &[u8; 32], role: StreamRole) -> Result<Vec<u8>, ContractError> {
        self.validate()?;
        let info = &self.host_info;
        let route = &info.route;
        let mut message = DOMAIN.to_vec();
        message.extend_from_slice(&info.version.to_be_bytes());
        for text in [
            route.tunnel_id.as_str(),
            route.cluster.as_str(),
            route.endpoint_id.as_str(),
            route.host.host.as_str(),
            route.host.host_key_fingerprint.as_str(),
            info.installation.as_str(),
            info.principal.as_str(),
        ] {
            message.extend_from_slice(&(text.len() as u32).to_be_bytes());
            message.extend_from_slice(text.as_bytes());
        }
        message.extend_from_slice(&route.port.to_be_bytes());
        message.extend_from_slice(&info.policy_epoch.get().to_be_bytes());
        message.extend_from_slice(&self.lifetime_ms.to_be_bytes());
        message.extend_from_slice(&unhex::<32>(&self.nonce)?);
        message.extend_from_slice(&unhex::<32>(&self.channel_binding)?);
        message.extend_from_slice(key);
        message.push(match role {
            StreamRole::Control => 0,
            StreamRole::Terminal => 1,
            StreamRole::Files => 2,
        });
        Ok(message)
    }
}

/// Native key snapshot, not serialized or accepted as an authorization grant.
pub struct OrdinaryDeviceKey(SigningKey);

impl OrdinaryDeviceKey {
    pub fn from_seed(seed: &[u8; 32]) -> Self {
        Self(SigningKey::from_bytes(seed))
    }

    pub fn public_key(&self) -> [u8; 32] {
        self.0.verifying_key().to_bytes()
    }

    pub fn prove(
        &self,
        connection: &ClientConnection,
        expected: &OrdinaryRoute,
        challenge: &OrdinaryChallenge,
        role: StreamRole,
    ) -> Result<OrdinaryProof, ContractError> {
        expected.validate()?;
        let binding = connection
            .owner_channel_binding()
            .map_err(|_| ContractError::Denied)?;
        let pin = connection
            .owner_pinned_fingerprint()
            .map_err(|_| ContractError::Denied)?;
        if &challenge.host_info.route != expected
            || pin != expected.host.host_key_fingerprint
            || unhex::<32>(&challenge.channel_binding)? != *binding.as_bytes()
        {
            return Err(ContractError::Denied);
        }
        self.sign(challenge, role)
    }

    fn sign(
        &self,
        challenge: &OrdinaryChallenge,
        role: StreamRole,
    ) -> Result<OrdinaryProof, ContractError> {
        let key = self.public_key();
        let signature = self.0.sign(&challenge.message(&key, role)?);
        Ok(OrdinaryProof {
            public_key: hex(&key),
            role,
            signature: hex(&signature.to_bytes()),
        })
    }
}

/// Consumed by verification; the nonce cannot be reused through this instance.
/// This primitive does not create an ordinary capability or consult cloud policy.
pub struct PendingOrdinaryChallenge {
    challenge: OrdinaryChallenge,
    expires: Instant,
}

impl PendingOrdinaryChallenge {
    pub fn new(
        host_info: OrdinaryHostInfo,
        binding: &russh::ChannelBinding,
        admission_deadline: Instant,
    ) -> Result<Self, ContractError> {
        host_info.validate()?;
        let now = Instant::now();
        let expires = admission_deadline.min(now + PROOF_LIFETIME);
        let lifetime_ms = expires.saturating_duration_since(now).as_millis().min(5000) as u16;
        if lifetime_ms == 0 {
            // This attempt is invalid; protected provider readiness may still be current.
            return Err(ContractError::Denied);
        }
        let mut nonce = [0; 32];
        getrandom::fill(&mut nonce).map_err(|_| ContractError::Denied)?;
        Ok(Self {
            challenge: OrdinaryChallenge {
                host_info,
                nonce: hex(&nonce),
                channel_binding: hex(binding.as_bytes()),
                lifetime_ms,
            },
            expires,
        })
    }

    pub fn challenge(&self) -> &OrdinaryChallenge {
        &self.challenge
    }

    pub fn verify(
        self,
        proof: &OrdinaryProof,
        current: &OrdinaryHostInfo,
        binding: &russh::ChannelBinding,
        denied_keys: &[[u8; 32]],
    ) -> Result<VerifiedOrdinaryProof, ContractError> {
        if Instant::now() >= self.expires
            || &self.challenge.host_info != current
            || unhex::<32>(&self.challenge.channel_binding)? != *binding.as_bytes()
        {
            // This attempt is invalid; protected provider readiness may still be current.
            return Err(ContractError::Denied);
        }
        proof.validate()?;
        let key = unhex::<32>(&proof.public_key)?;
        if denied_keys.contains(&key) {
            return Err(ContractError::Denied);
        }
        let signature = Signature::from_bytes(&unhex::<64>(&proof.signature)?);
        VerifyingKey::from_bytes(&key)
            .map_err(|_| ContractError::Malformed)?
            .verify_strict(&self.challenge.message(&key, proof.role)?, &signature)
            .map_err(|_| ContractError::Denied)?;
        Ok(VerifiedOrdinaryProof {
            public_key: key,
            role: proof.role,
        })
    }
}

/// Possession evidence only. No administrative or ordinary RPC authority.
pub struct VerifiedOrdinaryProof {
    public_key: [u8; 32],
    role: StreamRole,
}

/// Current protected local facts. This public value is not a capability.
#[derive(Clone)]
pub struct OrdinaryPolicy {
    pub host_info: OrdinaryHostInfo,
    pub session: crate::owner_auth::SessionPolicy,
    pub denied_keys: Vec<[u8; 32]>,
    /// Independent current host-file consent; never a SessionGrant.
    pub files: Option<crate::file_protocol::HostPolicy>,
}
impl Validate for OrdinaryPolicy {
    fn validate(&self) -> Result<(), ContractError> {
        self.host_info.validate()?;
        self.session.host.validate()?;
        self.session.capabilities.validate_permissions()?;
        if let Some(files) = &self.files {
            files
                .scope
                .validate()
                .map_err(|_| ContractError::Malformed)?;
            if files.scope.host != self.host_info.route.host.host
                || files.scope.installation != self.host_info.installation
            {
                return Err(ContractError::Denied);
            }
            if let Some(policy) = &files.filesystem {
                policy.validate().map_err(|_| ContractError::Malformed)?;
            }
        }
        if self.host_info.route.host != self.session.host
            || self.host_info.principal != self.session.principal
            || self.host_info.policy_epoch != self.session.grant_epoch
            || !self.session.remote_access
            || self.session.capabilities.issuance_epoch.is_some()
        {
            return Err(ContractError::Denied);
        }
        Ok(())
    }
}

#[cfg(feature = "relay")]
struct FileCapture {
    policy: crate::file_protocol::HostPolicy,
    context: OrdinaryHostInfo,
}

/// Minted only by the runtime after fresh provider observation and channel proof.
pub struct OrdinaryCapability {
    scope: crate::owner_auth::SessionScope,
    public_key: [u8; 32],
    role: StreamRole,
    #[cfg(feature = "relay")]
    files: Option<Box<FileCapture>>,
}
impl OrdinaryCapability {
    #[cfg(feature = "relay")]
    pub(crate) fn mint(
        proof: VerifiedOrdinaryProof,
        policy: &OrdinaryPolicy,
    ) -> Result<(Self, crate::owner_auth::ConnectionGuard), ContractError> {
        policy.validate()?;
        if policy.denied_keys.contains(&proof.public_key) {
            return Err(ContractError::Denied);
        }
        if proof.role == StreamRole::Files && policy.files.is_none() {
            return Err(ContractError::Denied);
        }
        let (scope, guard) = crate::owner_auth::SessionScope::ordinary(&policy.session)?;
        Ok((
            Self {
                scope,
                public_key: proof.public_key,
                role: proof.role,
                files: if proof.role == StreamRole::Files {
                    policy.files.as_ref().map(|files| {
                        Box::new(FileCapture {
                            policy: files.clone(),
                            context: policy.host_info.clone(),
                        })
                    })
                } else {
                    None
                },
            },
            guard,
        ))
    }
    pub(crate) fn principal(&self) -> &PrincipalId {
        self.scope.principal()
    }
    #[cfg(feature = "relay")]
    pub(crate) fn authorize_files<'a>(
        &self,
        current: &'a OrdinaryPolicy,
    ) -> Result<&'a crate::file_protocol::HostPolicy, ContractError> {
        current.validate()?;
        let captured = self.files.as_deref().ok_or(ContractError::Denied)?;
        if self.role != StreamRole::Files
            || captured.context != current.host_info
            || current.denied_keys.contains(&self.public_key)
            || current.files.as_ref() != Some(&captured.policy)
        {
            return Err(ContractError::Denied);
        }
        self.scope.authorize_connection(&current.session)?;
        current.files.as_ref().ok_or(ContractError::Denied)
    }
    pub(crate) fn authorize(
        &self,
        current: &OrdinaryPolicy,
        policy: &crate::owner_auth::SessionPolicy,
        request: &festerm_host_protocol::Envelope<festerm_host_protocol::Request>,
        epoch: Epoch,
        grant: Option<&festerm_host_protocol::SessionGrant>,
    ) -> Result<festerm_host_protocol::AdmissionFence, ContractError> {
        use festerm_host_protocol::{LogicalState, Request};
        current.validate()?;
        if self.role == StreamRole::Files
            || current.denied_keys.contains(&self.public_key)
            || matches!(request.body, Request::EnsureProfile { .. })
                && !policy.capabilities.creation_allowed
                && !grant.is_some_and(|g| matches!(g.state, LogicalState::Running(_)))
        {
            return Err(ContractError::Denied);
        }
        self.scope.authorize(policy, request, epoch, grant)
    }
}
impl VerifiedOrdinaryProof {
    pub fn public_key(&self) -> &[u8; 32] {
        &self.public_key
    }
    pub fn role(&self) -> StreamRole {
        self.role
    }
}

#[cfg(all(test, feature = "relay"))]
mod tests {
    use super::*;
    use crate::relay_host::tests::owner_pair;
    use russh::keys::{Algorithm, HashAlg, PrivateKey};

    #[test]
    fn ordinary_termination_is_separate_consent_and_rechecks_exact_current_grant() {
        use festerm_host_protocol::{
            Capabilities, Envelope, GenerationId, LogicalSessionId, LogicalState, OperationId,
            Request, SessionGrant, Target,
        };
        let epoch = Epoch::new(1).unwrap();
        let host = PinnedHost {
            host: HostId::new(),
            host_key_fingerprint: format!("SHA256:{}", "A".repeat(43)),
        };
        let principal = PrincipalId::new();
        let mut policy = OrdinaryPolicy {
            host_info: OrdinaryHostInfo {
                version: 1,
                route: OrdinaryRoute {
                    tunnel_id: "fixture".into(),
                    cluster: "usw3".into(),
                    endpoint_id: format!("{}-relay", host.host),
                    port: 47461,
                    host: host.clone(),
                },
                installation: HostId::new(),
                principal: principal.clone(),
                policy_epoch: epoch,
            },
            session: crate::owner_auth::SessionPolicy {
                host: host.clone(),
                principal: principal.clone(),
                host_epoch: epoch,
                grant_epoch: epoch,
                remote_access: true,
                capabilities: Capabilities {
                    inventory: true,
                    attach: true,
                    explicit_end: true,
                    ..Default::default()
                },
            },
            denied_keys: vec![],
            files: None,
        };
        let key = [7; 32];
        let (capability, guard) = OrdinaryCapability::mint(
            VerifiedOrdinaryProof {
                public_key: key,
                role: StreamRole::Control,
            },
            &policy,
        )
        .unwrap();
        let target = Target {
            logical: LogicalSessionId::new(),
            generation: GenerationId::new(9).unwrap(),
        };
        let request = Envelope::new(
            host.host,
            OperationId::new(),
            Request::ExplicitEnd {
                target: target.clone(),
            },
        );
        let grant = SessionGrant {
            logical: target.logical,
            owner: principal,
            remote_access: true,
            state: LogicalState::Running(target.generation),
            revision: epoch,
        };
        assert!(!policy.session.capabilities.creation_allowed);
        assert!(capability
            .authorize(&policy, &policy.session, &request, epoch, Some(&grant))
            .is_ok());
        for change in [
            "permission",
            "principal",
            "epoch",
            "remote",
            "generation",
            "unknown",
            "revoked",
            "private",
            "key",
        ] {
            let mut current = policy.clone();
            let mut grant = grant.clone();
            match change {
                "permission" => current.session.capabilities.explicit_end = false,
                "principal" => current.session.principal = PrincipalId::new(),
                "epoch" => current.session.grant_epoch = Epoch::new(2).unwrap(),
                "remote" => current.session.remote_access = false,
                "generation" => grant.state = LogicalState::Running(GenerationId::new(10).unwrap()),
                "unknown" => grant.state = LogicalState::Unknown,
                "revoked" => grant.state = LogicalState::Revoked,
                "private" => grant.remote_access = false,
                "key" => current.denied_keys.push(key),
                _ => unreachable!(),
            }
            assert!(
                capability
                    .authorize(&current, &current.session, &request, epoch, Some(&grant))
                    .is_err(),
                "{change}"
            );
        }
        policy.session.capabilities.explicit_end = false;
        policy.session.capabilities.ensure = true;
        policy.session.capabilities.creation_allowed = true;
        assert!(
            capability
                .authorize(&policy, &policy.session, &request, epoch, Some(&grant))
                .is_err(),
            "creation consent is never termination consent"
        );
        drop(guard);
    }

    #[tokio::test]
    async fn ordinary_proof_binds_real_ssh_channel_route_role_nonce_and_policy() {
        let ssh =
            PrivateKey::random(&mut russh::keys::key::safe_rng(), Algorithm::Ed25519).unwrap();
        let host = PinnedHost {
            host: HostId::new(),
            host_key_fingerprint: ssh.public_key().fingerprint(HashAlg::Sha256).to_string(),
        };
        let (client, server, ssh_task) = owner_pair(ssh, host.clone()).await;
        let route = OrdinaryRoute {
            tunnel_id: "owned".into(),
            cluster: "usw3".into(),
            endpoint_id: format!("{}-relay", host.host),
            port: 47461,
            host,
        };
        let info = OrdinaryHostInfo {
            version: 1,
            route: route.clone(),
            installation: HostId::new(),
            principal: PrincipalId::new(),
            policy_epoch: Epoch::new(1).unwrap(),
        };
        let key = OrdinaryDeviceKey::from_seed(&[7; 32]);
        let pending = || {
            PendingOrdinaryChallenge::new(
                info.clone(),
                server.owner_channel_binding().unwrap(),
                Instant::now() + PROOF_LIFETIME,
            )
            .unwrap()
        };
        assert!(matches!(
            PendingOrdinaryChallenge::new(
                info.clone(),
                server.owner_channel_binding().unwrap(),
                Instant::now(),
            ),
            Err(ContractError::Denied)
        ));
        let issued = pending();
        let proof = key
            .prove(&client, &route, issued.challenge(), StreamRole::Control)
            .unwrap();
        assert!(
            pending()
                .verify(&proof, &info, server.owner_channel_binding().unwrap(), &[])
                .is_err(),
            "nonce replay"
        );
        let verified = issued
            .verify(&proof, &info, server.owner_channel_binding().unwrap(), &[])
            .unwrap();
        assert_eq!(verified.public_key(), &key.public_key());
        assert_eq!(verified.role(), StreamRole::Control);
        for substituted in [StreamRole::Control, StreamRole::Terminal] {
            let issued = pending();
            let mut proof = key
                .prove(&client, &route, issued.challenge(), StreamRole::Files)
                .unwrap();
            proof.role = substituted;
            assert!(issued
                .verify(&proof, &info, server.owner_channel_binding().unwrap(), &[])
                .is_err());
        }
        for change in [
            "role",
            "key",
            "route",
            "epoch",
            "installation",
            "principal",
            "expired",
            "denied",
        ] {
            let mut issued = pending();
            let mut proof = key
                .prove(&client, &route, issued.challenge(), StreamRole::Terminal)
                .unwrap();
            let mut current = info.clone();
            let mut denied = Vec::new();
            match change {
                "role" => proof.role = StreamRole::Control,
                "key" => {
                    proof.public_key = hex(&OrdinaryDeviceKey::from_seed(&[9; 32]).public_key())
                }
                "route" => current.route.tunnel_id = "other".into(),
                "epoch" => current.policy_epoch = Epoch::new(2).unwrap(),
                "installation" => current.installation = HostId::new(),
                "principal" => current.principal = PrincipalId::new(),
                "expired" => issued.expires = Instant::now(),
                "denied" => denied.push(key.public_key()),
                _ => unreachable!(),
            }
            assert!(
                matches!(
                    issued.verify(
                        &proof,
                        &current,
                        server.owner_channel_binding().unwrap(),
                        &denied
                    ),
                    Err(ContractError::Denied)
                ),
                "{change}"
            );
        }
        ssh_task.abort();
    }

    #[tokio::test]
    async fn ordinary_private_key_refuses_cross_connection_changed_route_and_missing_exporter() {
        let ssh =
            PrivateKey::random(&mut russh::keys::key::safe_rng(), Algorithm::Ed25519).unwrap();
        let host = PinnedHost {
            host: HostId::new(),
            host_key_fingerprint: ssh.public_key().fingerprint(HashAlg::Sha256).to_string(),
        };
        let (mut client, server, task) = owner_pair(ssh.clone(), host.clone()).await;
        let (other_client, other_server, other_task) = owner_pair(ssh, host.clone()).await;
        let route = OrdinaryRoute {
            tunnel_id: "owned".into(),
            cluster: "usw3".into(),
            endpoint_id: format!("{}-relay", host.host),
            port: 47461,
            host,
        };
        let info = OrdinaryHostInfo {
            version: 1,
            route: route.clone(),
            installation: HostId::new(),
            principal: PrincipalId::new(),
            policy_epoch: Epoch::new(1).unwrap(),
        };
        let key = OrdinaryDeviceKey::from_seed(&[8; 32]);
        let issued = PendingOrdinaryChallenge::new(
            info.clone(),
            server.owner_channel_binding().unwrap(),
            Instant::now() + PROOF_LIFETIME,
        )
        .unwrap();
        assert!(key
            .prove(
                &other_client,
                &route,
                issued.challenge(),
                StreamRole::Control
            )
            .is_err());
        let mut other_route = route.clone();
        other_route.tunnel_id = "foreign".into();
        assert!(key
            .prove(
                &client,
                &other_route,
                issued.challenge(),
                StreamRole::Control
            )
            .is_err());
        let proof = key
            .prove(&client, &route, issued.challenge(), StreamRole::Control)
            .unwrap();
        assert!(matches!(
            issued.verify(
                &proof,
                &info,
                other_server.owner_channel_binding().unwrap(),
                &[]
            ),
            Err(ContractError::Denied)
        ));
        let issued = PendingOrdinaryChallenge::new(
            info,
            server.owner_channel_binding().unwrap(),
            Instant::now() + PROOF_LIFETIME,
        )
        .unwrap();
        client.owner_transport = None;
        assert!(key
            .prove(&client, &route, issued.challenge(), StreamRole::Control)
            .is_err());
        task.abort();
        other_task.abort();
    }
}
