//! A generic Mutex+Condvar+`tokio::sync::Notify` pause-and-resolve gate.
//!
//! [`HostKeyDecisionGate`](crate::HostKeyDecisionGate) and
//! [`PasswordDecisionGate`](crate::PasswordDecisionGate) (defined in `lib.rs`)
//! are thin, differently-typed wrappers around [`DecisionGate`]. They differ
//! only in their prompt type, their resolved value type, the value returned
//! on cancellation or timeout, and their error message strings; the
//! synchronisation behaviour below is shared verbatim.

use std::{
    sync::{Condvar, Mutex},
    time::Duration,
};

/// How a resolved decision value is taken out of the gate's state.
///
/// This lets the generic gate consume a resolved value while preserving each
/// concrete decision type's own security behaviour: a `Copy` decision (such
/// as a host-key trust decision) is copied out, while a secret-bearing value
/// (such as a password) is moved out and the slot is left with its `Default`
/// (for `String`, an empty string) rather than a lingering duplicate.
pub(crate) trait DecisionValue {
    fn take_from(slot: &mut Self) -> Self;
}

impl DecisionValue for crate::HostTrustDecision {
    fn take_from(slot: &mut Self) -> Self {
        *slot
    }
}

impl DecisionValue for String {
    fn take_from(slot: &mut Self) -> Self {
        std::mem::take(slot)
    }
}

/// A rejected or stale attempt to resolve a pending decision.
///
/// Shared by [`HostKeyDecisionResolutionError`](crate::HostKeyDecisionResolutionError)
/// and [`PasswordDecisionResolutionError`](crate::PasswordDecisionResolutionError),
/// which wrap this with their own (differently worded) public `Display` text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DecisionResolutionError {
    NoPendingPrompt,
    AlreadyResolved,
    PromptMismatch,
}

#[allow(dead_code)]
enum GateState<P, D> {
    Idle,
    Waiting(P),
    Resolved(D),
    Cancelled,
}

pub(crate) struct DecisionGate<P, D> {
    state: Mutex<GateState<P, D>>,
    changed: Condvar,
    notified: tokio::sync::Notify,
}

#[allow(dead_code)]
impl<P: Clone + PartialEq, D: DecisionValue> DecisionGate<P, D> {
    pub(crate) fn new() -> Self {
        Self {
            state: Mutex::new(GateState::Idle),
            changed: Condvar::new(),
            notified: tokio::sync::Notify::new(),
        }
    }

    pub(crate) fn begin(&self, prompt: P) -> Result<DecisionWaiter<P>, DecisionResolutionError> {
        let mut state = self
            .state
            .lock()
            .expect("decision gate lock is not poisoned");
        match *state {
            GateState::Idle => {
                *state = GateState::Waiting(prompt.clone());
                Ok(DecisionWaiter { prompt })
            }
            GateState::Resolved(_) => Err(DecisionResolutionError::AlreadyResolved),
            GateState::Waiting(_) | GateState::Cancelled => {
                Err(DecisionResolutionError::NoPendingPrompt)
            }
        }
    }

    pub(crate) fn resolve(&self, prompt: &P, decision: D) -> Result<(), DecisionResolutionError> {
        let mut state = self
            .state
            .lock()
            .expect("decision gate lock is not poisoned");
        match &*state {
            GateState::Waiting(current) if current == prompt => {
                *state = GateState::Resolved(decision);
                self.changed.notify_all();
                self.notified.notify_waiters();
                Ok(())
            }
            GateState::Waiting(_) => Err(DecisionResolutionError::PromptMismatch),
            GateState::Resolved(_) => Err(DecisionResolutionError::AlreadyResolved),
            GateState::Idle | GateState::Cancelled => Err(DecisionResolutionError::NoPendingPrompt),
        }
    }

    pub(crate) fn cancel(&self, prompt: &P) -> Result<(), DecisionResolutionError> {
        let mut state = self
            .state
            .lock()
            .expect("decision gate lock is not poisoned");
        match &*state {
            GateState::Waiting(current) if current == prompt => {
                *state = GateState::Cancelled;
                self.changed.notify_all();
                self.notified.notify_waiters();
                Ok(())
            }
            GateState::Waiting(_) => Err(DecisionResolutionError::PromptMismatch),
            GateState::Resolved(_) => Err(DecisionResolutionError::AlreadyResolved),
            GateState::Idle | GateState::Cancelled => Err(DecisionResolutionError::NoPendingPrompt),
        }
    }

    pub(crate) fn wait_for_decision(&self, timeout: Duration) -> Option<D> {
        let state = self
            .state
            .lock()
            .expect("decision gate lock is not poisoned");
        let (mut state, _) = self
            .changed
            .wait_timeout_while(state, timeout, |state| {
                matches!(state, GateState::Waiting(_))
            })
            .expect("decision gate lock is not poisoned");
        let decision = match &mut *state {
            GateState::Resolved(value) => Some(D::take_from(value)),
            GateState::Idle | GateState::Waiting(_) | GateState::Cancelled => None,
        };
        *state = GateState::Idle;
        self.notified.notify_waiters();
        decision
    }

    pub(crate) fn reject_pending(&self) {
        let mut state = self
            .state
            .lock()
            .expect("decision gate lock is not poisoned");
        if matches!(*state, GateState::Waiting(_)) {
            *state = GateState::Cancelled;
            self.changed.notify_all();
            self.notified.notify_waiters();
        }
    }

    pub(crate) async fn wait_for_decision_async(&self, timeout: Duration) -> Option<D> {
        let decision = tokio::time::timeout(timeout, async {
            loop {
                let notified = self.notified.notified();
                {
                    let mut state = self
                        .state
                        .lock()
                        .expect("decision gate lock is not poisoned");
                    match &mut *state {
                        GateState::Resolved(value) => return Some(D::take_from(value)),
                        GateState::Idle | GateState::Cancelled => return None,
                        GateState::Waiting(_) => {}
                    }
                }
                notified.await;
            }
        })
        .await
        .unwrap_or(None);
        *self
            .state
            .lock()
            .expect("decision gate lock is not poisoned") = GateState::Idle;
        self.changed.notify_all();
        self.notified.notify_waiters();
        decision
    }
}

/// Worker-only proof that a prompt has been emitted and may now be awaited.
#[allow(dead_code)]
pub(crate) struct DecisionWaiter<P> {
    prompt: P,
}

#[allow(dead_code)]
impl<P> DecisionWaiter<P> {
    pub(crate) fn prompt(&self) -> &P {
        &self.prompt
    }

    pub(crate) fn wait<D: DecisionValue>(
        self,
        gate: &DecisionGate<P, D>,
        timeout: Duration,
    ) -> Option<D>
    where
        P: Clone + PartialEq,
    {
        gate.wait_for_decision(timeout)
    }

    pub(crate) async fn wait_async<D: DecisionValue>(
        self,
        gate: &DecisionGate<P, D>,
        timeout: Duration,
    ) -> Option<D>
    where
        P: Clone + PartialEq,
    {
        gate.wait_for_decision_async(timeout).await
    }
}
