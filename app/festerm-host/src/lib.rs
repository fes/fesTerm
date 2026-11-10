//! Protected composition adapters, selected by the default headless build.
//! Explicit configure/setup/owner approval/run owns listener activation;
//! compilation and recorded status never establish native readiness.
#[cfg(feature = "owner-routing")]
pub mod routing;
