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

#[derive(Debug)]
pub(crate) struct RecordedManagedStop {
    pub(crate) source: io::Error,
}

impl fmt::Display for RecordedManagedStop {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("host.managed_run_failed_restart_suppressed")
    }
}

impl std::error::Error for RecordedManagedStop {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// Native-managed protocol only: an error remains an error to product callers.
/// Its protected disposition must have been recorded before constructing it.
pub fn is_recorded_managed_stop(error: &io::Error) -> bool {
    error
        .get_ref()
        .is_some_and(|inner| inner.is::<RecordedManagedStop>())
}

pub fn error_exit_code(error: &io::Error) -> u8 {
    if is_recorded_managed_stop(error) {
        0
    } else if error
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
