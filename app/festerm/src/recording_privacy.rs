use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, TryRecvError},
        Arc, Mutex,
    },
    thread,
};

use festerm_recording::{FinishReason, SharedRecording};

#[cfg(target_os = "macos")]
type Monitor = festerm_macos_window::PrivacyMonitor;
#[cfg(target_os = "linux")]
enum Monitor {
    Recording {
        _monitor: festerm_linux_power::RecordingPrivacyMonitor,
    },
    #[cfg(feature = "experimental-dev-tunnels")]
    Session {
        _monitor: festerm_linux_power::PrivacyMonitor,
    },
}
#[cfg(windows)]
type Monitor = festerm_windows_power::SharedPrivacyMonitor;

type Wake = Arc<dyn Fn() + Send + Sync>;
type PreparationFailure = Arc<dyn Fn(bool) + Send + Sync>;
type Slot = Arc<Mutex<Option<SharedRecording>>>;

pub(crate) struct PrivacyGuard {
    monitor: Option<Monitor>,
    loss: Arc<AtomicBool>,
    recording: Slot,
}

impl PrivacyGuard {
    pub(crate) fn is_lost(&self) -> bool {
        self.loss.load(Ordering::Acquire)
    }

    #[cfg(test)]
    pub(crate) fn for_test(loss: Arc<AtomicBool>) -> Self {
        Self {
            monitor: None,
            loss,
            recording: Arc::new(Mutex::new(None)),
        }
    }

    pub(crate) fn loss_flag(&self) -> Arc<AtomicBool> {
        self.loss.clone()
    }

    pub(crate) fn bind(&self, recording: &SharedRecording) -> Result<(), String> {
        let mut slot = self
            .recording
            .lock()
            .map_err(|_| "Recording privacy state is unavailable".to_owned())?;
        if self.loss.load(Ordering::Acquire) {
            drop(slot);
            recording
                .stop(FinishReason::Lifecycle)
                .map_err(|error| error.to_string())?;
            return Err("Recording stopped because device privacy changed".to_owned());
        }
        *slot = Some(recording.clone());
        if self.loss.load(Ordering::Acquire) {
            drop(slot);
            recording
                .stop(FinishReason::Lifecycle)
                .map_err(|error| error.to_string())?;
            return Err("Recording stopped because device privacy changed".to_owned());
        }
        Ok(())
    }
}

impl Drop for PrivacyGuard {
    fn drop(&mut self) {
        self.loss.store(true, Ordering::Release);
        let recording = self
            .recording
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(recording) = recording {
            if let Err(error) = recording.stop(FinishReason::Lifecycle) {
                tracing::error!("Could not stop recording during privacy cleanup: {error}");
            }
        }
        if let Some(monitor) = self.monitor.take() {
            if let Err(error) = thread::Builder::new()
                .name("festerm-recording-privacy-cleanup".to_owned())
                .spawn(move || {
                    #[cfg(windows)]
                    if !monitor.close() {
                        tracing::error!(
                            "Recording privacy cleanup is unresolved; restart is required"
                        );
                    }
                    #[cfg(not(windows))]
                    drop(monitor);
                })
            {
                tracing::error!("Could not start recording privacy cleanup: {error}");
            }
        }
    }
}

pub(crate) struct PrivacyTask {
    receiver: Receiver<Result<PrivacyGuard, String>>,
    cancelled: Arc<AtomicBool>,
}

impl PrivacyTask {
    pub(crate) fn start(wake: Wake) -> Result<Self, String> {
        Self::start_inner(wake, None, (), None)
    }

    /// Session access reuses the existing lock/sleep monitor; it does not
    /// inherit recording's additional Linux screen-capture prerequisites.
    #[cfg(all(feature = "experimental-dev-tunnels", not(test)))]
    pub(crate) fn start_with_loss(
        wake: Wake,
        on_loss: Wake,
        charge: impl Send + 'static,
        on_preparation_failure: PreparationFailure,
    ) -> Result<Self, String> {
        Self::start_inner(wake, Some(on_loss), charge, Some(on_preparation_failure))
    }

    fn start_inner(
        wake: Wake,
        on_loss: Option<Wake>,
        charge: impl Send + 'static,
        on_preparation_failure: Option<PreparationFailure>,
    ) -> Result<Self, String> {
        let activity = if on_loss.is_some() {
            "Remote connection"
        } else {
            "Recording"
        };
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancelled = cancelled.clone();
        let (send, receiver) = mpsc::sync_channel(1);
        thread::Builder::new()
            .name("festerm-recording-privacy".to_owned())
            .spawn(move || {
                // Cancellation of the waiter never releases admitted native work.
                let _charge = charge;
                let result =
                    install(wake.clone(), on_loss, on_preparation_failure).and_then(|guard| {
                        if worker_cancelled.load(Ordering::Acquire) {
                            drop(guard);
                            Err(format!("{activity} preparation was cancelled"))
                        } else {
                            Ok(guard)
                        }
                    });
                let _ = send.send(result);
                wake();
            })
            .map_err(|error| format!("{activity} privacy monitor could not start: {error}"))?;
        Ok(Self {
            receiver,
            cancelled,
        })
    }

