//! Error-only start observations. Preserve the original operation result.
use std::{io, path::Path};

#[derive(Clone, Copy)]
pub(crate) enum StartStage {
    ServiceLease,
    InitialFence,
    RegistrationRead,
    BeforeManagerFence,
    AfterManagerFence,
    FinalFence,
    BeforeObservation,
    AfterObservation,
    Manager,
    #[cfg(all(target_os = "macos", feature = "cloud", feature = "host-runtime"))]
    MacChildCurrent,
    #[cfg(all(target_os = "macos", feature = "cloud", feature = "host-runtime"))]
    MacChildSnapshot,
    #[cfg(all(target_os = "macos", feature = "cloud", feature = "host-runtime"))]
    MacParentCurrent,
}
#[cfg(feature = "host-runtime")]
impl StartStage {
    fn code(self) -> &'static str {
        match self {
            Self::ServiceLease => "service_lease",
            Self::InitialFence => "initial_fence",
            Self::RegistrationRead => "registration_read",
            Self::BeforeManagerFence => "before_manager_fence",
            Self::AfterManagerFence => "after_manager_fence",
            Self::FinalFence => "final_fence",
            Self::BeforeObservation => "before_observation",
            Self::AfterObservation => "after_observation",
            Self::Manager => "manager",
            #[cfg(all(target_os = "macos", feature = "cloud"))]
            Self::MacChildCurrent => "mac_child_current",
            #[cfg(all(target_os = "macos", feature = "cloud"))]
            Self::MacChildSnapshot => "mac_child_snapshot",
            #[cfg(all(target_os = "macos", feature = "cloud"))]
            Self::MacParentCurrent => "mac_parent_current",
        }
    }
}
#[cfg(feature = "host-runtime")]
fn kind_code(kind: io::ErrorKind) -> &'static str {
    match kind {
        io::ErrorKind::WouldBlock => "busy",
        io::ErrorKind::NotFound => "missing",
        io::ErrorKind::PermissionDenied => "permission_denied",
        io::ErrorKind::TimedOut => "timed_out",
        io::ErrorKind::Interrupted => "interrupted",
        io::ErrorKind::InvalidData => "invalid_data",
        io::ErrorKind::InvalidInput => "invalid_input",
        io::ErrorKind::Unsupported => "unsupported",
        io::ErrorKind::AlreadyExists => "already_exists",
        _ => "unclassified",
    }
}

pub(crate) fn observe<T>(state: &Path, stage: StartStage, result: io::Result<T>) -> io::Result<T> {
    #[cfg(feature = "host-runtime")]
    if let Err(error) = &result {
        let os = error
            .raw_os_error()
            .map_or_else(|| "none".into(), |code| code.to_string());
        festerm_tunnel_bridge::worker_log::WorkerLog::new(state).event(
            "host.start_observation",
            &[
                ("stage", stage.code()),
                ("kind", kind_code(error.kind())),
                ("os", &os),
            ],
        );
    }
    #[cfg(not(feature = "host-runtime"))]
    let _ = (state, stage);
    result
}

#[cfg(all(test, feature = "host-runtime"))]
mod tests {
    use super::*;
    use crate::lifecycle::{Origin, Settings, Startup, Store};
    use festerm_sessiond::private_storage as storage;
    use std::{fmt, sync::Arc};

    #[derive(Debug)]
    struct PrivateError(Arc<()>);
    impl fmt::Display for PrivateError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("PRIVATE-PATH-ACCOUNT-CREDENTIAL")
        }
    }
    impl std::error::Error for PrivateError {}

    #[test]
    fn start_observation_preserves_payload_raw_errno_and_redacts_source_text() {
        let root = storage::private_test_directory();
        let payload = Arc::new(());
        let error = io::Error::other(PrivateError(payload.clone()));
        let original = error.get_ref().unwrap() as *const _;
        let error = observe::<()>(root.path(), StartStage::Manager, Err(error)).unwrap_err();
        assert!(std::ptr::eq(error.get_ref().unwrap(), original));
        assert!(Arc::ptr_eq(
            &error
                .get_ref()
                .unwrap()
                .downcast_ref::<PrivateError>()
                .unwrap()
                .0,
            &payload
        ));
        assert_eq!(error.kind(), io::ErrorKind::Other);
        assert_eq!(error.raw_os_error(), None);
        let raw = io::Error::from_raw_os_error(1234);
        let kind = raw.kind();
        let raw = observe::<()>(root.path(), StartStage::AfterObservation, Err(raw)).unwrap_err();
        assert_eq!(raw.raw_os_error(), Some(1234));
        assert_eq!(raw.kind(), kind);
        let text = std::fs::read_to_string(root.path().join("worker.log")).unwrap();
        assert!(text.contains("host.start_observation stage=manager kind=unclassified os=none"));
        assert!(text.contains("stage=after_observation kind=unclassified os=1234"));
        assert!(!text.contains("PRIVATE") && !text.contains(&root.path().display().to_string()));
        assert_eq!(text.lines().count(), 2);
    }

    #[test]
    fn start_observation_actual_journal_contention_keeps_would_block_and_releases_cleanly() {
        let root = storage::private_test_directory();
        let state = root.path().join("state");
        let store = Store::create(
            &state,
            Origin::Headless,
            Settings {
                friendly_name: "Journal fixture".into(),
                allow_creation: false,
                allow_session_termination: false,
                startup: Startup::Manual,
                ordinary_access: Default::default(),
                filesystem: None,
            },
        )
        .unwrap();
        let directory = storage::private_directory(&state, false).unwrap();
        let file = storage::open_private_file(&directory, Path::new("host-lifecycle.lock"), false)
            .unwrap();
        let held = crate::protected_state::Lease::exclusive(file).unwrap();
        let result = store.snapshot();
        let expected_kind = result.as_ref().unwrap_err().kind();
        let expected_os = result.as_ref().unwrap_err().raw_os_error();
        let error = observe(&state, StartStage::InitialFence, result).unwrap_err();
        assert_eq!(expected_kind, io::ErrorKind::WouldBlock);
        assert_eq!(error.kind(), expected_kind);
        assert_eq!(error.raw_os_error(), expected_os);
        assert!(std::fs::read_to_string(state.join("worker.log"))
            .unwrap()
            .contains("stage=initial_fence kind=busy os=none"));
        drop(held);
        assert!(store.snapshot().is_ok());
    }

    #[test]
    fn start_observation_log_failure_does_not_replace_error_or_log_success() {
        let root = storage::private_test_directory();
        let success = observe(root.path(), StartStage::FinalFence, Ok(17)).unwrap();
        assert_eq!(success, 17);
        assert!(!root.path().join("worker.log").exists());
        let state = root.path().join("missing");
        let error = io::Error::other(PrivateError(Arc::new(())));
        let original = error.get_ref().unwrap() as *const _;
        let error = observe::<()>(&state, StartStage::FinalFence, Err(error)).unwrap_err();
        assert!(std::ptr::eq(error.get_ref().unwrap(), original));
        assert!(!state.exists());
    }
}
