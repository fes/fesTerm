//! Additional owner-only authentication, NOT Dev Tunnels provider identity.
//!
//! Reuses the repository MCP Ed25519/getrandom/zeroize primitives, with a
//! distinct domain and host/connection/epoch binding. No existing listener
//! calls this module. Enrollment, vault adapters and pinned-channel integration
//! require independent review before any HOST capability is exposed.

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use festerm_host_protocol::{
    AdmissionFence, Capabilities, ConnectionId, ContractError, Envelope, Epoch, HostId,
    LogicalState, PrincipalId, Request, SessionGrant, Validate, AUTH_DEADLINE_SECONDS, HOST_MARKER,
    HOST_VERSION,
};
use serde::{Deserialize, Serialize};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use zeroize::Zeroizing;

mod vault;
pub use vault::{ClientOwnerKey, OwnerBinding, OwnerKeyError};
#[cfg(feature = "relay")]
mod headless_store;
#[cfg(feature = "relay")]
pub use headless_store::ExplicitHeadlessFileStore;

const DOMAIN: &[u8] = b"fesTerm HOST owner authentication v1\0";
pub const MAX_OWNER_KEYS: usize = 4;

pub fn validate_owner_public_key(bytes: &[u8; 32]) -> Result<(), ContractError> {
    let key = VerifyingKey::from_bytes(bytes).map_err(|_| ContractError::Malformed)?;
    if key.is_weak() {
        return Err(ContractError::Malformed);
    }
    Ok(())
}

/// Public binding approved by existing fingerprint-first host trust. This
/// value alone does not establish that the current transport matches it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PinnedHost {
    pub host: HostId,
    pub host_key_fingerprint: String,
}

impl Validate for PinnedHost {
    fn validate(&self) -> Result<(), ContractError> {
        let value = &self.host_key_fingerprint;
        if value.len() != 50
            || !value.starts_with("SHA256:")
            || !value.as_bytes()[7..]
                .iter()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/'))
        {
            return Err(ContractError::Malformed);
        }
        Ok(())
    }
}

/// Protected host-local consent, never accepted from remote configuration.
/// The selected GitHub account is provisioning metadata, not this proof's
/// authority. Local enrollment explicitly maps approved keys to this owner.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OwnerPolicy {
    pub host: PinnedHost,
    pub principal: PrincipalId,
    pub host_epoch: Epoch,
    pub grant_epoch: Epoch,
    pub remote_access: bool,
    pub capabilities: Capabilities,
}

impl Validate for OwnerPolicy {
    fn validate(&self) -> Result<(), ContractError> {
        self.host.validate()?;
        self.capabilities.validate()
    }
}

/// No Debug/Serialize/Clone; private seed bytes belong only in the native vault
/// (SecretBytes/opaque reference), or an explicitly selected headless protected
/// credential file. Never portable metadata or automatic vault-failure fallback.
pub struct OwnerSigningKey(SigningKey);

impl OwnerSigningKey {
    pub fn generate() -> Result<Self, ContractError> {
        let mut seed = Zeroizing::new([0; 32]);
        getrandom::fill(seed.as_mut()).map_err(|_| ContractError::Denied)?;
        Ok(Self::from_seed(&seed))
    }

    /// Caller supplies a temporary borrowed secret-store view, never TOML,
    /// argv, environment, imported metadata, logs or a cloud token.
    pub fn from_seed(seed: &[u8; 32]) -> Self {
        Self(SigningKey::from_bytes(seed))
    }

    pub fn secret_seed(&self) -> Zeroizing<[u8; 32]> {
        Zeroizing::new(self.0.to_bytes())
    }

    pub fn public_key(&self) -> [u8; 32] {
        self.0.verifying_key().to_bytes()
    }

