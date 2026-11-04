use crate::{
    automation::{NativeCommand, SettingsSnapshot},
    tabs::{SessionTab, TabContent, TabId},
};

use super::FesTermApp;

impl FesTermApp {
    pub(crate) fn automation_mcp_enabled(&self) -> bool {
        self.state.mcp_enabled()
    }

    pub(crate) fn automation_javascript_enabled(&self) -> bool {
        self.state.javascript_enabled()
    }

    pub(crate) fn automation_session(&self, id: TabId) -> Option<&SessionTab> {
        self.state.session_tab(id)
    }

    pub(crate) fn automation_sessions(&self) -> impl Iterator<Item = (TabId, &SessionTab)> {
        self.state
            .tabs()
            .iter()
            .filter_map(|tab| match &tab.content {
                TabContent::Session(session) => Some((tab.id, session.as_ref())),
                _ => None,
            })
    }

    pub(crate) fn set_automation_settings(&mut self, snapshot: SettingsSnapshot) -> bool {
        let changed = !self.automation_settings.same_presentation(&snapshot);
        self.automation_settings = snapshot;
        changed
    }

    pub(crate) fn take_automation_command(&mut self) -> Option<NativeCommand> {
        self.automation_command.take()
    }

    pub(crate) fn queue_automation_command(&mut self, command: NativeCommand) {
        match &command {
            NativeCommand::RestartMcp => self.automation_settings.disable(),
            NativeCommand::Revoke { channel } => self.automation_settings.revoke(*channel),
            NativeCommand::ClearWorkflow => self.automation_settings.workflow.disable(),
            NativeCommand::ApproveObservation { .. } | NativeCommand::CaptureWorkflow(_) => {}
        }
        self.automation_command = Some(command);
    }

    #[cfg(test)]
    pub(crate) fn automation_fixture() -> (Self, TabId) {
        let mut app = Self::for_test_with_configuration(festerm_config::Configuration::empty());
        let id = app.state.replace_active_with_test_ssh_session(
            crate::session_controller::fake::FakeSshSession::new([]),
            "user",
            "automation.example.test",
            22,
        );
        app.state
            .session_tab_mut(id)
            .unwrap()
            .terminal
            .ingest(b"approved source");
        (app, id)
    }

    #[cfg(test)]
    pub(crate) fn automation_session_mut(&mut self, id: TabId) -> &mut SessionTab {
        self.state.session_tab_mut(id).unwrap()
    }

    #[cfg(test)]
    pub(crate) fn automation_replace_fixture_session(&mut self) -> TabId {
        self.state.replace_active_with_test_ssh_session(
            crate::session_controller::fake::FakeSshSession::new([]),
            "other",
            "replacement.example.test",
            22,
        )
    }
}
