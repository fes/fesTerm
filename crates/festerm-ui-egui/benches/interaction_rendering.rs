//! Run with `cargo bench -p festerm-ui-egui --bench interaction_rendering`.

use std::{
    hint::black_box,
    time::{Duration, Instant},
};

use criterion::{criterion_group, criterion_main, BatchSize, Criterion, Throughput};
use egui::{Context, RawInput, Rect};
use festerm_core::{Dimensions, Terminal};
use festerm_ui_egui::{
    install_terminal_fonts, selection_text, CellPosition, EncodedInputSink, Selection,
    TerminalRenderCache, TerminalSnapshot, TerminalView,
};

const COLUMNS: usize = 120;
const ROWS: usize = 40;
const HISTORY_LINES: usize = 2_000;
const VIEWPORT_OFFSETS: usize = 5;

fn scrolling(c: &mut Criterion) {
    let terminal = seeded_terminal();
    let history_rows = terminal.scrollback_stats().physical_rows();
    let offsets = [
        history_rows,
        history_rows.saturating_mul(3) / 4,
        history_rows / 2,
        history_rows / 4,
        0,
    ];
    let mut group = c.benchmark_group("scrolling");
    group.throughput(Throughput::Elements(VIEWPORT_OFFSETS as u64));
    group.bench_function("viewport_cache_refresh_sequence", |bencher| {
        bencher.iter_batched(
            TerminalRenderCache::default,
            |mut cache| {
                for offset in offsets {
                    let snapshot = TerminalSnapshot::from_terminal_viewport(&terminal, offset);
                    black_box(cache.update(snapshot, &[]));
                }
            },
            BatchSize::SmallInput,
        );
    });
    group.finish();
}

fn selection(c: &mut Criterion) {
    let terminal = seeded_terminal();
    let history_rows = terminal.scrollback_stats().physical_rows();
    let snapshot = TerminalSnapshot::from_terminal_viewport(&terminal, history_rows);
    let mut selection = Selection::default();
    selection.begin(CellPosition { column: 0, row: 0 });
    selection.extend(CellPosition {
        column: COLUMNS - 1,
        row: ROWS - 1,
    });
    selection.finish();

    let mut group = c.benchmark_group("selection");
    group.throughput(Throughput::Elements((COLUMNS * ROWS) as u64));
    group.bench_function("copy_visible_viewport", |bencher| {
        bencher.iter(|| black_box(selection_text(snapshot, black_box(&selection)).unwrap()));
    });
    group.finish();
}

fn rendering(c: &mut Criterion) {
    let mut steady = RenderState::new();
    c.bench_function("rendering/steady_frame_submission", |bencher| {
        bencher.iter(|| black_box(steady.frame()));
    });

    let mut dirty = RenderState::new();
    let updates = [
        b"\x1b[1;1Hdirty frame alpha 0123456789\x1b[K".as_slice(),
        b"\x1b[1;1Hdirty frame beta  9876543210\x1b[K".as_slice(),
    ];
    c.bench_function("rendering/dirty_row_submission", |bencher| {
        bencher.iter_custom(|iterations| {
            let mut measured = Duration::ZERO;
            for iteration in 0..iterations {
                dirty
                    .terminal
                    .ingest(updates[iteration as usize % updates.len()]);
                let started = Instant::now();
                black_box(dirty.frame());
                measured = measured.saturating_add(started.elapsed());
            }
            measured
        });
    });
}

fn seeded_terminal() -> Terminal {
    let mut terminal = Terminal::new(Dimensions::new(COLUMNS, ROWS).unwrap()).unwrap();
    let payload = "x".repeat(108);
    for line in 0..HISTORY_LINES {
        terminal.ingest(format!("line-{line:06} {payload}\r\n").as_bytes());
    }
    terminal
}

struct RenderState {
    context: Context,
    view: TerminalView,
    terminal: Terminal,
    sink: Sink,
}

impl RenderState {
    fn new() -> Self {
        let context = Context::default();
        install_terminal_fonts(&context);
        let mut state = Self {
            context,
            view: TerminalView::default(),
            terminal: seeded_terminal(),
            sink: Sink,
        };
        assert!(
            state.frame() > 0,
            "warm-up frame should submit paint shapes"
        );
        assert!(state.frame() > 0, "steady frame should submit paint shapes");
        state
    }

    fn frame(&mut self) -> usize {
        let context = self.context.clone();
        let mut output = context.run_ui(
            RawInput {
                screen_rect: Some(Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1_280.0, 800.0),
                )),
                ..RawInput::default()
            },
            |ui| {
                self.view.show_in_ui(ui, &mut self.terminal, &mut self.sink);
            },
        );
        let shape_count = output.shapes.len();
        output.textures_delta.clear();
        shape_count
    }
}

struct Sink;

impl EncodedInputSink for Sink {
    fn record_encoded_input(&mut self, _bytes: &[u8]) {}
}

criterion_group!(ui_benches, scrolling, selection, rendering);
criterion_main!(ui_benches);
