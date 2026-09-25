//! Document status: what the banner says, which commands can run, and what
//! Auto-save is actually doing (ADR 0034 §6, §7, §8).
//!
//! Status is *derived*, never stored, so the banner, the disabled Save button,
//! the Auto-save checkbox, the tab chip, and the status bar cannot drift apart
//! and tell the user three different stories about one document.

use std::fmt;

/// Whether the document's backing source can be reached.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Availability {
    /// The source is present and reachable.
    Available,
    /// A remote origin has dropped. The buffer stays editable and saving waits
    /// for revalidation (ADR 0034 §6).
    Offline,
    /// The source is gone or cannot be used. The buffer is kept.
    Unavailable(UnavailableReason),
}

impl Availability {
    pub const fn is_available(&self) -> bool {
        matches!(self, Self::Available)
    }
}

/// Why a source can no longer be written to.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnavailableReason {
    /// Deleted or renamed out from under the document.
    Missing,
    /// Present but no longer readable or writable by this user.
    PermissionDenied,
    /// Present but no longer a regular text file.
    NotAFile,
}

/// A detected divergence between the buffer and the source (ADR 0034 §6).
///
/// Holding the source text is what lets Compare open without a second fetch,
/// and what lets `Reload` apply exactly the bytes the conflict was raised for
/// rather than whatever the file happens to hold a minute later.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConflictState {
    source_text: Option<String>,
    detail: String,
}

impl ConflictState {
    pub fn new(detail: impl Into<String>) -> Self {
        Self {
            source_text: None,
            detail: detail.into(),
        }
    }

    /// Records the source's content, making Compare and Reload available.
    pub fn with_source_text(mut self, text: impl Into<String>) -> Self {
        self.source_text = Some(text.into());
        self
    }

    pub fn source_text(&self) -> Option<&str> {
        self.source_text.as_deref()
    }

    pub fn detail(&self) -> &str {
        &self.detail
    }

    /// Compare needs both versions; without the source it is offered disabled
    /// with a reason rather than opening an empty pane (ADR 0034 §6).
    pub const fn can_compare(&self) -> bool {
        self.source_text.is_some()
    }
}

/// Whether a write is in flight.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SaveProgress {
    Idle,
    InFlight,
}

/// A failed write, in words the user can act on.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SaveError {
    headline: String,
    detail: String,
}

impl SaveError {
    pub fn new(headline: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            headline: headline.into(),
            detail: detail.into(),
        }
    }

    pub fn headline(&self) -> &str {
        &self.headline
    }

    pub fn detail(&self) -> &str {
        &self.detail
    }
}

impl fmt::Display for SaveError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} — {}", self.headline, self.detail)
    }
}

impl std::error::Error for SaveError {}

/// How a completed write ended.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SaveOutcome {
    /// The bytes are on the source, and the recorded generation is the new one.
    Saved,
    /// The source changed under the document; nothing was written.
    Conflict(ConflictState),
    /// The source is gone or unusable; nothing was written.
    Unavailable(UnavailableReason),
    /// The write failed for any other reason; nothing was written.
    Failed(SaveError),
}

/// How severe a document's current state is (ADR 0034 §8).
#[derive(Clone, Copy, Debug, Eq, PartialEq, PartialOrd, Ord)]
pub enum Severity {
    /// Ordinary state: saved, unsaved, saving.
    Informational,
    /// The buffer is safe but something about the source is not true any more.
    Warning,
    /// The user must choose before this document can be written.
    Blocking,
}

/// An action a banner offers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BannerAction {
    Compare,
    ReloadFromSource,
    KeepMyVersion,
    SaveAs,
    CloseWithoutSaving,
    Retry,
}

impl BannerAction {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Compare => "Compare",
            Self::ReloadFromSource => "Reload",
            Self::KeepMyVersion => "Keep my version",
            Self::SaveAs => "Save As…",
            Self::CloseWithoutSaving => "Close without saving",
            Self::Retry => "Retry",
        }
    }
}

