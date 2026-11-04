//! Public Speech/AVFAudio APIs only. No file recording or network fallback.
use crate::dictation::{DictationPhase, Shared, MAX_DICTATION_BYTES};
use block2::RcBlock;
use objc2::{
    rc::Retained,
    runtime::{Bool, ProtocolObject},
    AllocAnyThread,
};
use objc2_avf_audio::{
    AVAudioEngine, AVAudioInputNode, AVAudioPCMBuffer, AVAudioSession,
    AVAudioSessionCategoryOptions, AVAudioSessionCategoryRecord,
    AVAudioSessionInterruptionNotification, AVAudioSessionModeMeasurement,
    AVAudioSessionRecordPermission, AVAudioSessionSetActiveOptions, AVAudioTime,
};
use objc2_foundation::{
    MainThreadMarker, NSError, NSNotification, NSNotificationCenter, NSObjectProtocol,
    NSOperationQueue, NSRunLoop, NSRunLoopCommonModes, NSTimer,
};
use objc2_speech::{
    SFSpeechAudioBufferRecognitionRequest, SFSpeechRecognitionResult, SFSpeechRecognitionTask,
    SFSpeechRecognizer, SFSpeechRecognizerAuthorizationStatus,
};
use objc2_ui_kit::{
    UIApplicationDidEnterBackgroundNotification, UIApplicationProtectedDataWillBecomeUnavailable,
    UIApplicationWillResignActiveNotification,
};
use std::{cell::RefCell, ptr::NonNull, rc::Rc, sync::Arc};

type Wake = Arc<dyn Fn() + Send + Sync>;
type Resources = Rc<RefCell<Option<Capture>>>;

struct Capture {
    generation: u64,
    _recognizer: Retained<SFSpeechRecognizer>,
    engine: Retained<AVAudioEngine>,
    input: Retained<AVAudioInputNode>,
    request: Retained<SFSpeechAudioBufferRecognitionRequest>,
    task: Option<Retained<SFSpeechRecognitionTask>>,
    timer: Option<Retained<NSTimer>>,
    tapped: bool,
}

fn stop_capture(resources: &Resources, generation: Option<u64>) {
    let capture = {
        let mut current = resources.borrow_mut();
        if current
            .as_ref()
            .is_some_and(|capture| generation.is_none_or(|id| capture.generation == id))
        {
            current.take()
        } else {
            None
        }
    };
    // Framework cancellation may re-enter callbacks; release RefCell first.
    drop(capture);
}

impl Drop for Capture {
    fn drop(&mut self) {
        // SAFETY: Capture is created/dropped only by the main-thread owner or
        // synchronous UIKit/main-run-loop callbacks. Each engine has one tap.
        unsafe {
            self.engine.stop();
            if self.tapped {
                self.input.removeTapOnBus(0);
            }
            self.request.endAudio();
            if let Some(task) = &self.task {
                task.cancel();
            }
            if let Some(timer) = &self.timer {
                timer.invalidate();
            }
            // A cleanup error is surfaced without logging native error details.
            if AVAudioSession::sharedInstance()
                .setActive_withOptions_error(
                    false,
                    AVAudioSessionSetActiveOptions::NotifyOthersOnDeactivation,
                )
                .is_err()
            {
                eprintln!("festerm-mobile: dictation audio-session deactivation failed");
            }
        }
    }
}

pub(crate) struct Speech {
    shared: Shared,
    wake: Wake,
    resources: Resources,
    center: Retained<NSNotificationCenter>,
    observers: Vec<Retained<ProtocolObject<dyn NSObjectProtocol>>>,
    _main: MainThreadMarker,
}

impl Speech {
    pub(crate) fn new(shared: Shared, wake: Wake) -> Option<Self> {
        let main = MainThreadMarker::new()?;
        let resources: Resources = Rc::default();
        let center = NSNotificationCenter::defaultCenter();
        let mut observers = Vec::new();
        // SAFETY: UIKit posts these lifecycle notifications on the main thread;
        // nil queues stop capture synchronously, without waiting for a UI frame.
        // Audio interruptions can originate elsewhere and use the main queue.
        unsafe {
            for (name, authorizing_allowed, main_queue) in [
                (UIApplicationWillResignActiveNotification, true, false),
                (UIApplicationDidEnterBackgroundNotification, false, false),
                (
                    UIApplicationProtectedDataWillBecomeUnavailable,
                    false,
                    false,
                ),
                (AVAudioSessionInterruptionNotification?, false, true),
            ] {
                let (state, resources, wake) = (shared.clone(), resources.clone(), wake.clone());
                let block = RcBlock::new(move |_: NonNull<NSNotification>| {
                    let mut draft = state.lock().expect("dictation state poisoned");
                    // System permission sheets may temporarily resign focus.
                    if authorizing_allowed && draft.phase == DictationPhase::Authorizing {
                        return;
                    }
                    draft.cancel();
                    drop(draft);
                    stop_capture(&resources, None);
                    wake();
                });
                observers.push(center.addObserverForName_object_queue_usingBlock(
                    Some(name),
                    None,
                    main_queue.then(NSOperationQueue::mainQueue).as_deref(),
                    &block,
                ));
            }
        }
        Some(Self {
            shared,
            wake,
            resources,
            center,
            observers,
            _main: main,
        })
    }

