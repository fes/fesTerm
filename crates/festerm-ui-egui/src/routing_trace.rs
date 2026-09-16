//! Opt-in bounded, content-free observations. IDs correlate an encoder result
//! with its queue outcome, not with an invented physical OS event identity.
use festerm_core::{InputEvent, InputEventOutcome, Key, MouseButton, MouseEventKind, MouseWheel};
use std::{
    collections::VecDeque,
    fmt::Write,
    sync::{Arc, Mutex},
};

pub const CAPACITY: usize = 256;
pub type SharedRecorder = Arc<Mutex<Recorder>>;
pub const CONTEXT_ID: &str = "active-session-input-recorder";

#[derive(Clone, Copy, Debug)]
pub struct Metadata {
    pub class: &'static str,
    pub modifiers: u8,
}

impl Metadata {
    pub fn from_input(event: &InputEvent) -> Self {
        let class = match event {
            InputEvent::Key(Key::Character(_)) => "text-redacted",
            InputEvent::Key(Key::Control('c' | 'C')) => "control-interrupt",
            InputEvent::Key(Key::Control('a' | 'A')) => "control-a",
            InputEvent::Key(Key::Control('b' | 'B')) => "control-b",
            InputEvent::Key(Key::Control('x' | 'X')) => "control-x",
            InputEvent::Key(Key::Control('v' | 'V')) => "control-v",
            InputEvent::Key(Key::Control(_)) => "control-key",
            InputEvent::Key(Key::Enter) => "enter",
            InputEvent::Key(Key::Tab) => "tab",
            InputEvent::Key(Key::Escape) => "escape",
            InputEvent::Key(_) => "nontext-key",
            InputEvent::Paste(_) => "paste-redacted",
            InputEvent::Focus(_) => "focus",
            InputEvent::Mouse(mouse) => match mouse.kind {
                MouseEventKind::Press(MouseButton::Left) => "mouse-left-press",
                MouseEventKind::Press(MouseButton::Right) => "mouse-right-press",
                MouseEventKind::Press(MouseButton::Middle) => "mouse-middle-press",
                MouseEventKind::Release(MouseButton::Left) => "mouse-left-release",
                MouseEventKind::Release(MouseButton::Right) => "mouse-right-release",
                MouseEventKind::Release(MouseButton::Middle) => "mouse-middle-release",
                MouseEventKind::Move { .. } => "mouse-move",
                MouseEventKind::Wheel(MouseWheel::Up) => "mouse-wheel-up",
                MouseEventKind::Wheel(MouseWheel::Down) => "mouse-wheel-down",
            },
        };
        let modifiers = match event {
            InputEvent::Mouse(mouse) => {
                u8::from(mouse.modifiers.contains(festerm_core::Modifiers::SHIFT))
                    | (u8::from(mouse.modifiers.contains(festerm_core::Modifiers::ALT)) << 1)
                    | (u8::from(mouse.modifiers.contains(festerm_core::Modifiers::CONTROL)) << 2)
            }
            InputEvent::Key(Key::Control(_)) => 4,
            _ => 0,
        };
        Self { class, modifiers }
    }
}

#[derive(Clone, Debug)]
struct Record {
    observation: u64,
    target: u64,
    generation: u64,
    source: &'static str,
    class: &'static str,
    modifiers: u8,
    outcome: &'static str,
    queue: &'static str,
    bytes: usize,
    selection: &'static str,
    focus: &'static str,
}

#[derive(Default)]
pub struct Recorder {
    recording: bool,
    target: u64,
    generation: u64,
    next: u64,
    dropped: u64,
    records: VecDeque<Record>,
    focus: &'static str,
}

impl Recorder {
    pub fn set_recording(&mut self, recording: bool) {
        self.recording = recording;
    }
    pub fn recording(&self) -> bool {
        self.recording
    }
    pub fn set_target(&mut self, target: u64, generation: u64) {
        self.target = target;
        self.generation = generation;
    }
    pub fn set_focus(&mut self, focus: &'static str) {
        self.focus = focus;
    }
    pub fn clear(&mut self) {
        self.records.clear();
        self.dropped = 0;
    }
    pub fn record(
        &mut self,
        source: &'static str,
        metadata: Metadata,
        outcome: &'static str,
        queue: &'static str,
        bytes: usize,
    ) -> Option<u64> {
        if !self.recording {
            return None;
        }
        self.next = self.next.checked_add(1)?;
        if self.records.len() == CAPACITY {
            self.records.pop_front();
            self.dropped = self.dropped.saturating_add(1);
        }
        self.records.push_back(Record {
            observation: self.next,
            target: self.target,
            generation: self.generation,
            source,
            class: metadata.class,
            modifiers: metadata.modifiers,
            outcome,
            queue,
            bytes,
            selection: "unchanged",
            focus: self.focus,
        });
        Some(self.next)
    }
    pub fn record_route(
        &mut self,
        metadata: Metadata,
        route: crate::InputRoute,
        queue: &'static str,
    ) -> Option<u64> {
        let outcome = match route.outcome {
            InputEventOutcome::SelectionAllowed => "local-selection-allowed",
            InputEventOutcome::SelectionClaimed => "terminal-owned-unreported-selection-cleared",
            InputEventOutcome::Encoded { .. } => "terminal-encoded",
            InputEventOutcome::QueueOverflow => "core-queue-overflow",
            InputEventOutcome::Rejected => "core-rejected",
        };
        self.record(
            "terminal-view",
            metadata,
            outcome,
            if route.delivered_bytes == 0 {
                "no-delivery"
            } else {
                queue
            },
            route.delivered_bytes,
        )
    }

