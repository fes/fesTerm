//! Run with `cargo bench -p festerm-core --bench sustained_output`.

use std::{hint::black_box, time::Instant};

use festerm_core::{Dimensions, Terminal};

fn main() {
    let input = vec![b'x'; 1_000_000];
    let mut terminal = Terminal::new(Dimensions::new(120, 40).expect("valid dimensions"))
        .expect("terminal allocation should succeed");
    let started = Instant::now();

    terminal.ingest(black_box(&input));

    println!(
        "sustained output: {} bytes in {:?}",
        input.len(),
        started.elapsed()
    );
}
