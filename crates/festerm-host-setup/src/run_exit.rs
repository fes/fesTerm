//! Process disposition shared by the headless CLI and its owned Linux unit.
use std::{fmt, io};

/// Reserved for unavailable current-login proof, never other runtime failures.
pub const LOGIN_PROOF_UNAVAILABLE_EXIT_CODE: u8 = 75;

#[derive(Debug)]
pub struct LoginProofUnavailable;

impl fmt::Display for LoginProofUnavailable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("host.linux_current_login_proof_required")
    }
}

impl std::error::Error for LoginProofUnavailable {}

pub fn error_exit_code(error: &io::Error) -> u8 {
    if error
        .get_ref()
        .is_some_and(|inner| inner.is::<LoginProofUnavailable>())
    {
        LOGIN_PROOF_UNAVAILABLE_EXIT_CODE
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_exit_only_typed_login_unavailability_is_retryable() {
        assert_eq!(
            error_exit_code(&io::Error::other(LoginProofUnavailable)),
            LOGIN_PROOF_UNAVAILABLE_EXIT_CODE
        );
        for reason in [
            "host.linux_current_login_proof_required",
            "host.runtime_current_provisioned_consent_required",
            "host.runtime_current_owner_consent_required",
            "host.runtime_consent_changed",
            "host.authenticated_bundle_required",
            "host.runtime_daemon_boundary_unavailable",
            "host.cloud_offline_or_unknown",
            "host.cloud_cancelled",
            "host.runtime_stream_closed",
            "host.runtime_failed",
        ] {
            assert_eq!(error_exit_code(&io::Error::other(reason)), 1, "{reason}");
        }
        for kind in [
            io::ErrorKind::WouldBlock,
            io::ErrorKind::PermissionDenied,
            io::ErrorKind::NotFound,
            io::ErrorKind::Unsupported,
            io::ErrorKind::Interrupted,
        ] {
            assert_eq!(error_exit_code(&io::Error::from(kind)), 1);
        }
    }
}