    pub(crate) fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    pub(crate) fn poll(&self) -> Option<Result<PrivacyGuard, String>> {
        match self.receiver.try_recv() {
            Ok(Ok(guard)) if self.cancelled.load(Ordering::Acquire) => {
                drop(guard);
                Some(Err("Recording preparation was cancelled".to_owned()))
            }
            Ok(result) => Some(result),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err(
                "Recording privacy monitor stopped unexpectedly".to_owned(),
            )),
        }
    }

    /// Called only by an admitted background worker, before the wire deadline.
    #[cfg(feature = "experimental-dev-tunnels")]
    pub(crate) fn wait(
        &self,
        timeout: std::time::Duration,
    ) -> Option<Result<PrivacyGuard, String>> {
        match self.receiver.recv_timeout(timeout) {
            Ok(Ok(guard)) if self.cancelled.load(Ordering::Acquire) => {
                drop(guard);
                Some(Err("Privacy preparation was cancelled".to_owned()))
            }
            Ok(result) => Some(result),
            Err(mpsc::RecvTimeoutError::Timeout) => None,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                Some(Err("Privacy monitor stopped unexpectedly".to_owned()))
            }
        }
    }
}

impl Drop for PrivacyTask {
    fn drop(&mut self) {
        self.cancel();
    }
}

fn privacy_loss_callback(
    loss: Arc<AtomicBool>,
    recording: Slot,
    wake: Wake,
    on_loss: Option<Wake>,
) -> Wake {
    Arc::new(move || {
        loss.store(true, Ordering::Release);
        if let Some(on_loss) = &on_loss {
            on_loss();
        }
        let recording = recording
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(recording) = recording {
            if let Err(error) = recording.stop(FinishReason::Lifecycle) {
                tracing::error!("Could not stop recording after privacy loss: {error}");
            }
        }
        wake();
    })
}

fn install(
    wake: Wake,
    on_loss: Option<Wake>,
    on_preparation_failure: Option<PreparationFailure>,
) -> Result<PrivacyGuard, String> {
    let loss = Arc::new(AtomicBool::new(false));
    let recording: Slot = Arc::new(Mutex::new(None));
    let session = on_loss.is_some();
    let activity = if session {
        "Remote connection"
    } else {
        "Recording"
    };
    let callback = privacy_loss_callback(loss.clone(), recording.clone(), wake, on_loss);
    #[cfg(windows)]
    let monitor = Monitor::install_with_loss_flag(callback, loss.clone());
    #[cfg(target_os = "macos")]
    let monitor = Monitor::install(callback);
    #[cfg(target_os = "linux")]
    let monitor = {
        #[cfg(feature = "experimental-dev-tunnels")]
        if session {
            festerm_linux_power::PrivacyMonitor::install(callback)
                .map(|monitor| Monitor::Session { _monitor: monitor })
        } else {
            festerm_linux_power::RecordingPrivacyMonitor::install(callback)
                .map(|monitor| Monitor::Recording { _monitor: monitor })
        }
        #[cfg(not(feature = "experimental-dev-tunnels"))]
        festerm_linux_power::RecordingPrivacyMonitor::install(callback)
            .map(|monitor| Monitor::Recording { _monitor: monitor })
    };
    let monitor = monitor.map_err(|mut error| {
        #[cfg(windows)]
        let unresolved = {
            let (cleanup, rejected) = error
                .get_mut()
                .and_then(|source| {
                    source.downcast_mut::<festerm_windows_power::PrivacyInstallFailure>()
                })
                .map(|failure| (failure.cleanup_status(), failure.take_rejected()))
                .unwrap_or((None, None));
            drop(rejected);
            cleanup
                .as_ref()
                .is_some_and(|status| !crate::automation::finish_privacy_cleanup(status))
        };
        #[cfg(not(windows))]
        let unresolved = false;
        if let Some(report) = &on_preparation_failure {
            report(unresolved);
        }
        #[cfg(not(windows))]
        let _ = &mut error;
        #[cfg(windows)]
        let hint = if unresolved {
            " Privacy cleanup is unresolved; restart the app."
        } else {
            " Check the Windows session and try again."
        };
        #[cfg(not(windows))]
        let hint = "";
        format!("{activity} cannot start without lock and sleep monitoring: {error}.{hint}")
    })?;
    if loss.load(Ordering::Acquire) {
        if let Some(report) = &on_preparation_failure {
            report(false);
        }
        return Err(format!(
            "{activity} cannot start because device privacy is unavailable"
        ));
    }
    Ok(PrivacyGuard {
        monitor: Some(monitor),
        loss,
        recording,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "experimental-dev-tunnels")]
    #[test]
    fn session_privacy_loss_cancels_before_wake_without_a_ui_frame() {
        let loss = Arc::new(AtomicBool::new(false));
        let cancel = tokio_util::sync::CancellationToken::new();
        let observed_loss = loss.clone();
        let observed_cancel = cancel.clone();
        let woke = Arc::new(AtomicBool::new(false));
        let observed_wake = woke.clone();
        let callback = privacy_loss_callback(
            loss.clone(),
            Arc::new(Mutex::new(None)),
            Arc::new(move || {
                assert!(observed_loss.load(Ordering::Acquire));
                assert!(observed_cancel.is_cancelled());
                observed_wake.store(true, Ordering::Release);
            }),
            Some(Arc::new(move || cancel.cancel())),
        );
        assert!(!loss.load(Ordering::Acquire));
        assert!(!woke.load(Ordering::Acquire));
        callback();
        assert!(woke.load(Ordering::Acquire));
    }

    #[cfg(feature = "experimental-dev-tunnels")]
    #[test]
    fn session_privacy_wait_refuses_late_ready_guard_after_cancellation() {
        let (send, receiver) = mpsc::sync_channel(1);
        let task = PrivacyTask {
            receiver,
            cancelled: Arc::new(AtomicBool::new(false)),
        };
        assert!(task.wait(std::time::Duration::ZERO).is_none());
        let loss = Arc::new(AtomicBool::new(false));
        assert!(send.send(Ok(PrivacyGuard::for_test(loss.clone()))).is_ok());
        task.cancel();
        assert!(matches!(task.wait(std::time::Duration::ZERO), Some(Err(_))));
        assert!(loss.load(Ordering::Acquire));
    }
}
