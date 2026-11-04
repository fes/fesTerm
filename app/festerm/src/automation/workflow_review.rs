use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use festerm_automation::TargetRef;
use festerm_automation_workflow::review::{Draft, Review, ReviewError, ReviewRevision};

use crate::{app::FesTermApp, tabs::TabId};

use super::{ipc::TargetGuard, review_target_description, PrivacyGate, Target, WorkflowView};

const START_REQUIRED: &str =
    "Initialize review targets using the shared broker; no access is granted.";
const STARTING: &str = "Starting privacy monitoring for review; no access is granted.";
const UNAVAILABLE: &str =
    "Review targets are unavailable. Privacy monitoring and the shared broker are required.";
const DISCARDED: &str = "The review changed or became stale and was discarded. Prepare it again.";

#[derive(Clone)]
pub(crate) struct Request {
    revision: ReviewRevision,
    cancelled: Arc<AtomicBool>,
    draft: Draft,
}

impl Request {
    pub(crate) fn new(
        selection: &WorkflowView,
        expected: &ReviewRevision,
        draft: Draft,
    ) -> Option<Self> {
        let (revision, cancelled) = selection.review_token()?;
        if !expected.matches(&revision) {
            return None;
        }
        Some(Self {
            cancelled,
            revision,
            draft,
        })
    }

    fn matches(&self, selection: &WorkflowView) -> bool {
        selection
            .review_token()
            .is_some_and(|(revision, cancelled)| {
                self.revision.matches(&revision) && Arc::ptr_eq(&self.cancelled, &cancelled)
            })
    }
}

impl std::fmt::Debug for Request {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("WorkflowReviewRequest(<private>)")
    }
}

#[derive(Clone)]
pub(crate) struct TargetRow {
    pub reference: TargetRef,
    pub label: String,
    guard: TargetGuard,
}

pub(crate) struct Proposal {
    review: Review,
    targets: Vec<TargetRow>,
    revision: ReviewRevision,
    cancelled: Arc<AtomicBool>,
    privacy: Arc<PrivacyGate>,
}

impl Proposal {
    fn current(&self) -> bool {
        let Some(_guard) = festerm_session::automation_admission_lock() else {
            return false;
        };
        self.current_under_admission()
    }

    /// The caller must hold the shared admission lock across this check and
    /// any later native decision that uses the proposal.
    pub(super) fn current_under_admission(&self) -> bool {
        !self.privacy.is_closed()
            && !self.cancelled.load(Ordering::Acquire)
            && self.revision.is_current()
            && self.targets.iter().all(|target| target.guard.is_valid())
    }

    pub(crate) fn review(&self) -> &Review {
        &self.review
    }

    pub(crate) fn targets(&self) -> &[TargetRow] {
        &self.targets
    }
}

#[derive(Clone, Default)]
pub(crate) struct View {
    targets: Vec<TargetRow>,
    proposal: Option<Arc<Proposal>>,
    privacy: Option<Arc<PrivacyGate>>,
    ready: bool,
    starting: bool,
    status: Option<String>,
}

impl View {
    pub(crate) fn ready(&self) -> bool {
        self.ready
            && self
                .privacy
                .as_ref()
                .is_some_and(|privacy| !privacy.is_closed())
    }

    pub(crate) fn starting(&self) -> bool {
        self.starting
    }

    pub(crate) fn live_targets(&self) -> Vec<&TargetRow> {
        let Some(_guard) = festerm_session::automation_admission_lock() else {
            return Vec::new();
        };
        if !self.ready() {
            return Vec::new();
        }
        self.targets
            .iter()
            .filter(|target| target.guard.is_valid())
            .collect()
    }

    pub(crate) fn proposal(&self) -> Option<&Proposal> {
        self.proposal
            .as_deref()
            .filter(|proposal| proposal.current())
    }

    pub(crate) fn status(&self) -> Option<&str> {
        self.status.as_deref()
    }

    pub(super) fn same_presentation(&self, other: &Self) -> bool {
        self.status == other.status
            && self.ready == other.ready
            && self.starting == other.starting
            && self.targets.len() == other.targets.len()
            && self
                .targets
                .iter()
                .zip(&other.targets)
                .all(|(left, right)| left.reference == right.reference && left.label == right.label)
            && match (&self.proposal, &other.proposal) {
                (Some(left), Some(right)) => Arc::ptr_eq(left, right),
                (None, None) => true,
                _ => false,
            }
    }
}