/// What the Auto-save control should show.
///
/// The distinction between `Paused` and `Unavailable` is ADR 0034 §8's rule: a
/// recoverable interruption keeps the user's standing intent, while a source
/// that can never be written again must not leave a checkbox on.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AutoSaveControl {
    Off,
    On,
    Paused,
    Unavailable,
}

impl AutoSaveControl {
    /// Whether the checkbox is drawn ticked.
    pub const fn checked(self) -> bool {
        matches!(self, Self::On | Self::Paused)
    }

    pub const fn enabled(self) -> bool {
        !matches!(self, Self::Unavailable)
    }

    /// Whether a debounced write may actually start.
    pub const fn writes(self) -> bool {
        matches!(self, Self::On)
    }

    /// The control's own words for its state. A paused Auto-save says so in
    /// text rather than relying on a tick that is on but not writing, which
    /// would be indistinguishable from one that is (ADR 0034 §8).
    pub const fn label(self) -> &'static str {
        match self {
            Self::Off | Self::On => "Auto-save",
            Self::Paused => "Auto-save · paused",
            Self::Unavailable => "Auto-save · unavailable",
        }
    }
}

/// Everything status is derived from.
#[derive(Clone, Debug)]
pub struct StatusInputs {
    pub dirty: bool,
    pub save: SaveProgress,
    pub availability: Availability,
    pub conflict: Option<ConflictState>,
    /// Whether the user has asked for Auto-save, regardless of whether it can
    /// currently run.
    pub auto_save_requested: bool,
    pub last_error: Option<SaveError>,
    /// Whether the origin is remote, which only changes the wording.
    pub remote: bool,
    /// Whether Save already has a concrete destination instead of needing
    /// Save As to choose one first.
    pub has_save_target: bool,
    /// Set briefly after an outside change was taken up by a clean document,
    /// so every view says so instead of silently showing different text than
    /// the reader last looked at (ADR 0034 §6).
    pub recently_reloaded: bool,
}

impl Default for StatusInputs {
    fn default() -> Self {
        Self {
            dirty: false,
            save: SaveProgress::Idle,
            availability: Availability::Available,
            conflict: None,
            auto_save_requested: false,
            last_error: None,
            remote: false,
            has_save_target: true,
            recently_reloaded: false,
        }
    }
}

/// What the banner's accent bar is saying at a glance, before a word is read.
/// Severity cannot carry this on its own: a saved document and a document with
/// unsaved changes are both merely informational, yet one is settled and the
/// other is waiting on the user.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StatusAccent {
    /// Everything is where it belongs.
    Settled,
    /// Something is in hand: typed but unsaved, or a save in flight.
    Working,
    /// The document still stands, but something around it does not.
    Warning,
    /// The document cannot be saved as things are.
    Failing,
}

/// The derived, presentable state of one document.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DocumentStatus {
    severity: Severity,
    accent: StatusAccent,
    headline: String,
    detail: String,
    actions: Vec<BannerAction>,
    can_save: bool,
    auto_save: AutoSaveControl,
    /// The short phrase the tab chip's accessible name is suffixed with.
    chip_state: &'static str,
    /// The same state as a standalone phrase for the status bar, where it is
    /// read on its own rather than after a file name.
    short_label: &'static str,
    /// Whether Compare has a source version to compare against. A conflict
    /// whose source could not be read still offers the action, disabled with
    /// a reason, rather than silently dropping it (ADR 0034 §6).
    can_compare: bool,
}

