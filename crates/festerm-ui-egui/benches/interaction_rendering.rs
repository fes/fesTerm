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

fn emoji_rendering(c: &mut Criterion) {
    const EMOJI_PER_FRAME: u64 = 7 * ROWS as u64;
    let mut group = c.benchmark_group("emoji_rendering");
    group.throughput(Throughput::Elements(EMOJI_PER_FRAME));
    group.sample_size(20);
    group.bench_function("cold_texture_population", |bencher| {
        bencher.iter_batched(
            EmojiRenderState::cold,
            |mut state| black_box(state.frame()),
            BatchSize::SmallInput,
        );
    });

    let mut warm = EmojiRenderState::warm();
    group.bench_function("warm_texture_reuse", |bencher| {
        bencher.iter(|| black_box(warm.frame()));
    });
    group.finish();
}

fn seeded_terminal() -> Terminal {
    let mut terminal = Terminal::new(Dimensions::new(COLUMNS, ROWS).unwrap()).unwrap();
    let payload = "x".repeat(108);
    for line in 0..HISTORY_LINES {
        terminal.ingest(format!("line-{line:06} {payload}\r\n").as_bytes());
    }
    terminal
}

fn emoji_terminal() -> Terminal {
    let mut terminal = Terminal::new(Dimensions::new(COLUMNS, ROWS).unwrap()).unwrap();
    let row = "🤖 🗑️ ⚠️ ℹ️ 👩‍🔬 1️⃣ 🇺🇸";
    for index in 0..ROWS {
        terminal.ingest(row.as_bytes());
        if index + 1 < ROWS {
            terminal.ingest(b"\r\n");
        }
    }
    terminal
}

struct RenderState {
    context: Context,
    view: TerminalView,
    terminal: Terminal,
    sink: Sink,
}

#[derive(Clone, Copy)]
struct EmojiFrameSample {
    shapes: usize,
    paints: usize,
    cache_hits: usize,
    cache_misses: usize,
    rasterization_attempts: usize,
}

struct EmojiRenderState {
    render: RenderState,
}

impl EmojiRenderState {
    fn cold() -> Self {
        let context = Context::default();
        install_terminal_fonts(&context);
        let mut render = RenderState {
            context,
            view: TerminalView::default(),
            terminal: Terminal::new(Dimensions::new(COLUMNS, ROWS).unwrap()).unwrap(),
            sink: Sink,
        };
        black_box(render.frame());
        black_box(render.frame());
        render.view = TerminalView::default();
        render.terminal = emoji_terminal();
        Self { render }
    }

    fn warm() -> Self {
        let mut state = Self::cold();
        let cold = state.frame();
        assert!(cold.shapes > 0);
        assert_eq!(cold.paints, 7 * ROWS);
        assert_eq!(cold.cache_misses, 7);
        assert_eq!(cold.rasterization_attempts, 7);
        assert_eq!(cold.cache_hits, cold.paints - cold.cache_misses);
        let warm = state.frame();
        assert!(warm.shapes > 0);
        assert_eq!(warm.paints, 7 * ROWS);
        assert_eq!(warm.cache_misses, 0);
        assert_eq!(warm.rasterization_attempts, 0);
        assert_eq!(warm.cache_hits, warm.paints);
        state
    }

    fn frame(&mut self) -> EmojiFrameSample {
        let shapes = self.render.frame();
        let diagnostics = self.render.view.diagnostics();
        EmojiFrameSample {
            shapes,
            paints: diagnostics.color_emoji_paints,
            cache_hits: diagnostics.color_emoji_cache_hits,
            cache_misses: diagnostics.color_emoji_cache_misses,
            rasterization_attempts: diagnostics.color_emoji_rasterization_attempts,
        }
    }
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

criterion_group!(ui_benches, scrolling, selection, rendering, emoji_rendering);
criterion_main!(ui_benches);
