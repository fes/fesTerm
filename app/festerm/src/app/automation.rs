use crate::{
    automation::{
        preferences::{Feature, Shared, Values},
        NativeCommand, SettingsSnapshot, MAX_NATIVE_COMMANDS,
    },
    tabs::{AppCommand, SessionTab, TabContent, TabId},
};
use eframe::egui;

use super::FesTermApp;

impl FesTermApp {
    pub(crate) fn set_automation_window(&mut self, window: crate::application::WindowId) {
        self.automation_window = window;
    }

    pub(crate) fn automation_window(&self) -> crate::application::WindowId {
        self.automation_window
    }

    pub(crate) fn set_presentation_workspace(
        &mut self,
        revision: &crate::automation::WorkspaceRevision,
    ) {
        self.state.set_presentation_workspace(revision);
    }

    pub(crate) fn activate_presentation_target(
        &mut self,
        id: TabId,
        context: &eframe::egui::Context,
    ) -> bool {
        if self.state.session_tab(id).is_none() {
            return false;
        }
        self.state.dispatch(AppCommand::ActivateTab(id), context);
        self.state.active() == id
    }

    pub(crate) fn initialize_automation_preferences(&mut self) {
        self.automation_preferences();
    }

    #[cfg(test)]
    pub(crate) fn toggle_automation_preference_for_test(
        &mut self,
        feature: Feature,
        context: &egui::Context,
    ) {
        self.toggle_automation_preference(feature, context);
    }

    #[cfg(test)]
    pub(crate) fn save_unrelated_interface_setting_for_test(
        &mut self,
        context: &egui::Context,
    ) -> bool {
        self.state
            .dispatch(crate::tabs::AppCommand::ToggleStatusBar, context);
        self.persist_interface_settings()
    }

    #[cfg(test)]
    pub(crate) fn retry_automation_preferences_for_test(&mut self, context: &egui::Context) {
        self.retry_automation_preferences(context);
    }

