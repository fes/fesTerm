//! One content-free failure fact for the existing Files tab error path.
use super::{file_client, rpc};
use std::{
    io,
    sync::{Arc, OnceLock},
};

#[derive(Clone, Copy, Debug)]
pub(crate) enum Channel {
    Browsing,
    Transfer,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_transfer_cleanup_retains_a_content_free_uncertainty_notice() {
        let first = FirstFailure::default();
        let error = file_client::Error::CleanupFailed {
            operation: Box::new(file_client::Error::LocalIo(io::ErrorKind::Interrupted)),
            cleanup: Box::new(file_client::Error::Remote {
                code: rpc::ErrorCode::Denied,
                recovery_path: Some("private-path-must-not-appear".into()),
            }),
        };
        first.record(Channel::Transfer, "upload", &error);
        let notice = first.details_or("missing".into());
        assert_eq!(
            notice,
            "Couldn’t confirm transfer cleanup. Check the files before trying again."
        );
        assert!(!notice.contains("private-path") && !notice.contains("completed"));
        first.record(
            Channel::Browsing,
            "keepalive",
            &file_client::Error::Cancelled,
        );
        assert_eq!(first.details_or("secondary".into()), notice);
    }
}

impl Channel {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Browsing => "browsing",
            Self::Transfer => "transfer",
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Fact {
    channel: Channel,
    stage: &'static str,
    class: &'static str,
    io_kind: Option<io::ErrorKind>,
    remote: Option<rpc::ErrorCode>,
}

#[derive(Clone, Default)]
pub(crate) struct FirstFailure(Arc<OnceLock<Fact>>);

impl FirstFailure {
    pub(super) fn record(&self, channel: Channel, stage: &'static str, error: &file_client::Error) {
        use file_client::Error;
        let (class, io_kind, remote) = match error {
            Error::Closed => ("closed", None, None),
            Error::Cancelled => ("cancelled", None, None),
            Error::Deadline => ("deadline", None, None),
            Error::Invalid => ("invalid", None, None),
            Error::Protocol => ("protocol", None, None),
            Error::Transport(kind) => ("transport", Some(*kind), None),
            Error::LocalIo(kind) => ("local_io", Some(*kind), None),
            Error::CleanupFailed { .. } => ("cleanup_unconfirmed", None, None),
            Error::Remote {
                code: rpc::ErrorCode::RecoveryRequired,
                ..
            } => (
                "recovery_required",
                None,
                Some(rpc::ErrorCode::RecoveryRequired),
            ),
            Error::Remote { code, .. } => ("remote", None, Some(*code)),
            Error::UncertainFinish(_) => ("uncertain_finish", None, None),
            Error::UncertainDelete(_) => ("uncertain_delete", None, None),
        };
        let fact = Fact {
            channel,
            stage,
            class,
            io_kind,
            remote,
        };
        if self.0.set(fact).is_ok() {
            tracing::warn!(target: "festerm::sftp", channel = fact.channel.label(), stage = fact.stage,
                class = fact.class, io_kind = ?fact.io_kind, remote_code = ?fact.remote,
                "First Files failure retained before tab retirement");
        }
    }

    pub(crate) fn details_or(&self, fallback: String) -> String {
        let Some(fact) = self.0.get() else {
            return fallback;
        };
        if fact.class == "cleanup_unconfirmed" {
            return "Couldn’t confirm transfer cleanup. Check the files before trying again."
                .into();
        }
        if matches!(
            fact.class,
            "uncertain_finish" | "uncertain_delete" | "recovery_required"
        ) {
            return "Couldn’t confirm whether the file operation completed. Check the files before trying again.".into();
        }
        let reason = match fact.class {
            "deadline" => "timed out",
            "closed" => "was closed",
            "cancelled" => "was interrupted",
            "transport" => "was interrupted",
            "remote" => "was refused by the host",
            _ => "could not continue",
        };
        format!(
            "The file {} connection {reason}. Reconnect.",
            fact.channel.label()
        )
    }
}
