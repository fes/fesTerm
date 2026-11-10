//! Protected native enrollment adapter for the unexposed routing core.
//! `run` stays unavailable until actual owner-channel binding and terminal
//! routing have independently passed their exposure gates.
use festerm_host_protocol::ContractError;
use festerm_host_setup::{enrollment::CurrentOwnerPolicy, lifecycle::Store};
use festerm_tunnel_bridge::{
    host_control::{PolicyGuard, ProtectedPolicySource},
    owner_auth::OwnerVerifier,
};

pub struct EnrollmentPolicy(Store);
pub struct EnrollmentGuard(CurrentOwnerPolicy);

impl EnrollmentPolicy {
    pub fn from_store(store: Store) -> Self {
        Self(store)
    }
}

impl PolicyGuard for EnrollmentGuard {
    fn verifier(&self) -> &OwnerVerifier {
        self.0.verifier()
    }
}

impl ProtectedPolicySource for EnrollmentPolicy {
    type Guard = EnrollmentGuard;

    fn current_policy(&self) -> Result<Self::Guard, ContractError> {
        self.0
            .owner_enrollment()
            .and_then(|owner| owner.current_policy())
            .map(EnrollmentGuard)
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::WouldBlock {
                    ContractError::Busy
                } else {
                    ContractError::Denied
                }
            })
    }
}
