//! Application-owned transient overlays and notices (#53 candidate seam:
//! "transient overlays and notices").
//!
//! Groups the confirmation prompts that must resolve before other terminal
//! input proceeds (close/paste/settings-reset), the in-flight native
//! secure-storage lookup, and the best-effort transient status banner into
//! one [`OverlayState`] instead of five separately maintained
//! `FesTermApp` fields. This is a data/query extraction only: rendering and
//! dispatch for these prompts still live on `FesTermApp` in `app.rs`, since
//! they reach into session/tab state that this module intentionally does
//! not own (see `docs/adr` ownership boundaries referenced from `app.rs`).

use std::{sync::mpsc, time::Instant};

use festerm_secret_store::{SecretReference, SecretStore, SecretStoreError};

use crate::tabs::TabId;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CloseConsequence {
    TerminateLocalProcess,
    DisconnectSsh,
}

impl CloseConsequence {
    pub(crate) const fn message(self) -> &'static str {
        match self {
            Self::TerminateLocalProcess => {
                "The local process will be terminated and its terminal history discarded."
            }
            Self::DisconnectSsh => {
                "The SSH connection will be disconnected and its terminal history discarded."
            }
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct PendingCloseConfirmation {
    pub(crate) tab: TabId,
    pub(crate) identity: String,
    pub(crate) consequence: CloseConsequence,
    pub(crate) lifecycle_generation: u64,
    pub(crate) restore_tab: TabId,
    pub(crate) cancel_focus_requested: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct PendingPasteConfirmation {
    pub(crate) tab: TabId,
    pub(crate) identity: String,
    pub(crate) text: String,
    pub(crate) transport_state: &'static str,
    pub(crate) lifecycle_generation: u64,
    pub(crate) bracketed_paste: bool,
    pub(crate) cancel_focus_requested: bool,
}

/// Confirmation shown only when resetting would actually discard a change
/// from defaults (`docs/gui-action-graph.md` SET-02).
#[derive(Clone, Debug)]
pub(crate) struct PendingSettingsResetConfirmation {
    pub(crate) cancel_focus_requested: bool,
}

pub(crate) struct PendingPasswordStore {
    pub(crate) receiver: mpsc::Receiver<Result<SecretReference, SecretStoreError>>,
    pub(crate) profile_id: String,
    pub(crate) options: festerm_ssh::SshSessionOptions,
    pub(crate) store: std::sync::Arc<dyn SecretStore>,
    /// Whether the profile should auto-launch once the credential finishes
    /// saving. True for the live-connect form's "remember password"
    /// checkbox (`AppCommand::StoreSshPassword`); false when the password is
    /// entered directly in the Profiles editor
    /// (`AppCommand::StoreProfilePassword`), which has no session to launch.
    pub(crate) launch_after_store: bool,
}

/// The confirmation prompts, in-flight secure-storage lookup, and transient
/// notice banner that can be active at once. `FesTermApp` holds exactly one
/// of these instead of five separate `Option` fields.
#[derive(Default)]
pub(crate) struct OverlayState {
    pub(crate) pending_close: Option<PendingCloseConfirmation>,
    pub(crate) pending_paste: Option<PendingPasteConfirmation>,
    pub(crate) pending_settings_reset: Option<PendingSettingsResetConfirmation>,
    pub(crate) pending_password_store: Option<PendingPasswordStore>,
    pub(crate) transient_notice: Option<(String, Instant)>,
    /// The About modal is open. Like the confirmation prompts above (and
    /// unlike the transient notice/password-store lookup), it is a
    /// full-backdrop modal that must intercept terminal input.
    pub(crate) about_open: bool,
    /// The About modal's licenses section is expanded. Only meaningful
    /// while `about_open` is true; kept alongside it rather than as a
    /// separate `FesTermApp` field.
    pub(crate) about_licenses_open: bool,
}

impl OverlayState {
    /// True while a destructive confirmation dialog or the About modal is
    /// open and must intercept terminal keyboard/pointer input, native menu
    /// commands, and most application shortcuts. Replaces the repeated
    /// `pending_close.is_some() || pending_paste.is_some() ||
    /// pending_settings_reset.is_some() || about_open` checks that were
    /// previously duplicated at several call sites in `app.rs`.
    ///
    /// Deliberately excludes `pending_password_store` and
    /// `transient_notice`: the secure-storage lookup runs in the
    /// background without a modal backdrop, and the transient notice is a
    /// passive banner, so neither blocks terminal input.
    pub(crate) fn blocks_terminal_input(&self) -> bool {
        self.pending_close.is_some()
            || self.pending_paste.is_some()
            || self.pending_settings_reset.is_some()
            || self.about_open
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_terminal_input_is_false_with_no_pending_overlays() {
        assert!(!OverlayState::default().blocks_terminal_input());
    }

    #[test]
    fn blocks_terminal_input_is_true_while_a_confirmation_is_pending() {
        let overlays = OverlayState {
            pending_settings_reset: Some(PendingSettingsResetConfirmation {
                cancel_focus_requested: false,
            }),
            ..OverlayState::default()
        };
        assert!(overlays.blocks_terminal_input());
    }

    #[test]
    fn blocks_terminal_input_ignores_password_store_and_transient_notice() {
        // A background secure-storage lookup and a passive status banner
        // must not intercept terminal input the way a destructive
        // confirmation dialog does.
        let overlays = OverlayState {
            transient_notice: Some(("notice".to_owned(), Instant::now())),
            ..OverlayState::default()
        };
        assert!(!overlays.blocks_terminal_input());
    }

    #[test]
    fn blocks_terminal_input_is_true_while_the_about_modal_is_open() {
        let overlays = OverlayState {
            about_open: true,
            ..OverlayState::default()
        };
        assert!(overlays.blocks_terminal_input());
    }
}
