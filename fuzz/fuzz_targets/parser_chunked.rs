//! Chunk-boundary fuzz target.
//!
//! Bytes arrive from a pseudoterminal in whatever sized pieces the kernel
//! feels like handing over, so a sequence is routinely split across two reads.
//! A parser that only works on whole sequences works by luck. This target
//! feeds the same bytes twice - once in one go, once cut at fuzzer-chosen
//! offsets - and requires the two terminals to agree.
//!
//! Equivalence is the assertion rather than any particular rendering, which
//! means this target stays correct as the terminal's behaviour changes.

#![no_main]

use festerm_core::{Dimensions, Terminal};
use libfuzzer_sys::fuzz_target;

const COLUMNS: usize = 24;
const ROWS: usize = 6;

fn terminal() -> Terminal {
    Terminal::new(Dimensions::new(COLUMNS, ROWS).expect("dimensions")).expect("terminal")
}

fn screen(terminal: &Terminal) -> Vec<String> {
    (0..ROWS)
        .map(|row| terminal.row_text(row).expect("row vanished"))
        .collect()
}

fuzz_target!(|input: (Vec<u8>, Vec<u8>)| {
    let (data, cuts) = input;

    let mut whole = terminal();
    whole.ingest(&data);

    let mut piecemeal = terminal();
    let mut offset = 0;
    for cut in &cuts {
        // Each byte of `cuts` is a chunk length, so the fuzzer controls both
        // how many splits there are and where they fall.
        let end = (offset + usize::from(*cut)).min(data.len());
        piecemeal.ingest(&data[offset..end]);
        offset = end;
    }
    piecemeal.ingest(&data[offset..]);

    assert_eq!(
        screen(&piecemeal),
        screen(&whole),
        "where the stream was cut changed what was drawn"
    );
    assert_eq!(
        piecemeal.cursor(),
        whole.cursor(),
        "where the stream was cut changed where the cursor landed"
    );
    assert_eq!(
        piecemeal.drain_replies(),
        whole.drain_replies(),
        "where the stream was cut changed what the terminal replied"
    );
});
