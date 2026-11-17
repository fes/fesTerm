//! Content-free diagnostics transported alongside the original I/O error.
//! Render the typed fields, never arbitrary source-chain text, in diagnostic UI.
pub use festerm_tunnel_client::diagnostics::Category;
use festerm_tunnel_client::{
    diagnostics::{HostFailure, Operation},
    Error as ClientError,
};
use std::{error::Error, fmt, io};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Stage {
    Placement,
    Observation,
    Creation,
    HostToken,
    Authentication,
    SecureStorage,
    NativeSetup,
}
impl Stage {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Placement => "Host placement",
            Self::Observation => "Host observation",
            Self::Creation => "Host creation",
            Self::HostToken => "Host token",
            Self::Authentication => "Account authentication",
            Self::SecureStorage => "Secure storage",
            Self::NativeSetup => "Native host setup",
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Recovery {
    Retry,
    SignIn,
    UnlockStore,
    Reconcile,
    UpgradeHost,
    CheckAccount,
    CheckConfiguration,
    Wait,
    Cancel,
}
impl Recovery {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Retry => "Retry",
            Self::SignIn => "Sign in",
            Self::UnlockStore => "Unlock secure storage",
            Self::Reconcile => "Review recovery",
            Self::UpgradeHost => "Update host helpers",
            Self::CheckAccount => "Check account",
            Self::CheckConfiguration => "Check configuration",
            Self::Wait => "Wait before retrying",
            Self::Cancel => "Cancelled",
        }
    }
    pub const fn guidance(self) -> &'static str {
        match self {
            Self::Retry => "Check connectivity, then retry the existing operation. Review Recovery first if its outcome is uncertain.",
            Self::SignIn => "Sign in again with the selected account.",
            Self::UnlockStore => "Unlock the native secure store, then retry sign-in.",
            Self::Reconcile => "Review Recovery to reconcile the original host resource before enabling again.",
            Self::UpgradeHost => "Update to matching host helpers, then review Recovery before enabling again.",
            Self::CheckAccount => "Check the selected account and its service access.",
            Self::CheckConfiguration => "Check this build's sign-in configuration.",
            Self::Wait => "Wait for the service or current operation, then retry.",
            Self::Cancel => "The operation was cancelled. Review Recovery if a host request may have been sent.",
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HostSetupDiagnostic {
    pub stage: Stage,
    pub http_status: Option<u16>,
    pub category: Category,
    pub recovery: Recovery,
}
fn recovery(category: Category) -> Recovery {
    match category {
        Category::Registration => Recovery::CheckConfiguration,
        Category::Cancelled => Recovery::Cancel,
        Category::Expired | Category::AccountChanged => Recovery::SignIn,
        Category::Denied => Recovery::CheckAccount,
        Category::Network => Recovery::Retry,
        Category::Store => Recovery::UnlockStore,
        Category::Busy | Category::RateLimited => Recovery::Wait,
        Category::UnsupportedUserAcl | Category::UnsupportedProtocol => Recovery::UpgradeHost,
        Category::Response
        | Category::IdentityChanged
        | Category::TunnelNotFound
        | Category::Tunnel
        | Category::Ambiguous
        | Category::ProviderRejected => Recovery::Reconcile,
    }
}
pub fn from_client(failure: HostFailure) -> HostSetupDiagnostic {
    HostSetupDiagnostic {
        stage: match failure.operation {
            Operation::Placement => Stage::Placement,
            Operation::Observation => Stage::Observation,
            Operation::Creation => Stage::Creation,
            Operation::HostToken => Stage::HostToken,
        },
        http_status: failure.http_status,
        category: failure.category,
        recovery: recovery(failure.category),
    }
}
pub fn from_bare_account_error(error: ClientError) -> HostSetupDiagnostic {
    if let ClientError::HostRequest(failure) = error {
        return from_client(failure);
    }
    let category = HostFailure::from_original(Operation::Observation, error, None).category;
    HostSetupDiagnostic {
        stage: if category == Category::Store {
            Stage::SecureStorage
        } else {
            Stage::Authentication
        },
        http_status: None,
        category,
        recovery: if category == Category::IdentityChanged {
            Recovery::SignIn
        } else {
            recovery(category)
        },
    }
}

struct DiagnosticError {
    original: io::Error,
    diagnostic: HostSetupDiagnostic,
}
impl fmt::Debug for DiagnosticError {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.debug_struct("DiagnosticError")
            .field("kind", &self.original.kind())
            .field("diagnostic", &self.diagnostic)
            .finish()
    }
}
impl fmt::Display for DiagnosticError {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.original.fmt(out)
    }
}
impl Error for DiagnosticError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.original)
    }
}
/// Preserves original display, error kind, and complete semantic source chain.
pub fn attach(original: io::Error, diagnostic: HostSetupDiagnostic) -> io::Error {
    io::Error::new(
        original.kind(),
        DiagnosticError {
            original,
            diagnostic,
        },
    )
}
// io::Error::source can bypass its immediate payload. Inspect get_ref explicitly
// so wrapped typed cancellation/transient markers remain discoverable.
fn next<'a>(error: &'a (dyn Error + 'static)) -> Option<&'a (dyn Error + 'static)> {
    if let Some(error) = error.downcast_ref::<io::Error>() {
        if let Some(inner) = error.get_ref() {
            return Some(inner);
        }
    }
    error.source()
}
pub fn contains_source_type<T: Error + 'static>(error: &io::Error) -> bool {
    let mut current: Option<&(dyn Error + 'static)> = Some(error);
    for _ in 0..64 {
        let Some(value) = current else { break };
        if value.is::<T>() {
            return true;
        }
        current = next(value);
    }
    false
}
pub fn from_io_error(error: &io::Error) -> Option<HostSetupDiagnostic> {
    let mut current: Option<&(dyn Error + 'static)> = Some(error);
    for _ in 0..64 {
        let value = current?;
        if let Some(value) = value.downcast_ref::<DiagnosticError>() {
            return Some(value.diagnostic);
        }
        if let Some(value) = value.downcast_ref::<HostFailure>() {
            return Some(from_client(*value));
        }
        if let Some(value) = value.downcast_ref::<ClientError>() {
            return Some(from_bare_account_error(*value));
        }
        current = next(value);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Debug)]
    struct Cancelled;
    impl fmt::Display for Cancelled {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("original cancellation")
        }
    }
    impl Error for Cancelled {}
    #[test]
    fn attached_diagnostic_preserves_kind_display_and_nested_source_markers() {
        let diagnostic = from_client(HostFailure::from_original(
            Operation::Creation,
            ClientError::Cancelled,
            None,
        ));
        let error = attach(
            attach(
                io::Error::new(io::ErrorKind::Interrupted, Cancelled),
                diagnostic,
            ),
            diagnostic,
        );
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        assert_eq!(error.to_string(), "original cancellation");
        assert!(contains_source_type::<Cancelled>(&error));
        assert!(!contains_source_type::<ClientError>(&error));
        assert_eq!(from_io_error(&error), Some(diagnostic));
    }
    #[test]
    fn diagnostic_debug_is_content_free_and_outermost_diagnostic_wins() {
        let diagnostic = from_client(HostFailure::from_http(Operation::Creation, 400, b"Adding individual users to an access control entry is currently disabled. ghp_SECRET https://private.example"));
        let error = attach(io::Error::other("raw ghp_SECRET provider text"), diagnostic);
        assert!(!format!("{error:?}").contains("SECRET"));
        assert_eq!(diagnostic.recovery, Recovery::UpgradeHost);
        let outer = from_bare_account_error(ClientError::Store);
        assert_eq!(from_io_error(&attach(error, outer)), Some(outer));
        assert_eq!(from_io_error(&io::Error::other("arbitrary text")), None);
    }
    #[test]
    fn bare_and_wrapped_client_failures_have_typed_stage_and_status() {
        let failure =
            HostFailure::from_original(Operation::HostToken, ClientError::Denied, Some(403));
        let found = from_io_error(&io::Error::other(ClientError::HostRequest(failure))).unwrap();
        assert_eq!(found.stage, Stage::HostToken);
        assert_eq!(found.http_status, Some(403));
        assert_eq!(found.recovery, Recovery::CheckAccount);
        assert_eq!(
            from_bare_account_error(ClientError::Store).stage,
            Stage::SecureStorage
        );
        assert_eq!(
            from_bare_account_error(ClientError::IdentityChanged).recovery,
            Recovery::SignIn
        );
    }
}
