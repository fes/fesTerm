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
    operation: Option<&'static str>,
}
impl KeyError {
    pub(super) fn storage_at(stage: KeyStage, error: io::Error) -> Self {
        #[cfg(unix)]
        let native =
            festerm_secret_store::NativeAppPrivateRoot::approval_directory_diagnostic(&error);
        #[cfg(not(unix))]
        let native: Option<(&'static str, io::ErrorKind, Option<i32>)> = None;
        #[cfg(unix)]
        let private = festerm_unix_security::private_storage_diagnostic(&error);
        #[cfg(not(unix))]
        let private: Option<(&'static str, io::ErrorKind, Option<i32>)> = None;
        let (native_stage, operation, kind, errno) = match (private, native) {
            (Some((operation, kind, errno)), _) => (None, Some(operation), kind, errno),
            (None, Some((stage, kind, errno))) => (Some(stage), None, kind, errno),
            (None, None) => (None, None, error.kind(), error.raw_os_error()),
        };
        Self::Diagnosed(KeyDiagnostic {
            stage,
            class: KeyFailureClass::Storage,
            kind: Some(kind),
            errno,
            native_stage,
            operation,
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
            operation: None,
        })
    }
    pub(super) fn random_failure(errno: Option<i32>) -> Self {
        Self::Diagnosed(KeyDiagnostic {
            stage: KeyStage::Random,
            class: KeyFailureClass::RandomUnavailable,
            kind: None,
            errno,
            native_stage: None,
            operation: None,
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
        let operation = value
            .operation
            .map_or_else(String::new, |operation| format!(" operation={operation};"));
        format!(
            "{} Device-key diagnostic: stage={};{} class={}; I/O kind={}; errno={}.",
            self.message(),
            value.native_stage.unwrap_or(value.stage.code()),
            operation,
            value.class.code(),
            kind,
            errno
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    #[test]
    fn ordinary_record_publication_keeps_actual_exclusive_rename_operation_and_errno() {
        let root = festerm_sessiond::private_storage::private_test_directory();
        std::fs::create_dir(root.path().join("source")).unwrap();
        std::fs::create_dir(root.path().join("source/child")).unwrap();
        let directory = std::fs::File::open(root.path()).unwrap();
        let child = std::fs::File::open(root.path().join("source/child")).unwrap();
        // POSIX refuses moving a directory into its own descendant. The real
        // primitive preserves Unsupported classification if the volume instead
        // refuses exclusive rename before checking the directory relationship.
        let error = festerm_unix_security::rename_noreplace(
            &directory,
            std::path::Path::new("source"),
            &child,
            std::path::Path::new("moved"),
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
        let (operation, kind, errno) =
            festerm_unix_security::private_storage_diagnostic(&error).unwrap();
        assert_eq!(operation, "private.exclusive_rename");
        assert_eq!(kind, io::ErrorKind::Unsupported);
        let errno = errno.expect("exclusive rename retains its original errno");
        assert!(matches!(
            io::Error::from_raw_os_error(errno).kind(),
            io::ErrorKind::InvalidInput | io::ErrorKind::Unsupported
        ));
        let error = KeyError::storage_at(KeyStage::RecordPublish, error);
        let message = error.diagnostic_message();
        assert_eq!(error.message(), KeyError::Storage.message());
        assert!(message
            .contains("stage=record_publish; operation=private.exclusive_rename; class=storage"));
        assert!(message.contains(&format!("errno={errno}")));
        assert!(!message.contains(&root.path().to_string_lossy().to_string()));
        assert!(root.path().join("source/child").is_dir());
        assert!(!root.path().join("source/child/moved").exists());
    }

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
