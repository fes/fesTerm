use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use zeroize::Zeroize;

pub const MAX_DICTATION_BYTES: usize = 4096;

static PERMISSION_PROMPT: AtomicBool = AtomicBool::new(false);

/// True while a Speech/Microphone permission alert may be on screen. Such an
/// alert resigns the app active without backgrounding it, so the host can
/// defer its suspension until the app actually enters the background.
pub fn dictation_permission_prompt_in_flight() -> bool {
    PERMISSION_PROMPT.load(Ordering::SeqCst)
}

#[cfg(target_os = "ios")]
pub(crate) fn set_permission_prompt(in_flight: bool) {
    PERMISSION_PROMPT.store(in_flight, Ordering::SeqCst);
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DictationPhase {
    #[default]
    Idle,
    Authorizing,
    Listening,
    /// Audio has ended; waiting briefly for the recognizer's final result.
    Finishing,
    Review,
    Failed(&'static str),
}

/// Volatile, bounded preview only. This type deliberately has no Debug/serde.
#[derive(Default)]
pub struct DictationText {
    pub text: String,
    pub phase: DictationPhase,
    pub(crate) generation: u64,
    pub(crate) permission_pending: bool,
}

impl Drop for DictationText {
    fn drop(&mut self) {
        self.text.zeroize();
    }
}

impl DictationText {
    pub fn insertion_error(&self) -> Option<&'static str> {
        if self.phase != DictationPhase::Review {
            Some("Stop recording before inserting.")
        } else if self.text.trim().is_empty() {
            Some("There is no text to insert.")
        } else if self.text.len() > MAX_DICTATION_BYTES {
            Some("Dictation is limited to 4096 UTF-8 bytes.")
        } else if self
            .text
            .chars()
            .any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}'))
        {
            Some("Remove line breaks and control characters before inserting.")
        } else {
            None
        }
    }

    pub(crate) fn cancel(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.text.zeroize();
        self.phase = DictationPhase::Idle;
        self.permission_pending = false;
    }

    /// Stop/deadline: Listening -> Finishing. Returns whether audio must end.
    pub(crate) fn finish(&mut self) -> bool {
        let finishing = self.phase == DictationPhase::Listening;
        if finishing {
            self.phase = DictationPhase::Finishing;
        }
        finishing
    }

    /// The final result did not arrive in time; review the latest text.
    #[cfg(any(target_os = "ios", test))]
    pub(crate) fn finish_timed_out(&mut self, generation: u64) {
        if self.generation == generation && self.phase == DictationPhase::Finishing {
            self.phase = DictationPhase::Review;
        }
    }

    /// After audio ends, a recognizer error (e.g. no speech) completes the
    /// review with the latest text instead of discarding it.
    #[cfg(any(target_os = "ios", test))]
    pub(crate) fn recognition_failed(&mut self, generation: u64) {
        if self.generation != generation {
            return;
        }
        match self.phase {
            DictationPhase::Finishing => self.phase = DictationPhase::Review,
            DictationPhase::Listening => {
                self.cancel();
                self.phase = DictationPhase::Failed(
                    "On-device recognition failed. Try again; no cloud fallback was used.",
                );
            }
            _ => {}
        }
    }

    #[cfg(any(target_os = "ios", test))]
    pub(crate) fn accepts_results(&self, generation: u64) -> bool {
        self.generation == generation
            && matches!(
                self.phase,
                DictationPhase::Listening | DictationPhase::Finishing
            )
    }

    #[cfg(any(target_os = "ios", test))]
    pub(crate) fn recognize(&mut self, generation: u64, text: &str, final_result: bool) {
        if !self.accepts_results(generation) {
            return;
        }
        self.text.zeroize();
        if text.len() > MAX_DICTATION_BYTES {
            self.phase = DictationPhase::Failed(
                "Dictation exceeded the 4096-byte limit. Try a shorter phrase.",
            );
            return;
        }
        self.text.push_str(text);
        if final_result {
            self.phase = DictationPhase::Review;
        }
    }
}

pub(crate) type Shared = Arc<Mutex<DictationText>>;

/// Native capture lives at this FFI boundary; insertion policy stays in the app.
pub struct Dictation {
    shared: Shared,
    #[cfg(target_os = "ios")]
    native: Option<crate::speech::Speech>,
}

impl Dictation {
    pub fn new(repaint: impl Fn() + Send + Sync + 'static) -> Self {
        let shared = Shared::default();
        #[cfg(target_os = "ios")]
        let native = crate::speech::Speech::new(shared.clone(), Arc::new(repaint));
        #[cfg(not(target_os = "ios"))]
        let _ = repaint;
        Self {
            shared,
            #[cfg(target_os = "ios")]
            native,
        }
    }

    pub fn with_text<R>(&self, edit: impl FnOnce(&mut DictationText) -> R) -> R {
        edit(&mut self.shared.lock().expect("dictation state poisoned"))
    }

    pub fn phase(&self) -> DictationPhase {
        self.with_text(|draft| draft.phase)
    }

    pub fn begin(&mut self) {
        self.cancel();
        self.with_text(|draft| draft.phase = DictationPhase::Authorizing);
        #[cfg(target_os = "ios")]
        if let Some(native) = &mut self.native {
            native.poll();
            return;
        }
        self.with_text(|draft| {
            draft.phase =
                DictationPhase::Failed("On-device dictation is unavailable on this platform.")
        });
    }

    pub fn poll(&mut self) {
        #[cfg(target_os = "ios")]
        if let Some(native) = &mut self.native {
            native.poll();
        }
    }

