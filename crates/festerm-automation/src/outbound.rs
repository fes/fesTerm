//! In-memory framing and bounded queue shared with the host transport.
//!
//! These types do not authorize a disclosure. The host still owns admission,
//! connection closure, frame limits and the final transport checks.

use festerm_admission::{AutomationAdmissionGuard, AutomationEpoch};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError, TrySendError},
        Arc,
    },
    time::{Duration, Instant},
};

#[derive(Clone)]
pub struct OutboundTargetGuard {
    pub liveness: Arc<AtomicBool>,
    pub registration: Arc<AtomicBool>,
    pub attachment: Arc<AutomationEpoch>,
    pub generation: u64,
}

impl OutboundTargetGuard {
    /// The caller retains the admission boundary through the protected action.
    pub fn is_valid(&self) -> bool {
        self.liveness.load(Ordering::Acquire)
            && self.registration.load(Ordering::Acquire)
            && self.attachment.is_current(self.generation)
    }
}

pub struct OutboundFrame {
    pub bytes: Vec<u8>,
    pub expires: Instant,
    pub targets: Vec<OutboundTargetGuard>,
    pub flags: Vec<Arc<AtomicBool>>,
}

impl OutboundFrame {
    /// The caller retains the admission boundary through the protected action.
    pub fn is_valid(&self) -> bool {
        self.targets.iter().all(OutboundTargetGuard::is_valid)
            && self.flags.iter().all(|flag| flag.load(Ordering::Acquire))
    }
}

#[derive(Clone)]
pub struct OutboundSender(SyncSender<OutboundFrame>);

pub struct OutboundReceiver(Receiver<OutboundFrame>);

/// Creates the existing four-frame queue. Neither endpoint grants authority.
pub fn outbound_channel() -> (OutboundSender, OutboundReceiver) {
    let (sender, receiver) = mpsc::sync_channel(4);
    (OutboundSender(sender), OutboundReceiver(receiver))
}

impl OutboundSender {
    /// Insert while retaining the caller's admission boundary.
    ///
    /// The guard alone does not authorize the frame. The host must validate
    /// the frame and its connection under this same guard before insertion.
    ///
    /// ```compile_fail
    /// use festerm_automation::{OutboundFrame, OutboundSender};
    /// fn enqueue_without_admission(sender: &OutboundSender, frame: OutboundFrame) {
    ///     let _ = sender.try_send(frame);
    /// }
    /// ```
    pub fn try_send(
        &self,
        frame: OutboundFrame,
        _guard: &AutomationAdmissionGuard,
    ) -> Result<(), TrySendError<OutboundFrame>> {
        self.0.try_send(frame)
    }
}

impl OutboundReceiver {
    pub fn recv_timeout(&self, timeout: Duration) -> Result<OutboundFrame, RecvTimeoutError> {
        self.0.recv_timeout(timeout)
    }

    pub fn try_recv(&self) -> Result<OutboundFrame, TryRecvError> {
        self.0.try_recv()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(byte: u8) -> OutboundFrame {
        OutboundFrame {
            bytes: vec![byte],
            expires: Instant::now() + Duration::from_secs(60),
            targets: Vec::new(),
            flags: Vec::new(),
        }
    }

    fn try_send_with_admission(
        sender: &OutboundSender,
        frame: OutboundFrame,
    ) -> Result<(), TrySendError<OutboundFrame>> {
        let guard = festerm_admission::automation_admission_lock().unwrap();
        sender.try_send(frame, &guard)
    }

    #[test]
    fn outbound_queue_retains_capacity_fifo_and_disconnect_semantics() {
        let (sender, receiver) = outbound_channel();
        let other_sender = sender.clone();
        for byte in 0..4 {
            assert!(try_send_with_admission(&sender, frame(byte)).is_ok());
        }
        let rejected = match try_send_with_admission(&other_sender, frame(4)) {
            Err(TrySendError::Full(frame)) => frame,
            _ => panic!("expected full queue"),
        };
        assert_eq!(rejected.bytes, [4]);
        for byte in 0..4 {
            assert_eq!(receiver.try_recv().unwrap().bytes, [byte]);
        }
        assert!(matches!(receiver.try_recv(), Err(TryRecvError::Empty)));
        drop(receiver);
        assert!(matches!(
            try_send_with_admission(&other_sender, rejected),
            Err(TrySendError::Disconnected(_))
        ));

        let (sender, receiver) = outbound_channel();
        assert!(try_send_with_admission(&sender, frame(5)).is_ok());
        drop(sender);
        assert_eq!(receiver.try_recv().unwrap().bytes, [5]);
        assert!(matches!(
            receiver.try_recv(),
            Err(TryRecvError::Disconnected)
        ));
    }

    #[test]
    fn outbound_frame_keeps_live_target_and_registration_checks() {
        let attachment = Arc::new(AutomationEpoch::new());
        attachment.transition(true);
        let generation = attachment.capture().unwrap();
        let registration = Arc::new(AtomicBool::new(true));
        let flag = Arc::new(AtomicBool::new(true));
        let mut frame = frame(1);
        frame.targets.push(OutboundTargetGuard {
            liveness: Arc::new(AtomicBool::new(true)),
            registration: registration.clone(),
            attachment: attachment.clone(),
            generation,
        });
        frame.flags.push(flag.clone());
        let (sender, receiver) = outbound_channel();
        assert!(try_send_with_admission(&sender, frame).is_ok());
        let frame = receiver.try_recv().unwrap();
        let (initial, flag_invalid, registration_invalid) = {
            let _guard = festerm_admission::automation_admission_lock().unwrap();
            let initial = frame.is_valid();
            flag.store(false, Ordering::Release);
            let flag_invalid = !frame.is_valid();
            flag.store(true, Ordering::Release);
            registration.store(false, Ordering::Release);
            let registration_invalid = !frame.is_valid();
            registration.store(true, Ordering::Release);
            (initial, flag_invalid, registration_invalid)
        };
        assert!(initial && flag_invalid && registration_invalid);
        attachment.transition(false);
        let valid_after_transition = {
            let _guard = festerm_admission::automation_admission_lock().unwrap();
            frame.is_valid()
        };
        assert!(!valid_after_transition);
    }
}
