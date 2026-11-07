use crate::dictation::{recognizer_level, set_permission_prompt, DictationPhase, Shared};
use festerm_android_platform::SpeechEvent;
use std::sync::Arc;

pub(crate) struct Speech {
    shared: Shared,
}

impl Speech {
    pub(crate) fn new(shared: Shared, repaint: Arc<dyn Fn() + Send + Sync>) -> Option<Self> {
        let state = shared.clone();
        let weak = Arc::downgrade(&state);
        let result = festerm_android_platform::set_speech_callback(Arc::new(
            move |generation, event| {
                let Some(state) = weak.upgrade() else { return };
                let mut draft = state.lock().expect("dictation state poisoned");
                if draft.generation != generation as u64 {
                    return;
                }
                let (status, text) = match event {
                    SpeechEvent::Level(level) => {
                        if draft.phase == DictationPhase::Listening {
                            draft.record_level(recognizer_level(level));
                            drop(draft);
                            repaint();
                        }
                        return;
                    }
                    SpeechEvent::Status { status, text } => (status, text),
                };
                set_permission_prompt(false);
                match status {
                    1 if draft.phase == DictationPhase::Authorizing => {
                        draft.phase = DictationPhase::Listening
                    }
                    2 | 3 => draft.recognize(generation as u64, text, status == 3),
                    4 => draft.finish_timed_out(generation as u64),
                    5 => {
                        draft.finish();
                    }
                    _ => draft.capture_failed(
                        "On-device speech is unavailable or was interrupted. No cloud fallback was used.",
                    ),
                }
                drop(draft);
                repaint();
            },
        ));
        if result.is_err() {
            return None;
        }
        Some(Self { shared })
    }

    pub(crate) fn poll(&mut self) {
        let mut draft = self.shared.lock().expect("dictation state poisoned");
        if draft.phase == DictationPhase::Authorizing && !draft.permission_pending {
            draft.permission_pending = true;
            set_permission_prompt(true);
            if festerm_android_platform::speech_command(draft.generation, 0).is_err() {
                set_permission_prompt(false);
                draft.capture_failed("Android speech service is unavailable.");
            }
        }
    }

    pub(crate) fn finish(&self) {
        let generation = self
            .shared
            .lock()
            .expect("dictation state poisoned")
            .generation;
        if festerm_android_platform::speech_command(generation, 1).is_err() {
            self.shared
                .lock()
                .expect("dictation state poisoned")
                .recognition_failed(generation);
        }
    }

    pub(crate) fn stop(&self) {
        set_permission_prompt(false);
        // Cancel uses the previous generation, since Dictation clears state first.
        let generation = self
            .shared
            .lock()
            .expect("dictation state poisoned")
            .generation;
        if let Err(error) = festerm_android_platform::speech_command(generation, 2) {
            eprintln!("fesTerm: {error}");
        }
    }
}
