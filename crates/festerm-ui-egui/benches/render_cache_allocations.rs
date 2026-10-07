//! Measures CPU presentation-cache allocation calls, not total heap or renderer work.

use std::{alloc::System, hint::black_box};

use festerm_core::{Dimensions, Terminal};
use festerm_ui_egui::{TerminalRenderCache, TerminalSnapshot};
use stats_alloc::{Region, Stats, StatsAlloc, INSTRUMENTED_SYSTEM};

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

const DIRTY_UPDATES: usize = 4_096;
const VIEWPORT_UPDATES: usize = 256;

fn report(name: &str, updates: usize, stats: Stats) {
    println!(
        "{{\"case\":\"{name}\",\"updates\":{updates},\"allocations\":{},\"reallocations\":{},\"deallocations\":{},\"bytes_allocated\":{},\"bytes_deallocated\":{},\"bytes_reallocated\":{}}}",
        stats.allocations,
        stats.reallocations,
        stats.deallocations,
        stats.bytes_allocated,
        stats.bytes_deallocated,
        stats.bytes_reallocated,
    );
}

fn main() {
    let mut terminal = Terminal::new(Dimensions::new(80, 24).unwrap()).unwrap();
    for _ in 0..64 {
        terminal.ingest(b"ASCII presentation-cache allocation fixture\r\n");
    }
    assert!(terminal.scrollback_stats().physical_rows() > 0);
    let mut cache = TerminalRenderCache::default();
    cache.update(TerminalSnapshot::from_terminal(&terminal), &[]);

    let region = Region::new(GLOBAL);
    for _ in 0..DIRTY_UPDATES {
        let update = black_box(cache.update(
            black_box(TerminalSnapshot::from_terminal(&terminal)),
            black_box(&[0]),
        ));
        assert_eq!(update.updated_rows, [0]);
        assert!(!update.full_refresh);
        black_box(cache.row(0).unwrap());
    }
    let dirty = region.change();
    report("same_dimension_dirty_row", DIRTY_UPDATES, dirty);

    let region = Region::new(GLOBAL);
    for iteration in 0..VIEWPORT_UPDATES {
        let offset = usize::from(iteration % 2 == 0);
        let update = black_box(cache.update(
            black_box(TerminalSnapshot::from_terminal_viewport(&terminal, offset)),
            &[],
        ));
        assert!(update.full_refresh);
        assert_eq!(update.updated_rows.len(), 24);
        black_box(cache.row(0).unwrap());
    }
    let viewport = region.change();
    report(
        "same_dimension_viewport_refresh",
        VIEWPORT_UPDATES,
        viewport,
    );

    let mut terminal = Terminal::new(Dimensions::new(80, 24).unwrap()).unwrap();
    let long = format!("a{}", "\u{301}".repeat(20));
    terminal.ingest(long.as_bytes());
    let mut cache = TerminalRenderCache::default();
    cache.update(TerminalSnapshot::from_terminal(&terminal), &[]);
    assert_eq!(cache.row(0).unwrap()[0].text(), long);
    terminal.ingest(b"\x1b[1;1H\x1b[2Kx");
    let region = Region::new(GLOBAL);
    black_box(cache.update(TerminalSnapshot::from_terminal(&terminal), &[0]));
    let retired = region.change();
    report("long_payload_replaced_by_inline_text", 1, retired);

    if std::env::args().any(|argument| argument == "--check") {
        for (stats, updates) in [(dirty, DIRTY_UPDATES), (viewport, VIEWPORT_UPDATES)] {
            assert_eq!(
                stats.allocations,
                2 * updates,
                "only the update-row vector and shared revision token may allocate"
            );
            assert_eq!(stats.reallocations, 0);
        }
        assert_eq!(retired.allocations, 2);
        assert_eq!(
            retired.deallocations, 2,
            "old long text and update-row vector must both be freed"
        );
        assert_eq!(retired.reallocations, 0);
        assert!(retired.bytes_deallocated >= long.len() + std::mem::size_of::<usize>());
    }
}