impl DocumentStatus {
    /// Derives the one true status. Order matters: the most severe true thing
    /// is what the user is shown.
    pub fn derive(inputs: &StatusInputs) -> Self {
        let where_it_is = if inputs.remote {
            "the remote host"
        } else {
            "disk"
        };

        if let Some(conflict) = &inputs.conflict {
            let mut actions = vec![BannerAction::Compare];
            actions.extend([
                BannerAction::ReloadFromSource,
                BannerAction::SaveAs,
                BannerAction::KeepMyVersion,
            ]);
            return Self {
                severity: Severity::Blocking,
                headline: format!("This file changed on {where_it_is}"),
                detail: conflict.detail().to_owned(),
                actions,
                can_save: false,
                auto_save: pause_or_keep(inputs.auto_save_requested),
                accent: StatusAccent::Failing,
                chip_state: "in conflict",
                short_label: "Conflict",
                can_compare: conflict.can_compare(),
            };
        }

        if let Availability::Unavailable(reason) = &inputs.availability {
            let (headline, detail) = match reason {
                UnavailableReason::Missing => (
                    format!("This file no longer exists on {where_it_is}"),
                    "It was deleted or renamed outside fesTerm. Your text is kept here. Auto-save is paused."
                        .to_owned(),
                ),
                UnavailableReason::PermissionDenied => (
                    "This file can no longer be written".to_owned(),
                    "Its permissions changed outside fesTerm. Your text is kept here. Auto-save is paused."
                        .to_owned(),
                ),
                UnavailableReason::NotAFile => (
                    "This file is no longer a text file".to_owned(),
                    "It was replaced outside fesTerm. Your text is kept here. Auto-save is paused."
                        .to_owned(),
                ),
            };
            return Self {
                severity: Severity::Warning,
                headline,
                detail,
                actions: vec![BannerAction::SaveAs, BannerAction::CloseWithoutSaving],
                can_save: false,
                auto_save: AutoSaveControl::Unavailable,
                accent: StatusAccent::Warning,
                chip_state: "source unavailable",
                short_label: "Source unavailable",
                can_compare: false,
            };
        }

        if inputs.availability == Availability::Offline {
            return Self {
                severity: Severity::Warning,
                headline: "Offline".to_owned(),
                detail:
                    "The connection to this host dropped. Keep editing; saving resumes once it reconnects."
                        .to_owned(),
                actions: vec![BannerAction::SaveAs],
                can_save: false,
                auto_save: pause_or_keep(inputs.auto_save_requested),
                accent: StatusAccent::Warning,
                chip_state: "offline",
            short_label: "Offline",
            can_compare: false,
            };
        }

        if let Some(error) = &inputs.last_error {
            return Self {
                severity: Severity::Warning,
                headline: error.headline().to_owned(),
                detail: error.detail().to_owned(),
                actions: vec![BannerAction::Retry, BannerAction::SaveAs],
                can_save: true,
                auto_save: auto_save_control(inputs),
                accent: StatusAccent::Warning,
                chip_state: "not saved",
                short_label: "Not saved",
                can_compare: false,
            };
        }

        if inputs.save == SaveProgress::InFlight {
            return Self {
                severity: Severity::Informational,
                headline: "Saving…".to_owned(),
                detail: format!("Writing your changes to {where_it_is}."),
                actions: Vec::new(),
                can_save: false,
                auto_save: auto_save_idle(inputs.auto_save_requested),
                accent: StatusAccent::Working,
                chip_state: "saving",
                short_label: "Saving",
                can_compare: false,
            };
        }

        if inputs.dirty {
            let detail = if !inputs.has_save_target {
                "Save will ask where to write this document. Changes remain shared with every open view.".to_owned()
            } else if inputs.auto_save_requested {
                "Auto-save is on. Changes are shared with every open view.".to_owned()
            } else {
                "Auto-save is off. Changes remain shared with other open views.".to_owned()
            };
            return Self {
                severity: Severity::Informational,
                headline: "Unsaved changes".to_owned(),
                detail,
                actions: Vec::new(),
                can_save: true,
                auto_save: auto_save_control(inputs),
                accent: StatusAccent::Working,
                chip_state: "unsaved",
                short_label: "Unsaved changes",
                can_compare: false,
            };
        }

        if inputs.recently_reloaded {
            return Self {
                severity: Severity::Informational,
                accent: StatusAccent::Working,
                headline: format!("Reloaded from {where_it_is}"),
                detail: "This file changed outside fesTerm. Every open view is showing the \
                         new version."
                    .to_owned(),
                actions: Vec::new(),
                can_save: false,
                auto_save: auto_save_control(inputs),
                chip_state: "reloaded",
                short_label: "Reloaded",
                can_compare: false,
            };
        }

        Self {
            severity: Severity::Informational,
            accent: StatusAccent::Settled,
            headline: "Saved".to_owned(),
            detail: format!("All changes are on {where_it_is}."),
            actions: Vec::new(),
            // Saving a clean document would rewrite bytes nobody changed, and
            // would make the file's modification time lie.
            can_save: false,
            auto_save: auto_save_control(inputs),
            chip_state: "saved",
            short_label: "Saved",
            can_compare: false,
        }
    }

