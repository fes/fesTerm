use std::alloc::System;

use festerm_core::{
    kitty::{KittyConfig, KittyGeometry},
    Dimensions, Terminal,
};
use stats_alloc::{Region, StatsAlloc, INSTRUMENTED_SYSTEM};

#[path = "support/allocation_harness.rs"]
mod allocation_harness;

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

const TEST_NAME: &str = "kitty_zlib_respects_expected_size_without_late_allocation";

fn main() {
    allocation_harness::run(
        TEST_NAME,
        kitty_zlib_respects_expected_size_without_late_allocation,
    );
}

fn kitty_zlib_respects_expected_size_without_late_allocation() {
    let mut terminal = Terminal::new(Dimensions::new(8, 3).unwrap()).unwrap();
    terminal
        .enable_kitty_graphics(KittyConfig {
            rgba_budget: 4,
            png_validator: None,
        })
        .unwrap();
    terminal
        .set_kitty_geometry(KittyGeometry::new(1, 1))
        .unwrap();
    const QUERY: &[u8] = b"\x1b_Ga=q,i=9,o=z,f=32,s=1,v=1,S=4;eJxjZGJmAQAAGAAL\x1b\\";

    for _ in 0..2 {
        let region = Region::new(GLOBAL);
        assert_eq!(terminal.ingest_with_reply_backpressure(QUERY), QUERY.len());
        assert_eq!(terminal.queued_replies(), b"\x1b_Gi=9;OK\x1b\\");
        let stats = region.change();
        assert_eq!(stats.allocations, 0);
        assert_eq!(stats.reallocations, 0);
        terminal.drain_replies();
    }
}
