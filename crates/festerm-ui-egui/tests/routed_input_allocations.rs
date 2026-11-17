use std::{alloc::System, hint::black_box};

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

const TEST_NAME: &str = "warmed_character_routing_does_not_allocate_in_core_or_view";

// stats_alloc measures the whole process. Even --test-threads=1 leaves libtest's
// scheduler running concurrently, so this one target uses a single-thread main.
// Keep the small listing/filter interface required by Cargo and nextest.
fn main() {
    let mut args = std::env::args().skip(1);
    let mut filters = Vec::new();
    let mut skips = Vec::new();
    let mut list = false;
    let mut exact = false;
    let mut ignored = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--list" => list = true,
            "--exact" => exact = true,
            "--ignored" => ignored = true,
            "--include-ignored" | "--nocapture" | "--show-output" | "--quiet" | "-q" => {}
            "--format" => {
                let value = args.next().expect("--format requires a value");
                assert!(matches!(value.as_str(), "terse" | "pretty"));
            }
            "--test-threads" => assert_eq!(args.next().as_deref(), Some("1")),
            "--test-threads=1" => {}
            "--skip" => skips.push(args.next().expect("--skip requires a filter")),
            _ if arg.starts_with('-') => panic!("unsupported allocation-test argument: {arg}"),
            _ => filters.push(arg),
        }
    }
    let matches = |filter: &str| {
        if exact {
            TEST_NAME == filter
        } else {
            TEST_NAME.contains(filter)
        }
    };
    let selected = !ignored
        && (filters.is_empty() || filters.iter().any(|filter| matches(filter)))
        && !skips.iter().any(|filter| matches(filter));
    if list {
        if selected {
            println!("{TEST_NAME}: test");
        }
        return;
    }
    if selected {
        warmed_character_routing_does_not_allocate_in_core_or_view();
        println!("test {TEST_NAME} ... ok");
    }
    println!(
        "test result: ok. {} passed; 0 failed",
        usize::from(selected)
    );
}

fn warmed_character_routing_does_not_allocate_in_core_or_view() {
    // A positive control keeps a disabled or broken counter from passing the
    // zero-allocation assertion. The buffer remains live until after the read.
    let region = Region::new(GLOBAL);
    let allocated = vec![black_box(0u8); 128];
    black_box(&allocated);
    let stats = region.change();
    assert_eq!(stats.allocations, 1);
    assert_eq!(stats.reallocations, 0);
    drop(allocated);
    println!("allocator_positive_control allocations=1 reallocations=0");

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
