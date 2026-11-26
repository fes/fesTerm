//! One explicit remote deletion review. No handles or authority enter UI state.
use super::*;
use tokio_util::sync::CancellationToken;

pub(crate) type Reply<T> = mpsc::Receiver<Result<T, String>>;
#[derive(Debug)]
pub(crate) struct Review {
    pub id: u64,
    pub names: Vec<String>,
}
#[derive(Debug)]
pub(crate) struct Outcome {
    pub confirmed: usize,
    pub total: usize,
    pub unknown: bool,
    pub failed: bool,
}
enum Pending {
    Review(Reply<Review>),
    Commit(Reply<Outcome>),
}
pub(crate) struct Draft {
    path: String,
    generation: u64,
    selected: BTreeSet<String>,
    stop: CancellationToken,
    pending: Option<Pending>,
    pub review: Option<Review>,
}
impl Draft {
    pub(crate) fn path(&self) -> &str {
        &self.path
    }
    pub(crate) fn committing(&self) -> bool {
        matches!(self.pending, Some(Pending::Commit(_)))
    }
    pub(crate) fn ready(&self) -> bool {
        self.pending.is_none() && self.review.is_some() && !self.stop.is_cancelled()
    }
}
impl Drop for Draft {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}
impl FilesSession {
    pub(crate) fn deletion_result(&self) -> Option<&str> {
        self.delete_result.as_deref()
    }
    pub(crate) fn deletion_available(&self) -> bool {
        self.is_ready()
            && self
                .transfers
                .as_ref()
                .is_some_and(|t| t.delete_available())
    }
    pub(crate) fn can_review_delete(&self) -> bool {
        self.deletion_available() && self.delete_paths().is_some()
    }
    fn delete_paths(&self) -> Option<Vec<String>> {
        let pane = self.remote.as_ref()?;
        if pane.loading() || pane.selected.is_empty() || pane.selected.len() > 8 {
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
                || !matches!(entry.kind, EntryKind::File | EntryKind::Directory)
            {
                return None;
            }
            paths.push(pane.fs.join(&pane.path, name).ok()?);
        }
        Some(paths)
    }
    fn delete_current(&self, draft: &Draft) -> bool {
        self.deletion_available()
            && self.remote.as_ref().is_some_and(|pane| {
                pane.path == draft.path
                    && pane.generation == draft.generation
                    && pane.selected == draft.selected
                    && !pane.loading()
            })
    }
    pub(crate) fn review_delete(&mut self) {
        if !self.can_review_delete() || self.deletion.is_some() {
            return;
        }
        let paths = self.delete_paths().expect("validated selection");
        let stop = CancellationToken::new();
        match self
            .transfers
            .as_ref()
            .expect("ready transfers")
            .review_delete(paths, stop.clone())
        {
            Ok(reply) => {
                let pane = self.remote.as_ref().expect("validated remote pane");
                self.folder = None;
                self.delete_result = None;
                self.notice = None;
                self.transfers_sheet = false;
                self.deletion = Some(Draft {
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
                transfers.cancel_delete();
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
        match self
            .transfers
            .as_ref()
            .expect("ready transfers")
            .commit_delete(id)
        {
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
                self.cancel_delete();
                let message = if outcome.unknown {
                    format!("{} of {} removals confirmed. A result is unknown. Inspect and refresh; do not retry automatically.", outcome.confirmed, outcome.total)
                } else if outcome.failed {
                    format!("{} of {} removals confirmed. Deletion stopped. Refresh before reviewing untouched entries.", outcome.confirmed, outcome.total)
                } else {
                    format!("{} entries deleted", outcome.confirmed)
                };
                self.record_delete_result(message);
                if self.is_ready() {
                    if let Some(pane) = &mut self.remote {
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
                paths: Mutex::default(),
                stop: Mutex::default(),
                review_reply: Mutex::default(),
                commit_reply: Mutex::default(),
                commits: AtomicUsize::new(0),
            }
        }
    }
    impl Transfers for Script {
        fn delete_available(&self) -> bool {
            self.available.load(Ordering::SeqCst)
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
                names: vec![]
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
    fn files_delete_rendered_folder_selection_review_and_confirm_are_separate_actions() {
        use egui_kittest::{
            kittest::{By, Queryable},
            Harness,
        };
        let (session, script, _) = fixture();
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
            pane.path, "/",
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
        harness.get_by_label("Delete selected").click();
        harness.run_steps(3);
        assert_eq!(*script.paths.lock().unwrap(), ["/empty", "/public.txt"]);
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
}
