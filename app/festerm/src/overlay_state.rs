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

use std::{path::PathBuf, sync::mpsc, time::Instant};

use festerm_document::DocumentId;
use festerm_secret_store::{SecretReference, SecretStore, SecretStoreError};

use crate::{
    port_forward_draft::PortForwardDraft as LivePortForwardDraft,
    sftp_file_manager::MarkdownFilePicker, tabs::TabId,
};

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
    pub(crate) clipboard_token: Option<u64>,
    pub(crate) opened_frame: u64,
    pub(crate) tab: TabId,
    pub(crate) identity: String,
    pub(crate) text: String,
    pub(crate) transport_state: &'static str,
    pub(crate) lifecycle_generation: u64,
    pub(crate) input_ownership_epoch: u64,
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

/// The prompt raised when closing the **final** view of a document that is
/// holding unsaved changes (ADR 0034 §7).
///
/// It carries the document's name and fully qualified origin because it can be
/// raised from a background window or by a quit that is closing several tabs at
/// once: a prompt that only said "Save changes?" could be answered for the
/// wrong file. It also carries the document identity rather than only the tab,
/// so a document released or saved while the prompt is up is noticed rather
/// than answered for.
#[derive(Clone, Debug)]
pub(crate) struct PendingDocumentCloseConfirmation {
    pub(crate) tab: TabId,
    pub(crate) document: DocumentId,
    /// The file's name, as the heading asks about it.
    pub(crate) title: String,
    /// The fully qualified origin, shown under the question in monospace.
    pub(crate) origin: String,
    pub(crate) restore_tab: TabId,
    /// Set once Save has been given focus, so the focus request happens on the
    /// frame the prompt opens and not on every frame after it.
    pub(crate) save_focus_requested: bool,
    /// What to do once this document is dealt with: a quit or window close
    /// closing several dirty documents asks about each in turn.
    pub(crate) then: AfterDocumentClose,
}

/// What raised a dirty-close prompt, and therefore what happens after it is
/// answered.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AfterDocumentClose {
    /// One tab was being closed; nothing else follows.
    CloseTab,
    /// A window close or quit was interrupted; resume it once every dirty
    /// document has been answered for.
    ResumeClose(QuitConfirmationPurpose),
}

#[derive(Clone, Debug)]
pub(crate) struct LivePortForwardManager {
    pub(crate) tab: TabId,
    pub(crate) draft: LivePortForwardDraft,
    pub(crate) error: Option<String>,
    pub(crate) request_focus: bool,
}

impl LivePortForwardManager {
    pub(crate) fn new(tab: TabId) -> Self {
        Self {
            tab,
            draft: LivePortForwardDraft::default(),
            error: None,
            request_focus: true,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum QuitConfirmationPurpose {
    Quit,
    /// Closing one additional window (ADR 0033) rather than the application:
    /// only that window's own sessions are at stake, so it gets its own
    /// wording and leaves every other window running.
    CloseWindow,
    InstallUpdate,
}

/// Aggregate confirmation shown once, for the whole application, before an
/// action that will close every live session. Deliberately summarizes exact
/// counts instead of per-session identity, unlike [`PendingCloseConfirmation`].
#[derive(Clone, Copy, Debug)]
pub(crate) struct PendingQuitConfirmation {
    pub(crate) counts: crate::tabs::LiveSessionCounts,
    pub(crate) cancel_focus_requested: bool,
    pub(crate) purpose: QuitConfirmationPurpose,
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
    pub(crate) store: std::sync::Arc<dyn SecretStore>,
    /// Whether the profile should auto-launch once the credential finishes
    /// saving, and if so as which remote session kind.
    pub(crate) launch_after_store: Option<StoredCredentialLaunch>,
    /// Which kind of secret was just stored, so the saved profile's
    /// `credential_kind` metadata matches what was actually written.
    pub(crate) credential_kind: festerm_config::CredentialKind,
}

pub(crate) enum StoredCredentialLaunch {
    Ssh(festerm_ssh::SshSessionOptions),
    Sftp,
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
    /// The final-view dirty-close prompt for a text document (ADR 0034 §7).
    pub(crate) pending_document_close: Option<PendingDocumentCloseConfirmation>,
    pub(crate) port_forward_manager: Option<LivePortForwardManager>,
    pub(crate) pending_quit: Option<PendingQuitConfirmation>,
    pub(crate) pending_password_store: Option<PendingPasswordStore>,
    /// The "Open Markdown File…" picker (#132), reusing the SFTP file
    /// manager's local-pane browsing widget instead of an OS-native file
    /// dialog.
    pub(crate) markdown_file_picker: Option<MarkdownFilePicker>,
    /// Which Markdown viewer tab the open picker should retarget when it
    /// resolves, set when the picker was opened from inside a viewer
    /// (`Ctrl+O`). `None` means "open the picked file in a new tab".
    pub(crate) markdown_file_picker_replaces: Option<TabId>,
    /// The directory the last "Open Markdown File…" picker was browsing when
    /// it closed. The next picker resumes here instead of starting over at
    /// the home directory, which is the behaviour users expect from a file
    /// dialog when opening several files from the same folder.
    pub(crate) markdown_file_picker_directory: Option<PathBuf>,
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
        self.pending_paste.is_some() || self.blocks_terminal_input_except_paste()
    }

    pub(crate) fn blocks_terminal_input_except_paste(&self) -> bool {
        self.pending_close.is_some()
            || self.pending_document_close.is_some()
            || self.pending_file_drop.is_some()
            || self.pending_settings_reset.is_some()
            || self.port_forward_manager.is_some()
            || self.pending_quit.is_some()
            || self.markdown_file_picker.is_some()
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
                purpose: QuitConfirmationPurpose::Quit,
            }),
            ..OverlayState::default()
        };
        assert!(overlays.blocks_terminal_input());
    }

    #[test]
    fn blocks_terminal_input_is_true_while_port_forward_manager_is_open() {
        let overlays = OverlayState {
            port_forward_manager: Some(LivePortForwardManager::new(
                crate::tabs::AppState::for_test().active(),
            )),
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
            purpose: QuitConfirmationPurpose::Quit,
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
            purpose: QuitConfirmationPurpose::Quit,
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
            purpose: QuitConfirmationPurpose::Quit,
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
