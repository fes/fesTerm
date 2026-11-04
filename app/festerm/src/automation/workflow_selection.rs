use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    thread,
    time::Duration,
};

use eframe::egui;
use festerm_automation_js::snapshot::ApprovedBundle;
use festerm_automation_workflow::intake::load_user_selected_bundle;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum Status {
    #[default]
    Empty,
    Capturing,
    Waiting,
    Cancelling,
    Captured,
    Failed(String),
}

#[derive(Clone, Default)]
pub(crate) struct View {
    pub status: Status,
    bundle: Option<Arc<ApprovedBundle>>,
    cancelled: Option<Arc<AtomicBool>>,
}

impl View {
    pub(super) fn same_presentation(&self, other: &Self) -> bool {
        self.status == other.status
            && self.bundle.as_ref().map(|bundle| bundle.digest())
                == other.bundle.as_ref().map(|bundle| bundle.digest())
    }

    pub(crate) fn busy(&self) -> bool {
        matches!(
            self.status,
            Status::Capturing | Status::Waiting | Status::Cancelling
        )
    }

    pub(crate) fn current_bundle(&self) -> Option<&Arc<ApprovedBundle>> {
        if self
            .cancelled
            .as_ref()
            .is_some_and(|cancelled| cancelled.load(Ordering::Acquire))
        {
            None
        } else {
            self.bundle.as_ref()
        }
    }

    pub(crate) fn disable(&self) {
        if let Some(cancelled) = &self.cancelled {
            cancelled.store(true, Ordering::Release);
        }
    }

    #[cfg(test)]
    pub(crate) fn captured_for_test(bundle: ApprovedBundle) -> Self {
        Self {
            status: Status::Captured,
            bundle: Some(Arc::new(bundle)),
            cancelled: Some(Arc::new(AtomicBool::new(false))),
        }
    }
}

struct Pending {
    result: mpsc::Receiver<Result<ApprovedBundle, String>>,
    cancelled: Arc<AtomicBool>,
}

struct Queued {
    capture: Box<dyn FnOnce() -> Result<ApprovedBundle, String> + Send>,
    cancelled: Arc<AtomicBool>,
}

#[derive(Default)]
pub(super) struct Selection {
    pending: Option<Pending>,
    queued: Option<Queued>,
    bundle: Option<Arc<ApprovedBundle>>,
    error: Option<String>,
    cancelled: Option<Arc<AtomicBool>>,
}

impl Selection {
    pub(super) fn capture(&mut self, path: PathBuf, context: &egui::Context) {
        self.begin(context, move || {
            load_user_selected_bundle(&path).map_err(|error| error.to_string())
        });
    }

    fn begin(
        &mut self,
        context: &egui::Context,
        capture: impl FnOnce() -> Result<ApprovedBundle, String> + Send + 'static,
    ) {
        self.cancel();
        let cancelled = Arc::new(AtomicBool::new(false));
        self.cancelled = Some(cancelled.clone());
        let queued = Queued {
            capture: Box::new(capture),
            cancelled,
        };
        if self.pending.is_some() {
            self.queued = Some(queued);
            context.request_repaint();
        } else {
            self.start(context, queued);
        }
    }

    fn start(&mut self, context: &egui::Context, queued: Queued) {
        let Queued { capture, cancelled } = queued;
        let (send, result) = mpsc::sync_channel(1);
        let notify = context.clone();
        match thread::Builder::new()
            .name("festerm-workflow-capture".into())
            .spawn(move || {
                let captured = capture();
                let _ = send.send(captured);
                drop(send);
                notify.request_repaint();
            }) {
            Ok(thread) => {
                drop(thread);
                self.pending = Some(Pending { result, cancelled });
                context.request_repaint();
            }
            Err(error) => {
                tracing::error!("workflow capture worker unavailable: {error}");
                self.error = Some(format!("Could not start workflow capture: {error}"));
            }
        }
    }

    pub(super) fn cancel(&mut self) {
        self.bundle = None;
        self.error = None;
        self.queued = None;
        if let Some(pending) = &self.pending {
            pending.cancelled.store(true, Ordering::Release);
        }
        if let Some(cancelled) = &self.cancelled {
            cancelled.store(true, Ordering::Release);
        }
    }

    pub(super) fn refuse(&mut self, message: &'static str) {
        self.cancel();
        self.cancelled = None;
        self.error = Some(message.into());
    }

    pub(super) fn poll(&mut self, context: &egui::Context) {
        if self
            .cancelled
            .as_ref()
            .is_some_and(|cancelled| cancelled.load(Ordering::Acquire))
        {
            self.bundle = None;
            self.error = None;
            self.queued = None;
        }
        let result = self
            .pending
            .as_ref()
            .map(|pending| pending.result.try_recv());
        match result {
            Some(Ok(result)) => {
                let pending = self.pending.take().expect("pending capture exists");
                if !pending.cancelled.load(Ordering::Acquire) {
                    match result {
                        Ok(bundle) => self.bundle = Some(Arc::new(bundle)),
                        Err(error) => self.error = Some(error),
                    }
                }
            }
            Some(Err(mpsc::TryRecvError::Disconnected)) => {
                let pending = self.pending.take().expect("pending capture exists");
                if !pending.cancelled.load(Ordering::Acquire) {
                    tracing::error!("workflow capture worker failed");
                    self.error = Some("Workflow capture worker failed.".into());
                }
            }
            Some(Err(mpsc::TryRecvError::Empty)) => {
                context.request_repaint_after(Duration::from_secs(1));
            }
            None => {}
        }
        if self.pending.is_none() {
            if let Some(queued) = self.queued.take() {
                if !queued.cancelled.load(Ordering::Acquire) {
                    self.start(context, queued);
                }
            }
        }
    }

