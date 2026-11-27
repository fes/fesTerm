//! One explicit, side-bound deletion review. No handles or authority enter UI state.
use super::*;
use tokio_util::sync::CancellationToken;

pub(crate) type Reply<T> = mpsc::Receiver<Result<T, String>>;
/// Display facts from the current review, never a source of deletion authority.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ReviewDetail {
    File(Option<u64>),
    EmptyFolder,
}
#[derive(Debug)]
pub(crate) struct Review {
    pub id: u64,
    pub names: Vec<String>,
    pub details: Option<Vec<ReviewDetail>>,
}
#[derive(Debug)]
pub(crate) struct Outcome {
    pub confirmed: usize,
    pub total: usize,
    pub unknown: bool,
    pub failed: bool,
    pub recovery: Option<String>,
}
enum Pending {
    Review(Reply<Review>),
    Commit(Reply<Outcome>),
}
pub(crate) struct Draft {
    local: bool,
    path: String,
    generation: u64,
    selected: BTreeSet<String>,
    stop: CancellationToken,
    pending: Option<Pending>,
    pub review: Option<Review>,
}
impl Draft {
    pub(crate) fn local(&self) -> bool {
        self.local
    }
    pub(crate) fn path(&self) -> &str {
        &self.path
    }
    pub(crate) fn committing(&self) -> bool {
        matches!(self.pending, Some(Pending::Commit(_)))
    }
    pub(crate) fn ready(&self) -> bool {
        self.pending.is_none()
            && !self.stop.is_cancelled()
            && self.review.as_ref().is_some_and(|review| {
                review.details.as_ref().is_some_and(|details| {
                    details.len() == review.names.len() && !details.is_empty()
                })
            })
    }
}
impl Drop for Draft {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}
impl FilesSession {
    pub(crate) fn clear_retired_delete_recovery(&mut self) {
        if !self.local_deletion_available() {
            self.delete_recovery = None;
        }
    }
    pub(crate) fn deletion_recovery(&self) -> Option<&str> {
        self.local_deletion_available()
            .then_some(self.delete_recovery.as_deref())
            .flatten()
    }
    pub(crate) fn deletion_result(&self) -> Option<&str> {
        self.delete_result.as_deref()
    }
    pub(crate) fn deletion_available(&self) -> bool {
        self.is_ready()
            && self.remote_mutations_available()
            && self
                .transfers
                .as_ref()
                .is_some_and(|t| t.delete_available())
    }
    pub(crate) fn local_deletion_available(&self) -> bool {
        self.is_ready()
            && self
                .transfers
                .as_ref()
                .is_some_and(|t| t.local_delete_available())
    }
    pub(crate) fn can_review_delete(&self) -> bool {
        self.can_review_delete_side(false)
    }
    pub(crate) fn can_review_delete_side(&self, local: bool) -> bool {
        (if local {
            self.local_deletion_available()
        } else {
            self.deletion_available()
        }) && self.delete_paths(local).is_some()
    }
    fn delete_paths(&self, local: bool) -> Option<Vec<String>> {
        let pane = if local {
            self.local.as_ref()
        } else {
            self.remote.as_ref()
        }?;
        if pane.loading()
            || pane.error.is_some()
            || pane.selected.is_empty()
            || pane.selected.len() > 8
        {
            return None;
        }
        let mut paths = Vec::new();
        for name in &pane.selected {
            let entry = pane
                .listing
                .as_ref()?
                .entries
                .iter()
                .find(|e| &e.name == name)?;
            if !entry.name_safe
                || entry.symlink.is_some()
                || (!local && entry.permissions.is_some_and(|bits| bits & 0o222 == 0))
                || !matches!(entry.kind, EntryKind::File | EntryKind::Directory)
            {
                return None;
            }
            paths.push(pane.fs.join(&pane.path, name).ok()?);
        }
        Some(paths)
    }
    fn delete_current(&self, draft: &Draft) -> bool {
        (if draft.local {
            self.local_deletion_available()
        } else {
            self.deletion_available()
        }) && (if draft.local {
            self.local.as_ref()
        } else {
            self.remote.as_ref()
        })
        .is_some_and(|pane| {
            pane.path == draft.path
                && pane.generation == draft.generation
                && pane.selected == draft.selected
                && !pane.loading()
                && pane.error.is_none()
                && pane.listing.is_some()
        })
    }
    pub(crate) fn review_delete(&mut self) {
        self.review_delete_side(false);
    }
    pub(crate) fn review_delete_side(&mut self, local: bool) {
        if !self.can_review_delete_side(local) || self.deletion.is_some() {
            return;
        }
        let paths = self.delete_paths(local).expect("validated selection");
        let stop = CancellationToken::new();
        let transfers = self.transfers.as_ref().expect("ready transfers");
        match if local {
            transfers.review_local_delete(paths, stop.clone())
        } else {
            transfers.review_delete(paths, stop.clone())
        } {
            Ok(reply) => {
                let pane = if local {
                    self.local.as_ref().expect("validated local pane")
                } else {
                    self.remote.as_ref().expect("validated remote pane")
                };
                self.folder = None;
                self.delete_result = None;
                self.delete_recovery = None;
                self.notice = None;
                self.transfers_sheet = false;
                self.deletion = Some(Draft {
                    local,
                    path: pane.path.clone(),
                    generation: pane.generation,
                    selected: pane.selected.clone(),
                    stop,
                    pending: Some(Pending::Review(reply)),
                    review: None,
                });
            }
            Err(message) => self.notice = Some(message),
        }
    }
    pub(crate) fn cancel_delete(&mut self) {
        if let Some(draft) = self.deletion.take() {
            draft.stop.cancel();
            if let Some(transfers) = &self.transfers {
                if draft.local {
                    transfers.cancel_local_delete();
                } else {
                    transfers.cancel_delete();
                }
            }
        }
    }
    pub(crate) fn commit_delete(&mut self) {
        let Some(draft) = self.deletion.as_ref() else {
            return;
        };
        if draft.committing() || !draft.ready() {
            return;
        }
        if !self.delete_current(draft) {
            self.cancel_delete();
            return;
        }
        let id = draft.review.as_ref().expect("ready review").id;
        let transfers = self.transfers.as_ref().expect("ready transfers");
        match if draft.local {
            transfers.commit_local_delete(id)
        } else {
            transfers.commit_delete(id)
        } {
            Ok(reply) => {
                self.deletion.as_mut().expect("current draft").pending =
                    Some(Pending::Commit(reply))
            }
            Err(message) => {
                self.cancel_delete();
                self.notice = Some(message);
            }
        }
    }
    fn record_delete_result(&mut self, message: String) {
        self.delete_result = Some(message.clone());
        if !self.is_ready() {
            // An unknown mutation must remain visible even after the normal
            // Failed body replaces the connected panes and their notices.
            self.link = Link::Failed(format!("Connection closed. {message}"));
        }
        self.notice = Some(message);
    }
    pub(crate) fn poll_delete(&mut self, ctx: &egui::Context) {
        self.clear_retired_delete_recovery();
        let Some(draft) = self.deletion.as_ref() else {
            return;
        };
        if !self.delete_current(draft) {
            draft.stop.cancel();
            if !draft.committing() {
                self.cancel_delete();
                return;
            }
        }
        let Some(draft) = self.deletion.as_mut() else {
            return;
        };
        enum Received {
            Review(Result<Review, String>),
            Commit(Result<Outcome, String>),
        }
        let received = match &draft.pending {
            Some(Pending::Review(reply)) => match reply.try_recv() {
                Ok(value) => Some(Received::Review(value)),
                Err(mpsc::TryRecvError::Disconnected) => Some(Received::Review(Err("Deletion review closed".into()))),
                Err(mpsc::TryRecvError::Empty) => None,
            },
            Some(Pending::Commit(reply)) => match reply.try_recv() {
                Ok(value) => Some(Received::Commit(value)),
                Err(mpsc::TryRecvError::Disconnected) => Some(Received::Commit(Err("Deletion result was not observed. Inspect and refresh; do not retry automatically.".into()))),
                Err(mpsc::TryRecvError::Empty) => None,
            },
            None => return,
        };
        let Some(received) = received else {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
            return;
        };
        draft.pending = None;
        match received {
            Received::Review(Ok(review)) => draft.review = Some(review),
            Received::Review(Err(message)) => {
                self.cancel_delete();
                self.notice = Some(message);
            }
            Received::Commit(Err(message)) => {
                self.cancel_delete();
                self.record_delete_result(message);
            }
            Received::Commit(Ok(outcome)) => {
                let local = draft.local;
                let show_recovery = local && !draft.stop.is_cancelled();
                self.cancel_delete();
                let message = if outcome.unknown {
                    format!("{} of {} removals confirmed. A result is unknown. Inspect and refresh; do not retry automatically.", outcome.confirmed, outcome.total)
                } else if outcome.failed {
                    format!("{} of {} removals confirmed. Deletion stopped. Refresh before reviewing untouched entries.", outcome.confirmed, outcome.total)
                } else {
                    format!("{} entries deleted", outcome.confirmed)
                };
                self.delete_recovery = outcome
                    .recovery
                    .filter(|_| show_recovery)
                    .map(|path| festerm_sftp::path::display_name(&path));
                self.record_delete_result(message);
                if self.is_ready() {
                    if let Some(pane) = if local {
                        self.local.as_mut()
                    } else {
                        self.remote.as_mut()
                    } {
                        pane.selected.clear();
                        pane.refresh(ctx);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::files_fixture::{fixture_entry, FixtureFs};
    use std::sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Mutex,
    };

    struct Script {
        available: AtomicBool,
        remote_delete: AtomicBool,
        root_label: Mutex<String>,
        locations: Mutex<Option<Vec<RemoteLocation>>>,
        paths: Mutex<Vec<String>>,
        stop: Mutex<Option<CancellationToken>>,
        review_reply: Mutex<Option<mpsc::SyncSender<Result<Review, String>>>>,
        commit_reply: Mutex<Option<mpsc::SyncSender<Result<Outcome, String>>>>,
        commits: AtomicUsize,
    }
    impl Default for Script {
        fn default() -> Self {
            Self {
                available: AtomicBool::new(true),
                remote_delete: AtomicBool::new(true),
                root_label: Mutex::new("Shared folder".into()),
                locations: Mutex::default(),
                paths: Mutex::default(),
                stop: Mutex::default(),
                review_reply: Mutex::default(),
                commit_reply: Mutex::default(),
                commits: AtomicUsize::new(0),
            }
        }
    }
    impl Transfers for Script {
        fn remote_locations(&self) -> Option<Vec<RemoteLocation>> {
            self.locations.lock().unwrap().clone()
        }
        fn remote_root_label(&self) -> String {
            self.root_label.lock().unwrap().clone()
        }
        fn delete_available(&self) -> bool {
            self.available.load(Ordering::SeqCst) && self.remote_delete.load(Ordering::SeqCst)
        }
        fn review_delete(
            &self,
            paths: Vec<String>,
            stop: CancellationToken,
        ) -> Result<Reply<Review>, String> {
            *self.paths.lock().unwrap() = paths;
            *self.stop.lock().unwrap() = Some(stop);
            let (send, receive) = mpsc::sync_channel(1);
            *self.review_reply.lock().unwrap() = Some(send);
            Ok(receive)
        }
        fn commit_delete(&self, id: u64) -> Result<Reply<Outcome>, String> {
            assert_eq!(id, 51);
            self.commits.fetch_add(1, Ordering::SeqCst);
            let (send, receive) = mpsc::sync_channel(1);
            *self.commit_reply.lock().unwrap() = Some(send);
            Ok(receive)
        }
        fn local_delete_available(&self) -> bool {
            self.available.load(Ordering::SeqCst)
        }
        fn review_local_delete(
            &self,
            paths: Vec<String>,
            stop: CancellationToken,
        ) -> Result<Reply<Review>, String> {
            self.review_delete(paths, stop)
        }
        fn commit_local_delete(&self, id: u64) -> Result<Reply<Outcome>, String> {
            self.commit_delete(id)
        }
        fn connection_error(&self) -> Option<String> {
            (!self.available.load(Ordering::SeqCst)).then(|| "Connection closed".into())
        }
        fn rows(&self) -> Vec<TransferRow> {
            vec![]
        }
        fn enqueue(&self, _: Direction, _: &str, _: &str) -> Result<u64, String> {
            unreachable!()
        }
        fn cancel(&self, _: u64) {}
        fn resolve(&self, _: u64, _: ConflictPolicy, _: bool) {}
        fn clear_finished(&self) {}
    }
    fn fixture() -> (FilesSession, Arc<Script>, egui::Context) {
        let ctx = egui::Context::default();
        let script = Arc::new(Script::default());
        let fs = Arc::new(FixtureFs {
            kind: festerm_sftp::PaneKind::Remote,
            home: "/",
            entries: vec![
                fixture_entry("public.txt", false, 13, 1),
                fixture_entry("empty", true, 0, 1),
            ],
        });
        let mut pane = Pane::new(fs.clone(), "/".into());
        pane.listing = Some(fs.list("/", &ListOptions::default()).unwrap());
        pane.selected.insert("public.txt".into());
        let mut session = FilesSession::connecting(
            "Files".into(),
            "Dev Tunnels Files".into(),
            Box::new(super::super::NoConnector),
        );
        session.connector = None;
        session.remote = Some(pane);
        session.transfers = Some(script.clone());
        session.link = Link::Ready;
        (session, script, ctx)
    }
    fn prepared(session: &mut FilesSession, script: &Script, ctx: &egui::Context) {
        session.review_delete();
        assert_eq!(*script.paths.lock().unwrap(), ["/public.txt"]);
        script
            .review_reply
            .lock()
            .unwrap()
            .take()
            .unwrap()
            .send(Ok(Review {
                id: 51,
                names: vec!["public.txt".into()],
                details: Some(vec![ReviewDetail::File(Some(13))]),
            }))
            .unwrap();
        session.poll_delete(ctx);
        assert!(session.deletion.as_ref().unwrap().ready());
        assert_eq!(
            script.commits.load(Ordering::SeqCst),
            0,
            "review alone sends no mutation"
        );
    }
    #[test]
    fn files_delete_requires_capability_bounded_regular_selection_and_explicit_confirmation() {
        let (mut session, script, ctx) = fixture();
        script.available.store(false, Ordering::SeqCst);
        session.review_delete();
        assert!(session.deletion.is_none());
        script.available.store(true, Ordering::SeqCst);
        session
            .remote
            .as_mut()
            .unwrap()
            .selected
            .insert("missing".into());
        assert!(!session.can_review_delete());
        session.remote.as_mut().unwrap().selected.remove("missing");
        prepared(&mut session, &script, &ctx);
        session.commit_delete();
        session.commit_delete();
        assert_eq!(script.commits.load(Ordering::SeqCst), 1);
        script
            .commit_reply
            .lock()
            .unwrap()
            .take()
            .unwrap()
            .send(Ok(Outcome {
                confirmed: 1,
                total: 1,
                failed: false,
                recovery: None,
                unknown: false,
            }))
            .unwrap();
        session.poll_delete(&ctx);
        assert!(session.deletion.is_none());
        assert_eq!(session.notice.as_deref(), Some("1 entries deleted"));
    }
    #[test]
    fn files_delete_selection_refresh_and_authority_cutover_retire_review_before_commit() {
        for change in 0..3 {
            let (mut session, script, ctx) = fixture();
            prepared(&mut session, &script, &ctx);
            match change {
                0 => {
                    session.remote.as_mut().unwrap().selected.clear();
                }
                1 => {
                    session.remote.as_mut().unwrap().generation += 1;
                }
                _ => script.available.store(false, Ordering::SeqCst),
            }
            session.commit_delete();
            assert!(session.deletion.is_none());
            assert!(script.stop.lock().unwrap().as_ref().unwrap().is_cancelled());
            assert_eq!(script.commits.load(Ordering::SeqCst), 0);
        }
    }
    #[test]
    fn files_delete_cancel_pending_review_and_drop_never_commit() {
        let (mut session, script, ctx) = fixture();
        session.review_delete();
        let stop = script.stop.lock().unwrap().clone().unwrap();
        session.cancel_delete();
        assert!(stop.is_cancelled());
        assert!(script
            .review_reply
            .lock()
            .unwrap()
            .take()
            .unwrap()
            .send(Ok(Review {
                id: 51,
                names: vec![],
                details: None,
            }))
            .is_err());
        prepared(&mut session, &script, &ctx);
        drop(session);
        assert!(script.stop.lock().unwrap().as_ref().unwrap().is_cancelled());
        assert_eq!(script.commits.load(Ordering::SeqCst), 0);
    }
    #[test]
    fn files_delete_unknown_partial_result_is_retained_without_replay_after_retirement() {
        let (mut session, script, ctx) = fixture();
        prepared(&mut session, &script, &ctx);
        session.commit_delete();
        script.available.store(false, Ordering::SeqCst);
        session.poll(&ctx);
        assert!(script.stop.lock().unwrap().as_ref().unwrap().is_cancelled());
        script
            .commit_reply
            .lock()
            .unwrap()
            .take()
            .unwrap()
            .send(Ok(Outcome {
                confirmed: 1,
                total: 3,
                failed: true,
                recovery: None,
                unknown: true,
            }))
            .unwrap();
        session.poll_delete(&ctx);
        assert!(session
            .notice
            .as_ref()
            .unwrap()
            .contains("1 of 3 removals confirmed"));
        assert!(session
            .notice
            .as_ref()
            .unwrap()
            .contains("do not retry automatically"));
        assert!(
            matches!(&session.link, Link::Failed(message) if message.contains("1 of 3 removals confirmed") && message.contains("do not retry automatically"))
        );
        assert_eq!(script.commits.load(Ordering::SeqCst), 1);
        session.commit_delete();
        session.poll(&ctx);
        session.poll(&ctx);
        assert_eq!(
            session.link,
            Link::Failed("Connection closed".into()),
            "normal connection failure can refresh independently"
        );
        assert!(session
            .deletion_result()
            .unwrap()
            .contains("1 of 3 removals confirmed"));
        use egui_kittest::{kittest::Queryable, Harness};
        let mut harness = Harness::builder()
            .with_size(egui::vec2(390.0, 844.0))
            .build_ui_state(
                |ui, session: &mut FilesSession| {
                    crate::files_ui::show(ui, session);
                },
                session,
            );
        harness.run();
        harness.get_by_label("Last deletion: 1 of 3 removals confirmed. A result is unknown. Inspect and refresh; do not retry automatically.");
        assert_eq!(script.commits.load(Ordering::SeqCst), 1);
    }
    #[test]
    fn files_delete_remote_review_renders_fresh_kind_and_size_before_confirm() {
        use egui_kittest::{kittest::Queryable, Harness};
        for (detail, label) in [
            (ReviewDetail::EmptyFolder, "Empty folder"),
            (ReviewDetail::File(Some(31)), "File · 31 B"),
            (ReviewDetail::File(None), "File · Size unavailable"),
        ] {
            let (mut session, script, ctx) = fixture();
            // The cached row remains a 13-byte file. The fresh worker review
            // must determine what the confirmation sheet actually displays.
            session.review_delete();
            script
                .review_reply
                .lock()
                .unwrap()
                .take()
                .unwrap()
                .send(Ok(Review {
                    id: 51,
                    names: vec!["public.txt".into()],
                    details: Some(vec![detail]),
                }))
                .unwrap();
            session.poll_delete(&ctx);
            let mut harness = Harness::builder()
                .with_size(egui::vec2(390.0, 844.0))
                .build_ui_state(
                    |ui, session: &mut FilesSession| {
                        crate::files_ui::show(ui, session);
                    },
                    session,
                );
            harness.run_steps(3);
            assert!(!harness.state().deletion.as_ref().unwrap().local());
            harness.get_by_label(label);
            assert!(harness.state().deletion.as_ref().unwrap().ready());
            assert_eq!(script.commits.load(Ordering::SeqCst), 0);
            harness.get_by_label("Cancel").click();
            harness.run_steps(3);
            assert!(harness.state().deletion.is_none());
            assert_eq!(script.commits.load(Ordering::SeqCst), 0);
        }
    }
    #[test]
    fn files_delete_remote_incomplete_details_never_enable_confirmation() {
        for details in [
            None,
            Some(vec![]),
            Some(vec![ReviewDetail::EmptyFolder, ReviewDetail::EmptyFolder]),
        ] {
            let (mut session, script, ctx) = fixture();
            session.review_delete();
            script
                .review_reply
                .lock()
                .unwrap()
                .take()
                .unwrap()
                .send(Ok(Review {
                    id: 51,
                    names: vec!["public.txt".into()],
                    details,
                }))
                .unwrap();
            session.poll_delete(&ctx);
            assert!(!session.deletion.as_ref().unwrap().ready());
            session.commit_delete();
            assert_eq!(script.commits.load(Ordering::SeqCst), 0);
            session.cancel_delete();
            assert!(script.stop.lock().unwrap().as_ref().unwrap().is_cancelled());
        }
    }
    #[test]
    fn files_delete_rendered_folder_selection_review_and_confirm_are_separate_actions() {
        use egui_kittest::{
            kittest::{By, Queryable},
            Harness,
        };
        let (mut session, script, _) = fixture();
        const LOCATION: &str = "/11111111111111111111111111111111";
        *script.locations.lock().unwrap() = Some(vec![RemoteLocation {
            path: LOCATION.into(),
            label: "/owned".into(),
            available: true,
            mutations_available: true,
        }]);
        session.remote_write = true;
        let pane = session.remote.as_mut().unwrap();
        pane.path = LOCATION.into();
        let snapshot = pane.listing.as_mut().unwrap();
        snapshot.path = LOCATION.into();
        snapshot.directory_read_only = Some(true);

        let local_fs = Arc::new(FixtureFs {
            kind: festerm_sftp::PaneKind::Local,
            home: "",
            entries: vec![fixture_entry("local.txt", false, 1, 1)],
        });
        let mut local = Pane::new(local_fs.clone(), String::new());
        let mut local_snapshot = local_fs.list("", &ListOptions::default()).unwrap();
        local_snapshot.entries[0].permissions = Some(0o500);
        local.listing = Some(local_snapshot);
        local.selected.insert("local.txt".into());
        session.local = Some(local);
        assert!(session.can_review_delete_side(true));
        assert!(!session.deletion_available());
        session.review_delete();
        assert!(script.paths.lock().unwrap().is_empty());
        session.local = None;

        let snapshot = session.remote.as_mut().unwrap().listing.as_mut().unwrap();
        snapshot.directory_read_only = Some(false);
        for entry in &mut snapshot.entries {
            entry.permissions = Some(if entry.name == "empty" { 0o500 } else { 0o700 });
        }
        assert!(
            session.can_review_delete(),
            "selected direct file remains deletable"
        );
        let mut harness = Harness::builder()
            .with_size(egui::vec2(390.0, 844.0))
            .build_ui_state(
                |ui, session: &mut FilesSession| {
                    session.poll_delete(ui.ctx());
                    crate::files_ui::show(ui, session);
                },
                session,
            );
        harness.run_steps(3);
        let folder_choice = harness.get_by_label("Select folder empty");
        assert!(folder_choice.rect().width() >= 44.0 && folder_choice.rect().height() >= 44.0);
        folder_choice.click();
        harness.run_steps(3);
        let pane = harness.state().remote.as_ref().unwrap();
        assert_eq!(
            pane.path, LOCATION,
            "checkbox selects the folder without navigating"
        );
        assert!(pane.selected.contains("empty"));
        harness.get_by_label("2 items · 2 selected");
        harness
            .get_all_by_label("Sort and view options")
            .next()
            .unwrap()
            .click();
        harness.run_steps(3);
        harness.get(
            By::new()
                .label("Delete selected")
                .predicate(|node| node.is_disabled()),
        );
        harness.state_mut().review_delete();
        assert!(script.paths.lock().unwrap().is_empty());
        let snapshot = harness
            .state_mut()
            .remote
            .as_mut()
            .unwrap()
            .listing
            .as_mut()
            .unwrap();
        snapshot
            .entries
            .iter_mut()
            .find(|entry| entry.name == "empty")
            .unwrap()
            .permissions = Some(0o700);
        harness.run_steps(3);
        harness.get_by_label("Delete selected").click();
        harness.run_steps(3);
        assert_eq!(
            *script.paths.lock().unwrap(),
            [
                format!("{LOCATION}/empty"),
                format!("{LOCATION}/public.txt")
            ]
        );
        harness.get(
            By::new()
                .label("Delete reviewed entries")
                .predicate(|node| node.is_disabled()),
        );
        assert_eq!(script.commits.load(Ordering::SeqCst), 0);
        script
            .review_reply
            .lock()
            .unwrap()
            .take()
            .unwrap()
            .send(Ok(Review {
                id: 51,
                names: vec!["empty".into(), "public.txt".into()],
                details: Some(vec![
                    ReviewDetail::File(Some(13)),
                    ReviewDetail::File(Some(13)),
                ]),
            }))
            .unwrap();
        harness.run_steps(3);
        harness.get_by_label("Delete reviewed entries").click();
        harness.run_steps(3);
        assert_eq!(script.commits.load(Ordering::SeqCst), 1);
        harness.get(
            By::new()
                .label("Delete reviewed entries")
                .predicate(|node| node.is_disabled()),
        );
    }
    #[test]
    fn files_delete_local_failed_listing_side_generation_and_confirmation() {
        let (mut session, script, ctx) = fixture();
        session.local = session.remote.take();
        assert!(
            !session.can_review_delete(),
            "missing remote pane does not authorize remote deletion"
        );
        assert!(session.can_review_delete_side(true));
        session.local.as_mut().unwrap().error = Some("Refresh failed".into());
        session.review_delete_side(true);
        assert!(session.deletion.is_none());
        session.local.as_mut().unwrap().error = None;
        session.review_delete_side(true);
        assert!(session.deletion.as_ref().unwrap().local());
        script
            .review_reply
            .lock()
            .unwrap()
            .take()
            .unwrap()
            .send(Ok(Review {
                id: 51,
                names: vec!["public.txt".into()],
                details: Some(vec![ReviewDetail::File(Some(13))]),
            }))
            .unwrap();
        session.poll_delete(&ctx);
        session.local.as_mut().unwrap().generation += 1;
        session.commit_delete();
        assert_eq!(script.commits.load(Ordering::SeqCst), 0);
        assert!(session.deletion.is_none());
        session.review_delete_side(true);
        script
            .review_reply
            .lock()
            .unwrap()
            .take()
            .unwrap()
            .send(Ok(Review {
                id: 51,
                names: vec!["public.txt".into()],
                details: Some(vec![ReviewDetail::File(Some(13))]),
            }))
            .unwrap();
        session.poll_delete(&ctx);
        session.commit_delete();
        assert_eq!(script.commits.load(Ordering::SeqCst), 1);
        script
            .commit_reply
            .lock()
            .unwrap()
            .take()
            .unwrap()
            .send(Ok(Outcome {
                confirmed: 1,
                total: 1,
                unknown: false,
                failed: false,
                recovery: None,
            }))
            .unwrap();
        session.poll_delete(&ctx);
        assert!(session.local.as_ref().unwrap().selected.is_empty());
        assert!(session.remote.is_none());
    }
    #[test]
    fn files_delete_local_rendered_menu_review_cancel_and_confirm_with_remote_delete_off() {
        use egui_kittest::{kittest::Queryable, Harness};
        let (mut session, script, _) = fixture();
        script.remote_delete.store(false, Ordering::SeqCst);
        let fs = Arc::new(FixtureFs {
            kind: festerm_sftp::PaneKind::Local,
            home: "",
            entries: vec![
                fixture_entry("public.txt", false, 13, 1),
                fixture_entry("empty", true, 0, 1),
            ],
        });
        let mut pane = Pane::new(fs.clone(), "".into());
        pane.listing = Some(fs.list("", &ListOptions::default()).unwrap());
        pane.selected.insert("public.txt".into());
        session.local = Some(pane);
        let mut harness = Harness::builder()
            .with_size(egui::vec2(390.0, 844.0))
            .build_ui_state(
                |ui, session: &mut FilesSession| {
                    session.poll_delete(ui.ctx());
                    crate::files_ui::show(ui, session);
                },
                session,
            );
        harness.run_steps(3);
        let folder = harness.get_by_label("Select folder empty");
        assert!(folder.rect().width() >= 44.0 && folder.rect().height() >= 44.0);
        folder.click();
        harness.run_steps(3);
        assert!(harness
            .state()
            .local
            .as_ref()
            .unwrap()
            .selected
            .contains("empty"));
        assert_eq!(harness.state().local.as_ref().unwrap().path, "");
        harness
            .get_all_by_label("Sort and view options")
            .nth(1)
            .unwrap()
            .click();
        harness.run_steps(3);
        harness.get_by_label("Delete selected").click();
        harness.run_steps(3);
        assert!(harness.state().deletion.as_ref().unwrap().local());
        assert_eq!(*script.paths.lock().unwrap(), ["empty", "public.txt"]);
        assert_eq!(script.commits.load(Ordering::SeqCst), 0);
        harness.get_by_label("Cancel").click();
        harness.run_steps(3);
        assert!(script.stop.lock().unwrap().as_ref().unwrap().is_cancelled());
        assert!(harness.state().deletion.is_none());
        harness
            .get_all_by_label("Sort and view options")
            .nth(1)
            .unwrap()
            .click();
        harness.run_steps(3);
        harness.get_by_label("Delete selected").click();
        harness.run_steps(3);
        script
            .review_reply
            .lock()
            .unwrap()
            .take()
            .unwrap()
            .send(Ok(Review {
                id: 51,
                names: vec!["empty".into(), "public.txt".into()],
                details: Some(vec![
                    ReviewDetail::EmptyFolder,
                    ReviewDetail::File(Some(13)),
                ]),
            }))
            .unwrap();
        harness.run_steps(3);
        assert!(harness.state().deletion.as_ref().unwrap().ready());
        assert!(harness.state().can_review_delete_side(true));
        harness.get_by_label("Delete reviewed entries").click();
        harness.run_steps(3);
        assert_eq!(
            script.commits.load(Ordering::SeqCst),
            1,
            "draft ready={}, present={}, notice={:?}",
            harness.state().deletion.as_ref().is_some_and(|d| d.ready()),
            harness.state().deletion.is_some(),
            harness.state().notice
        );
    }
    #[test]
    fn files_delete_local_recovery_locator_is_hidden_after_review_retirement() {
        for retire in [false, true] {
            let (mut session, script, ctx) = fixture();
            session.local = session.remote.take();
            session.review_delete_side(true);
            script
                .review_reply
                .lock()
                .unwrap()
                .take()
                .unwrap()
                .send(Ok(Review {
                    id: 51,
                    names: vec!["public.txt".into()],
                    details: Some(vec![ReviewDetail::File(Some(13))]),
                }))
                .unwrap();
            session.poll_delete(&ctx);
            session.commit_delete();
            if retire {
                session.local.as_mut().unwrap().generation += 1;
                session.poll_delete(&ctx);
            }
            script
                .commit_reply
                .lock()
                .unwrap()
                .take()
                .unwrap()
                .send(Ok(Outcome {
                    confirmed: 0,
                    total: 1,
                    unknown: true,
                    failed: true,
                    recovery: Some("owned/.festerm-delete-test".into()),
                }))
                .unwrap();
            session.poll_delete(&ctx);
            assert_eq!(session.deletion_recovery().is_some(), !retire);
            assert!(session.deletion_result().unwrap().contains("unknown"));
        }
    }
    #[test]
    fn files_delete_local_received_locator_is_separate_and_cleared_before_retired_render() {
        use egui_kittest::{kittest::Queryable, Harness};
        for cut in 0..3 {
            let (mut session, script, ctx) = fixture();
            session.local = session.remote.take();
            session.review_delete_side(true);
            script
                .review_reply
                .lock()
                .unwrap()
                .take()
                .unwrap()
                .send(Ok(Review {
                    id: 51,
                    names: vec!["public.txt".into()],
                    details: Some(vec![ReviewDetail::File(Some(13))]),
                }))
                .unwrap();
            session.poll_delete(&ctx);
            session.commit_delete();
            script
                .commit_reply
                .lock()
                .unwrap()
                .take()
                .unwrap()
                .send(Ok(Outcome {
                    confirmed: 0,
                    total: 1,
                    unknown: true,
                    failed: true,
                    recovery: Some("owned/.festerm-delete-test".into()),
                }))
                .unwrap();
            session.poll_delete(&ctx);
            assert_eq!(
                session.deletion_recovery(),
                Some("owned/.festerm-delete-test")
            );
            assert!(!session
                .deletion_result()
                .unwrap()
                .contains(".festerm-delete"));
            assert!(!session.notice.as_ref().unwrap().contains(".festerm-delete"));
            let label = format!(
                "Recovery copy in {}: owned/.festerm-delete-test. Keep this copy until recovery is complete.",
                crate::platform_text("This iPhone", "This device")
            );
            let mut harness = Harness::builder()
                .with_size(egui::vec2(390.0, 844.0))
                .build_ui_state(
                    |ui, session: &mut FilesSession| {
                        crate::files_ui::show(ui, session);
                    },
                    session,
                );
            harness.run_steps(3);
            harness.get_by_label(&label);
            match cut {
                0 => script.available.store(false, Ordering::SeqCst),
                1 => harness.state_mut().link = Link::Failed("Connection closed".into()),
                _ => harness.state_mut().cleanup(),
            };
            harness.run_steps(3);
            assert!(harness.query_by_label(&label).is_none());
            assert!(harness.state().delete_recovery.is_none());
            harness.state_mut().link = Link::Failed("Connection closed".into());
            harness.run_steps(3);
            assert!(harness.query_by_label(&label).is_none());
            harness.get_by_label("Last deletion: 0 of 1 removals confirmed. A result is unknown. Inspect and refresh; do not retry automatically.");
            assert!(!format!("{:?}", harness.state().link).contains(".festerm-delete"));
            assert!(!harness
                .state()
                .notice
                .as_ref()
                .unwrap()
                .contains(".festerm-delete"));
        }
    }
    #[test]
    fn files_delete_review_widget_sanitizes_folder_labels_without_changing_raw_paths() {
        use egui_kittest::{kittest::Queryable, Harness};
        for (local, path, root_label, expected) in [
            (true, "", "Shared folder", "This device: Documents"),
            (true, "/", "Shared folder", "This device: Documents"),
            (
                true,
                "العربية/part\u{202e}name",
                "Shared folder",
                "This device: Documents/العربية/part�name",
            ),
            (
                false,
                "/العربية/part\u{2066}name",
                "Shared folder",
                "Remote: Shared folder/العربية/part�name",
            ),
            (
                false,
                "/العربية",
                "/srv/Allowed",
                "Remote: /srv/Allowed/العربية",
            ),
            (false, "/العربية", "/", "Remote: /العربية"),
        ] {
            let (mut session, script, _) = fixture();
            *script.root_label.lock().unwrap() = root_label.into();
            if local {
                session.local = session.remote.take();
            }
            let pane = if local {
                session.local.as_mut().unwrap()
            } else {
                session.remote.as_mut().unwrap()
            };
            pane.path = path.into();
            session.review_delete_side(local);
            assert!(session.deletion.is_some());
            let mut harness = Harness::builder()
                .with_size(egui::vec2(390.0, 844.0))
                .build_ui_state(
                    |ui, session: &mut FilesSession| {
                        crate::files_ui::show(ui, session);
                    },
                    session,
                );
            harness.run_steps(3);
            let expected = expected.replace(
                "This device",
                crate::platform_text("This iPhone", "This device"),
            );
            harness.get_by_label(&expected);
            assert_eq!(harness.state().deletion.as_ref().unwrap().path(), path);
        }
    }
}
