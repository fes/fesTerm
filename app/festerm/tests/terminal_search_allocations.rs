use std::{alloc::System, process::Command};

use festerm_core::{Dimensions, Terminal};
use stats_alloc::{Region, StatsAlloc, INSTRUMENTED_SYSTEM};

#[allow(dead_code)]
#[path = "../src/search.rs"]
mod search;

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

#[test]
fn ascii_history_search_avoids_per_row_allocations() {
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
#[ignore = "run in an isolated process by ascii_history_search_avoids_per_row_allocations"]
fn allocation_probe() {
    let mut terminal =
        Terminal::with_scrollback_limit(Dimensions::new(80, 24).unwrap(), 8 * 1024 * 1024).unwrap();
    for _ in 0..100_000 {
        terminal.ingest(b"2026-11-07 INFO worker alpha processed a completed chunk\r\n");
        if terminal.scrollback_stats().evicted_lines() > 0 {
            break;
        }
    }
    let history = terminal.scrollback_stats();
    assert!(history.evicted_lines() > 0);
    assert!(history.physical_rows() > 1_000);
    let mut search = search::TerminalSearchState::default();
    search.set_query(&terminal, "missing needle".to_owned());
    assert_eq!(search.match_count(), 0);

    let region = Region::new(GLOBAL);
    search.rescan(&terminal);
    let stats = region.change();
    assert_eq!(search.match_count(), 0);
    println!(
        "ascii_history_rows={} search_allocations={}",
        history.physical_rows(),
        stats.allocations
    );
    assert!(
        stats.allocations < history.physical_rows() / 4,
        "search must not allocate once per retained ASCII row"
    );
}
