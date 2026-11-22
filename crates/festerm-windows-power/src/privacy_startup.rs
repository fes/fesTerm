//! Finite startup evidence only; never an admission or cleanup capability.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PrivacyStartupStage {
    ClassId,
    Module,
    WindowClass,
    Window,
    WtsRegistration,
    SuspendRegistration,
    ServiceManager,
    ServiceOpen,
    ServiceManagerClose,
    ServiceNotification,
    ServiceQuery,
    ServiceState,
    ProcessSession,
    SessionQuery,
    SessionState,
}
impl PrivacyStartupStage {
    pub fn code(self) -> &'static str {
        match self {
            Self::ClassId => "class_id",
            Self::Module => "module",
            Self::WindowClass => "window_class",
            Self::Window => "window",
            Self::WtsRegistration => "wts_registration",
            Self::SuspendRegistration => "suspend_registration",
            Self::ServiceManager => "service_manager",
            Self::ServiceOpen => "service_open",
            Self::ServiceManagerClose => "service_manager_close",
            Self::ServiceNotification => "service_notification",
            Self::ServiceQuery => "service_query",
            Self::ServiceState => "service_state",
            Self::ProcessSession => "process_session",
            Self::SessionQuery => "session_query",
            Self::SessionState => "session_state",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PrivacyStartupDiagnostic {
    stage: PrivacyStartupStage,
    code: Option<u32>,
    service_state: Option<u32>,
}
impl PrivacyStartupDiagnostic {
    pub(crate) fn new(stage: PrivacyStartupStage, code: Option<u32>) -> Self {
        Self {
            stage,
            code,
            service_state: None,
        }
    }
    pub(crate) fn service_state(state: u32) -> Self {
        Self {
            stage: PrivacyStartupStage::ServiceState,
            code: None,
            service_state: Some(state),
        }
    }
    pub fn stage_code(self) -> &'static str {
        self.stage.code()
    }
    pub fn win32_code(self) -> Option<u32> {
        self.code
    }
    pub fn service_state_code(self) -> Option<&'static str> {
        self.service_state.map(|state| match state {
            1 => "stopped",
            2 => "start_pending",
            3 => "stop_pending",
            4 => "running",
            5 => "continue_pending",
            6 => "pause_pending",
            7 => "paused",
            _ => "unknown",
        })
    }
    pub fn service_not_running(self) -> bool {
        self.service_state
            .is_some_and(|state| matches!(state, 1..=3 | 5..=7))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn privacy_startup_diagnostics_distinguish_native_codes_from_service_state() {
        for stage in [
            PrivacyStartupStage::ClassId,
            PrivacyStartupStage::Module,
            PrivacyStartupStage::WindowClass,
            PrivacyStartupStage::Window,
            PrivacyStartupStage::WtsRegistration,
            PrivacyStartupStage::SuspendRegistration,
            PrivacyStartupStage::ServiceManager,
            PrivacyStartupStage::ServiceOpen,
            PrivacyStartupStage::ServiceManagerClose,
            PrivacyStartupStage::ServiceNotification,
            PrivacyStartupStage::ServiceQuery,
            PrivacyStartupStage::ServiceState,
            PrivacyStartupStage::ProcessSession,
            PrivacyStartupStage::SessionQuery,
            PrivacyStartupStage::SessionState,
        ] {
            let value = PrivacyStartupDiagnostic::new(stage, None);
            assert!(value
                .stage_code()
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b == b'_'));
            assert_eq!(value.win32_code(), None);
            assert_eq!(value.service_state_code(), None);
        }
        let code =
            PrivacyStartupDiagnostic::new(PrivacyStartupStage::ServiceNotification, Some(1722));
        assert_eq!(code.stage_code(), "service_notification");
        assert_eq!(code.win32_code(), Some(1722));
        assert_eq!(code.service_state_code(), None);
        assert!(!code.service_not_running());
        for (raw, expected) in [
            (1, "stopped"),
            (2, "start_pending"),
            (3, "stop_pending"),
            (4, "running"),
            (5, "continue_pending"),
            (6, "pause_pending"),
            (7, "paused"),
            (999, "unknown"),
        ] {
            let state = PrivacyStartupDiagnostic::service_state(raw);
            assert_eq!(state.stage_code(), "service_state");
            assert_eq!(
                state.win32_code(),
                None,
                "successful query cannot reuse thread last-error"
            );
            assert_eq!(state.service_state_code(), Some(expected));
            assert_eq!(state.service_not_running(), matches!(raw, 1..=3 | 5..=7));
        }
    }
}
