use std::{alloc::System, hint::black_box, process::Command};

use festerm_core::{Dimensions, InputEvent, InputEventOutcome, Key, Terminal};
use festerm_ui_egui::{route_input, EncodedInputSink};
use stats_alloc::{Region, StatsAlloc, INSTRUMENTED_SYSTEM};

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

const EVENTS: usize = 4_096;

struct Sink {
    expected: [u8; 4],
    expected_len: usize,
    delivered: usize,
}

impl EncodedInputSink for Sink {
    fn record_encoded_input(&mut self, bytes: &[u8]) {
        assert_eq!(bytes, &self.expected[..self.expected_len]);
        self.delivered += 1;
    }
}

#[test]
fn warmed_character_routing_does_not_allocate_in_core_or_view() {
    let output = Command::new(std::env::current_exe().expect("test executable"))
        .args([
            "--exact",
            "allocation_probe",
            "--ignored",
            "--test-threads=1",
            "--nocapture",
        ])
        .output()
        .expect("isolated allocation probe must start");
    assert!(
        output.status.success(),
        "allocation probe failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    print!("{}", String::from_utf8_lossy(&output.stdout));
}

#[test]
#[ignore = "run in an isolated process by warmed_character_routing_does_not_allocate_in_core_or_view"]
fn allocation_probe() {
    let mut allocating_cases = 0;
    for character in ['x', '🙂'] {
        let mut terminal = Terminal::new(Dimensions::new(80, 24).unwrap()).unwrap();
        let mut expected = [0; 4];
        let expected_len = character.encode_utf8(&mut expected).len();
        let mut sink = Sink {
            expected,
            expected_len,
            delivered: 0,
        };
        let event = || InputEvent::Key(Key::Character(black_box(character)));
        route_input(&mut terminal, event(), &mut sink);
        sink.delivered = 0;
        let region = Region::new(GLOBAL);
        for _ in 0..EVENTS {
            let route = route_input(&mut terminal, event(), &mut sink);
            assert_eq!(
                route.outcome,
                InputEventOutcome::Encoded {
                    bytes: expected_len
                }
            );
            assert_eq!(route.queue_depth, expected_len);
            assert_eq!(route.delivered_bytes, expected_len);
        }
        let stats = region.change();
        assert_eq!(sink.delivered, EVENTS);
        println!(
            "character_bytes={expected_len} events={EVENTS} allocations={} reallocations={}",
            stats.allocations, stats.reallocations
        );
        allocating_cases += usize::from(stats.allocations != 0 || stats.reallocations != 0);
    }
    assert_eq!(allocating_cases, 0);
}