    pub(super) fn view(&self) -> View {
        let cancelled = self
            .cancelled
            .as_ref()
            .is_some_and(|cancelled| cancelled.load(Ordering::Acquire));
        let status = match &self.pending {
            Some(_) if !cancelled && self.queued.is_some() => Status::Waiting,
            Some(pending) if pending.cancelled.load(Ordering::Acquire) => Status::Cancelling,
            Some(_) => Status::Capturing,
            None if cancelled => Status::Empty,
            None if self.bundle.is_some() => Status::Captured,
            None => self
                .error
                .as_ref()
                .map_or(Status::Empty, |error| Status::Failed(error.clone())),
        };
        View {
            status,
            bundle: if cancelled { None } else { self.bundle.clone() },
            cancelled: self.cancelled.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use festerm_automation_js::snapshot::{Manifest, SourceSnapshot};
    use std::{
        collections::{BTreeMap, BTreeSet},
        time::Instant,
    };

    fn bundle() -> ApprovedBundle {
        ApprovedBundle::new(SourceSnapshot {
            manifest: Manifest {
                version: 1,
                api_version: 1,
                name: "test".into(),
                entry: "main.js".into(),
                modules: vec!["main.js".into()],
                parameters: BTreeMap::new(),
                capabilities: BTreeSet::new(),
            },
            modules: BTreeMap::from([("main.js".into(), "export default 1;".into())]),
        })
        .unwrap()
    }

    fn wait_for_capture(selection: &mut Selection, context: &egui::Context) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while selection.pending.is_some() {
            selection.poll(context);
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
    }

    #[test]
    fn cancelled_workflow_capture_keeps_its_slot_and_discards_late_result() {
        let context = egui::Context::default();
        let (release, blocked) = mpsc::sync_channel::<()>(1);
        let mut selection = Selection::default();
        selection.begin(&context, move || {
            blocked.recv_timeout(Duration::from_secs(5)).unwrap();
            Ok(bundle())
        });
        selection.view().disable();
        assert_eq!(selection.view().status, Status::Cancelling);
        release.send(()).unwrap();
        wait_for_capture(&mut selection, &context);
        assert!(selection.view().bundle.is_none());

        selection.begin(&context, || Ok(bundle()));
        wait_for_capture(&mut selection, &context);
        assert_eq!(selection.view().status, Status::Captured);
        assert_eq!(selection.view().bundle.unwrap().digest(), bundle().digest());
    }

    #[test]
    fn newer_workflow_selection_waits_for_io_and_discards_superseded_results() {
        let context = egui::Context::default();
        let (release, blocked) = mpsc::sync_channel::<()>(1);
        let started = Arc::new(AtomicBool::new(false));
        let mut selection = Selection::default();
        selection.begin(&context, move || {
            blocked.recv_timeout(Duration::from_secs(5)).unwrap();
            Ok(bundle())
        });
        selection.begin(&context, || panic!("superseded capture must not start"));
        let latest_started = started.clone();
        selection.begin(&context, move || {
            latest_started.store(true, Ordering::Release);
            Ok(bundle())
        });
        assert_eq!(selection.view().status, Status::Waiting);
        assert!(!started.load(Ordering::Acquire));
        release.send(()).unwrap();
        wait_for_capture(&mut selection, &context);
        assert!(started.load(Ordering::Acquire));
        assert_eq!(selection.view().status, Status::Captured);
    }

    #[test]
    fn cancelling_a_waiting_selection_never_starts_its_capture() {
        let context = egui::Context::default();
        let (release, blocked) = mpsc::sync_channel::<()>(1);
        let mut selection = Selection::default();
        selection.begin(&context, move || {
            blocked.recv_timeout(Duration::from_secs(5)).unwrap();
            Ok(bundle())
        });
        selection.begin(&context, || panic!("cancelled capture must not start"));
        selection.view().disable();
        assert_eq!(selection.view().status, Status::Cancelling);
        release.send(()).unwrap();
        wait_for_capture(&mut selection, &context);
        assert_eq!(selection.view().status, Status::Empty);
        assert!(selection.view().bundle.is_none());
    }

    #[test]
    fn workflow_capture_failure_is_visible_and_never_approves_a_bundle() {
        let context = egui::Context::default();
        let mut selection = Selection::default();
        selection.begin(&context, || Err("invalid snapshot".into()));
        wait_for_capture(&mut selection, &context);
        assert_eq!(
            selection.view().status,
            Status::Failed("invalid snapshot".into())
        );
        assert!(selection.view().bundle.is_none());
    }
}
