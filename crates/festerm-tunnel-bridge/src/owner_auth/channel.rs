//! Owner proofs on the actual pinned inner SSH channel, never the outer relay.
use super::*;
use festerm_secret_store::{native_store, SecretStore};
use russh::ChannelBinding;

const CHANNEL_DOMAIN: &[u8] = b"fesTerm HOST owner client-to-host SSH channel proof v1\0";

// A native snapshot authorizes exactly one proof attempt, not a reusable signer.
// Its authority expires after relay + authentication budgets, even if queued.
#[cfg(feature = "host-routing")]
pub(crate) struct NativeOwnerAttempt {
    seed: Zeroizing<[u8; 32]>,
    public_key: [u8; 32],
    approved: OwnerBinding,
    guard: NativeProofGuard,
}

#[cfg(feature = "host-routing")]
#[derive(Clone)]
pub(crate) struct NativeProofGuard {
    expires: Instant,
    cancel: tokio_util::sync::CancellationToken,
}

#[cfg(feature = "host-routing")]
impl NativeProofGuard {
    pub(crate) fn check(&self) -> Result<(), OwnerKeyError> {
        if self.cancel.is_cancelled() || Instant::now() >= self.expires {
            Err(OwnerKeyError::ProofDenied)
        } else {
            Ok(())
        }
    }
}

