use std::collections::BTreeSet;

use festerm_recording::{ExportDestination, RecordingSettings};

const RECONNECT_MESSAGE: &str = "A live recording session is unavailable. Close this sheet, connect or reconnect, and reopen recording; these options will be kept for this attempt.";

#[derive(Default)]
pub(super) struct RecordingDraft {
    pub(super) settings: Option<RecordingSettings>,
    pub(super) include_new: bool,
    resume_after_picker: bool,
}

impl RecordingDraft {
    pub(super) fn open(&mut self, saved: Option<&RecordingSettings>) -> Result<(), &'static str> {
        if std::mem::take(&mut self.resume_after_picker) && self.settings.is_some() {
            return Ok(());
        }
        let settings =
            saved.ok_or("Recording settings are unavailable. Open Settings to resolve this.")?;
        self.settings = Some(settings.clone());
        self.include_new = false;
        Ok(())
    }

    pub(super) fn finish_picker(
        &mut self,
        result: Result<Option<ExportDestination>, String>,
        needs_reconnection: bool,
    ) -> Option<String> {
        let status = match result {
            Ok(Some(destination)) => match self.settings.as_mut() {
                Some(settings) => {
                    settings.destination = Some(destination);
                    None
                }
                None => Some("The recording options are no longer available.".to_owned()),
            },
            Ok(None) => None,
            Err(error) => Some(error),
        };
        if !needs_reconnection || self.settings.is_none() {
            return status;
        }
        self.resume_after_picker = true;
        Some(match status {
            Some(status) => format!("{status} {RECONNECT_MESSAGE}"),
            None => RECONNECT_MESSAGE.to_owned(),
        })
    }
}

pub(super) fn selected_sessions_are_live(
    selected: &BTreeSet<u64>,
    sessions: &[(u64, String)],
) -> bool {
    selected
        .iter()
        .all(|id| sessions.iter().any(|(live, _)| live == id))
}

pub(super) fn picker_needs_reconnection(
    selected: &BTreeSet<u64>,
    sessions: &[(u64, String)],
) -> bool {
    sessions.is_empty() || !selected_sessions_are_live(selected, sessions)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picker_disconnect_preserves_only_the_current_recording_draft() {
        let saved = RecordingSettings::default();
        let mut draft = RecordingDraft::default();
        draft.open(Some(&saved)).unwrap();
        draft.include_new = true;
        let destination = ExportDestination::AndroidTree("content://fixture/tree".to_owned());
        let selected = BTreeSet::from([42]);
        let live = Vec::new();
        let status = draft
            .finish_picker(
                Ok(Some(destination.clone())),
                picker_needs_reconnection(&selected, &live),
            )
            .unwrap();
        assert!(status.contains("reconnect"));

        draft.open(Some(&saved)).unwrap();
        assert_eq!(
            draft.settings.as_ref().unwrap().destination,
            Some(destination)
        );
        assert!(draft.include_new);
        assert!(saved.destination.is_none());

        draft.open(Some(&saved)).unwrap();
        assert_eq!(draft.settings.as_ref(), Some(&saved));
        assert!(!draft.include_new);
    }

    #[test]
    fn picker_cancellation_reports_session_loss_and_errors_are_not_hidden() {
        let saved = RecordingSettings::default();
        let mut draft = RecordingDraft::default();
        draft.open(Some(&saved)).unwrap();
        draft.settings.as_mut().unwrap().normalize_typing = true;
        let status = draft
            .finish_picker(Err("Folder permission failed.".to_owned()), true)
            .unwrap();
        assert!(status.contains("Folder permission failed."));
        assert!(status.contains("reconnect"));
        draft.open(Some(&saved)).unwrap();
        assert!(draft.settings.as_ref().unwrap().normalize_typing);

        let status = draft.finish_picker(Ok(None), true).unwrap();
        assert!(status.contains("reconnect"));
        draft.open(Some(&saved)).unwrap();
        assert!(draft.settings.as_ref().unwrap().normalize_typing);

        assert!(draft.finish_picker(Ok(None), false).is_none());
        draft.open(Some(&saved)).unwrap();
        assert_eq!(draft.settings.as_ref(), Some(&saved));
    }

    #[test]
    fn selection_is_valid_only_while_every_selected_session_is_live() {
        let selected = BTreeSet::from([1, 2]);
        let sessions = vec![(1, "first".to_owned()), (2, "second".to_owned())];
        assert!(selected_sessions_are_live(&selected, &sessions));
        assert!(!selected_sessions_are_live(&selected, &sessions[..1]));
        assert!(picker_needs_reconnection(&selected, &sessions[..1]));
        assert!(picker_needs_reconnection(&BTreeSet::new(), &[]));
        assert!(!picker_needs_reconnection(&BTreeSet::new(), &sessions));
    }
}
