use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
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

    pub const fn retry_button_label(self) -> &'static str {
        match self {
            Self::FaceId => "Try Face ID Again",
            Self::TouchId => "Try Touch ID Again",
            Self::OpticId => "Try Optic ID Again",
            Self::None => "Try Unlock Again",
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthMethod {
    OwnerAuthentication,
    DevicePasscode,
}

/// Retains the platform request and cancels it when its owner retires it.
pub struct Authentication {
    cancel: Option<Box<dyn FnOnce() + Send>>,
}

impl Authentication {
    pub fn new(cancel: impl FnOnce() + Send + 'static) -> Self {
        Self {
            cancel: Some(Box::new(cancel)),
        }
    }
}

impl Drop for Authentication {
    fn drop(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            cancel();
        }
    }
}

/// System owner authentication; the application never handles a passcode.
pub trait Locker: Send + Sync {
    fn availability(&self) -> Availability;

    /// Presents the system dialog. A reply can arrive after cancellation.
    fn authenticate(
        &self,
        method: AuthMethod,
        reason: &str,
        done: Box<dyn FnOnce(AuthOutcome) + Send>,
    ) -> Authentication;
}

/// Scriptable locker for tests. Outcomes complete synchronously unless
/// `defer` is set, in which case `complete_pending` finishes them.
pub struct FakeLocker {
    state: Mutex<FakeState>,
    cancellations: Arc<AtomicUsize>,
}

struct FakeState {
    availability: Availability,
    script: VecDeque<AuthOutcome>,
    defer: bool,
    pending: VecDeque<Box<dyn FnOnce(AuthOutcome) + Send>>,
    reasons: Vec<String>,
    methods: Vec<AuthMethod>,
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
                methods: Vec::new(),
            }),
            cancellations: Arc::new(AtomicUsize::new(0)),
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

    pub fn prompt_methods(&self) -> Vec<AuthMethod> {
        self.state().methods.clone()
    }

    pub fn cancellation_count(&self) -> usize {
        self.cancellations.load(Ordering::SeqCst)
    }
}

impl Locker for FakeLocker {
    fn availability(&self) -> Availability {
        self.state().availability
    }

    fn authenticate(
        &self,
        method: AuthMethod,
        reason: &str,
        done: Box<dyn FnOnce(AuthOutcome) + Send>,
    ) -> Authentication {
        let completed = Arc::new(AtomicBool::new(false));
        let completion = completed.clone();
        let done: Box<dyn FnOnce(AuthOutcome) + Send> = Box::new(move |outcome| {
            completion.store(true, Ordering::SeqCst);
            done(outcome);
        });
        let cancellations = self.cancellations.clone();
        let authentication = Authentication::new(move || {
            if !completed.load(Ordering::SeqCst) {
                cancellations.fetch_add(1, Ordering::SeqCst);
            }
        });
        let outcome = {
            let mut state = self.state();
            state.reasons.push(reason.to_owned());
            state.methods.push(method);
            if state.defer {
                state.pending.push_back(done);
                return authentication;
            }
            state
                .script
                .pop_front()
                .unwrap_or(Err(AuthFailure::Cancelled))
        };
        done(outcome);
        authentication
    }
}
