use std::{alloc::System, hint::black_box};

use festerm_core::{Dimensions, InputEvent, InputEventOutcome, Key, Terminal};
use stats_alloc::{Region, StatsAlloc, INSTRUMENTED_SYSTEM};

#[path = "support/allocation_harness.rs"]
mod allocation_harness;

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

const EVENTS: usize = 4_096;
const TEST_NAME: &str = "character_input_avoids_temporary_allocations";

fn main() {
    allocation_harness::run(TEST_NAME, character_input_avoids_temporary_allocations);
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
