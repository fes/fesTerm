use std::{
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, MutexGuard,
    },
    time::Instant,
};

use festerm_core::Terminal;

use crate::{CaptureError, Completion, FinishReason, Recorder, RecordingId, StreamId};

struct State {
    recorder: Recorder,
    failure: Option<String>,
}

/// One application-wide recording shared by terminal owners and lifecycle
/// callbacks. The mutex only protects in-memory capture admission; disk I/O
/// remains on the recording worker.
#[derive(Clone)]
pub struct SharedRecording {
    state: Arc<Mutex<State>>,
    admitting: Arc<AtomicBool>,
    wake: Arc<dyn Fn() + Send + Sync>,
    id: RecordingId,
}

impl SharedRecording {
    pub fn start(
        private_directory: &Path,
        terminals: &[&Terminal],
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Result<(Self, Vec<RecordingStream>), CaptureError> {
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(wake);
        let worker_wake = Arc::clone(&wake);
        let (recorder, ids) = Recorder::start(private_directory, terminals, move || worker_wake())?;
        let shared = Self {
            id: recorder.id(),
            state: Arc::new(Mutex::new(State {
                recorder,
                failure: None,
            })),
            admitting: Arc::new(AtomicBool::new(true)),
            wake,
        };
        let streams = ids
            .into_iter()
            .map(|id| RecordingStream {
                recording: shared.clone(),
                id,
                ended: false,
            })
            .collect();
        Ok((shared, streams))
    }

    pub const fn id(&self) -> RecordingId {
        self.id
    }

    pub fn enroll(&self, terminal: &Terminal) -> Result<RecordingStream, CaptureError> {
        let id = self.apply(|recorder| {
            if self.admitting.load(Ordering::Acquire) {
                recorder.enroll(terminal)
            } else {
                Err(CaptureError::new("recording is not active"))
            }
        })?;
        Ok(RecordingStream {
            recording: self.clone(),
            id,
            ended: false,
        })
    }

    /// Stops every included stream, including when called by a native
    /// lifecycle callback without a subsequent UI frame.
    pub fn stop(&self, reason: FinishReason) -> Result<bool, CaptureError> {
        let stopped_at = Instant::now();
        self.admitting.store(false, Ordering::Release);
        self.apply(|recorder| {
            if recorder.is_recording() {
                recorder.stop_at(reason, stopped_at)?;
                Ok(true)
            } else {
                Ok(false)
            }
        })
    }

    /// Stops the entire set when the host can no longer establish the
    /// ordering or integrity of one included terminal.
    pub fn fail_integrity(&self, message: &'static str) -> Result<(), CaptureError> {
        let stopped_at = Instant::now();
        self.admitting.store(false, Ordering::Release);
        let mut state = self.lock()?;
        if !state.recorder.is_recording() {
            return Ok(());
        }
        state
            .recorder
            .stop_at(FinishReason::IntegrityFailure, stopped_at)?;
        if state.failure.is_none() {
            state.failure = Some(message.to_owned());
        }
        drop(state);
        (self.wake)();
        Ok(())
    }

    pub fn check_duration(&self) -> Result<(), CaptureError> {
        self.apply(|recorder| {
            if recorder.is_recording() {
                recorder.check_duration()
            } else {
                Ok(())
            }
        })
    }

    pub fn is_recording(&self) -> Result<bool, CaptureError> {
        if self.admitting.load(Ordering::Acquire) {
            Ok(self.lock()?.recorder.is_recording())
        } else {
            Ok(false)
        }
    }

    pub fn poll_completion(&self) -> Result<Option<Completion>, CaptureError> {
        Ok(self.lock()?.recorder.poll_completion())
    }

    /// Returns and clears the first capture admission error for presentation.
    pub fn take_failure(&self) -> Result<Option<String>, CaptureError> {
        Ok(self.lock()?.failure.take())
    }

    fn lock(&self) -> Result<MutexGuard<'_, State>, CaptureError> {
        match self.state.lock() {
            Ok(state) => Ok(state),
            Err(poisoned) => {
                self.admitting.store(false, Ordering::Release);
                let mut state = poisoned.into_inner();
                if state.recorder.is_recording() {
                    if let Err(error) = state.recorder.stop(FinishReason::IntegrityFailure) {
                        drop(state);
                        (self.wake)();
                        return Err(CaptureError::new(format!(
                            "recording state was poisoned and could not be stopped: {error}"
                        )));
                    }
                }
                drop(state);
                (self.wake)();
                Err(CaptureError::new(
                    "recording state was poisoned and stopped",
                ))
            }
        }
    }

    fn apply<T>(
        &self,
        operation: impl FnOnce(&mut Recorder) -> Result<T, CaptureError>,
    ) -> Result<T, CaptureError> {
        let mut state = self.lock()?;
        let result = operation(&mut state.recorder);
        if !state.recorder.is_recording() {
            self.admitting.store(false, Ordering::Release);
        }
        let notify = if let Err(error) = &result {
            if state.failure.is_none() {
                state.failure = Some(error.to_string());
                true
            } else {
                false
            }
        } else {
            false
        };
        drop(state);
        if notify {
            (self.wake)();
        }
        result
    }
}

/// The recording-local stream bound to one presented terminal. Dropping the
/// binding ends the stream but leaves the other sessions recording.
pub struct RecordingStream {
    recording: SharedRecording,
    id: StreamId,
    ended: bool,
}

impl RecordingStream {
    pub const fn id(&self) -> StreamId {
        self.id
    }

    pub fn observe_output(
        &self,
        bytes: &[u8],
        previous_generation: u64,
        terminal_after: &Terminal,
    ) -> Result<(), CaptureError> {
        self.observe(|recorder, id| {
            recorder.observe_output(id, bytes, previous_generation, terminal_after)
        })
    }