    fn fail(&self, message: &'static str) {
        let mut draft = self.shared.lock().expect("dictation state poisoned");
        draft.cancel();
        draft.phase = DictationPhase::Failed(message);
        drop(draft);
        self.stop();
        (self.wake)();
    }

    pub(crate) fn stop(&self) {
        stop_capture(&self.resources, None);
    }

    #[allow(deprecated)] // AVAudioSession permission APIs preserve the iOS 15 floor.
    pub(crate) fn poll(&mut self) {
        let (phase, generation, pending) = {
            let draft = self.shared.lock().expect("dictation state poisoned");
            (draft.phase, draft.generation, draft.permission_pending)
        };
        if phase != DictationPhase::Authorizing {
            if phase != DictationPhase::Listening {
                self.stop();
            }
            return;
        }
        if pending {
            return;
        }
        // SAFETY: main-thread owner. Capability is checked before permission
        // prompts, session activation, request creation, or microphone capture.
        unsafe {
            let Some(recognizer) = SFSpeechRecognizer::init(SFSpeechRecognizer::alloc()) else {
                self.fail("Speech recognition is unavailable for this language.");
                return;
            };
            if !recognizer.supportsOnDeviceRecognition() || !recognizer.isAvailable() {
                self.fail("On-device dictation is unavailable for this device or language. No audio was sent to a server.");
                return;
            }
            let audio = AVAudioSession::sharedInstance();
            let speech_permission = SFSpeechRecognizer::authorizationStatus();
            if speech_permission == SFSpeechRecognizerAuthorizationStatus::NotDetermined {
                self.shared
                    .lock()
                    .expect("dictation state poisoned")
                    .permission_pending = true;
                let (state, wake) = (self.shared.clone(), self.wake.clone());
                SFSpeechRecognizer::requestAuthorization(&RcBlock::new(move |_| {
                    let mut draft = state.lock().expect("dictation state poisoned");
                    if draft.generation == generation {
                        draft.permission_pending = false;
                    }
                    drop(draft);
                    wake();
                }));
                return;
            }
            if speech_permission != SFSpeechRecognizerAuthorizationStatus::Authorized {
                self.fail(
                    "Speech recognition permission is denied or restricted. Check iOS Settings.",
                );
                return;
            }
            if audio.recordPermission() == AVAudioSessionRecordPermission::Undetermined {
                self.shared
                    .lock()
                    .expect("dictation state poisoned")
                    .permission_pending = true;
                let (state, wake) = (self.shared.clone(), self.wake.clone());
                audio.requestRecordPermission(&RcBlock::new(move |_: Bool| {
                    let mut draft = state.lock().expect("dictation state poisoned");
                    if draft.generation == generation {
                        draft.permission_pending = false;
                    }
                    drop(draft);
                    wake();
                }));
                return;
            }
            if audio.recordPermission() != AVAudioSessionRecordPermission::Granted {
                self.fail(
                    "Microphone permission is denied. Allow microphone access in iOS Settings.",
                );
                return;
            }
            let (Some(category), Some(mode)) =
                (AVAudioSessionCategoryRecord, AVAudioSessionModeMeasurement)
            else {
                self.fail("The recording audio-session APIs are unavailable.");
                return;
            };
            if audio
                .setCategory_mode_options_error(
                    category,
                    mode,
                    AVAudioSessionCategoryOptions::empty(),
                )
                .is_err()
                || audio.setActive_error(true).is_err()
            {
                self.fail("The microphone audio session could not be activated.");
                return;
            }
            let engine = AVAudioEngine::new();
            let input = engine.inputNode();
            let request = SFSpeechAudioBufferRecognitionRequest::new();
            request.setRequiresOnDeviceRecognition(true);
            request.setShouldReportPartialResults(true);
            let mut capture = Capture {
                generation,
                _recognizer: recognizer.clone(),
                engine,
                input,
                request,
                task: None,
                timer: None,
                tapped: false,
            };
            let format = capture.input.outputFormatForBus(0);
            if format.sampleRate() <= 0.0 || format.channelCount() == 0 {
                self.fail("No microphone audio input is available.");
                return;
            }
            let (request, state) = (capture.request.clone(), self.shared.clone());
            let tap = RcBlock::new(
                move |buffer: NonNull<AVAudioPCMBuffer>, _: NonNull<AVAudioTime>| {
                    let accepts_audio = {
                        let draft = state.lock().expect("dictation state poisoned");
                        draft.generation == generation && draft.phase == DictationPhase::Listening
                    };
                    if accepts_audio {
                        request.appendAudioPCMBuffer(buffer.as_ref());
                    }
                },
            );
            capture.input.installTapOnBus_bufferSize_format_block(
                0,
                1024,
                Some(&format),
                RcBlock::as_ptr(&tap),
            );
            capture.tapped = true;
            recognizer.setQueue(&NSOperationQueue::mainQueue());
            let (state, wake, resources) = (
                self.shared.clone(),
                self.wake.clone(),
                Rc::downgrade(&self.resources),
            );
            let result = RcBlock::new(
                move |result: *mut SFSpeechRecognitionResult, error: *mut NSError| {
                    let mut draft = state.lock().expect("dictation state poisoned");
                    if draft.generation != generation || draft.phase != DictationPhase::Listening {
                        return;
                    }
                    if !error.is_null() {
                        draft.cancel();
                        draft.phase = DictationPhase::Failed(
                            "On-device recognition failed. Try again; no cloud fallback was used.",
                        );
                    } else if let Some(result) = result.as_ref() {
                        let text = result.bestTranscription().formattedString();
                        // Bound before allocating a Rust copy; NSString length is UTF-16.
                        if text.length() > MAX_DICTATION_BYTES {
                            draft.cancel();
                            draft.phase = DictationPhase::Failed(
                                "Dictation exceeded the preview size limit.",
                            );
                        } else {
                            draft.recognize(generation, &text.to_string(), result.isFinal());
                        }
                    }
                    let finished = draft.phase != DictationPhase::Listening;
                    drop(draft);
                    if finished {
                        if let Some(resources) = resources.upgrade() {
                            stop_capture(&resources, Some(generation));
                        }
                    }
                    wake();
                },
            );
            {
                let mut draft = self.shared.lock().expect("dictation state poisoned");
                if draft.generation != generation || draft.phase != DictationPhase::Authorizing {
                    return;
                }
                draft.phase = DictationPhase::Listening;
            }
            capture.task = Some(
                recognizer.recognitionTaskWithRequest_resultHandler(&capture.request, &result),
            );
            let (state, resources, wake) = (
                self.shared.clone(),
                Rc::downgrade(&self.resources),
                self.wake.clone(),
            );
            capture.timer = Some(NSTimer::timerWithTimeInterval_repeats_block(
                45.0,
                false,
                &RcBlock::new(move |_: NonNull<NSTimer>| {
                    let mut draft = state.lock().expect("dictation state poisoned");
                    if draft.generation == generation && draft.phase == DictationPhase::Listening {
                        draft.phase = DictationPhase::Review;
                    }
                    drop(draft);
                    if let Some(resources) = resources.upgrade() {
                        stop_capture(&resources, Some(generation));
                    }
                    wake();
                }),
            ));
            NSRunLoop::mainRunLoop().addTimer_forMode(
                capture.timer.as_ref().expect("capture timer"),
                NSRunLoopCommonModes,
            );
            let engine = capture.engine.clone();
            *self.resources.borrow_mut() = Some(capture);
            engine.prepare();
            let draft = self.shared.lock().expect("dictation state poisoned");
            let should_start =
                draft.generation == generation && draft.phase == DictationPhase::Listening;
            drop(draft);
            if !should_start {
                self.stop();
                return;
            }
            if engine.startAndReturnError().is_err() {
                self.fail("Microphone capture could not start.");
                return;
            }
            let draft = self.shared.lock().expect("dictation state poisoned");
            let cancelled =
                draft.generation != generation || draft.phase != DictationPhase::Listening;
            drop(draft);
            if cancelled {
                // Native startup may re-enter a lifecycle/interruption callback.
                engine.stop();
                self.stop();
            }
        }
    }
}

impl Drop for Speech {
    fn drop(&mut self) {
        self.stop();
        for observer in &self.observers {
            // SAFETY: retained tokens from this notification center.
            unsafe { self.center.removeObserver((**observer).as_ref()) };
        }
    }
}
