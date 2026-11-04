use std::sync::{Arc, Mutex};
use zeroize::Zeroize;

pub const MAX_DICTATION_BYTES: usize = 4096;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DictationPhase {
    #[default]
    Idle,
    Authorizing,
    Listening,
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

    #[cfg(any(target_os = "ios", test))]
    pub(crate) fn recognize(&mut self, generation: u64, text: &str, final_result: bool) {
        if self.generation != generation || self.phase != DictationPhase::Listening {
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

    pub fn stop(&mut self) {
        self.with_text(|draft| {
            if draft.phase == DictationPhase::Listening {
                draft.phase = DictationPhase::Review;
            }
        });
        #[cfg(target_os = "ios")]
        if let Some(native) = &mut self.native {
            native.stop();
        }
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
}
