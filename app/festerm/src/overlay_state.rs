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

/// A bounded confirmation shown before inserting file paths dropped onto a
/// local live session (`docs/gui-design.md` "Drag-and-drop input"). fesTerm
/// has no reliably known per-profile shell family yet, so this is always
/// shown rather than ever silently guessing PowerShell/POSIX-shell/`cmd.exe`
/// quoting - the same "otherwise" fallback the design doc specifies.
#[derive(Clone, Debug)]
pub(crate) struct PendingFileDropConfirmation {
    pub(crate) tab: TabId,
    pub(crate) identity: String,
    /// The literal, unquoted, space-joined absolute paths in drop order -
    /// exactly what gets inserted as one ordered `Paste` input operation on
    /// confirmation. Never auto-sent with a trailing Enter.
    pub(crate) text: String,
    pub(crate) path_count: usize,
    pub(crate) lifecycle_generation: u64,
    pub(crate) cancel_focus_requested: bool,
}

/// Confirmation shown only when resetting would actually discard a change
/// from defaults (`docs/gui-action-graph.md` SET-02).
#[derive(Clone, Debug)]
pub(crate) struct PendingSettingsResetConfirmation {
    pub(crate) cancel_focus_requested: bool,
}

/// Aggregate confirmation shown once, for the whole application, when the
/// OS requests that the window close while any session still has something
/// to lose (`docs/gui-design.md` "Closing sessions and quitting",
/// `docs/gui-action-graph.md` `QUIT-01`/`QUIT-02`). Deliberately summarizes
/// exact counts instead of per-session identity, unlike
/// [`PendingCloseConfirmation`] - fesTerm has exactly one native window, so
/// this same dialog serves both the window-close button and "Quit fesTerm".
#[derive(Clone, Copy, Debug)]
pub(crate) struct PendingQuitConfirmation {
    pub(crate) counts: crate::tabs::LiveSessionCounts,
    pub(crate) cancel_focus_requested: bool,
}

impl PendingQuitConfirmation {
    /// A plain-language summary of exactly what will be discarded, e.g.
    /// "1 local process, 2 SSH connections, and 1 serial device are still
    /// open." Singular/plural nouns are chosen per count so the message
    /// never reads oddly for the common one-session case.
    pub(crate) fn summary_message(&self) -> String {
        fn phrase(count: usize, singular: &str, plural: &str) -> Option<String> {
            match count {
                0 => None,
                1 => Some(format!("1 {singular}")),
                n => Some(format!("{n} {plural}")),
            }
        }
        let parts: Vec<String> = [
            phrase(self.counts.local, "local process", "local processes"),
            phrase(self.counts.ssh, "SSH connection", "SSH connections"),
            phrase(self.counts.serial, "serial device", "serial devices"),
        ]
        .into_iter()
        .flatten()
        .collect();
        let joined = match parts.as_slice() {
            [] => "0 sessions".to_owned(),
            [only] => only.clone(),
            [first, second] => format!("{first} and {second}"),
            [init @ .., last] => format!("{}, and {last}", init.join(", ")),
        };
        let verb = if self.counts.total() == 1 {
            "is"
        } else {
            "are"
        };
        format!("{joined} {verb} still open.")
    }
}

pub(crate) struct PendingPasswordStore {
    pub(crate) receiver: mpsc::Receiver<Result<SecretReference, SecretStoreError>>,
    pub(crate) profile_id: String,
    pub(crate) options: festerm_ssh::SshSessionOptions,
    pub(crate) store: std::sync::Arc<dyn SecretStore>,
    /// Whether the profile should auto-launch once the credential finishes
    /// saving. True for the live-connect form's "remember password"
    /// checkbox (`AppCommand::StoreSshPassword`); false when the credential
    /// is entered directly in the Profiles editor
    /// (`AppCommand::StoreProfilePassword`/`AppCommand::StoreProfilePrivateKey`),
    /// which has no session to launch.
    pub(crate) launch_after_store: bool,
    /// Which kind of secret was just stored, so the saved profile's
    /// `credential_kind` metadata matches what was actually written.
    pub(crate) credential_kind: festerm_config::CredentialKind,
}

/// The confirmation prompts, in-flight secure-storage lookup, and transient
/// notice banner that can be active at once. `FesTermApp` holds exactly one
/// of these instead of five separate `Option` fields.
#[derive(Default)]
pub(crate) struct OverlayState {
    pub(crate) pending_close: Option<PendingCloseConfirmation>,
    pub(crate) pending_paste: Option<PendingPasteConfirmation>,
    pub(crate) pending_file_drop: Option<PendingFileDropConfirmation>,
    pub(crate) pending_settings_reset: Option<PendingSettingsResetConfirmation>,
    pub(crate) pending_quit: Option<PendingQuitConfirmation>,
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
            || self.pending_file_drop.is_some()
            || self.pending_settings_reset.is_some()
            || self.pending_quit.is_some()
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
    fn blocks_terminal_input_is_true_while_quit_is_pending() {
        let overlays = OverlayState {
            pending_quit: Some(PendingQuitConfirmation {
                counts: crate::tabs::LiveSessionCounts {
                    local: 1,
                    ssh: 0,
                    serial: 0,
                },
                cancel_focus_requested: false,
            }),
            ..OverlayState::default()
        };
        assert!(overlays.blocks_terminal_input());
    }

    #[test]
    fn quit_summary_uses_singular_nouns_and_verb_for_exactly_one_session() {
        let pending = PendingQuitConfirmation {
            counts: crate::tabs::LiveSessionCounts {
                local: 1,
                ssh: 0,
                serial: 0,
            },
            cancel_focus_requested: false,
        };
        assert_eq!(pending.summary_message(), "1 local process is still open.");
    }

    #[test]
    fn quit_summary_lists_every_nonzero_transport_with_oxford_comma() {
        let pending = PendingQuitConfirmation {
            counts: crate::tabs::LiveSessionCounts {
                local: 1,
                ssh: 2,
                serial: 1,
            },
            cancel_focus_requested: false,
        };
        assert_eq!(
            pending.summary_message(),
            "1 local process, 2 SSH connections, and 1 serial device are still open."
        );
    }

    #[test]
    fn quit_summary_joins_exactly_two_transports_with_and_only() {
        let pending = PendingQuitConfirmation {
            counts: crate::tabs::LiveSessionCounts {
                local: 0,
                ssh: 3,
                serial: 1,
            },
            cancel_focus_requested: false,
        };
        assert_eq!(
            pending.summary_message(),
            "3 SSH connections and 1 serial device are still open."
        );
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
