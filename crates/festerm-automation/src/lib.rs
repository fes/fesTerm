//! Memory-only ADR 0065 authority kernel. Adapters never own this broker.
//!
//! The application must serialize its controller's input/target transitions with
//! broker commits. A prepared request or returned ID is not authorization to
//! dispatch or disclose. See the crate README for the integration contract.

mod broker;
mod types;

pub use broker::*;
pub use festerm_admission::{
    automation_admission_lock, try_automation_admission_lock, AutomationAdmissionGuard,
    AutomationAdmissionTryError,
};
pub use types::*;

#[cfg(test)]
mod tests;