    /// Ends audio capture. Native recognition keeps its task alive in
    /// `Finishing` until the final result, an error, or a short grace timeout.
    pub fn stop(&mut self) {
        #[cfg(target_os = "ios")]
        if let Some(native) = &self.native {
            let (finishing, phase) = {
                let mut draft = self.shared.lock().expect("dictation state poisoned");
                (draft.finish(), draft.phase)
            };
            if finishing {
                native.finish();
            } else if phase != DictationPhase::Finishing {
                native.stop();
            }
            return;
        }
        self.with_text(|draft| {
            if draft.finish() {
                draft.phase = DictationPhase::Review;
            }
        });
    }

    pub fn cancel(&mut self) {
        self.with_text(DictationText::cancel);
        #[cfg(target_os = "ios")]
        if let Some(native) = &mut self.native {
            native.stop();
        }
    }
}

impl Drop for Dictation {
    fn drop(&mut self) {
        self.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dictation_requires_review_and_single_line_bounded_text() {
        let mut draft = DictationText {
            phase: DictationPhase::Listening,
            text: String::new(),
            generation: 0,
            permission_pending: false,
        };
        draft.text = "echo hello".to_owned();
        assert!(draft.insertion_error().is_some());
        draft.phase = DictationPhase::Review;
        assert!(draft.insertion_error().is_none());
        for text in [
            "",
            "  ",
            "echo a\nwhoami",
            "a\rb",
            "a\tb",
            "\u{1b}[A",
            "\u{7f}",
            "\u{85}",
            "\u{2028}",
            "\u{2029}",
        ] {
            draft.text = text.to_owned();
            assert!(draft.insertion_error().is_some(), "{text:?}");
        }
        draft.text = "é".repeat(MAX_DICTATION_BYTES / 2);
        assert!(draft.insertion_error().is_none());
        draft.text.push('é');
        assert!(draft.insertion_error().is_some());
    }

    #[test]
    fn dictation_revisions_replace_and_stale_callbacks_cannot_restore_cancelled_text() {
        let mut draft = DictationText {
            phase: DictationPhase::Listening,
            text: String::new(),
            generation: 0,
            permission_pending: false,
        };
        draft.recognize(0, "one", false);
        draft.recognize(0, "one two", false);
        assert_eq!(draft.text, "one two");
        draft.phase = DictationPhase::Review;
        draft.recognize(0, "late change", true);
        assert_eq!(draft.text, "one two");
        draft.cancel();
        draft.phase = DictationPhase::Listening;
        draft.recognize(0, "stale", true);
        assert!(draft.text.is_empty());
        assert_eq!(draft.phase, DictationPhase::Listening);
        draft.recognize(
            draft.generation,
            &"a".repeat(MAX_DICTATION_BYTES + 1),
            false,
        );
        assert!(matches!(draft.phase, DictationPhase::Failed(_)));
        assert!(draft.text.is_empty());
    }

    fn listening(text: &str) -> DictationText {
        DictationText {
            phase: DictationPhase::Listening,
            text: text.to_owned(),
            generation: 3,
            permission_pending: false,
        }
    }

    #[test]
    fn dictation_stop_finishes_and_accepts_the_final_transcript() {
        let mut draft = listening("hello world");
        assert!(draft.finish());
        assert!(!draft.finish());
        assert_eq!(draft.phase, DictationPhase::Finishing);
        assert!(draft.insertion_error().is_some());
        draft.recognize(3, "hello world how", false);
        assert_eq!(draft.phase, DictationPhase::Finishing);
        draft.recognize(3, "Hello world, how are you?", true);
        assert_eq!(draft.phase, DictationPhase::Review);
        assert_eq!(draft.text, "Hello world, how are you?");
        draft.finish_timed_out(3);
        draft.recognition_failed(3);
        draft.recognize(3, "late", true);
        assert_eq!(draft.phase, DictationPhase::Review);
        assert_eq!(draft.text, "Hello world, how are you?");
    }

    #[test]
    fn dictation_finishing_timeout_or_error_reviews_latest_text_but_cancel_wins() {
        let mut draft = listening("partial");
        draft.finish();
        draft.finish_timed_out(2);
        assert_eq!(draft.phase, DictationPhase::Finishing);
        draft.finish_timed_out(3);
        assert_eq!(draft.phase, DictationPhase::Review);
        assert_eq!(draft.text, "partial");

        let mut draft = listening("partial");
        draft.finish();
        draft.recognition_failed(3);
        assert_eq!(draft.phase, DictationPhase::Review);
        assert_eq!(draft.text, "partial");

        let mut draft = listening("partial");
        draft.recognition_failed(3);
        assert!(matches!(draft.phase, DictationPhase::Failed(_)));
        assert!(draft.text.is_empty());

        // Privacy cancellation during Finishing discards text and late results.
        let mut draft = listening("private");
        draft.finish();
        draft.cancel();
        assert_eq!(draft.phase, DictationPhase::Idle);
        assert!(draft.text.is_empty());
        draft.recognize(3, "private final", true);
        draft.finish_timed_out(3);
        assert_eq!(draft.phase, DictationPhase::Idle);
        assert!(draft.text.is_empty());
    }

    #[test]
    fn dictation_host_stop_without_native_recognizer_reviews_immediately() {
        let mut dictation = Dictation::new(|| {});
        dictation.with_text(|draft| {
            draft.phase = DictationPhase::Listening;
            draft.text = "manual".to_owned();
        });
        dictation.stop();
        assert_eq!(dictation.phase(), DictationPhase::Review);
    }
}