#[derive(Default)]
pub(super) struct State {
    epoch: Option<ReviewRevision>,
    targets: Vec<TargetRow>,
    proposal: Option<Arc<Proposal>>,
    privacy: Option<Arc<PrivacyGate>>,
    ready: bool,
    starting: bool,
    failure: Option<&'static str>,
    feedback: Option<(ReviewRevision, String)>,
}

impl State {
    pub(super) fn reset(&mut self) {
        self.epoch = None;
        self.targets.clear();
        self.proposal = None;
        self.privacy = None;
        self.ready = false;
        self.starting = false;
        self.failure = None;
        self.feedback = None;
    }

    pub(super) fn begin(&mut self) {
        self.starting = true;
        self.failure = None;
        self.feedback = None;
    }

    pub(super) fn starting(&self) -> bool {
        self.starting
    }

    pub(super) fn fail(&mut self) {
        self.starting = false;
        self.failure = Some(UNAVAILABLE);
        self.ready = false;
        self.targets.clear();
        self.proposal = None;
        self.privacy = None;
    }

    pub(super) fn startup_failed(&mut self) {
        if self.starting {
            self.fail();
        }
    }

    pub(super) fn reconcile(
        &mut self,
        selection: &WorkflowView,
        apps: &[&FesTermApp],
        targets: &BTreeMap<u64, Target>,
        privacy: &Arc<PrivacyGate>,
        broker_ready: bool,
        startup_pending: bool,
    ) {
        let Some((revision, _)) = selection.review_token() else {
            self.reset();
            return;
        };
        if self
            .epoch
            .as_ref()
            .is_none_or(|current| !current.same_epoch(&revision))
        {
            self.reset();
            self.epoch = Some(revision.clone());
        }
        if !broker_ready || privacy.is_closed() {
            self.ready = false;
            self.targets.clear();
            self.privacy = None;
            if self.proposal.take().is_some() {
                if let Some(changed) = selection.invalidate_review() {
                    self.feedback = Some((changed, DISCARDED.into()));
                }
            }
            if self.starting && !startup_pending {
                self.fail();
            }
        } else {
            let mut rows = Vec::new();
            for target in targets.values() {
                let mut sessions = apps.iter().enumerate().filter_map(|(window, app)| {
                    app.automation_session(TabId::from_chip_id(target.reference.session()))
                        .map(|session| (window, session))
                });
                let Some((window, session)) = sessions.next() else {
                    continue;
                };
                if sessions.next().is_some() {
                    selection.invalidate_review();
                    self.fail();
                    return;
                }
                rows.push(TargetRow {
                    reference: target.reference,
                    label: format!(
                        "{} (tab {}, attachment {})",
                        review_target_description(window, session),
                        target.reference.session(),
                        target.reference.generation()
                    ),
                    guard: TargetGuard {
                        liveness: target.liveness.clone(),
                        registration: target.registration.clone(),
                        attachment: target.attachment.clone(),
                        generation: target.attachment_generation,
                    },
                });
            }
            if self.proposal.as_ref().is_some_and(|proposal| {
                proposal.targets.iter().any(|old| {
                    !rows
                        .iter()
                        .any(|row| row.reference == old.reference && row.label == old.label)
                })
            }) {
                self.proposal = None;
                if let Some(changed) = selection.invalidate_review() {
                    self.feedback = Some((changed, DISCARDED.into()));
                }
            }
            self.targets = rows;
            self.privacy = Some(privacy.clone());
            self.ready = true;
            self.starting = false;
            self.failure = None;
        }
        if self
            .proposal
            .as_ref()
            .is_some_and(|proposal| !proposal.current())
        {
            self.proposal = None;
            self.feedback = Some((revision, DISCARDED.into()));
        }
    }

    pub(super) fn discard(&mut self) {
        self.proposal = None;
        self.feedback = None;
    }

