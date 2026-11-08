use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU8, Ordering},
        mpsc::{self, Receiver, TryRecvError},
        Arc,
    },
    thread,
};

use festerm_recording::{
    ExportDestination, LocalStart, PreparedRecording, RecordingId, RecordingLease,
    RecordingManifest, RecordingSettings,
};

type Wake = Arc<dyn Fn() + Send + Sync>;
const PREFLIGHT: u8 = 0;
const PREPARING: u8 = 1;
const CANCELLED_PREFLIGHT: u8 = 2;

pub struct PreparedCapture {
    pub manifest: RecordingManifest,
    pub source: PreparedRecording,
    pub lease: Arc<RecordingLease>,
}

pub struct PrepareTask {
    receiver: Receiver<Result<PreparedCapture, String>>,
    cancelled: Arc<AtomicBool>,
    phase: Arc<AtomicU8>,
    wake: Wake,
}

impl PrepareTask {
    pub fn start(
        root: PathBuf,
        settings: RecordingSettings,
        preflight: impl FnOnce(&ExportDestination) -> Result<(), String> + Send + 'static,
        wake: Wake,
    ) -> Result<Self, String> {
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancelled = cancelled.clone();
        let phase = Arc::new(AtomicU8::new(PREFLIGHT));
        let worker_phase = phase.clone();
        let worker_wake = wake.clone();
        let (send, receiver) = mpsc::sync_channel(1);
        thread::Builder::new()
            .name("festerm-recording-prepare".to_owned())
            .spawn(move || {
                let result = prepare_capture(
                    &root,
                    &settings,
                    &worker_cancelled,
                    &worker_phase,
                    preflight,
                );
                let _ = send.send(result);
                worker_wake();
            })
            .map_err(|error| format!("Recording preparation could not start: {error}"))?;
        Ok(Self {
            receiver,
            cancelled,
            phase,
            wake,
        })
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    pub fn poll(&mut self) -> Option<Result<PreparedCapture, String>> {
        if self.cancelled.load(Ordering::Acquire)
            && self
                .phase
                .compare_exchange(
                    PREFLIGHT,
                    CANCELLED_PREFLIGHT,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
        {
            return Some(Err("Recording preparation was cancelled".to_owned()));
        }
        match self.receiver.try_recv() {
            Ok(Ok(prepared)) if self.cancelled.load(Ordering::Acquire) => {
                let (send, receiver) = mpsc::sync_channel(1);
                let wake = self.wake.clone();
                let cleanup = thread::Builder::new()
                    .name("festerm-recording-cancel".to_owned())
                    .spawn(move || {
                        let result = prepared
                            .lease
                            .verify()
                            .map_err(|error| error.to_string())
                            .and_then(|()| {
                                prepared.source.discard().map_err(|error| error.to_string())
                            })
                            .and_then(|()| {
                                prepared
                                    .manifest
                                    .verify(&prepared.lease)
                                    .and_then(|()| prepared.manifest.remove())
                                    .map_err(|error| error.to_string())
                            })
                            .map_err(|error| {
                                format!("Recording cancellation cleanup was incomplete; check the library: {error}")
                            })
                            .and_then(|()| Err("Recording preparation was cancelled".to_owned()));
                        let _ = send.send(result);
                        wake();
                    });
                match cleanup {
                    Ok(_) => {
                        self.receiver = receiver;
                        None
                    }
                    Err(error) => Some(Err(format!(
                        "Recording cancellation cleanup could not start: {error}; check the recording library"
                    ))),
                }
            }
            Ok(result) => Some(result),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err(
                "Recording preparation stopped unexpectedly; check the recording library"
                    .to_owned(),
            )),
        }
    }
}

impl Drop for PrepareTask {
    fn drop(&mut self) {
        self.cancel();
    }
}

fn prepare_capture(
    root: &Path,
    settings: &RecordingSettings,
    cancelled: &AtomicBool,
    phase: &AtomicU8,
    preflight: impl FnOnce(&ExportDestination) -> Result<(), String>,
) -> Result<PreparedCapture, String> {
    if cancelled.load(Ordering::Acquire) {
        return Err("Recording preparation was cancelled".to_owned());
    }
    let validated = settings.validate()?;
    let destination = validated
        .destination
        .as_ref()
        .ok_or_else(|| "Choose and save a recording export folder first".to_owned())?;
    preflight(destination)?;
    if cancelled.load(Ordering::Acquire)
        || phase
            .compare_exchange(PREFLIGHT, PREPARING, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
    {
        return Err("Recording preparation was cancelled".to_owned());
    }
    let lease = Arc::new(
        RecordingLease::acquire(root)
            .map_err(|error| format!("Recording storage is unavailable: {error}"))?,
    );
    if cancelled.load(Ordering::Acquire) {
        return Err("Recording preparation was cancelled".to_owned());
    }
    let id = RecordingId::new();
    let start = LocalStart::now().map_err(|error| error.to_string())?;
    let manifest = RecordingManifest::create(root, id, start, settings).map_err(|error| {
        format!("Recording settings could not be saved for this capture: {error}")
    })?;
    if cancelled.load(Ordering::Acquire) {
        manifest
            .verify(&lease)
            .and_then(|()| manifest.remove())
            .map_err(|error| {
                format!("Cancelled recording settings could not be removed completely: {error}")
            })?;
        return Err("Recording preparation was cancelled".to_owned());
    }
    let source = PreparedRecording::create(root, id).map_err(|error| {
        format!(
            "Recording preparation failed: {error}; check the recording library for an unfinished source"
        )
    })?;
    if cancelled.load(Ordering::Acquire) {
        lease.verify_for(root).map_err(|error| {
            format!("Recording storage changed during cancellation; check the library: {error}")
        })?;
        source.discard().map_err(|error| {
            format!("Cancelled recording cleanup was incomplete; check the library: {error}")
        })?;
        manifest
            .verify(&lease)
            .and_then(|()| manifest.remove())
            .map_err(|error| {
                format!("Cancelled recording settings could not be removed completely: {error}")
            })?;
        return Err("Recording preparation was cancelled".to_owned());
    }
    lease.verify_for(root).map_err(|error| {
        format!("Recording storage changed during preparation; the source may remain: {error}")
    })?;
    Ok(PreparedCapture {
        manifest,
        source,
        lease,
    })
}
