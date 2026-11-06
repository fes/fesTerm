//! Native service boundary. No terminal, UI or secret-store policy lives here.

use std::collections::VecDeque;
use zeroize::Zeroize;

pub const MAX_INPUT_BYTES: usize = 64 * 1024;
const MAX_INPUT_EVENTS: usize = 256;

/// Character offsets, never UTF-16 offsets. Native code converts at the seam.
#[derive(Default)]
pub struct Input {
    pub target: u64,
    pub sequence: u64,
    pub kind: i32,
    pub text: String,
    pub start: usize,
    pub end: usize,
}

impl Drop for Input {
    fn drop(&mut self) {
        self.text.zeroize();
    }
}

#[derive(Default)]
pub struct InputQueue {
    events: VecDeque<Input>,
    bytes: usize,
    overflow: bool,
}

impl InputQueue {
    pub fn has_pending(&self) -> bool {
        self.overflow || !self.events.is_empty()
    }

    pub fn reject(&mut self) {
        self.clear();
        self.overflow = true;
    }

    pub fn push(&mut self, input: Input) {
        if self.overflow {
            return;
        }
        if self.events.len() >= MAX_INPUT_EVENTS
            || input.text.len() > MAX_INPUT_BYTES.saturating_sub(self.bytes)
        {
            self.clear();
            self.overflow = true;
            return;
        }
        self.bytes += input.text.len();
        self.events.push_back(input);
    }

    /// Overflow is an explicit failure, never a partially replayed command.
    pub fn take(&mut self) -> Result<Vec<Input>, &'static str> {
        if std::mem::take(&mut self.overflow) {
            return Err("Android input overflowed. Input was cleared; reconnect before typing.");
        }
        let mut batch = Vec::new();
        while let Some(input) = self.events.front() {
            if input.kind == 3 && !batch.is_empty() {
                break;
            }
            let input = self.events.pop_front().expect("front exists");
            self.bytes -= input.text.len();
            batch.push(input);
        }
        Ok(batch)
    }

    pub fn clear(&mut self) {
        self.events.clear();
        self.bytes = 0;
        self.overflow = false;
    }
}

/// Result codes shared with `FestermActivity`. Unknown codes are failures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthResult {
    Success,
    Cancelled,
    LockedOut,
    NoCredential,
    Unavailable,
}

impl AuthResult {
    pub fn from_code(code: i32) -> Self {
        match code {
            0 => Self::Success,
            1 => Self::Cancelled,
            2 => Self::LockedOut,
            3 => Self::NoCredential,
            _ => Self::Unavailable,
        }
    }
}

pub type AuthCallback = Box<dyn FnOnce(AuthResult) + Send>;

/// The newest native authentication request. Replies and cancellations name
/// their request, so a late reply from a retired prompt cannot complete a newer
/// one and a stale cancellation cannot dismiss it.
#[derive(Default)]
pub struct AuthRequests {
    last: u64,
    pending: Option<(u64, AuthCallback)>,
}

impl AuthRequests {
    /// Returns the new token and any superseded request. The caller cancels the
    /// superseded prompt natively and completes it outside the owning lock.
    pub fn begin(&mut self, done: AuthCallback) -> (u64, Option<(u64, AuthCallback)>) {
        self.last = self.last.saturating_add(1);
        let superseded = self.pending.replace((self.last, done));
        (self.last, superseded)
    }

    /// Removes the callback only when `token` names the pending request.
    pub fn take(&mut self, token: u64) -> Option<AuthCallback> {
        match &self.pending {
            Some((pending, _)) if *pending == token => self.pending.take().map(|(_, done)| done),
            _ => None,
        }
    }

    pub fn take_pending(&mut self) -> Option<AuthCallback> {
        self.pending.take().map(|(_, done)| done)
    }
}

#[cfg(target_os = "android")]
mod native;
#[cfg(target_os = "android")]
pub use native::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authentication_replies_and_cancellations_affect_only_their_request() {
        let mut requests = AuthRequests::default();
        let (first, superseded) = requests.begin(Box::new(|_| {}));
        assert!(superseded.is_none());
        let (second, superseded) = requests.begin(Box::new(|_| {}));
        assert_eq!(superseded.map(|(token, _)| token), Some(first));
        assert!(requests.take(first).is_none());
        assert!(requests.take(0).is_none());
        assert!(requests.take(second).is_some());
        assert!(requests.take(second).is_none());
        assert!(requests.take_pending().is_none());
    }

    #[test]
    fn unknown_authentication_codes_fail_closed() {
        assert_eq!(AuthResult::from_code(0), AuthResult::Success);
        assert_eq!(AuthResult::from_code(2), AuthResult::LockedOut);
        assert_eq!(AuthResult::from_code(3), AuthResult::NoCredential);
        for code in [-1, 4, 99, i32::MAX] {
            assert_eq!(AuthResult::from_code(code), AuthResult::Unavailable);
        }
    }

    #[test]
    fn overflow_rejects_the_whole_batch_and_reports_failure() {
        let mut queue = InputQueue::default();
        queue.push(Input {
            text: "command".into(),
            ..Input::default()
        });
        queue.push(Input {
            text: "x".repeat(MAX_INPUT_BYTES),
            ..Input::default()
        });
        queue.push(Input {
            text: "\n".into(),
            ..Input::default()
        });
        assert!(queue.take().is_err());
        assert!(queue.take().unwrap().is_empty());
    }

    #[test]
    fn selection_starts_its_own_ordered_batch() {
        let mut queue = InputQueue::default();
        queue.push(Input {
            kind: 1,
            text: "é".into(),
            ..Input::default()
        });
        queue.push(Input {
            target: 0,
            sequence: 0,
            kind: 3,
            text: String::new(),
            start: 1,
            end: 1,
        });
        queue.push(Input {
            kind: 1,
            text: "b".into(),
            ..Input::default()
        });
        assert_eq!(queue.take().unwrap().len(), 1);
        let next = queue.take().unwrap();
        assert_eq!(next.len(), 2);
        assert_eq!(next[0].kind, 3);
        assert_eq!(next[1].text, "b");
    }
}