    pub(super) fn prepare(&mut self, request: Request, selection: &WorkflowView) {
        if !request.matches(selection) {
            return;
        }
        if self
            .proposal
            .as_ref()
            .is_some_and(|proposal| proposal.current())
        {
            self.proposal = None;
            self.feedback = selection
                .invalidate_review()
                .map(|revision| (revision, DISCARDED.into()));
            return;
        }
        self.proposal = None;
        self.feedback = None;
        let Some(bundle) = selection.current_bundle() else {
            return;
        };
        let Some(privacy) = self.privacy.as_ref().filter(|_| self.ready) else {
            self.feedback = Some((request.revision, UNAVAILABLE.into()));
            return;
        };
        let Some(_guard) = festerm_session::automation_admission_lock() else {
            self.feedback = Some((request.revision, UNAVAILABLE.into()));
            return;
        };
        if privacy.is_closed() {
            self.feedback = Some((request.revision, UNAVAILABLE.into()));
            return;
        }
        let live: BTreeSet<_> = self
            .targets
            .iter()
            .filter(|target| target.guard.is_valid())
            .map(|target| target.reference)
            .collect();
        let chosen: Vec<_> = self
            .targets
            .iter()
            .filter(|target| request.draft.targets.contains(&target.reference))
            .cloned()
            .collect();
        drop(_guard);

        let review = match Review::prepare(bundle, request.draft, &live) {
            Ok(review) => review,
            Err(error) => {
                self.feedback = Some((request.revision, error.to_string()));
                return;
            }
        };
        let proposal = Proposal {
            review,
            targets: chosen,
            cancelled: request.cancelled,
            revision: request.revision,
            privacy: privacy.clone(),
        };
        if proposal.current() {
            self.proposal = Some(Arc::new(proposal));
        } else {
            self.feedback = Some((proposal.revision, ReviewError::StaleTarget.to_string()));
        }
    }