    pub fn observe_checkpoint(&self, terminal: &Terminal) -> Result<(), CaptureError> {
        self.observe(|recorder, id| recorder.observe_checkpoint(id, terminal))
    }

    pub fn observe_resize(&self, terminal: &Terminal) -> Result<(), CaptureError> {
        self.observe(|recorder, id| recorder.observe_resize(id, terminal))
    }

    pub fn end(&mut self) -> Result<(), CaptureError> {
        if self.ended {
            return Ok(());
        }
        self.ended = true;
        self.observe_inner(|recorder, id| recorder.end_stream(id))
    }

    pub fn fail_integrity(&self, message: &'static str) -> Result<(), CaptureError> {
        if self.ended {
            Ok(())
        } else {
            self.recording.fail_integrity(message)
        }
    }

    fn observe(
        &self,
        operation: impl FnOnce(&mut Recorder, StreamId) -> Result<(), CaptureError>,
    ) -> Result<(), CaptureError> {
        if self.ended {
            return Ok(());
        }
        self.observe_inner(operation)
    }

    fn observe_inner(
        &self,
        operation: impl FnOnce(&mut Recorder, StreamId) -> Result<(), CaptureError>,
    ) -> Result<(), CaptureError> {
        self.recording.apply(|recorder| {
            if self.recording.admitting.load(Ordering::Acquire) && recorder.is_recording() {
                operation(recorder, self.id)
            } else {
                Ok(())
            }
        })
    }
}

impl Drop for RecordingStream {
    fn drop(&mut self) {
        // Admission errors are retained by SharedRecording and wake the host.
        let _ = self.end();
    }
}

#[cfg(test)]
mod tests {
    use std::{fs::File, sync::mpsc, thread, time::Duration};

    use festerm_core::{Dimensions, Terminal};

    use super::*;
    use crate::{ArtifactEvent, ArtifactReader};

    #[test]
    fn lifecycle_closes_admission_before_waiting_for_the_capture_lock() {
        let directory = tempfile::tempdir().unwrap();
        let mut terminal = Terminal::new(Dimensions::new(20, 2).unwrap()).unwrap();
        let (wake, ready) = mpsc::channel();
        let (recording, mut streams) =
            SharedRecording::start(directory.path(), &[&terminal], move || {
                let _ = wake.send(());
            })
            .unwrap();
        let guard = recording.state.lock().unwrap();
        let lifecycle = recording.clone();
        let stop = thread::spawn(move || lifecycle.stop(FinishReason::Lifecycle).unwrap());
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while recording.admitting.load(Ordering::Acquire) {
            assert!(
                std::time::Instant::now() < deadline,
                "lifecycle cut timed out"
            );
            thread::yield_now();
        }
        let cutoff_upper_bound = guard.recorder.elapsed();
        assert!(!recording.is_recording().unwrap());
        let stream = streams.remove(0);
        let output = thread::spawn(move || {
            let generation = terminal.screen_generation();
            terminal.ingest(b"after lifecycle cut");
            stream
                .observe_output(b"after lifecycle cut", generation, &terminal)
                .unwrap();
        });
        thread::sleep(Duration::from_millis(50));
        drop(guard);
        stop.join().unwrap();
        output.join().unwrap();
        ready.recv_timeout(Duration::from_secs(10)).unwrap();
        let Completion::Complete {
            path,
            reason,
            duration,
        } = recording.poll_completion().unwrap().unwrap()
        else {
            panic!("expected a completed lifecycle cut");
        };
        assert_eq!(reason, FinishReason::Lifecycle);
        assert!(duration <= cutoff_upper_bound);
        let mut reader = ArtifactReader::new(File::open(path).unwrap()).unwrap();
        while let Some(event) = reader.next_event().unwrap() {
            assert!(!matches!(event, ArtifactEvent::Output { .. }));
        }
    }

    #[test]
    fn lifecycle_finish_is_not_before_an_already_admitted_mutation() {
        let directory = tempfile::tempdir().unwrap();
        let mut terminal = Terminal::new(Dimensions::new(20, 2).unwrap()).unwrap();
        let (wake, ready) = mpsc::channel();
        let (recording, streams) =
            SharedRecording::start(directory.path(), &[&terminal], move || {
                let _ = wake.send(());
            })
            .unwrap();
        let mut guard = recording.state.lock().unwrap();
        let generation = terminal.screen_generation();
        terminal.ingest(b"before cut");
        let lifecycle = recording.clone();
        let stop = thread::spawn(move || lifecycle.stop(FinishReason::Lifecycle).unwrap());
        let deadline = Instant::now() + Duration::from_secs(10);
        while recording.admitting.load(Ordering::Acquire) {
            assert!(Instant::now() < deadline, "lifecycle cut timed out");
            thread::yield_now();
        }
        guard
            .recorder
            .observe_output(streams[0].id(), b"before cut", generation, &terminal)
            .unwrap();
        drop(guard);
        stop.join().unwrap();
        ready.recv_timeout(Duration::from_secs(10)).unwrap();
        let Completion::Complete { path, .. } = recording.poll_completion().unwrap().unwrap()
        else {
            panic!("expected a completed lifecycle cut");
        };
        let mut reader = ArtifactReader::new(File::open(path).unwrap()).unwrap();
        let mut output_at = None;
        while let Some(event) = reader.next_event().unwrap() {
            match event {
                ArtifactEvent::Output { at, .. } => output_at = Some(at),
                ArtifactEvent::Finished { at, .. } => assert!(at >= output_at.unwrap()),
                _ => {}
            }
        }
        assert!(output_at.is_some());
    }
}
