use std::{alloc::System, hint::black_box};

use festerm_core::{Dimensions, InputEvent, InputEventOutcome, Key, Terminal};
use stats_alloc::{Region, StatsAlloc, INSTRUMENTED_SYSTEM};

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

const EVENTS: usize = 4_096;

const TEST_NAME: &str = "character_input_avoids_temporary_allocations";

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
        character_input_avoids_temporary_allocations();
        println!("test {TEST_NAME} ... ok");
    }
    println!(
        "test result: ok. {} passed; 0 failed",
        usize::from(selected)
    );
}

fn character_input_avoids_temporary_allocations() {
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

    for character in ['x', '🙂'] {
        let mut terminal = Terminal::new(Dimensions::new(80, 24).unwrap()).unwrap();
        let mut encoded = [0; 4];
        let expected = character.encode_utf8(&mut encoded).as_bytes();
        let region = Region::new(GLOBAL);
        for _ in 0..EVENTS {
            assert_eq!(
                terminal.handle_input(InputEvent::Key(Key::Character(black_box(character)))),
                InputEventOutcome::Encoded {
                    bytes: expected.len()
                }
            );
            assert_eq!(terminal.drain_input(), expected);
        }
        let stats = region.change();
        println!(
            "character_bytes={} events={EVENTS} allocations={} reallocations={}",
            expected.len(),
            stats.allocations,
            stats.reallocations
        );
        assert!(
            stats.allocations <= EVENTS,
            "encoding must not allocate in addition to the owned input queue"
        );
        assert_eq!(stats.reallocations, 0);
    }
}
