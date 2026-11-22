//! Finite local preparation context; never retain an I/O message or key data.
use super::KeyError;
use std::io;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum KeyStage {
    Directory,
    LockOpen,
    LockAcquire,
    RecordRead,
    RecordDecode,
    RecordBinding,
    RecordEncode,
    RecordPublish,
    ReferenceDecode,
    AccountGuard,
    SeedBinding,
    Random,
    RecordReplace,
}
impl KeyStage {
    fn code(self) -> &'static str {
        match self {
            Self::Directory => "approval_directory",
            Self::LockOpen => "lock_open",
            Self::LockAcquire => "lock_acquire",
            Self::RecordRead => "record_read",
            Self::RecordDecode => "record_decode",
            Self::RecordBinding => "record_binding",
            Self::RecordEncode => "record_encode",
            Self::RecordPublish => "record_publish",
            Self::ReferenceDecode => "reference_decode",
            Self::AccountGuard => "account_guard",
            Self::SeedBinding => "seed_binding",
            Self::Random => "random",
            Self::RecordReplace => "record_replace",
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum KeyFailureClass {
    Storage,
    BindingMismatch,
    RandomUnavailable,
}
impl KeyFailureClass {
    fn code(self) -> &'static str {
        match self {
            Self::Storage => "storage",
            Self::BindingMismatch => "binding-mismatch",
            Self::RandomUnavailable => "random-unavailable",
        }
    }
}

/// Bounded diagnostic metadata, not an index, key, or storage capability.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct KeyDiagnostic {
    pub(super) stage: KeyStage,
    pub(super) class: KeyFailureClass,
    kind: Option<io::ErrorKind>,
    errno: Option<i32>,
    native_stage: Option<&'static str>,
}
impl KeyError {
    pub(super) fn storage_at(stage: KeyStage, error: io::Error) -> Self {
        #[cfg(unix)]
        let native =
            festerm_secret_store::NativeAppPrivateRoot::approval_directory_diagnostic(&error);
        #[cfg(not(unix))]
        let native: Option<(&'static str, io::ErrorKind, Option<i32>)> = None;
        let (native_stage, kind, errno) = match native {
            Some((stage, kind, errno)) => (Some(stage), kind, errno),
            None => (None, error.kind(), error.raw_os_error()),
        };
        Self::Diagnosed(KeyDiagnostic {
            stage,
            class: KeyFailureClass::Storage,
            kind: Some(kind),
            errno,
            native_stage,
        })
    }
    pub(super) fn at_stage(self, stage: KeyStage) -> Self {
        let class = match self {
            Self::Storage => KeyFailureClass::Storage,
            Self::BindingMismatch => KeyFailureClass::BindingMismatch,
            Self::RandomUnavailable => KeyFailureClass::RandomUnavailable,
            // Preserve an earlier more specific stage and existing vault/busy errors.
            other => return other,
        };
        Self::Diagnosed(KeyDiagnostic {
            stage,
            class,
            kind: None,
            errno: None,
            native_stage: None,
        })
    }
    pub(super) fn random_failure(errno: Option<i32>) -> Self {
        Self::Diagnosed(KeyDiagnostic {
            stage: KeyStage::Random,
            class: KeyFailureClass::RandomUnavailable,
            kind: None,
            errno,
            native_stage: None,
        })
    }
    /// Extends the unchanged short notice only with finite local categories.
    pub fn diagnostic_message(self) -> String {
        let Self::Diagnosed(value) = self else {
            return self.message().to_owned();
        };
        let kind = value
            .kind
            .map_or_else(|| "not-recorded".into(), |kind| format!("{kind:?}"));
        let errno = value
            .errno
            .map_or_else(|| "not-recorded".into(), |errno| errno.to_string());
        format!(
            "{} Device-key diagnostic: stage={}; class={}; I/O kind={}; errno={}.",
            self.message(),
            value.native_stage.unwrap_or(value.stage.code()),
            value.class.code(),
            kind,
            errno
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ordinary_key_diagnostics_keep_short_notice_and_never_expose_source_strings() {
        for stage in [
            KeyStage::Directory,
            KeyStage::LockOpen,
            KeyStage::LockAcquire,
            KeyStage::RecordRead,
            KeyStage::RecordDecode,
            KeyStage::RecordBinding,
            KeyStage::RecordEncode,
            KeyStage::RecordPublish,
            KeyStage::ReferenceDecode,
            KeyStage::AccountGuard,
            KeyStage::SeedBinding,
            KeyStage::Random,
            KeyStage::RecordReplace,
        ] {
            let error = KeyError::storage_at(
                stage,
                io::Error::other("ghp_secret /private/seed reference=opaque account=42"),
            );
            assert_eq!(error.message(), KeyError::Storage.message());
            let detail = error.diagnostic_message();
            assert!(detail.starts_with(error.message()));
            assert!(detail.contains(stage.code()) && detail.contains("class=storage"));
            assert!(detail.contains("errno=not-recorded") && detail.len() < 400);
            assert!(
                !detail.contains("ghp_")
                    && !detail.contains("/private")
                    && !detail.contains("opaque")
                    && !detail.contains("42")
            );
            let binding = KeyError::BindingMismatch.at_stage(stage);
            assert_eq!(binding.message(), KeyError::BindingMismatch.message());
            assert!(binding
                .diagnostic_message()
                .contains("class=binding-mismatch"));
        }
        let error = KeyError::storage_at(KeyStage::LockOpen, io::Error::from_raw_os_error(13));
        assert!(error.diagnostic_message().contains("errno=13"));
        let error = KeyError::random_failure(Some(5));
        assert_eq!(error.message(), KeyError::RandomUnavailable.message());
        assert!(error
            .diagnostic_message()
            .contains("stage=random; class=random-unavailable"));
        assert!(error.diagnostic_message().contains("errno=5"));
        let busy = KeyError::Busy;
        assert_eq!(busy.diagnostic_message(), busy.message());
        let vault = KeyError::Vault(festerm_secret_store::SecretStoreError::Missing);
        assert_eq!(vault.diagnostic_message(), vault.message());
        assert_eq!(
            crate::host_client::Failure::OrdinaryKey(error).diagnostic_message(),
            error.diagnostic_message()
        );
    }
}
