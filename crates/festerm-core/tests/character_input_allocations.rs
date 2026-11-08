use std::{alloc::System, hint::black_box, process::Command};

use festerm_core::{Dimensions, InputEvent, InputEventOutcome, Key, Terminal};
use stats_alloc::{Region, StatsAlloc, INSTRUMENTED_SYSTEM};

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

const EVENTS: usize = 4_096;

#[test]
fn character_input_avoids_temporary_allocations() {
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
#[ignore = "run in an isolated process by character_input_avoids_temporary_allocations"]
fn allocation_probe() {
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
