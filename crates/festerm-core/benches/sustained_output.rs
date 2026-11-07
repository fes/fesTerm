//! Run with `cargo bench -p festerm-core --bench sustained_output`.

use std::hint::black_box;

use criterion::{criterion_group, criterion_main, BatchSize, Criterion, Throughput};
use festerm_core::{Dimensions, Terminal};

const OUTPUT_BYTES: usize = 256 * 1024;
const REFLOW_LINES: usize = 2_000;

fn sustained_output(c: &mut Criterion) {
    let mut group = c.benchmark_group("sustained_output");
    group.throughput(Throughput::Bytes(OUTPUT_BYTES as u64));

    for (name, input) in workloads() {
        group.bench_function(name, |bencher| {
            bencher.iter_batched(
                || {
                    Terminal::new(Dimensions::new(120, 40).expect("valid dimensions"))
                        .expect("terminal allocation should succeed")
                },
                |mut terminal| terminal.ingest(black_box(&input)),
                BatchSize::SmallInput,
            );
        });
    }
    group.finish();

    // Steady state: history already at its byte limit, so every scrolled line
    // also pays eviction. This is the long-running `cat`/build-log case.
    let mut warmed = Terminal::new(Dimensions::new(120, 40).expect("valid dimensions"))
        .expect("terminal allocation should succeed");
    let filler = repeated_workload(
        b"warm history line 0123456789 abcdefghijklmnopqrstuvwxyz\r\n",
        1 << 20,
    );
    while warmed.scrollback_stats().evicted_lines() == 0 {
        warmed.ingest(&filler);
    }
    let mut group = c.benchmark_group("sustained_output_full_history");
    group.throughput(Throughput::Bytes(OUTPUT_BYTES as u64));
    for (name, input) in workloads()
        .into_iter()
        .filter(|(name, _)| matches!(*name, "plain_ascii" | "sgr_dense" | "yes_scroll"))
    {
        group.bench_function(name, |bencher| {
            bencher.iter_batched(
                || warmed.clone(),
                |mut terminal| terminal.ingest(black_box(&input)),
                BatchSize::LargeInput,
            );
        });
    }
    group.finish();
}

fn workloads() -> Vec<(&'static str, Vec<u8>)> {
    let mut tui = Vec::new();
    // Cursor-addressed mid-row overwrites, as a full-screen TUI repaint does.
    let mut row = 0;
    while tui.len() < OUTPUT_BYTES {
        row = row % 40 + 1;
        tui.extend_from_slice(
            format!("\x1b[{row};{}H\x1b[3{}mstatus {row:02}", 10 + row, row % 8).as_bytes(),
        );
    }
    tui.truncate(OUTPUT_BYTES);
    let mut alternate = b"\x1b[?1049h".to_vec();
    while alternate.len() < OUTPUT_BYTES {
        alternate.extend_from_slice(b"\x1b[H");
        for line in 0..40 {
            alternate.extend_from_slice(format!("\x1b[{};1H{:<119}", line + 1, line).as_bytes());
        }
    }
    alternate.truncate(OUTPUT_BYTES);
    vec![
        (
            "plain_ascii",
            repeated_workload(b"plain terminal output 0123456789\r\n", OUTPUT_BYTES),
        ),
        (
            "styled_utf8",
            repeated_workload(
                b"\x1b[1;38;5;33mstyled\x1b[0m output \xe7\x95\x8c e\xcc\x81\r\n",
                OUTPUT_BYTES,
            ),
        ),
        ("ascii_200_cols", repeated_workload(&[b"x".repeat(199), b"\r\n".to_vec()].concat(), OUTPUT_BYTES)),
        (
            "sgr_dense",
            repeated_workload(
                b"\x1b[01;34mdir\x1b[0m  \x1b[01;32mexec.sh\x1b[0m  file.txt  \x1b[38;2;10;200;30mrgb\x1b[0m\r\n",
                OUTPUT_BYTES,
            ),
        ),
        (
            "cjk_emoji",
            repeated_workload(
                "\u{754c}\u{9762}\u{6587}\u{5b57} \u{1f600}\u{1f44d}\u{1f3fd} \u{1f468}\u{200d}\u{1f4bb} text\r\n".as_bytes(),
                OUTPUT_BYTES,
            ),
        ),
        (
            "osc8_links",
            repeated_workload(
                b"\x1b]8;;https://example.com/a\x1b\\link\x1b]8;;\x1b\\ tail\r\n",
                OUTPUT_BYTES,
            ),
        ),
        ("yes_scroll", repeated_workload(b"y\r\n", OUTPUT_BYTES)),
        ("tui_overwrite", tui),
        ("alternate_repaint", alternate),
    ]
}

fn resize_reflow(c: &mut Criterion) {
    let mut seeded = Terminal::new(Dimensions::new(120, 40).expect("valid dimensions"))
        .expect("terminal allocation should succeed");
    for line in 0..REFLOW_LINES {
        seeded.ingest(
            format!(
                "\x1b[{}mline-{line:04} representative reflow content \u{754c} e\u{301}\x1b[0m\r\n",
                31 + line % 6
            )
            .as_bytes(),
        );
    }

    let dimensions = [
        Dimensions::new(80, 32).unwrap(),
        Dimensions::new(48, 24).unwrap(),
        Dimensions::new(132, 50).unwrap(),
        Dimensions::new(96, 40).unwrap(),
    ];
    let mut group = c.benchmark_group("resize_reflow");
    group.bench_function("representative_scrollback_sequence", |bencher| {
        bencher.iter_batched(
            || seeded.clone(),
            |mut terminal| {
                for dimensions in dimensions {
                    terminal.resize(black_box(dimensions)).unwrap();
                }
                black_box(terminal.scrollback_stats());
            },
            BatchSize::LargeInput,
        );
    });
    group.finish();
}

fn repeated_workload(pattern: &[u8], target_bytes: usize) -> Vec<u8> {
    let repetitions = target_bytes.div_ceil(pattern.len());
    let mut workload = Vec::with_capacity(repetitions * pattern.len());
    for _ in 0..repetitions {
        workload.extend_from_slice(pattern);
    }
    workload.truncate(target_bytes);
    workload
}

criterion_group!(terminal_benches, sustained_output, resize_reflow);
criterion_main!(terminal_benches);