    pub const fn severity(&self) -> Severity {
        self.severity
    }

    pub const fn accent(&self) -> StatusAccent {
        self.accent
    }

    pub fn headline(&self) -> &str {
        &self.headline
    }

    pub fn detail(&self) -> &str {
        &self.detail
    }

    pub fn actions(&self) -> &[BannerAction] {
        &self.actions
    }

    pub const fn can_save(&self) -> bool {
        self.can_save
    }

    /// Whether Compare can actually show two versions.
    pub const fn can_compare(&self) -> bool {
        self.can_compare
    }

    /// Why Compare is disabled, for the button that cannot be pressed. Said
    /// rather than left to be guessed at (ADR 0034 §8).
    pub const fn compare_unavailable_reason(&self) -> &'static str {
        "The version on the source could not be read."
    }

    pub const fn auto_save(&self) -> AutoSaveControl {
        self.auto_save
    }

    /// Save As is the escape hatch from every state, so it is never disabled
    /// (ADR 0034 §3).
    pub const fn can_save_as(&self) -> bool {
        true
    }

    /// The suffix on the chip's accessible name, so document state is legible
    /// without seeing the dot at all (ADR 0034 §8).
    pub const fn chip_state(&self) -> &'static str {
        self.chip_state
    }

    /// The same state as a standalone phrase, for the status bar, where it is
    /// read on its own rather than after a file name.
    pub const fn short_label(&self) -> &'static str {
        self.short_label
    }
}

const fn pause_or_keep(requested: bool) -> AutoSaveControl {
    if requested {
        AutoSaveControl::Paused
    } else {
        AutoSaveControl::Off
    }
}

const fn auto_save_idle(requested: bool) -> AutoSaveControl {
    if requested {
        AutoSaveControl::On
    } else {
        AutoSaveControl::Off
    }
}