    /// Complete a retained observation without creating a new event. Stopping
    /// recording does not hide settlement; clearing/eviction removes the ID.
    pub fn settle(&mut self, observation: u64, queue: &'static str) {
        if let Some(record) = self
            .records
            .iter_mut()
            .find(|record| record.observation == observation)
        {
            record.queue = queue;
        }
    }

    pub fn clipboard_wait_became_backpressure(&mut self) {
        for record in &mut self.records {
            if record.queue == "queued-clipboard" {
                record.queue = "queued-backpressure";
            }
        }
    }
    pub fn report(&self) -> String {
        let mut report = format!("fesTerm redacted input routing\nrecording={} retained={} capacity={} dropped={}\nphysical-event-id=unknown; observation IDs correlate core/queue outcomes only\nmodifiers: Shift=1 Alt=2 Ctrl=4 Command=8; text modifiers redacted\nqueue acceptance is not evidence of which remote program handled input\n", self.recording, self.records.len(), CAPACITY, self.dropped);
        for record in &self.records {
            let _ = writeln!(report,             "observation={} target={} generation={} source={} focus={} class={} modifiers={} outcome={} queue={} bytes={} local-selection={}",
            record.observation, record.target, record.generation, record.source, record.focus, record.class, record.modifiers, record.outcome, record.queue, record.bytes, record.selection);
        }
        report
    }

    pub fn annotate_selection(&mut self, decision: &'static str) {
        if self.recording {
            if let Some(record) = self.records.back_mut() {
                record.selection = decision;
            }
        }
    }
}

pub fn record_local(
    context: &egui::Context,
    class: &'static str,
    outcome: &'static str,
    modifiers: u8,
) {
    let recorder = context.data(|data| data.get_temp::<SharedRecorder>(egui::Id::new(CONTEXT_ID)));
    if let Some(recorder) = recorder {
        recorder
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .record(
                "application",
                Metadata { class, modifiers },
                outcome,
                "no-delivery",
                0,
            );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EncodedInputSink;

    #[test]
    fn keyboard_mouse_trace_distinguishes_local_claimed_and_reported_without_shift_override() {
        struct Sink(Recorder);
        impl EncodedInputSink for Sink {
            fn record_encoded_input(&mut self, _: &[u8]) {}
            fn observe_input_event(&mut self, metadata: Metadata, route: crate::InputRoute) {
                self.0
                    .record_route(metadata, route, "accepted-by-controlled-sink");
            }
            fn observe_local_selection(&mut self, decision: &'static str) {
                self.0.annotate_selection(decision);
            }
        }
        let mut terminal =
            festerm_core::Terminal::new(festerm_core::Dimensions::new(20, 5).unwrap()).unwrap();
        terminal.ingest(b"controlled selection");
        let mut selection = crate::Selection::default();
        let mut sink = Sink(Recorder::default());
        sink.0.set_recording(true);
        let mut mouse = festerm_core::MouseEvent {
            kind: MouseEventKind::Press(MouseButton::Left),
            column: 1,
            row: 0,
            modifiers: festerm_core::Modifiers::SHIFT,
        };
        crate::route_mouse_input(&mut terminal, mouse, &mut selection, &mut sink);
        assert!(selection.is_active());
        terminal.ingest(b"\x1b[?1000h\x1b[?1006h");
        mouse.kind = MouseEventKind::Move {
            button: Some(MouseButton::Left),
        };
        let claimed = crate::route_mouse_input(&mut terminal, mouse, &mut selection, &mut sink);
        assert_eq!(claimed.outcome, InputEventOutcome::SelectionClaimed);
        assert_eq!(claimed.delivered_bytes, 0);
        assert!(selection.range().is_none());
        mouse.kind = MouseEventKind::Press(MouseButton::Left);
        let reported = crate::route_mouse_input(&mut terminal, mouse, &mut selection, &mut sink);
        assert!(
            reported.delivered_bytes > 0,
            "Shift is a reported modifier, not a local override"
        );
        let report = sink.0.report();
        assert!(report.contains("local-selection=began"));
        assert!(report.contains("terminal-owned-unreported"));
        assert!(report.contains("terminal-encoded"));
        assert!(report.contains("modifiers=1"));
        assert!(!report.contains("controlled selection"));
    }

    #[test]
    fn keyboard_routing_recorder_is_opt_in_bounded_and_redacts_payloads() {
        let metadata = Metadata::from_input(&InputEvent::Paste("fake-auth-token-secret".into()));
        let mut recorder = Recorder::default();
        recorder.record(
            "terminal-view",
            metadata,
            "terminal-encoded",
            "accepted",
            22,
        );
        assert!(recorder.records.is_empty());
        recorder.set_recording(true);
        for _ in 0..CAPACITY + 3 {
            recorder.record(
                "terminal-view",
                metadata,
                "terminal-encoded",
                "accepted",
                22,
            );
        }
        let report = recorder.report();
        assert_eq!(recorder.records.len(), CAPACITY);
        assert_eq!(recorder.dropped, 3);
        assert!(!report.contains("fake-auth-token-secret"));
        assert!(report.contains("paste-redacted"));
        recorder.set_recording(false);
        recorder.clear();
        assert!(recorder.records.is_empty());
        assert!(!recorder.recording());
    }
}