    pub(super) fn view(&self, selection: &WorkflowView) -> View {
        let revision = selection.review_token().map(|(revision, _)| revision);
        let status = self
            .failure
            .map(str::to_owned)
            .or_else(|| {
                self.feedback.as_ref().and_then(|(expected, status)| {
                    revision
                        .as_ref()
                        .is_some_and(|revision| expected.matches(revision))
                        .then(|| status.clone())
                })
            })
            .or_else(|| {
                (!self.ready).then(|| {
                    if self.starting {
                        STARTING
                    } else {
                        START_REQUIRED
                    }
                    .into()
                })
            });
        View {
            targets: self.targets.clone(),
            proposal: self.proposal.clone(),
            privacy: self.privacy.clone(),
            ready: self.ready,
            starting: self.starting,
            status,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use festerm_automation::Broker;
    use festerm_automation_js::snapshot::{ApprovedBundle, Capability, Manifest, SourceSnapshot};

    fn bundle() -> ApprovedBundle {
        ApprovedBundle::new(SourceSnapshot {
            manifest: Manifest {
                version: 1,
                api_version: 1,
                name: "review".into(),
                entry: "main.js".into(),
                modules: vec!["main.js".into()],
                parameters: BTreeMap::new(),
                capabilities: BTreeSet::from([Capability::ReadScreen]),
            },
            modules: BTreeMap::from([("main.js".into(), "export default 1;".into())]),
        })
        .unwrap()
    }

    #[test]
    fn queued_review_requires_the_exact_current_selection_and_revision() {
        let selection = WorkflowView::captured_for_test(bundle());
        let revision = selection.review_token().unwrap().0;
        let request = Request::new(&selection, &revision, Draft::default()).unwrap();
        assert!(request.matches(&selection));
        assert!(!request.matches(&WorkflowView::captured_for_test(bundle())));
        selection.invalidate_review();
        assert!(!request.matches(&selection));
        assert!(Request::new(&selection, &revision, Draft::default()).is_none());
        let revision = selection.review_token().unwrap().0;
        let replacement = Request::new(&selection, &revision, Draft::default()).unwrap();
        assert!(replacement.matches(&selection));
        selection.disable();
        assert!(!replacement.matches(&selection));
    }

    #[test]
    fn workflow_review_fails_closed_without_a_live_privacy_gate() {
        let selection = WorkflowView::captured_for_test(bundle());
        let privacy = Arc::new(PrivacyGate::new(true));
        let mut state = State::default();
        state.reconcile(&selection, &[], &BTreeMap::new(), &privacy, true, false);
        assert!(!state.view(&selection).ready());
        assert!(state.view(&selection).live_targets().is_empty());
        let revision = selection.review_token().unwrap().0;
        let request = Request::new(&selection, &revision, Draft::default()).unwrap();
        state.prepare(request, &selection);
        assert!(state.view(&selection).proposal().is_none());
        assert_eq!(state.view(&selection).status(), Some(UNAVAILABLE));
    }

    #[test]
    fn workflow_review_requires_a_live_monitored_shared_broker() {
        let selection = WorkflowView::captured_for_test(bundle());
        let privacy = Arc::new(PrivacyGate::new(false));
        let mut state = State::default();
        state.reconcile(&selection, &[], &BTreeMap::new(), &privacy, false, false);
        assert!(!state.view(&selection).ready());
        assert!(state.view(&selection).live_targets().is_empty());
        let revision = selection.review_token().unwrap().0;
        let request = Request::new(&selection, &revision, Draft::default()).unwrap();
        state.prepare(request, &selection);
        assert!(state.view(&selection).proposal().is_none());
        assert_eq!(state.view(&selection).status(), Some(UNAVAILABLE));
    }

    #[test]
    fn replacing_a_prepared_review_revokes_prior_views() {
        let selection = WorkflowView::captured_for_test(bundle());
        let target = Broker::new([2; 16]).attach_target(1, 1).unwrap();
        let attachment = Arc::new(festerm_session::AutomationEpoch::new());
        attachment.transition(true);
        let generation = attachment.capture().unwrap();
        let mut state = State {
            targets: vec![TargetRow {
                reference: target,
                label: "description only".into(),
                guard: TargetGuard {
                    liveness: Arc::new(AtomicBool::new(true)),
                    registration: Arc::new(AtomicBool::new(true)),
                    attachment,
                    generation,
                },
            }],
            privacy: Some(Arc::new(PrivacyGate::new(false))),
            ready: true,
            ..State::default()
        };
        let request = || {
            let revision = selection.review_token().unwrap().0;
            Request::new(
                &selection,
                &revision,
                Draft {
                    parameters: BTreeMap::new(),
                    targets: BTreeSet::from([target]),
                },
            )
            .unwrap()
        };
        state.prepare(request(), &selection);
        let old_view = state.view(&selection);
        assert!(old_view.proposal().is_some());

        state.prepare(request(), &selection);
        assert!(old_view.proposal().is_none());
        assert!(state.view(&selection).proposal().is_none());
        assert_eq!(state.view(&selection).status(), Some(DISCARDED));
    }

    #[test]
    fn proposal_disappears_on_attachment_replacement_or_revision_change() {
        let selection = WorkflowView::captured_for_test(bundle());
        let (revision, cancelled) = selection.review_token().unwrap();
        let target = Broker::new([4; 16]).attach_target(9, 1).unwrap();
        let attachment = Arc::new(festerm_session::AutomationEpoch::new());
        attachment.transition(true);
        let generation = attachment.capture().unwrap();
        let row = TargetRow {
            reference: target,
            label: "description only".into(),
            guard: TargetGuard {
                liveness: Arc::new(AtomicBool::new(true)),
                registration: Arc::new(AtomicBool::new(true)),
                attachment: attachment.clone(),
                generation,
            },
        };
        let review = Review::prepare(
            selection.current_bundle().unwrap(),
            Draft {
                parameters: BTreeMap::new(),
                targets: BTreeSet::from([target]),
            },
            &BTreeSet::from([target]),
        )
        .unwrap();
        let proposal = Proposal {
            review,
            targets: vec![row],
            revision,
            cancelled,
            privacy: Arc::new(PrivacyGate::new(false)),
        };
        assert!(proposal.current());
        attachment.transition(false);
        assert!(!proposal.current());
        attachment.transition(true);
        assert!(!proposal.current());

        let revision = selection.review_token().unwrap().0;
        let request = Request::new(&selection, &revision, Draft::default()).unwrap();
        selection.invalidate_review();
        assert!(!request.matches(&selection));
    }

    #[test]
    fn privacy_loss_hides_review_before_coordinator_reconciliation() {
        let selection = WorkflowView::captured_for_test(bundle());
        let (revision, cancelled) = selection.review_token().unwrap();
        let privacy = Arc::new(PrivacyGate::new(false));
        let target = Broker::new([7; 16]).attach_target(3, 1).unwrap();
        let attachment = Arc::new(festerm_session::AutomationEpoch::new());
        attachment.transition(true);
        let generation = attachment.capture().unwrap();
        let row = TargetRow {
            reference: target,
            label: "description only".into(),
            guard: TargetGuard {
                liveness: Arc::new(AtomicBool::new(true)),
                registration: Arc::new(AtomicBool::new(true)),
                attachment,
                generation,
            },
        };
        let review = Review::prepare(
            selection.current_bundle().unwrap(),
            Draft {
                parameters: BTreeMap::new(),
                targets: BTreeSet::from([target]),
            },
            &BTreeSet::from([target]),
        )
        .unwrap();
        let proposal = Proposal {
            review,
            targets: vec![row],
            revision,
            cancelled,
            privacy: privacy.clone(),
        };
        assert!(proposal.current());
        privacy.close();
        assert!(!proposal.current());
    }
}