#[cfg(feature = "host-routing")]
impl NativeOwnerAttempt {
    pub(super) fn from_native_snapshot(
        seed: Zeroizing<[u8; 32]>,
        public_key: [u8; 32],
        approved: OwnerBinding,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Self {
        Self {
            seed,
            public_key,
            approved,
            guard: NativeProofGuard {
                expires: Instant::now() + Duration::from_secs(30),
                cancel,
            },
        }
    }

    pub(crate) fn publication_guard(&self) -> NativeProofGuard {
        self.guard.clone()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChannelOwnerChallenge {
    challenge: OwnerChallenge,
    ssh_channel: String,
}

impl Validate for ChannelOwnerChallenge {
    fn validate(&self) -> Result<(), ContractError> {
        self.challenge.validate()?;
        unhex::<32>(&self.ssh_channel)?;
        Ok(())
    }
}

impl ChannelOwnerChallenge {
    fn message(&self) -> Result<Vec<u8>, ContractError> {
        self.validate()?;
        let mut message = CHANNEL_DOMAIN.to_vec();
        message.extend_from_slice(&unhex::<32>(&self.ssh_channel)?);
        message.extend_from_slice(&self.challenge.message()?);
        Ok(message)
    }

    fn matches(&self, binding: &ChannelBinding) -> bool {
        self.ssh_channel == hex(binding.as_bytes())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ChannelOwnerProof(OwnerProof);

impl Validate for ChannelOwnerProof {
    fn validate(&self) -> Result<(), ContractError> {
        self.0.validate()
    }
}

#[cfg(feature = "relay")]
pub struct PendingChannelOwnerChallenge {
    pending: PendingOwnerChallenge,
    challenge: ChannelOwnerChallenge,
}

#[cfg(feature = "relay")]
impl PendingChannelOwnerChallenge {
    pub fn challenge(&self) -> &ChannelOwnerChallenge {
        &self.challenge
    }

    #[cfg(all(test, feature = "relay"))]
    pub(crate) fn expire_fixture(&mut self) {
        self.pending.expires = Instant::now() - Duration::from_secs(1);
    }
}

/// Authority and the exact SSH stream that proved it, transferred together.
#[cfg(feature = "relay")]
pub struct OwnerAuthenticatedStream {
    stream: crate::relay::HostStream,
    owner: VerifiedOwner,
    guard: OwnerConnectionGuard,
}

/// Failed authentication retains only the unauthenticated stream and reason.
#[cfg(feature = "relay")]
pub struct OwnerAuthenticationFailure {
    stream: Box<crate::relay::HostStream>,
    reason: ContractError,
}

#[cfg(feature = "relay")]
impl std::fmt::Debug for OwnerAuthenticationFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OwnerAuthenticationFailure")
            .field("reason", &self.reason)
            .finish_non_exhaustive()
    }
}

#[cfg(feature = "relay")]
impl OwnerAuthenticationFailure {
    /// Send only a bounded typed denial under the endpoint's deadline, then
    /// close. This stream has no owner authority; the challenge is consumed.
    pub fn into_parts(self) -> (crate::relay::HostStream, ContractError) {
        (*self.stream, self.reason)
    }
}

#[cfg(feature = "relay")]
impl OwnerAuthenticatedStream {
    #[cfg(feature = "host-routing")]
    pub(crate) fn stream_mut(&mut self) -> &mut crate::relay::HostStream {
        &mut self.stream
    }

    /// Dispatch only this stream with these authority values. Retain the guard
    /// until stream shutdown and freshly authorize each operation.
    pub fn into_parts(
        self,
    ) -> (
        crate::relay::HostStream,
        VerifiedOwner,
        OwnerConnectionGuard,
    ) {
        (self.stream, self.owner, self.guard)
    }
}

impl crate::relay::ClientConnection {
    /// Generates no key and never substitutes account/provider credentials.
    /// The vault binding must equal the current explicitly approved local
    /// installation/host/account/owner and this connection's actual pinned key.
    pub fn prove_owner(
        &self,
        key: &ClientOwnerKey,
        approved: &OwnerBinding,
        challenge: &ChannelOwnerChallenge,
    ) -> Result<ChannelOwnerProof, OwnerKeyError> {
        if !key.uses_native_storage() {
            return Err(OwnerKeyError::InvalidBinding);
        }
        self.prove_owner_using(native_store()?.as_ref(), key, approved, challenge)
    }

    /// Explicit headless file credentials, never selected on native failure.
    #[cfg(feature = "relay")]
    pub fn prove_owner_headless(
        &self,
        selected: &ExplicitHeadlessFileStore,
        key: &ClientOwnerKey,
        approved: &OwnerBinding,
        challenge: &ChannelOwnerChallenge,
    ) -> Result<ChannelOwnerProof, OwnerKeyError> {
        if !key.uses_headless_storage(selected.installation())
            || selected.installation() != approved.installation
        {
            return Err(OwnerKeyError::InvalidBinding);
        }
        self.prove_owner_using(selected, key, approved, challenge)
    }

    fn prove_owner_using(
        &self,
        vault: &dyn SecretStore,
        key: &ClientOwnerKey,
        approved: &OwnerBinding,
        challenge: &ChannelOwnerChallenge,
    ) -> Result<ChannelOwnerProof, OwnerKeyError> {
        let message = self.owner_proof_message(approved, challenge)?;
        ClientOwnerKey::with_key(vault, &key.reference(), approved, |signing| {
            if signing.public_key() != key.public_key() {
                return Err(OwnerKeyError::ProofDenied);
            }
            Ok(ChannelOwnerProof(OwnerProof {
                signature: hex(&signing.0.sign(&message).to_bytes()),
            }))
        })
    }

    fn owner_proof_message(
        &self,
        approved: &OwnerBinding,
        challenge: &ChannelOwnerChallenge,
    ) -> Result<Vec<u8>, OwnerKeyError> {
        challenge
            .validate()
            .map_err(|_| OwnerKeyError::ProofDenied)?;
        let binding = self.owner_channel_binding()?;
        if !challenge.matches(binding)
            || challenge.challenge.host != approved.host
            || challenge.challenge.principal != approved.principal
            || self.owner_pinned_fingerprint()? != approved.host.host_key_fingerprint
        {
            return Err(OwnerKeyError::ProofDenied);
        }
        challenge.message().map_err(|_| OwnerKeyError::ProofDenied)
    }

    #[cfg(feature = "host-routing")]
    pub(crate) fn prove_native_attempt(
        &self,
        attempt: NativeOwnerAttempt,
        challenge: &ChannelOwnerChallenge,
    ) -> Result<ChannelOwnerProof, OwnerKeyError> {
        attempt.guard.check()?;
        let message = self.owner_proof_message(&attempt.approved, challenge)?;
        let signing = OwnerSigningKey::from_seed(&attempt.seed);
        if signing.public_key() != attempt.public_key {
            return Err(OwnerKeyError::ProofDenied);
        }
        // No await or native call separates this check from signing. The seed
        // and temporary signing key are destroyed after this single attempt.
        attempt.guard.check()?;
        let proof = ChannelOwnerProof(OwnerProof {
            signature: hex(&signing.0.sign(&message).to_bytes()),
        });
        attempt.guard.check()?;
        Ok(proof)
    }

    #[cfg(all(test, feature = "relay"))]
    pub(crate) fn prove_owner_fixture(
        &self,
        vault: &dyn SecretStore,
        key: &ClientOwnerKey,
        approved: &OwnerBinding,
        challenge: &ChannelOwnerChallenge,
    ) -> Result<ChannelOwnerProof, OwnerKeyError> {
        self.prove_owner_using(vault, key, approved, challenge)
    }

    #[cfg(all(test, feature = "relay"))]
    pub(crate) fn prove_unbound_fixture(
        &self,
        vault: &dyn SecretStore,
        key: &ClientOwnerKey,
        approved: &OwnerBinding,
        challenge: &ChannelOwnerChallenge,
    ) -> ChannelOwnerProof {
        ClientOwnerKey::with_key(vault, &key.reference(), approved, |key| {
            Ok(ChannelOwnerProof(
                key.prove(&challenge.challenge, &approved.host)
                    .map_err(|_| OwnerKeyError::ProofDenied)?,
            ))
        })
        .unwrap()
    }
}

#[cfg(feature = "relay")]
impl crate::relay::HostStream {
    /// Reserve an aggregate client slot before this call; retain this pending
    /// value on this exact stream, not in a map keyed by client assertions.
    pub fn challenge_owner(
        &self,
        current: &OwnerVerifier,
    ) -> Result<PendingChannelOwnerChallenge, ContractError> {
        let binding = self
            .owner_channel_binding()
            .map_err(|_| ContractError::Denied)?;
        if current.policy.host != *self.owner_host() {
            return Err(ContractError::Denied);
        }
        let pending = current.challenge()?;
        let challenge = ChannelOwnerChallenge {
            challenge: pending.challenge.clone(),
            ssh_channel: hex(binding.as_bytes()),
        };
        Ok(PendingChannelOwnerChallenge { pending, challenge })
    }

    /// Every outcome consumes the challenge. Keep the returned guard on this
    /// stream, and freshly authorize under a current protected policy guard.
    pub(crate) fn verify_owner(
        &self,
        pending: PendingChannelOwnerChallenge,
        proof: &ChannelOwnerProof,
        current: &OwnerVerifier,
    ) -> Result<(VerifiedOwner, OwnerConnectionGuard), ContractError> {
        let binding = self
            .owner_channel_binding()
            .map_err(|_| ContractError::Denied)?;
        if !pending.challenge.matches(binding) || current.policy.host != *self.owner_host() {
            return Err(ContractError::Denied);
        }
        let message = pending.challenge.message()?;
        pending
            .pending
            .verify_message(&proof.0, current, Instant::now(), &message)
    }

    /// Failure returns the same unauthenticated stream for a bounded denial
    /// followed by close; every outcome consumes the pending challenge.
    pub fn authenticate_owner(
        self,
        pending: PendingChannelOwnerChallenge,
        proof: &ChannelOwnerProof,
        current: &OwnerVerifier,
    ) -> Result<OwnerAuthenticatedStream, OwnerAuthenticationFailure> {
        match self.verify_owner(pending, proof, current) {
            Ok((owner, guard)) => Ok(OwnerAuthenticatedStream {
                stream: self,
                owner,
                guard,
            }),
            Err(reason) => Err(OwnerAuthenticationFailure {
                stream: Box::new(self),
                reason,
            }),
        }
    }
}

#[cfg(all(test, feature = "relay"))]
pub(crate) fn fixture_key(vault: &dyn SecretStore, approved: &OwnerBinding) -> ClientOwnerKey {
    ClientOwnerKey::generate(vault, approved).unwrap()
}

#[cfg(all(test, feature = "relay", feature = "host-routing"))]
#[path = "native_attempt_tests.rs"]
mod native_attempt_tests;
