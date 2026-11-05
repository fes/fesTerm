use std::{cell::RefCell, rc::Rc};

use festerm_config::{ConfigError, Configuration};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Feature {
    Mcp,
    Javascript,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Values {
    pub(crate) mcp: bool,
    pub(crate) javascript: bool,
}

#[derive(Clone)]
pub(crate) struct Shared(Rc<RefCell<State>>);

struct State {
    values: Values,
    disabled_mcp_this_pass: bool,
    disabled_javascript_this_pass: bool,
    mcp_ui_revision: u64,
    javascript_ui_revision: u64,
    ui_revision_exhausted: bool,
    stopped_after_save_failure: bool,
    unconfirmed: bool,
    conflicted: bool,
}

impl Shared {
    pub(crate) fn new(values: Values) -> Self {
        Self(Rc::new(RefCell::new(State {
            values,
            disabled_mcp_this_pass: false,
            disabled_javascript_this_pass: false,
            mcp_ui_revision: 0,
            javascript_ui_revision: 0,
            ui_revision_exhausted: false,
            stopped_after_save_failure: false,
            unconfirmed: false,
            conflicted: false,
        })))
    }

    pub(crate) fn values(&self) -> Values {
        self.0.borrow().values
    }

    pub(crate) fn effective(&self) -> Values {
        let state = self.0.borrow();
        if state.stopped_after_save_failure || state.ui_revision_exhausted {
            Values {
                mcp: false,
                javascript: false,
            }
        } else {
            state.values
        }
    }

    pub(crate) fn toggle(&self, feature: Feature) -> Option<Values> {
        let mut state = self.0.borrow_mut();
        let before = state.values;
        let (enabled, disabled_this_pass, revision) = match feature {
            Feature::Mcp => (
                before.mcp,
                state.disabled_mcp_this_pass,
                state.mcp_ui_revision,
            ),
            Feature::Javascript => (
                before.javascript,
                state.disabled_javascript_this_pass,
                state.javascript_ui_revision,
            ),
        };
        if !enabled
            && (state.stopped_after_save_failure
                || state.ui_revision_exhausted
                || disabled_this_pass)
        {
            state.conflicted = true;
            return None;
        }
        let next_revision = revision.checked_add(1);
        if next_revision.is_none() {
            state.ui_revision_exhausted = true;
            if !enabled {
                state.conflicted = true;
                return None;
            }
        }
        match feature {
            Feature::Mcp => {
                state.values.mcp = !enabled;
                state.disabled_mcp_this_pass |= enabled;
                state.mcp_ui_revision = next_revision.unwrap_or(revision);
            }
            Feature::Javascript => {
                state.values.javascript = !enabled;
                state.disabled_javascript_this_pass |= enabled;
                state.javascript_ui_revision = next_revision.unwrap_or(revision);
            }
        }
        state.conflicted = false;
        Some(before)
    }

    pub(crate) fn reset(&self) {
        let mut state = self.0.borrow_mut();
        let next_mcp = state.mcp_ui_revision.checked_add(1);
        let next_javascript = state.javascript_ui_revision.checked_add(1);
        state.ui_revision_exhausted |= next_mcp.is_none() || next_javascript.is_none();
        state.mcp_ui_revision = next_mcp.unwrap_or(state.mcp_ui_revision);
        state.javascript_ui_revision = next_javascript.unwrap_or(state.javascript_ui_revision);
        state.disabled_mcp_this_pass = true;
        state.disabled_javascript_this_pass = true;
        state.values = Values {
            mcp: false,
            javascript: false,
        };
        state.conflicted = false;
    }

    pub(crate) fn save_failed(&self, before: Values) {
        let mut state = self.0.borrow_mut();
        // A failed enable may already have reached disk, but must not be
        // admitted in this process or copied by a later unrelated save.
        state.values.mcp &= before.mcp;
        state.values.javascript &= before.javascript;
        state.stopped_after_save_failure = true;
        state.unconfirmed = true;
    }

    pub(crate) fn explicit_save_succeeded(&self) {
        let mut state = self.0.borrow_mut();
        state.unconfirmed = false;
        state.conflicted = false;
    }

    pub(crate) fn action_conflicted(&self) {
        self.0.borrow_mut().conflicted = true;
    }

    pub(crate) fn clear_action_conflict(&self) {
        self.0.borrow_mut().conflicted = false;
    }

    pub(crate) fn retry_available(&self) -> bool {
        self.0.borrow().unconfirmed
    }

    pub(crate) fn ui_revision(&self, feature: Feature) -> u64 {
        let state = self.0.borrow();
        match feature {
            Feature::Mcp => state.mcp_ui_revision,
            Feature::Javascript => state.javascript_ui_revision,
        }
    }

    pub(crate) fn feedback(&self) -> Option<&'static str> {
        let state = self.0.borrow();
        if state.unconfirmed {
            Some("Automation preferences could not be saved or their outcome is uncertain. MCP and JavaScript are stopped for this run; an enabled preference may remain on disk and be read after restart, including after a failed enable. Repair configuration access and retry saving before restarting.")
        } else if state.ui_revision_exhausted {
            Some("The automation preference interaction limit was reached. MCP and JavaScript are stopped until restart; review the saved settings before restarting.")
        } else if state.stopped_after_save_failure && state.conflicted {
            Some("MCP and JavaScript remain stopped until restart. The recent automation action conflicted or was not applied; review the saved settings before restarting.")
        } else if state.stopped_after_save_failure {
            Some("Automation preferences were saved. MCP and JavaScript remain stopped until restart; previous grants will not be restored.")
        } else if state.conflicted {
            Some("An automation action conflicted or overflowed and was blocked; MCP may be stopped and workflow previews cancelled. Settings changes may not have been applied. Review settings and retry explicitly.")
        } else {
            None
        }
    }

    pub(crate) fn finish_pass(&self) {
        let mut state = self.0.borrow_mut();
        state.disabled_mcp_this_pass = false;
        state.disabled_javascript_this_pass = false;
    }

    pub(crate) fn apply(
        &self,
        configuration: &Configuration,
    ) -> Result<Configuration, ConfigError> {
        let values = self.values();
        let settings = configuration
            .interface_settings()
            .clone()
            .with_mcp_enabled(values.mcp)
            .with_javascript_enabled(values.javascript);
        configuration.with_interface_settings(settings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exhausted_preference_ui_revision_fails_closed_but_allows_disable() {
        let preferences = Shared::new(Values {
            mcp: true,
            javascript: true,
        });
        preferences.0.borrow_mut().mcp_ui_revision = u64::MAX;
        assert!(preferences.toggle(Feature::Mcp).is_some());
        assert!(!preferences.values().mcp);
        assert_eq!(
            preferences.effective(),
            Values {
                mcp: false,
                javascript: false
            }
        );
        preferences.finish_pass();
        assert!(preferences.toggle(Feature::Mcp).is_none());
        assert!(preferences
            .feedback()
            .unwrap()
            .contains("limit was reached"));
        assert!(preferences.toggle(Feature::Javascript).is_some());
        assert_eq!(
            preferences.values(),
            Values {
                mcp: false,
                javascript: false
            }
        );
    }
}
