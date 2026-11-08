//! Measures Rust allocation requests during warmed terminal scrolling.

use std::{alloc::System, hint::black_box};

use festerm_core::{Dimensions, ScrollbackStats, Terminal};
use stats_alloc::{Region, Stats, StatsAlloc, INSTRUMENTED_SYSTEM};

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

const SCROLLS: usize = 4_096;
const HISTORY_LIMIT: usize = 256 * 1024;
const LINE_FEED: &[u8] = b"row\r\n";
const SCROLL_UP: &[u8] = b"\x1b[24;1Hrow\x1b[S";

fn terminal() -> Terminal {
    Terminal::with_scrollback_limit(Dimensions::new(80, 24).unwrap(), HISTORY_LIMIT).unwrap()
}

fn warm(terminal: &mut Terminal, input: &[u8]) {
    for _ in 0..96 {
        terminal.ingest(input);
    }
    assert_eq!(terminal.cursor().row(), 23);
}

fn observe(terminal: &mut Terminal, input: &[u8]) -> Stats {
    let region = Region::new(GLOBAL);
    for _ in 0..SCROLLS {
        terminal.ingest(black_box(input));
    }
    region.change()
}

fn full_history(input: &[u8]) -> Terminal {
    let mut retained = terminal();
    for _ in 0..SCROLLS * 4 {
        if retained.scrollback_stats().evicted_lines() > 0 {
            for _ in 0..SCROLLS {
                retained.ingest(input);
            }
            assert!(retained
                .scrollback_lines()
                .all(|line| line.cells().len() == 3 && line.has_hard_break()));
            return retained;
        }
        retained.ingest(input);
    }
    panic!("history must be full before measurement");
}

fn report(name: &str, stats: Stats, history: ScrollbackStats) {
    println!(
        "{{\"case\":\"{name}\",\"scrolls\":{SCROLLS},\"allocations\":{},\"reallocations\":{},\"deallocations\":{},\"bytes_allocated\":{},\"bytes_deallocated\":{},\"bytes_reallocated\":{},\"history_lines\":{},\"history_charged_bytes\":{},\"history_limit_bytes\":{}}}",
        stats.allocations,
        stats.reallocations,
        stats.deallocations,
        stats.bytes_allocated,
        stats.bytes_deallocated,
        stats.bytes_reallocated,
        history.logical_lines(),
        history.charged_bytes(),
        history.limit_bytes(),
    );
}

fn main() {
    let mut alternate = terminal();
    alternate.ingest(b"\x1b[?1049h");
    warm(&mut alternate, LINE_FEED);
    let alternate_stats = observe(&mut alternate, LINE_FEED);
    assert!(alternate.modes().alternate_screen());
    assert_eq!(alternate.row_text(0).unwrap().trim_end(), "row");
    assert_eq!(alternate.scrollback_stats().physical_rows(), 0);
    report(
        "alternate_line_feed",
        alternate_stats,
        alternate.scrollback_stats(),
    );

    let mut partial = terminal();
    partial.ingest(b"\x1b[2;24r\x1b[24;1H");
    warm(&mut partial, LINE_FEED);
    let partial_stats = observe(&mut partial, LINE_FEED);
    assert_eq!(partial.row_text(1).unwrap().trim_end(), "row");
    assert_eq!(partial.scrollback_stats().physical_rows(), 0);
    report(
        "partial_region_line_feed",
        partial_stats,
        partial.scrollback_stats(),
    );

    let mut alternate_scroll = terminal();
    alternate_scroll.ingest(b"\x1b[?1049h");
    warm(&mut alternate_scroll, SCROLL_UP);
    let alternate_scroll_stats = observe(&mut alternate_scroll, SCROLL_UP);
    assert_eq!(alternate_scroll.row_text(0).unwrap().trim_end(), "row");
    assert_eq!(alternate_scroll.scrollback_stats().physical_rows(), 0);
    report(
        "alternate_explicit_scroll",
        alternate_scroll_stats,
        alternate_scroll.scrollback_stats(),
    );

    let mut retained = full_history(LINE_FEED);
    let before = retained.scrollback_stats();
    let retained_stats = observe(&mut retained, LINE_FEED);
    let after = retained.scrollback_stats();
    assert_eq!(
        after.evicted_lines() - before.evicted_lines(),
        SCROLLS as u64
    );
    assert!(after.charged_bytes() <= after.limit_bytes());
    report("full_history_line_feed", retained_stats, after);

    let mut retained_explicit = full_history(SCROLL_UP);
    let before = retained_explicit.scrollback_stats();
    let retained_explicit_stats = observe(&mut retained_explicit, SCROLL_UP);
    let after = retained_explicit.scrollback_stats();
    assert_eq!(
        after.evicted_lines() - before.evicted_lines(),
        SCROLLS as u64
    );
    assert!(after.charged_bytes() <= after.limit_bytes());
    report(
        "full_history_explicit_scroll",
        retained_explicit_stats,
        after,
    );

    if std::env::args().any(|argument| argument == "--check") {
        for stats in [alternate_stats, partial_stats, alternate_scroll_stats] {
            assert_eq!(
                stats.allocations, 0,
                "the inline-text unretained scrolling fixture must not allocate"
            );
            assert_eq!(stats.reallocations, 0);
        }
        for stats in [retained_stats, retained_explicit_stats] {
            assert_eq!(
                stats.allocations, 0,
                "warmed full-history scrolling must reuse the evicted line's storage"
            );
            assert_eq!(stats.reallocations, 0);
        }
    }
}
