//! Finite attachment context only. Never retain transport strings or payloads.
use super::{Error, Failure};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AttachStage {
    Relay,
    Authentication,
    Capabilities,
    Snapshot,
    FrontendAdoption,
    HostActivation,
    LiveStream,
}
impl AttachStage {
    fn label(self) -> &'static str {
        match self {
            Self::Relay => "relay connection",
            Self::Authentication => "host authentication",
            Self::Capabilities => "terminal capabilities",
            Self::Snapshot => "attachment snapshot",
            Self::FrontendAdoption => "frontend adoption",
            Self::HostActivation => "host activation",
            Self::LiveStream => "live terminal stream",
        }
    }
}
impl Failure {
    /// App-facing detail with finite local device-key preparation context only.
    pub fn diagnostic_message(self) -> String {
        match self {
            Self::OrdinaryKey(error) => error.diagnostic_message(),
            _ => self.message().to_owned(),
        }
    }

    pub(crate) fn at_attach_stage_for(self, role: super::StreamRole, stage: AttachStage) -> Self {
        if role == super::StreamRole::Terminal {
            self.at_attach_stage(stage)
        } else {
            self
        }
    }
    pub(crate) fn at_attach_stage(self, stage: AttachStage) -> Self {
        match self {
            Self::Control(error) => Self::Attachment { stage, error },
            other => other,
        }
    }
    pub(super) fn terminal_message(self) -> String {
        let Self::Attachment { stage, error } = self else {
            return self.message().to_owned();
        };
        let category = match error {
            Error::Offline | Error::Io(_) => "offline",
            Error::Closed => "closed",
            Error::Deadline => "deadline",
            Error::Cancelled => "cancelled",
            Error::Contract(_) => "contract",
            Error::Refused(_) => "refused",
            Error::HostUpdateRequired => "host-update-required",
            Error::HostPolicyRefreshRequired => "host-policy-refresh-required",
        };
        let kind = match error {
            Error::Io(kind) => match kind {
                std::io::ErrorKind::UnexpectedEof => "unexpected-eof",
                std::io::ErrorKind::BrokenPipe => "broken-pipe",
                std::io::ErrorKind::ConnectionReset => "connection-reset",
                std::io::ErrorKind::ConnectionAborted => "connection-aborted",
                std::io::ErrorKind::NotConnected => "not-connected",
                std::io::ErrorKind::TimedOut => "timed-out",
                std::io::ErrorKind::WouldBlock => "would-block",
                std::io::ErrorKind::Interrupted => "interrupted",
                std::io::ErrorKind::InvalidData => "invalid-data",
                std::io::ErrorKind::InvalidInput => "invalid-input",
                std::io::ErrorKind::PermissionDenied => "permission-denied",
                std::io::ErrorKind::NotFound => "not-found",
                _ => "other",
            },
            _ => "not-recorded",
        };
        let refusal = match error {
            Error::Refused(reason) => match reason {
                festerm_host_protocol::Refusal::Denied => "denied",
                festerm_host_protocol::Refusal::Offline => "offline",
                festerm_host_protocol::Refusal::Unknown => "unknown",
                festerm_host_protocol::Refusal::Busy => "busy",
                festerm_host_protocol::Refusal::Incompatible => "incompatible",
                festerm_host_protocol::Refusal::Stale => "stale",
                festerm_host_protocol::Refusal::RecipeConflict => "recipe-conflict",
                festerm_host_protocol::Refusal::Revoked => "revoked",
                festerm_host_protocol::Refusal::OutcomeUncertain => "outcome-uncertain",
                festerm_host_protocol::Refusal::Unsupported => "unsupported",
            },
            _ => "not-recorded",
        };
        format!(
            "{} Attachment stage: {}; category: {}; I/O kind: {}; refusal: {}.",
            self.message(),
            stage.label(),
            category,
            kind,
            refusal
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn attachment_failure_detail_is_finite_preserves_cause_and_base_message() {
        for stage in [
            AttachStage::Relay,
            AttachStage::Authentication,
            AttachStage::Capabilities,
            AttachStage::Snapshot,
            AttachStage::FrontendAdoption,
            AttachStage::HostActivation,
            AttachStage::LiveStream,
        ] {
            for (error, category) in [
                (Error::Offline, "offline"),
                (Error::Closed, "closed"),
                (Error::Io(std::io::ErrorKind::UnexpectedEof), "offline"),
                (Error::Cancelled, "cancelled"),
                (Error::Deadline, "deadline"),
                (
                    Error::Refused(festerm_host_protocol::Refusal::Busy),
                    "refused",
                ),
                (
                    Error::Refused(festerm_host_protocol::Refusal::Denied),
                    "refused",
                ),
                (
                    Error::Contract(festerm_host_protocol::ContractError::Stale),
                    "contract",
                ),
            ] {
                let original = Failure::Control(error);
                let contextual = original.at_attach_stage(stage);
                assert_eq!(contextual.message(), original.message());
                assert!(
                    matches!(contextual, Failure::Attachment { stage: actual, error: cause } if actual == stage && cause == error)
                );
                let message = contextual.terminal_message();
                assert!(message.contains(stage.label()));
                assert!(message.contains(&format!("category: {category}")));
                assert_eq!(
                    original.at_attach_stage_for(super::super::StreamRole::Control, stage),
                    original
                );
            }
        }
        for (reason, label) in [
            (festerm_host_protocol::Refusal::Denied, "denied"),
            (festerm_host_protocol::Refusal::Offline, "offline"),
            (festerm_host_protocol::Refusal::Unknown, "unknown"),
            (festerm_host_protocol::Refusal::Busy, "busy"),
            (festerm_host_protocol::Refusal::Incompatible, "incompatible"),
            (festerm_host_protocol::Refusal::Stale, "stale"),
            (
                festerm_host_protocol::Refusal::RecipeConflict,
                "recipe-conflict",
            ),
            (festerm_host_protocol::Refusal::Revoked, "revoked"),
            (
                festerm_host_protocol::Refusal::OutcomeUncertain,
                "outcome-uncertain",
            ),
            (festerm_host_protocol::Refusal::Unsupported, "unsupported"),
        ] {
            let original = Failure::Control(Error::Refused(reason));
            let diagnostic = original.at_attach_stage(AttachStage::Snapshot);
            assert_eq!(diagnostic.message(), original.message());
            assert!(diagnostic
                .terminal_message()
                .contains(&format!("refusal: {label}.")));
        }
        let error = Error::from(std::io::Error::other(
            "ghp_secret https://credential.invalid/private",
        ));
        let message = Failure::Control(error)
            .at_attach_stage(AttachStage::Relay)
            .terminal_message();
        assert!(message.contains("I/O kind: other"));
        assert!(message.contains("refusal: not-recorded."));
        assert!(!message.contains("ghp_") && !message.contains("credential.invalid"));
        assert_eq!(
            Failure::Control(error).message(),
            Failure::Control(Error::Offline).message()
        );
    }
}