    #[cfg(test)]
    pub(crate) fn automation_preference_feedback_for_test(&self) -> Option<&'static str> {
        self.automation_preference_feedback()
    }

    #[cfg(test)]
    pub(crate) fn automation_preference_retry_available_for_test(&self) -> bool {
        self.automation_preference_retry_available()
    }

    #[cfg(test)]
    pub(crate) fn reset_automation_preferences_for_test(&mut self, context: &egui::Context) {
        self.state
            .dispatch(crate::tabs::AppCommand::ResetAllSettings, context);
        let before = self.reset_automation_preferences(context);
        self.persist_reset_automation_preferences(before);
    }

    #[cfg(test)]
    pub(crate) fn request_reset_all_settings_for_test(&mut self, context: &egui::Context) {
        self.request_reset_all_settings(context);
    }

    fn automation_preferences(&mut self) -> Shared {
        if let Some(preferences) = self.configuration_reloader.automation_preferences() {
            return preferences;
        }
        let preferences = Shared::new(Values {
            mcp: self.state.mcp_enabled(),
            javascript: self.state.javascript_enabled(),
        });
        self.configuration_reloader
            .set_automation_preferences(preferences.clone());
        preferences
    }

    pub(crate) fn sync_automation_preferences(&mut self) {
        if let Some(preferences) = self.configuration_reloader.automation_preferences() {
            let values = preferences.values();
            let settings = self.state.configuration().interface_settings();
            if settings.mcp_enabled() != values.mcp
                || settings.javascript_enabled() != values.javascript
            {
                match preferences.apply(self.state.configuration()) {
                    Ok(configuration) => self.state.replace_configuration(configuration),
                    Err(_) => self.queue_automation_command(NativeCommand::SafetyStop),
                }
            }
            self.state
                .set_automation_preferences(values.mcp, values.javascript);
        }
    }

    pub(crate) fn finish_automation_preference_pass(&self) {
        if let Some(preferences) = self.configuration_reloader.automation_preferences() {
            preferences.finish_pass();
        }
    }

    pub(super) fn automation_preference_feedback(&self) -> Option<&'static str> {
        use crate::configuration_startup::SaveGateState;
        let preferences = self.configuration_reloader.automation_preferences()?;
        if preferences.retry_available() {
            match self.configuration_reloader.save_gate_state() {
                SaveGateState::ReconciliationRequired => return Some("Automation preferences could not be saved or their outcome is uncertain. MCP and JavaScript are stopped for this run; an enabled preference may remain on disk. Configuration may have changed on disk; reconcile manually and restart all windows."),
                SaveGateState::InProgress => return Some("Automation preferences could not be saved or their outcome is uncertain. MCP and JavaScript are stopped for this run; an enabled preference may remain on disk. Another configuration save is in progress; wait for its outcome before taking further action."),
                SaveGateState::Healthy => {}
            }
        }
        preferences.feedback()
    }

    pub(super) fn automation_preference_retry_available(&self) -> bool {
        self.configuration_reloader
            .automation_preferences()
            .is_some_and(|preferences| preferences.retry_available())
            && self.configuration_reloader.save_gate_state()
                == crate::configuration_startup::SaveGateState::Healthy
    }

    pub(super) fn automation_preference_ui_revision(&self, feature: Feature) -> u64 {
        self.configuration_reloader
            .automation_preferences()
            .map_or(0, |preferences| preferences.ui_revision(feature))
    }

    pub(crate) fn report_automation_action_conflict(&self) {
        if let Some(preferences) = self.configuration_reloader.automation_preferences() {
            preferences.action_conflicted();
        }
    }

    fn finish_automation_preference_save(&mut self, before: Values, saved: bool) {
        let preferences = self.automation_preferences();
        if saved {
            preferences.explicit_save_succeeded();
        } else {
            preferences.save_failed(before);
            self.queue_automation_command(NativeCommand::SafetyStop);
            self.sync_automation_preferences();
        }
    }

    pub(super) fn toggle_automation_preference(
        &mut self,
        feature: Feature,
        context: &egui::Context,
    ) {
        let preferences = self.automation_preferences();
        let Some(before) = preferences.toggle(feature) else {
            self.queue_automation_command(NativeCommand::SafetyStop);
            context.request_repaint();
            return;
        };
        let effective = preferences.effective();
        if !effective.mcp {
            self.automation_settings.disable();
        }
        if !effective.javascript {
            self.automation_settings.workflow.disable();
            self.cancel_automation_workflow_picker(context);
        }
        self.sync_automation_preferences();
        self.queue_automation_command(NativeCommand::PreferenceChanged {
            clear_workflow: before.javascript && !effective.javascript,
            reenable_workflow: feature == Feature::Javascript
                && !before.javascript
                && effective.javascript,
        });
        let saved = self.persist_interface_settings();
        self.finish_automation_preference_save(before, saved);
        context.request_repaint();
    }

    pub(super) fn reset_automation_preferences(&mut self, context: &egui::Context) -> Values {
        let preferences = self.automation_preferences();
        let before = preferences.values();
        preferences.reset();
        self.automation_settings.disable();
        self.automation_settings.workflow.disable();
        self.cancel_automation_workflow_picker(context);
        self.sync_automation_preferences();
        self.queue_automation_command(NativeCommand::PreferenceChanged {
            clear_workflow: true,
            reenable_workflow: false,
        });
        before
    }

    pub(super) fn persist_reset_automation_preferences(&mut self, before: Values) {
        let saved = self.persist_interface_settings();
        self.finish_automation_preference_save(before, saved);
    }

    pub(super) fn retry_automation_preferences(&mut self, context: &egui::Context) {
        let preferences = self.automation_preferences();
        if !self.automation_preference_retry_available() {
            context.request_repaint();
            return;
        }
        let before = preferences.values();
        self.sync_automation_preferences();
        self.automation_settings.disable();
        self.automation_settings.workflow.disable();
        self.cancel_automation_workflow_picker(context);
        self.queue_automation_command(NativeCommand::PreferenceChanged {
            clear_workflow: true,
            reenable_workflow: false,
        });
        let saved = self.persist_interface_settings();
        self.finish_automation_preference_save(before, saved);
        context.request_repaint();
    }

    pub(crate) fn automation_mcp_enabled(&self) -> bool {
        self.configuration_reloader
            .automation_preferences()
            .map_or_else(
                || self.state.mcp_enabled(),
                |preferences| preferences.effective().mcp,
            )
    }

    pub(crate) fn automation_javascript_enabled(&self) -> bool {
        self.configuration_reloader
            .automation_preferences()
            .map_or_else(
                || self.state.javascript_enabled(),
                |preferences| preferences.effective().javascript,
            )
    }

    pub(crate) fn automation_session(&self, id: TabId) -> Option<&SessionTab> {
        self.state.session_tab(id)
    }

    pub(crate) fn automation_control_session_mut(&mut self, id: TabId) -> Option<&mut SessionTab> {
        self.state.session_tab_mut(id)
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

    pub(crate) fn automation_session_context(&self, id: TabId) -> Option<(usize, bool)> {
        let position = self.state.tabs().iter().position(|tab| tab.id == id)?;
        Some((position + 1, self.state.active() == id))
    }

    pub(crate) fn set_automation_settings(&mut self, snapshot: SettingsSnapshot) -> bool {
        let changed = !self.automation_settings.same_presentation(&snapshot);
        self.automation_settings = snapshot;
        changed
    }

    pub(crate) fn cancel_automation_workflow_picker(&mut self, context: &egui::Context) {
        if self.overlays.workflow_file_picker {
            self.close_markdown_file_picker(context);
        }
    }

    pub(crate) fn take_automation_commands(&mut self) -> (Vec<NativeCommand>, bool) {
        (
            std::mem::take(&mut self.automation_commands),
            std::mem::take(&mut self.automation_command_overflow),
        )
    }

    #[cfg(test)]
    pub(crate) fn take_automation_command(&mut self) -> Option<NativeCommand> {
        if self.automation_commands.is_empty() {
            None
        } else {
            Some(self.automation_commands.remove(0))
        }
    }

    pub(crate) fn queue_automation_command(&mut self, command: NativeCommand) {
        match &command {
            NativeCommand::SafetyStop => {
                self.report_automation_action_conflict();
                self.automation_settings.disable();
                self.automation_settings.workflow.disable();
                self.automation_commands.clear();
                self.automation_command_overflow = true;
                return;
            }
            NativeCommand::RestartMcp => {
                self.automation_settings.disable();
                if let (true, Some(preferences)) = (
                    !self.automation_command_overflow && self.automation_commands.is_empty(),
                    self.configuration_reloader.automation_preferences(),
                ) {
                    preferences.clear_action_conflict();
                }
            }
            NativeCommand::PreferenceChanged { clear_workflow, .. } => {
                if *clear_workflow {
                    self.automation_settings.workflow.disable();
                }
            }
            NativeCommand::Revoke { channel } => self.automation_settings.revoke(*channel),
            NativeCommand::UnregisterController {
                channel,
                registration,
            } => self
                .automation_settings
                .unregister_controller(*channel, *registration),
            NativeCommand::CancelControllerMetadata {
                channel,
                registration,
                review,
            } => self.automation_settings.cancel_controller_metadata_review(
                *channel,
                *registration,
                *review,
            ),
            NativeCommand::CancelControllerInput {
                channel,
                registration,
                input,
            } => self.automation_settings.cancel_controller_input_review(
                *channel,
                *registration,
                *input,
            ),
            NativeCommand::ClearWorkflow => self.automation_settings.workflow.disable(),
            NativeCommand::ApproveObservation { .. }
            | NativeCommand::CompareDirectPeer { .. }
            | NativeCommand::DenyDirectPeer { .. }
            | NativeCommand::EnableControllerEnrollment
            | NativeCommand::ChooseController { .. }
            | NativeCommand::SelectControllerTarget { .. }
            | NativeCommand::ConfirmController { .. }
            | NativeCommand::CancelController { .. }
            | NativeCommand::ReviewControllerMetadata { .. }
            | NativeCommand::ConfirmControllerMetadata { .. }
            | NativeCommand::RevokeControllerMetadata { .. }
            | NativeCommand::ReviewControllerInput { .. }
            | NativeCommand::ConfirmControllerInput { .. }
            | NativeCommand::DisarmControllerInput { .. }
            | NativeCommand::CaptureWorkflow(_)
            | NativeCommand::StartWorkflowMonitoring
            | NativeCommand::StartWorkflowRun { .. }
            | NativeCommand::CancelWorkflowRun { .. }
            | NativeCommand::ApproveWorkflowInput { .. }
            | NativeCommand::DenyWorkflowInput { .. } => {}
        }
        if self.automation_command_overflow {
            return;
        }
        if self.automation_commands.len() >= MAX_NATIVE_COMMANDS {
            self.report_automation_action_conflict();
            self.automation_settings.disable();
            self.automation_settings.workflow.disable();
            self.automation_commands.clear();
            self.automation_command_overflow = true;
        } else {
            self.automation_commands.push(command);
        }
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
    pub(crate) fn automation_local_fixture() -> (Self, TabId) {
        let (mut app, id) = Self::automation_fixture();
        let session = app.state.session_tab_mut(id).unwrap();
        session.controller = crate::session_controller::SessionController::for_test(
            crate::tabs::ApplicationSession::TestLocal(
                crate::session_controller::fake::FakeSshSession::new([]),
            ),
        );
        session
            .controller
            .set_lifecycle_for_test(festerm_session::SessionLifecycle::Running);
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
