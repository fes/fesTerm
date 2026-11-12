//! Thin protected enrollment delegation for the authenticated headless runtime.
//! Fresh native-vault authority is the default; explicit file selection is not
//! a vault-error fallback. Native/first-connect acceptance remains separate.
use festerm_host_protocol::ContractError;
use festerm_host_setup::{
    enrollment::{CurrentOwnerPolicy, CurrentOwnerPolicySource, OwnerStorage},
    lifecycle::Store,
};
use festerm_tunnel_bridge::{
    host_control::{PolicyGuard, ProtectedPolicySource},
    owner_auth::OwnerVerifier,
};
use std::sync::Arc;

pub struct EnrollmentPolicy(CurrentOwnerPolicySource);
pub struct EnrollmentGuard(CurrentOwnerPolicy);

impl EnrollmentPolicy {
    pub fn from_store(store: Store) -> Self {
        Self::with_storage(Arc::new(store), OwnerStorage::NativeVault)
    }

    pub fn with_storage(store: Arc<Store>, storage: OwnerStorage) -> Self {
        Self(CurrentOwnerPolicySource::new(store, storage))
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
        self.0.current_policy().map(EnrollmentGuard)
    }
}
