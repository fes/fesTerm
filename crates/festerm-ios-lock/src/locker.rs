use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BiometryKind {
    None,
    FaceId,
    TouchId,
    OpticId,
}

impl BiometryKind {
    /// Label of the primary unlock button ("Use Face ID").
    pub const fn button_label(self) -> &'static str {
        match self {
            Self::FaceId => "Use Face ID",
            Self::TouchId => "Use Touch ID",
            Self::OpticId => "Use Optic ID",
            Self::None => "Unlock",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Availability {
    Available(BiometryKind),
    /// No device passcode: the OS cannot authenticate the owner at all.
    PasscodeNotSet,
    Unavailable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthFailure {
    /// The user dismissed the dialog or the system cancelled it.
    Cancelled,
    NotRecognized,
    BiometryLockedOut,
    PasscodeNotSet,
    Unavailable,
}

pub type AuthOutcome = Result<(), AuthFailure>;

/// Owner authentication, with passcode fallback.
pub trait Locker: Send + Sync {
    fn availability(&self) -> Availability;

    /// Presents the system dialog. `done` is invoked exactly once, on any thread.
    fn authenticate(&self, reason: &str, done: Box<dyn FnOnce(AuthOutcome) + Send>);
}

/// Scriptable locker for tests. Outcomes complete synchronously unless
/// `defer` is set, in which case `complete_pending` finishes them.
pub struct FakeLocker {
    state: Mutex<FakeState>,
}

struct FakeState {
    availability: Availability,
    script: VecDeque<AuthOutcome>,
    defer: bool,
    pending: VecDeque<Box<dyn FnOnce(AuthOutcome) + Send>>,
    reasons: Vec<String>,
}

impl FakeLocker {
    pub fn new(availability: Availability) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(FakeState {
                availability,
                script: VecDeque::new(),
                defer: false,
                pending: VecDeque::new(),
                reasons: Vec::new(),
            }),
        })
    }

    fn state(&self) -> std::sync::MutexGuard<'_, FakeState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn push_outcome(&self, outcome: AuthOutcome) {
        self.state().script.push_back(outcome);
    }

    pub fn set_availability(&self, availability: Availability) {
        self.state().availability = availability;
    }

    pub fn set_deferred(&self, defer: bool) {
        self.state().defer = defer;
    }

    pub fn pending_count(&self) -> usize {
        self.state().pending.len()
    }

    pub fn complete_pending(&self, outcome: AuthOutcome) {
        let done = self.state().pending.pop_front();
        if let Some(done) = done {
            done(outcome);
        }
    }

    pub fn prompt_reasons(&self) -> Vec<String> {
        self.state().reasons.clone()
    }
}

impl Locker for FakeLocker {
    fn availability(&self) -> Availability {
        self.state().availability
    }

    fn authenticate(&self, reason: &str, done: Box<dyn FnOnce(AuthOutcome) + Send>) {
        let outcome = {
            let mut state = self.state();
            state.reasons.push(reason.to_owned());
            if state.defer {
                state.pending.push_back(done);
                return;
            }
            state
                .script
                .pop_front()
                .unwrap_or(Err(AuthFailure::Cancelled))
        };
        done(outcome);
    }
}