const fn auto_save_control(inputs: &StatusInputs) -> AutoSaveControl {
    if !inputs.has_save_target {
        AutoSaveControl::Unavailable
    } else {
        auto_save_idle(inputs.auto_save_requested)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn remote_inputs() -> StatusInputs {
        StatusInputs {
            remote: true,
            ..StatusInputs::default()
        }
    }

    #[test]
    fn a_clean_document_is_saved_and_cannot_be_saved_again() {
        let status = DocumentStatus::derive(&remote_inputs());
        assert_eq!(status.headline(), "Saved");
        assert_eq!(status.detail(), "All changes are on the remote host.");
        assert_eq!(status.severity(), Severity::Informational);
        assert!(!status.can_save());
        assert!(status.actions().is_empty());
        assert_eq!(status.chip_state(), "saved");
        assert_eq!(status.short_label(), "Saved");
    }

    #[test]
    fn a_local_document_says_disk_rather_than_the_remote_host() {
        let status = DocumentStatus::derive(&StatusInputs::default());
        assert_eq!(status.detail(), "All changes are on disk.");
    }

    #[test]
    fn a_settled_document_and_an_edited_one_accent_differently() {
        let clean = DocumentStatus::derive(&StatusInputs::default());
        let dirty = DocumentStatus::derive(&StatusInputs {
            dirty: true,
            ..StatusInputs::default()
        });

        assert_eq!(clean.accent(), StatusAccent::Settled);
        assert_eq!(dirty.accent(), StatusAccent::Working);
        // Severity alone cannot tell these apart, which is why accent exists.
        assert_eq!(clean.severity(), dirty.severity());
    }

    #[test]
    fn a_dirty_document_offers_save_and_names_auto_saves_state() {
        let status = DocumentStatus::derive(&StatusInputs {
            dirty: true,
            ..remote_inputs()
        });
        assert_eq!(status.headline(), "Unsaved changes");
        assert_eq!(
            status.detail(),
            "Auto-save is off. Changes remain shared with other open views."
        );
        assert!(status.can_save());
        assert_eq!(status.auto_save(), AutoSaveControl::Off);
        assert_eq!(status.chip_state(), "unsaved");
        assert_eq!(status.short_label(), "Unsaved changes");
    }

    #[test]
    fn an_untitled_dirty_document_routes_save_and_disables_auto_save() {
        let status = DocumentStatus::derive(&StatusInputs {
            dirty: true,
            auto_save_requested: true,
            has_save_target: false,
            ..StatusInputs::default()
        });

        assert!(status.can_save());
        assert_eq!(status.auto_save(), AutoSaveControl::Unavailable);
        assert_eq!(
            status.detail(),
            "Save will ask where to write this document. Changes remain shared with every open view."
        );
    }

    #[test]
    fn a_save_in_flight_is_truthful_and_blocks_a_second_save() {
        let status = DocumentStatus::derive(&StatusInputs {
            dirty: true,
            save: SaveProgress::InFlight,
            ..remote_inputs()
        });
        assert_eq!(status.headline(), "Saving…");
        assert!(!status.can_save());
        assert_eq!(status.chip_state(), "saving");
    }

    #[test]
    fn a_conflict_blocks_saving_and_offers_all_four_recoveries() {
        let status = DocumentStatus::derive(&StatusInputs {
            dirty: true,
            auto_save_requested: true,
            conflict: Some(ConflictState::new(
                "Auto-save is paused. Compare versions before deciding which content to keep.",
            )),
            ..remote_inputs()
        });
        assert_eq!(status.severity(), Severity::Blocking);
        assert_eq!(status.headline(), "This file changed on the remote host");
        assert_eq!(
            status.actions(),
            [
                BannerAction::Compare,
                BannerAction::ReloadFromSource,
                BannerAction::SaveAs,
                BannerAction::KeepMyVersion,
            ]
        );
        assert!(!status.can_save());
        assert_eq!(status.chip_state(), "in conflict");
    }

    #[test]
    fn a_conflict_pauses_auto_save_but_keeps_the_users_intent() {
        let status = DocumentStatus::derive(&StatusInputs {
            dirty: true,
            auto_save_requested: true,
            conflict: Some(ConflictState::new("detail")),
            ..remote_inputs()
        });
        assert_eq!(status.auto_save(), AutoSaveControl::Paused);
        assert!(status.auto_save().checked());
        assert!(!status.auto_save().writes());
        assert!(status.auto_save().enabled());
    }

    #[test]
    fn an_unavailable_source_turns_auto_save_off_and_disables_it() {
        let status = DocumentStatus::derive(&StatusInputs {
            dirty: true,
            auto_save_requested: true,
            availability: Availability::Unavailable(UnavailableReason::Missing),
            ..remote_inputs()
        });
        assert_eq!(status.severity(), Severity::Warning);
        assert_eq!(
            status.headline(),
            "This file no longer exists on the remote host"
        );
        assert_eq!(
            status.detail(),
            "It was deleted or renamed outside fesTerm. Your text is kept here. Auto-save is paused."
        );
        assert_eq!(status.auto_save(), AutoSaveControl::Unavailable);
        assert!(!status.auto_save().checked());
        assert!(!status.auto_save().enabled());
        assert!(!status.can_save());
        assert_eq!(
            status.actions(),
            [BannerAction::SaveAs, BannerAction::CloseWithoutSaving]
        );
        assert_eq!(status.chip_state(), "source unavailable");
    }

    #[test]
    fn every_unavailable_reason_keeps_the_buffer_and_offers_save_as() {
        for reason in [
            UnavailableReason::Missing,
            UnavailableReason::PermissionDenied,
            UnavailableReason::NotAFile,
        ] {
            let status = DocumentStatus::derive(&StatusInputs {
                availability: Availability::Unavailable(reason),
                ..remote_inputs()
            });
            assert!(status.actions().contains(&BannerAction::SaveAs));
            assert!(status.can_save_as());
            assert!(!status.can_save());
        }
    }

    #[test]
    fn an_offline_remote_stays_editable_and_pauses_saving() {
        let status = DocumentStatus::derive(&StatusInputs {
            dirty: true,
            auto_save_requested: true,
            availability: Availability::Offline,
            ..remote_inputs()
        });
        assert_eq!(status.severity(), Severity::Warning);
        assert_eq!(status.headline(), "Offline");
        assert_eq!(status.auto_save(), AutoSaveControl::Paused);
        assert!(!status.can_save());
        assert_eq!(status.chip_state(), "offline");
    }

    #[test]
    fn a_failed_write_is_persistent_actionable_and_never_claims_saved() {
        let status = DocumentStatus::derive(&StatusInputs {
            dirty: true,
            last_error: Some(SaveError::new(
                "Could not save NOTES.md",
                "The remote host refused the write.",
            )),
            ..remote_inputs()
        });
        assert_eq!(status.severity(), Severity::Warning);
        assert_eq!(status.headline(), "Could not save NOTES.md");
        assert_eq!(
            status.actions(),
            [BannerAction::Retry, BannerAction::SaveAs]
        );
        assert!(status.can_save());
        assert_eq!(status.chip_state(), "not saved");
    }

    #[test]
    fn a_conflict_outranks_an_error_and_an_unavailable_source() {
        let status = DocumentStatus::derive(&StatusInputs {
            dirty: true,
            conflict: Some(ConflictState::new("detail")),
            availability: Availability::Unavailable(UnavailableReason::Missing),
            last_error: Some(SaveError::new("headline", "detail")),
            ..remote_inputs()
        });
        assert_eq!(status.severity(), Severity::Blocking);
    }

    #[test]
    fn compare_needs_the_source_text() {
        let without = ConflictState::new("detail");
        assert!(!without.can_compare());
        assert_eq!(without.source_text(), None);
        let with = without.with_source_text("on disk\n");
        assert!(with.can_compare());
        assert_eq!(with.source_text(), Some("on disk\n"));
    }

    #[test]
    fn every_banner_action_has_the_label_the_mockups_use() {
        assert_eq!(BannerAction::Compare.label(), "Compare");
        assert_eq!(BannerAction::ReloadFromSource.label(), "Reload");
        assert_eq!(BannerAction::KeepMyVersion.label(), "Keep my version");
        assert_eq!(BannerAction::SaveAs.label(), "Save As…");
        assert_eq!(
            BannerAction::CloseWithoutSaving.label(),
            "Close without saving"
        );
    }

    #[test]
    fn no_status_is_expressed_by_colour_alone() {
        let states = [
            StatusInputs::default(),
            StatusInputs {
                dirty: true,
                ..StatusInputs::default()
            },
            StatusInputs {
                save: SaveProgress::InFlight,
                ..StatusInputs::default()
            },
            StatusInputs {
                conflict: Some(ConflictState::new("detail")),
                ..StatusInputs::default()
            },
            StatusInputs {
                availability: Availability::Offline,
                ..StatusInputs::default()
            },
            StatusInputs {
                availability: Availability::Unavailable(UnavailableReason::Missing),
                ..StatusInputs::default()
            },
        ];
        for inputs in states {
            let status = DocumentStatus::derive(&inputs);
            assert!(!status.headline().is_empty());
            assert!(!status.detail().is_empty());
            assert!(!status.chip_state().is_empty());
            assert!(!status.short_label().is_empty());
        }
    }
}