    /// Call ONLY on the same host-key-verified encrypted channel that received
    /// this challenge. The expected binding comes from approved local trust,
    /// not from the challenge or discovery metadata. Never offer a general
    /// signing oracle, cross-channel proof forwarding, or weaker fallback.
    pub fn prove(
        &self,
        challenge: &OwnerChallenge,
        expected: &PinnedHost,
    ) -> Result<OwnerProof, ContractError> {
        challenge.validate()?;
        expected.validate()?;
        if challenge.host != *expected {
            return Err(ContractError::Denied);
        }
        Ok(OwnerProof {
            signature: hex(&self.0.sign(&challenge.message()?).to_bytes()),
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnerChallenge {
    protocol: String,
    version: u16,
    host: PinnedHost,
    principal: PrincipalId,
    connection: ConnectionId,
    connection_epoch: Epoch,
    host_epoch: Epoch,
    grant_epoch: Epoch,
    nonce: String,
}

impl Validate for OwnerChallenge {
    fn validate(&self) -> Result<(), ContractError> {
        if self.protocol != HOST_MARKER || self.version != HOST_VERSION {
            return Err(ContractError::UnsupportedVersion);
        }
        self.host.validate()?;
        unhex::<32>(&self.nonce)?;
        Ok(())
    }
}

impl OwnerChallenge {
    fn message(&self) -> Result<Vec<u8>, ContractError> {
        self.validate()?;
        let mut message = DOMAIN.to_vec();
        // All variable strings are validated fixed-length encodings. Keep a
        // binary domain-separated transcript independent of JSON formatting.
        message.extend_from_slice(&self.version.to_be_bytes());
        message.extend_from_slice(self.host.host.as_str().as_bytes());
        message.extend_from_slice(self.host.host_key_fingerprint.as_bytes());
        message.extend_from_slice(self.principal.as_str().as_bytes());
        message.extend_from_slice(self.connection.as_str().as_bytes());
        message.extend_from_slice(&self.connection_epoch.get().to_be_bytes());
        message.extend_from_slice(&self.host_epoch.get().to_be_bytes());
        message.extend_from_slice(&self.grant_epoch.get().to_be_bytes());
        message.extend_from_slice(&unhex::<32>(&self.nonce)?);
        Ok(message)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnerProof {
    signature: String,
}

impl Validate for OwnerProof {
    fn validate(&self) -> Result<(), ContractError> {
        unhex::<64>(&self.signature)?;
        Ok(())
    }
}

/// Retain exactly one pending challenge per admitted encrypted connection.
/// Non-cloneable and non-deserializable; verification consumes it on *every*
/// result, including failure. Network layer must close on failed/late proof.
pub struct PendingOwnerChallenge {
    challenge: OwnerChallenge,
    expires: Instant,
}

impl PendingOwnerChallenge {
    pub fn challenge(&self) -> &OwnerChallenge {
        &self.challenge
    }

    pub fn verify(
        self,
        proof: &OwnerProof,
        current: &OwnerVerifier,
    ) -> Result<(VerifiedOwner, OwnerConnectionGuard), ContractError> {
        self.verify_at(proof, current, Instant::now())
    }

    fn verify_at(
        self,
        proof: &OwnerProof,
        current: &OwnerVerifier,
        now: Instant,
    ) -> Result<(VerifiedOwner, OwnerConnectionGuard), ContractError> {
        if now >= self.expires {
            return Err(ContractError::Stale);
        }
        if !current.policy.remote_access || !self.matches(current) {
            return Err(ContractError::Denied);
        }
        proof.validate()?;
        let signature = Signature::from_bytes(&unhex::<64>(&proof.signature)?);
        let message = self.challenge.message()?;
        if !current
            .keys
            .iter()
            .any(|key| key.verify_strict(&message, &signature).is_ok())
        {
            return Err(ContractError::Denied);
        }
        let active = Arc::new(AtomicBool::new(true));
        Ok((
            VerifiedOwner {
                challenge: self.challenge,
                active: active.clone(),
            },
            OwnerConnectionGuard { active },
        ))
    }

    fn matches(&self, current: &OwnerVerifier) -> bool {
        self.challenge.host == current.policy.host
            && self.challenge.principal == current.policy.principal
            && self.challenge.host_epoch == current.policy.host_epoch
            && self.challenge.grant_epoch == current.policy.grant_epoch
    }
}

/// Construct only from protected consent/enrollment. No account-ID assertion,
/// tunnel bearer, friendly name or guessed session ID is an enrollment.
pub struct OwnerVerifier {
    policy: OwnerPolicy,
    keys: Vec<VerifyingKey>,
    highest_issuance_epoch: Option<Epoch>,
}

impl OwnerVerifier {
    pub fn policy(&self) -> &OwnerPolicy {
        &self.policy
    }

    pub fn new(policy: OwnerPolicy, keys: &[[u8; 32]]) -> Result<Self, ContractError> {
        policy.validate()?;
        if keys.is_empty() || keys.len() > MAX_OWNER_KEYS {
            return Err(ContractError::Limit);
        }
        let mut verified = Vec::new();
        for bytes in keys {
            validate_owner_public_key(bytes)?;
            let key = VerifyingKey::from_bytes(bytes).map_err(|_| ContractError::Malformed)?;
            if verified.contains(&key) {
                return Err(ContractError::Malformed);
            }
            verified.push(key);
        }
        Ok(Self {
            highest_issuance_epoch: policy.capabilities.issuance_epoch,
            policy,
            keys: verified,
        })
    }

    /// All permission/key changes advance a protected monotonic epoch first.
    /// Journal and serialize the transition with operation admission before
    /// publishing this replacement; rollback must not restore older epochs.
    pub fn replace(&mut self, policy: OwnerPolicy, keys: &[[u8; 32]]) -> Result<(), ContractError> {
        if policy.host != self.policy.host
            || policy.principal != self.policy.principal
            || policy.host_epoch.get() < self.policy.host_epoch.get()
            || policy.grant_epoch.get() < self.policy.grant_epoch.get()
            || matches!(
                (self.highest_issuance_epoch, policy.capabilities.issuance_epoch),
                (Some(previous), Some(next)) if next.get() < previous.get()
            )
            || (policy.host_epoch == self.policy.host_epoch
                && policy.grant_epoch == self.policy.grant_epoch)
        {
            return Err(ContractError::Stale);
        }
        let mut replacement = Self::new(policy, keys)?;
        replacement.highest_issuance_epoch = replacement
            .highest_issuance_epoch
            .or(self.highest_issuance_epoch);
        *self = replacement;
        Ok(())
    }

    /// The caller must already own an aggregate client slot and must bind this
    /// pending object to the exact host-key-verified stream, not a global map
    /// searchable by client-supplied connection ID.
    pub fn challenge(&self) -> Result<PendingOwnerChallenge, ContractError> {
        if !self.policy.remote_access {
            return Err(ContractError::Denied);
        }
        let mut nonce = [0; 32];
        getrandom::fill(&mut nonce).map_err(|_| ContractError::Denied)?;
        Ok(self.challenge_with_nonce(nonce, Instant::now()))
    }

    fn challenge_with_nonce(&self, nonce: [u8; 32], now: Instant) -> PendingOwnerChallenge {
        PendingOwnerChallenge {
            challenge: OwnerChallenge {
                protocol: HOST_MARKER.into(),
                version: HOST_VERSION,
                host: self.policy.host.clone(),
                principal: self.policy.principal.clone(),
                connection: ConnectionId::new(),
                connection_epoch: Epoch::new(1).expect("nonzero epoch"),
                host_epoch: self.policy.host_epoch,
                grant_epoch: self.policy.grant_epoch,
                nonce: hex(&nonce),
            },
            expires: now + Duration::from_secs(u64::from(AUTH_DEADLINE_SECONDS)),
        }
    }
}

/// Local verified proof, never serializable/deserializable or client asserted.
/// Grants only the current enrolled owner; no creator/foreign-user ACL claim.
pub struct VerifiedOwner {
    challenge: OwnerChallenge,
    active: Arc<AtomicBool>,
}

/// Retain on the exact authenticated stream, never in an operation task.
/// Dropping it fences further admission even if pending work retains the
/// verified owner. Serialize retirement with the manager's admission boundary.
pub struct OwnerConnectionGuard {
    active: Arc<AtomicBool>,
}

impl Drop for OwnerConnectionGuard {
    fn drop(&mut self) {
        self.active.store(false, Ordering::Release);
    }
}

impl VerifiedOwner {
    pub fn principal(&self) -> &PrincipalId {
        &self.challenge.principal
    }

    /// Admission is separate from authentication and default-deny. Retain the
    /// returned capture and recheck under the manager's journal/revocation
    /// boundary before any disclosure or daemon IPC, never over network I/O.
    pub fn authorize(
        &self,
        current: &OwnerVerifier,
        request: &Envelope<Request>,
        operation_epoch: Epoch,
        session_grant: Option<&SessionGrant>,
    ) -> Result<AdmissionFence, ContractError> {
        request.validate()?;
        if !self.active.load(Ordering::Acquire)
            || !current.policy.remote_access
            || request.host != self.challenge.host.host
            || self.challenge.host != current.policy.host
            || self.challenge.principal != current.policy.principal
            || self.challenge.host_epoch != current.policy.host_epoch
            || self.challenge.grant_epoch != current.policy.grant_epoch
        {
            return Err(ContractError::Denied);
        }
        let caps = current.policy.capabilities;
        let allowed = match &request.body {
            Request::Capabilities {} => true,
            Request::List { .. } | Request::Lookup { .. } => caps.inventory,
            Request::Ensure { creation } => {
                caps.ensure
                    && (creation.lifetime == (festerm_host_protocol::Lifetime::Persistent {})
                        || caps.temporary)
            }
            Request::Create { creation } => {
                caps.create
                    && caps.creation_allowed
                    && (creation.lifetime == (festerm_host_protocol::Lifetime::Persistent {})
                        || caps.temporary)
            }
            Request::Attach { .. } => caps.attach,
            Request::Detach { .. } => caps.detach,
            Request::ExplicitEnd { .. } => caps.explicit_end,
        };
        if !allowed {
            return Err(ContractError::Denied);
        }
        // Older epochs remain the durable ledger's decision. A client cannot
        // claim an epoch the protected host has never advertised.
        if request.body.issuance_epoch_required() {
            let current_epoch = caps.issuance_epoch.ok_or(ContractError::Stale)?;
            if request
                .issuance_epoch
                .ok_or(ContractError::Malformed)?
                .get()
                > current_epoch.get()
            {
                return Err(ContractError::Stale);
            }
        }
        let (logical, target) = match &request.body {
            Request::Lookup { logical } => (Some(logical.clone()), None),
            Request::Ensure { creation } | Request::Create { creation } => {
                (Some(creation.logical.clone()), None)
            }
            Request::Attach { target, .. }
            | Request::Detach { target, .. }
            | Request::ExplicitEnd { target } => {
                (Some(target.logical.clone()), Some(target.clone()))
            }
            _ => (None, None),
        };
        let (logical_state, logical_revision) = if let Some(logical) = &logical {
            let grant = session_grant.ok_or(ContractError::Denied)?;
            if grant.owner != self.challenge.principal
                || grant.logical != *logical
                || !grant.remote_access
                || matches!(grant.state, LogicalState::Revoked)
            {
                return Err(ContractError::Denied);
            }
            if matches!(grant.state, LogicalState::Unknown) {
                return Err(ContractError::Stale);
            }
            if matches!(request.body, Request::Ensure { .. })
                && grant.state == LogicalState::Absent
                && !caps.creation_allowed
            {
                return Err(ContractError::Denied);
            }
            if let Some(target) = &target {
                if grant.state != LogicalState::Running(target.generation) {
                    return Err(ContractError::Stale);
                }
            }
            if matches!(request.body, Request::Create { .. }) && grant.state != LogicalState::Absent
            {
                return Err(ContractError::Busy);
            }
            (Some(grant.state), Some(grant.revision))
        } else {
            (None, None)
        };
        Ok(AdmissionFence {
            host: request.host.clone(),
            principal: self.challenge.principal.clone(),
            connection: self.challenge.connection.clone(),
            connection_epoch: self.challenge.connection_epoch,
            host_epoch: self.challenge.host_epoch,
            grant_epoch: self.challenge.grant_epoch,
            operation_epoch,
            operation: request.operation.clone(),
            intent: (&request.body).into(),
            issuance_epoch: request.issuance_epoch,
            logical,
            logical_state,
            logical_revision,
            target,
        })
    }
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(DIGITS[usize::from(byte >> 4)]));
        output.push(char::from(DIGITS[usize::from(byte & 15)]));
    }
    output
}

fn unhex<const N: usize>(value: &str) -> Result<[u8; N], ContractError> {
    if value.len() != N * 2
        || !value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(ContractError::Malformed);
    }
    let mut output = [0; N];
    for (index, pair) in value.as_bytes().as_chunks::<2>().0.iter().enumerate() {
        let digit = |byte: u8| {
            if byte <= b'9' {
                byte - b'0'
            } else {
                byte - b'a' + 10
            }
        };
        output[index] = digit(pair[0]) << 4 | digit(pair[1]);
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use festerm_host_protocol::{
        decode, encode, AttachmentId, Creation, GenerationId, Lifetime, OperationId, Recipe, Target,
    };
    use serde_json::json;
    use std::str::FromStr;

    fn id<T: FromStr>(n: u32) -> T
    where
        T::Err: std::fmt::Debug,
    {
        format!("{n:08x}-0000-4000-8000-000000000001")
            .parse()
            .unwrap()
    }

    fn policy() -> OwnerPolicy {
        OwnerPolicy {
            host: PinnedHost {
                host: id(1),
                host_key_fingerprint: format!("SHA256:{}", "a".repeat(43)),
            },
            principal: id(7),
            host_epoch: Epoch::new(1).unwrap(),
            grant_epoch: Epoch::new(2).unwrap(),
            remote_access: true,
            capabilities: Capabilities::default(),
        }
    }

    fn request(body: Request) -> Envelope<Request> {
        let mut envelope = Envelope::new(id(1), id::<OperationId>(8), body);
        if envelope.body.issuance_epoch_required() {
            envelope.issuance_epoch = Some(Epoch::new(1).unwrap());
        }
        envelope
    }

    fn signed(
        verifier: &OwnerVerifier,
        key: &OwnerSigningKey,
    ) -> (PendingOwnerChallenge, OwnerProof) {
        let mut pending = verifier.challenge_with_nonce([9; 32], Instant::now());
        pending.challenge.connection = id(6);
        let proof = key
            .prove(pending.challenge(), &verifier.policy.host)
            .unwrap();
        (pending, proof)
    }

    #[test]
    fn host_owner_creation_epoch_refuses_future_without_refreshing_older_requests() {
        let key = OwnerSigningKey::from_seed(&[7; 32]);
        let mut allowed = policy();
        allowed.capabilities = Capabilities {
            issuance_epoch: Some(Epoch::new(2).unwrap()),
            ensure: true,
            create: true,
            attach: true,
            creation_allowed: true,
            ..Capabilities::default()
        };
        let mut verifier = OwnerVerifier::new(allowed, &[key.public_key()]).unwrap();
        let (pending, proof) = signed(&verifier, &key);
        let (owner, _connection) = pending.verify(&proof, &verifier).unwrap();
        let grant = SessionGrant {
            logical: id(2),
            owner: id(7),
            remote_access: true,
            state: LogicalState::Absent,
            revision: Epoch::new(1).unwrap(),
        };
        let creation = Creation {
            logical: id(2),
            attachment: id(4),
            name: None,
            recipe: Recipe::default(),
            lifetime: Lifetime::Persistent {},
        };
        for body in [
            Request::Ensure {
                creation: creation.clone(),
            },
            Request::Create {
                creation: creation.clone(),
            },
        ] {
            let mut request = request(body);
            let original = request.issuance_epoch;
            let fence = owner
                .authorize(&verifier, &request, Epoch::new(1).unwrap(), Some(&grant))
                .unwrap();
            assert_eq!(fence.issuance_epoch, original);
            request.issuance_epoch = Some(Epoch::new(2).unwrap());
            owner
                .authorize(&verifier, &request, Epoch::new(1).unwrap(), Some(&grant))
                .unwrap();
            request.issuance_epoch = Some(Epoch::new(3).unwrap());
            assert!(matches!(
                owner.authorize(&verifier, &request, Epoch::new(1).unwrap(), Some(&grant)),
                Err(ContractError::Stale)
            ));
        }
        verifier.policy.capabilities.issuance_epoch = None;
        assert!(matches!(
            owner.authorize(
                &verifier,
                &request(Request::Ensure { creation }),
                Epoch::new(1).unwrap(),
                Some(&grant)
            ),
            Err(ContractError::Stale)
        ));
    }

    #[test]
    fn host_owner_policy_replacement_retains_issuance_high_water_mark() {
        let key = OwnerSigningKey::from_seed(&[7; 32]);
        let mut allowed = policy();
        allowed.capabilities = Capabilities {
            issuance_epoch: Some(Epoch::new(5).unwrap()),
            ensure: true,
            attach: true,
            creation_allowed: true,
            ..Capabilities::default()
        };
        let mut verifier = OwnerVerifier::new(allowed.clone(), &[key.public_key()]).unwrap();
        allowed.grant_epoch = allowed.grant_epoch.next().unwrap();
        allowed.capabilities.issuance_epoch = Some(Epoch::new(3).unwrap());
        assert_eq!(
            verifier.replace(allowed.clone(), &[key.public_key()]),
            Err(ContractError::Stale)
        );
        assert_eq!(
            verifier.policy.capabilities.issuance_epoch,
            Some(Epoch::new(5).unwrap())
        );

        let mut disabled = allowed.clone();
        disabled.capabilities = Capabilities::default();
        verifier.replace(disabled, &[key.public_key()]).unwrap();
        allowed.grant_epoch = allowed.grant_epoch.next().unwrap();
        allowed.capabilities.issuance_epoch = Some(Epoch::new(4).unwrap());
        assert_eq!(
            verifier.replace(allowed.clone(), &[key.public_key()]),
            Err(ContractError::Stale)
        );
        allowed.capabilities.issuance_epoch = Some(Epoch::new(6).unwrap());
        verifier.replace(allowed, &[key.public_key()]).unwrap();
    }

    #[test]
    fn host_owner_mutating_policy_requires_advertised_issuance_epoch() {
        let key = OwnerSigningKey::from_seed(&[7; 32]);
        for create in [false, true] {
            let mut invalid = policy();
            invalid.capabilities = Capabilities {
                ensure: !create,
                create,
                attach: true,
                ..Capabilities::default()
            };
            assert_eq!(invalid.validate(), Err(ContractError::Malformed));
            assert!(matches!(
                OwnerVerifier::new(invalid.clone(), &[key.public_key()]),
                Err(ContractError::Malformed)
            ));
            let mut verifier = OwnerVerifier::new(policy(), &[key.public_key()]).unwrap();
            invalid.grant_epoch = invalid.grant_epoch.next().unwrap();
            assert_eq!(
                verifier.replace(invalid.clone(), &[key.public_key()]),
                Err(ContractError::Malformed)
            );
            invalid.capabilities.issuance_epoch = Some(Epoch::new(1).unwrap());
            invalid.validate().unwrap();
            OwnerVerifier::new(invalid, &[key.public_key()]).unwrap();
        }
    }

    #[test]
    fn host_owner_authentication_matches_domain_bound_signature_fixture() {
        let key = OwnerSigningKey::from_seed(&[7; 32]);
        let verifier = OwnerVerifier::new(policy(), &[key.public_key()]).unwrap();
        let (pending, proof) = signed(&verifier, &key);
        let fixture: OwnerProof =
            decode(include_bytes!("../tests/fixtures/host-owner-proof-v1.json")).unwrap();
        assert_eq!(proof, fixture);
        let wire = encode(pending.challenge()).unwrap();
        assert_eq!(
            decode::<OwnerChallenge>(&wire).unwrap(),
            *pending.challenge()
        );
        let proof = decode::<OwnerProof>(&encode(&proof).unwrap()).unwrap();
        let (owner, connection) = pending.verify(&proof, &verifier).unwrap();
        assert_eq!(owner.principal(), &verifier.policy.principal);
        // Authentication does not silently grant even read-only inventory.
        assert!(owner
            .authorize(
                &verifier,
                &request(Request::List { cursor: None }),
                Epoch::new(1).unwrap(),
                None
            )
            .is_err());
        assert!(owner
            .authorize(
                &verifier,
                &request(Request::Capabilities {}),
                Epoch::new(1).unwrap(),
                None
            )
            .is_ok());
        drop(connection);
        assert!(owner
            .authorize(
                &verifier,
                &request(Request::Capabilities {}),
                Epoch::new(1).unwrap(),
                None
            )
            .is_err());
    }

    #[test]
    fn host_owner_proof_rejects_foreign_key_tampering_and_cross_connection_replay() {
        let key = OwnerSigningKey::from_seed(&[7; 32]);
        let other = OwnerSigningKey::from_seed(&[8; 32]);
        let verifier = OwnerVerifier::new(policy(), &[key.public_key()]).unwrap();
        let (pending, _) = signed(&verifier, &key);
        let forged = other
            .prove(pending.challenge(), &verifier.policy.host)
            .unwrap();
        assert!(matches!(
            pending.verify(&forged, &verifier),
            Err(ContractError::Denied)
        ));
        for field in 0..7 {
            let (pending, _) = signed(&verifier, &key);
            let mut changed = pending.challenge().clone();
            match field {
                0 => changed.connection = id(9),
                1 => changed.nonce = hex(&[8; 32]),
                2 => changed.principal = id(9),
                3 => changed.host_epoch = changed.host_epoch.next().unwrap(),
                4 => changed.grant_epoch = changed.grant_epoch.next().unwrap(),
                5 => changed.connection_epoch = changed.connection_epoch.next().unwrap(),
                _ => changed.host.host_key_fingerprint = format!("SHA256:{}", "b".repeat(43)),
            }
            let proof = key.prove(&changed, &changed.host).unwrap();
            assert!(pending.verify(&proof, &verifier).is_err());
        }
        let (first, proof) = signed(&verifier, &key);
        let second = verifier.challenge().unwrap();
        assert!(second.verify(&proof, &verifier).is_err());
        assert!(first.verify(&proof, &verifier).is_ok());
    }

    #[test]
    fn host_owner_challenge_expiry_revocation_and_epoch_rollback_fail_closed() {
        let key = OwnerSigningKey::from_seed(&[7; 32]);
        let mut verifier = OwnerVerifier::new(policy(), &[key.public_key()]).unwrap();
        let (pending, proof) = signed(&verifier, &key);
        let expired = pending.expires;
        assert!(matches!(
            pending.verify_at(&proof, &verifier, expired),
            Err(ContractError::Stale)
        ));
        let (pending, proof) = signed(&verifier, &key);
        let (prior, prior_proof) = signed(&verifier, &key);
        let (owner, _connection) = prior.verify(&prior_proof, &verifier).unwrap();
        let mut disabled = verifier.policy.clone();
        disabled.remote_access = false;
        disabled.grant_epoch = disabled.grant_epoch.next().unwrap();
        verifier.replace(disabled, &[key.public_key()]).unwrap();
        assert!(pending.verify(&proof, &verifier).is_err());
        assert!(verifier.challenge().is_err());
        assert!(owner
            .authorize(
                &verifier,
                &request(Request::Capabilities {}),
                Epoch::new(1).unwrap(),
                None
            )
            .is_err());
        assert!(verifier.replace(policy(), &[key.public_key()]).is_err());
    }

    #[test]
    fn host_owner_admission_requires_current_shared_exact_generation_and_absence() {
        let key = OwnerSigningKey::from_seed(&[7; 32]);
        let mut allowed = policy();
        allowed.capabilities = Capabilities {
            issuance_epoch: Some(Epoch::new(1).unwrap()),
            inventory: true,
            attach: true,
            ensure: true,
            create: true,
            creation_allowed: true,
            ..Capabilities::default()
        };
        let verifier = OwnerVerifier::new(allowed, &[key.public_key()]).unwrap();
        let (pending, proof) = signed(&verifier, &key);
        let (owner, _connection) = pending.verify(&proof, &verifier).unwrap();
        let target = Target {
            logical: id(2),
            generation: GenerationId::new(42).unwrap(),
        };
        let attach = request(Request::Attach {
            target: target.clone(),
            attachment: id::<AttachmentId>(4),
            takeover: false,
        });
        let mut grant = SessionGrant {
            logical: id(2),
            owner: id(7),
            remote_access: true,
            state: LogicalState::Running(target.generation),
            revision: Epoch::new(1).unwrap(),
        };
        let epoch = Epoch::new(1).unwrap();
        let admitted = owner
            .authorize(&verifier, &attach, epoch, Some(&grant))
            .unwrap();
        admitted.revalidate(&admitted).unwrap();
        assert!(owner.authorize(&verifier, &attach, epoch, None).is_err());
        for field in 0..6 {
            let mut invalid = grant.clone();
            match field {
                0 => invalid.owner = id(9),
                1 => invalid.remote_access = false,
                2 => invalid.state = LogicalState::Revoked,
                3 => invalid.state = LogicalState::Unknown,
                4 => invalid.state = LogicalState::Absent,
                _ => invalid.state = LogicalState::Running(GenerationId::new(43).unwrap()),
            }
            assert!(owner
                .authorize(&verifier, &attach, epoch, Some(&invalid))
                .is_err());
        }
        let creation = Creation {
            logical: id(2),
            attachment: id(4),
            name: None,
            recipe: Recipe::default(),
            lifetime: Lifetime::Persistent {},
        };
        let create = request(Request::Create {
            creation: creation.clone(),
        });
        assert!(owner
            .authorize(&verifier, &create, epoch, Some(&grant))
            .is_err());
        grant.state = LogicalState::Absent;
        assert!(owner
            .authorize(&verifier, &create, epoch, Some(&grant))
            .is_ok());
        grant.state = LogicalState::Unknown;
        assert!(owner
            .authorize(
                &verifier,
                &request(Request::Ensure { creation }),
                epoch,
                Some(&grant)
            )
            .is_err());
        grant.state = LogicalState::Absent;
        let temporary = Creation {
            logical: id(2),
            attachment: id(4),
            name: None,
            recipe: Recipe::default(),
            lifetime: Lifetime::Temporary {
                reconnect_grace_seconds: 60,
            },
        };
        assert!(owner
            .authorize(
                &verifier,
                &request(Request::Create {
                    creation: temporary
                }),
                epoch,
                Some(&grant)
            )
            .is_err());
        let mut resume_only = verifier.policy.clone();
        resume_only.capabilities.creation_allowed = false;
        let resume_verifier = OwnerVerifier::new(resume_only, &[key.public_key()]).unwrap();
        let (pending, proof) = signed(&resume_verifier, &key);
        let (resume_owner, _resume_connection) = pending.verify(&proof, &resume_verifier).unwrap();
        let ensure = request(Request::Ensure {
            creation: Creation {
                logical: id(2),
                attachment: id(4),
                name: None,
                recipe: Recipe::default(),
                lifetime: Lifetime::Persistent {},
            },
        });
        grant.state = LogicalState::Running(target.generation);
        assert!(resume_owner
            .authorize(&resume_verifier, &ensure, epoch, Some(&grant))
            .is_ok());
        grant.state = LogicalState::Absent;
        assert!(resume_owner
            .authorize(&resume_verifier, &ensure, epoch, Some(&grant))
            .is_err());
        assert!(resume_owner
            .authorize(&resume_verifier, &create, epoch, Some(&grant))
            .is_err());
    }

    #[test]
    fn host_owner_authentication_rejects_changed_trust_assertions_and_invalid_keys() {
        let key = OwnerSigningKey::from_seed(&[7; 32]);
        let verifier = OwnerVerifier::new(policy(), &[key.public_key()]).unwrap();
        let (pending, proof) = signed(&verifier, &key);
        let mut wrong_host = verifier.policy.host.clone();
        wrong_host.host = id(9);
        assert!(key.prove(pending.challenge(), &wrong_host).is_err());
        let mut asserted = serde_json::to_value(proof).unwrap();
        asserted["account_id"] = json!(123);
        assert!(decode::<OwnerProof>(&serde_json::to_vec(&asserted).unwrap()).is_err());
        for signature in ["0".repeat(127), "g".repeat(128), "AA".repeat(64)] {
            assert!(decode::<OwnerProof>(
                &serde_json::to_vec(&json!({ "signature": signature })).unwrap()
            )
            .is_err());
        }
        assert!(OwnerVerifier::new(policy(), &[]).is_err());
        assert!(OwnerVerifier::new(policy(), &[[0; 32]]).is_err());
        assert!(OwnerVerifier::new(policy(), &[key.public_key(), key.public_key()]).is_err());
        let mut disabled = policy();
        disabled.remote_access = false;
        assert!(OwnerVerifier::new(disabled, &[key.public_key()])
            .unwrap()
            .challenge()
            .is_err());
    }

    #[test]
    fn host_owner_fence_reauthorization_rejects_logical_state_and_revision_changes() {
        let key = OwnerSigningKey::from_seed(&[7; 32]);
        let mut allowed = policy();
        allowed.capabilities = Capabilities {
            issuance_epoch: Some(Epoch::new(1).unwrap()),
            inventory: true,
            attach: true,
            ensure: true,
            create: true,
            creation_allowed: true,
            ..Capabilities::default()
        };
        let verifier = OwnerVerifier::new(allowed, &[key.public_key()]).unwrap();
        let (pending, proof) = signed(&verifier, &key);
        let (owner, _connection) = pending.verify(&proof, &verifier).unwrap();
        let creation = Creation {
            logical: id(2),
            attachment: id(4),
            name: None,
            recipe: Recipe::default(),
            lifetime: Lifetime::Persistent {},
        };
        let epoch = Epoch::new(1).unwrap();
        for body in [
            Request::Ensure {
                creation: creation.clone(),
            },
            Request::Lookup { logical: id(2) },
        ] {
            let request = request(body);
            let mut grant = SessionGrant {
                logical: id(2),
                owner: id(7),
                remote_access: true,
                state: LogicalState::Absent,
                revision: epoch,
            };
            let absent = owner
                .authorize(&verifier, &request, epoch, Some(&grant))
                .unwrap();
            grant.state = LogicalState::Running(GenerationId::new(42).unwrap());
            let running = owner
                .authorize(&verifier, &request, epoch, Some(&grant))
                .unwrap();
            assert_eq!(absent.revalidate(&running), Err(ContractError::Stale));
            grant.state = LogicalState::Running(GenerationId::new(43).unwrap());
            let replacement = owner
                .authorize(&verifier, &request, epoch, Some(&grant))
                .unwrap();
            assert_eq!(running.revalidate(&replacement), Err(ContractError::Stale));
            grant.state = LogicalState::Absent;
            grant.revision = grant.revision.next().unwrap();
            let returned_absent = owner
                .authorize(&verifier, &request, epoch, Some(&grant))
                .unwrap();
            assert_eq!(
                absent.revalidate(&returned_absent),
                Err(ContractError::Stale)
            );
        }
        let create = request(Request::Create { creation });
        let mut grant = SessionGrant {
            logical: id(2),
            owner: id(7),
            remote_access: true,
            state: LogicalState::Absent,
            revision: epoch,
        };
        owner
            .authorize(&verifier, &create, epoch, Some(&grant))
            .unwrap();
        grant.state = LogicalState::Running(GenerationId::new(42).unwrap());
        assert!(matches!(
            owner.authorize(&verifier, &create, epoch, Some(&grant)),
            Err(ContractError::Busy)
        ));
    }

    #[test]
    fn host_owner_fence_rejects_changed_intent_for_the_same_operation() {
        let key = OwnerSigningKey::from_seed(&[7; 32]);
        let mut allowed = policy();
        allowed.capabilities = Capabilities {
            issuance_epoch: Some(Epoch::new(1).unwrap()),
            attach: true,
            ensure: true,
            creation_allowed: true,
            ..Capabilities::default()
        };
        let verifier = OwnerVerifier::new(allowed, &[key.public_key()]).unwrap();
        let (pending, proof) = signed(&verifier, &key);
        let (owner, _connection) = pending.verify(&proof, &verifier).unwrap();
        let target = Target {
            logical: id(2),
            generation: GenerationId::new(42).unwrap(),
        };
        let mut grant = SessionGrant {
            logical: id(2),
            owner: id(7),
            remote_access: true,
            state: LogicalState::Running(target.generation),
            revision: Epoch::new(1).unwrap(),
        };
        let epoch = Epoch::new(1).unwrap();
        let attach = |takeover| {
            request(Request::Attach {
                target: target.clone(),
                attachment: id(4),
                takeover,
            })
        };
        let original = owner
            .authorize(&verifier, &attach(false), epoch, Some(&grant))
            .unwrap();
        let changed = owner
            .authorize(&verifier, &attach(true), epoch, Some(&grant))
            .unwrap();
        assert_eq!(original.revalidate(&changed), Err(ContractError::Stale));

        grant.state = LogicalState::Absent;
        let creation = Creation {
            logical: id(2),
            attachment: id(4),
            name: None,
            recipe: Recipe::default(),
            lifetime: Lifetime::Persistent {},
        };
        let original = owner
            .authorize(
                &verifier,
                &request(Request::Ensure {
                    creation: creation.clone(),
                }),
                epoch,
                Some(&grant),
            )
            .unwrap();
        let mut changed_creation = creation;
        changed_creation.recipe.shell = Some("/different-shell".into());
        let changed = owner
            .authorize(
                &verifier,
                &request(Request::Ensure {
                    creation: changed_creation,
                }),
                epoch,
                Some(&grant),
            )
            .unwrap();
        assert_eq!(original.revalidate(&changed), Err(ContractError::Stale));
        assert!(!format!("{changed:?}").contains("/different-shell"));
    }
}
