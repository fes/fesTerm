//! Run with `cargo bench -p festerm-core --bench sustained_output`.

use std::hint::black_box;

use criterion::{criterion_group, criterion_main, BatchSize, Criterion, Throughput};
use festerm_core::{Dimensions, Terminal};

const OUTPUT_BYTES: usize = 256 * 1024;
const REFLOW_LINES: usize = 2_000;

fn sustained_output(c: &mut Criterion) {
    let mut group = c.benchmark_group("sustained_output");
    let plain = repeated_workload(b"plain terminal output 0123456789\r\n", OUTPUT_BYTES);
    let styled = repeated_workload(
        b"\x1b[1;38;5;33mstyled\x1b[0m output \xe7\x95\x8c e\xcc\x81\r\n",
        OUTPUT_BYTES,
    );
    group.throughput(Throughput::Bytes(OUTPUT_BYTES as u64));

    for (name, input) in [("plain_ascii", plain), ("styled_utf8", styled)] {
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
