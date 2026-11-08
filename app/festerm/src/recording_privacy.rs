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
type Monitor = festerm_linux_power::RecordingPrivacyMonitor;
#[cfg(windows)]
type Monitor = festerm_windows_power::SharedPrivacyMonitor;

type Wake = Arc<dyn Fn() + Send + Sync>;
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
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancelled = cancelled.clone();
        let (send, receiver) = mpsc::sync_channel(1);
        thread::Builder::new()
            .name("festerm-recording-privacy".to_owned())
            .spawn(move || {
                let result = install(wake.clone()).and_then(|guard| {
                    if worker_cancelled.load(Ordering::Acquire) {
                        drop(guard);
                        Err("Recording preparation was cancelled".to_owned())
                    } else {
                        Ok(guard)
                    }
                });
                let _ = send.send(result);
                wake();
            })
            .map_err(|error| format!("Recording privacy monitor could not start: {error}"))?;
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
}

impl Drop for PrivacyTask {
    fn drop(&mut self) {
        self.cancel();
    }
}

fn install(wake: Wake) -> Result<PrivacyGuard, String> {
    let loss = Arc::new(AtomicBool::new(false));
    let recording: Slot = Arc::new(Mutex::new(None));
    let callback_loss = loss.clone();
    let callback_slot = recording.clone();
    let callback: Wake = Arc::new(move || {
        callback_loss.store(true, Ordering::Release);
        let recording = callback_slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(recording) = recording {
            if let Err(error) = recording.stop(FinishReason::Lifecycle) {
                tracing::error!("Could not stop recording after privacy loss: {error}");
            }
        }
        wake();
    });
    #[cfg(windows)]
    let monitor = Monitor::install_with_loss_flag(callback, loss.clone());
    #[cfg(not(windows))]
    let monitor = Monitor::install(callback);
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
        let _ = &mut error;
        #[cfg(windows)]
        let hint = if unresolved {
            " Privacy cleanup is unresolved; restart the app."
        } else {
            " Check the Windows session and try again."
        };
        #[cfg(not(windows))]
        let hint = "";
        format!("Recording cannot start without lock and sleep monitoring: {error}.{hint}")
    })?;
    if loss.load(Ordering::Acquire) {
        return Err("Recording cannot start because device privacy is unavailable".to_owned());
    }
    Ok(PrivacyGuard {
        monitor: Some(monitor),
        loss,
        recording,
    })
}
