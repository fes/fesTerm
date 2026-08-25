//! Run with `cargo bench -p festerm-core --bench interaction`.

use std::hint::black_box;

use criterion::{criterion_group, criterion_main, BatchSize, Criterion, Throughput};
use festerm_core::{
    Dimensions, FocusEvent, InputEvent, Key, KeypadKey, Modifiers, MouseButton, MouseEvent,
    MouseEventKind, MouseWheel, Terminal, TRANSPORT_QUEUE_HIGH_WATERMARK,
};

const INPUT_EVENTS: usize = 256;
const QUEUE_CHUNK_BYTES: usize = 1_024;

fn input_handling(c: &mut Criterion) {
    let events = representative_input_events();
    let mut group = c.benchmark_group("input_handling");
    group.throughput(Throughput::Elements(events.len() as u64));
    group.bench_function("mixed_mode_aware_events", |bencher| {
        bencher.iter_batched(
            input_terminal,
            |mut terminal| {
                for event in events.iter().cloned() {
                    black_box(terminal.handle_input(event));
                }
                black_box(terminal.queued_input().len());
            },
            BatchSize::SmallInput,
        );
    });
    group.finish();

    let paste = "representative paste payload\n".repeat(160);
    let mut group = c.benchmark_group("input_handling_paste");
    group.throughput(Throughput::Bytes(paste.len() as u64));
    group.bench_function("bracketed_4k", |bencher| {
        bencher.iter_batched(
            || (input_terminal(), paste.clone()),
            |(mut terminal, paste)| {
                black_box(terminal.handle_input(InputEvent::Paste(paste)));
                black_box(terminal.queued_input().len());
            },
            BatchSize::SmallInput,
        );
    });
    group.finish();
}

fn queue_pressure(c: &mut Criterion) {
    let chunk = vec![b'q'; QUEUE_CHUNK_BYTES];
    let writes = TRANSPORT_QUEUE_HIGH_WATERMARK / chunk.len();
    let mut group = c.benchmark_group("queue_pressure");
    group.throughput(Throughput::Bytes(TRANSPORT_QUEUE_HIGH_WATERMARK as u64));
    group.bench_function("fill_reject_and_drain", |bencher| {
        bencher.iter_batched(
            || Terminal::new(Dimensions::new(80, 24).unwrap()).unwrap(),
            |mut terminal| {
                for _ in 0..writes {
                    black_box(terminal.queue_input(&chunk));
                }
                black_box(terminal.queue_input(b"overflow"));
                black_box(terminal.drain_input());
            },
            BatchSize::SmallInput,
        );
    });
    group.finish();
}

fn input_terminal() -> Terminal {
    let mut terminal = Terminal::new(Dimensions::new(120, 40).unwrap()).unwrap();
    terminal.ingest(b"\x1b[?1h\x1b[?66h\x1b[?1004h\x1b[?1000h\x1b[?1006h\x1b[?2004h");
    terminal
}

fn representative_input_events() -> Vec<InputEvent> {
    let base = [
        InputEvent::Key(Key::Character('x')),
        InputEvent::Key(Key::ArrowUp),
        InputEvent::Key(Key::ArrowRight),
        InputEvent::Key(Key::Keypad(KeypadKey::Digit(7))),
        InputEvent::Key(Key::Enter),
        InputEvent::Focus(FocusEvent::In),
        InputEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Press(MouseButton::Left),
            column: 72,
            row: 18,
            modifiers: Modifiers::SHIFT.with(Modifiers::CONTROL),
        }),
        InputEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Release(MouseButton::Left),
            column: 72,
            row: 18,
            modifiers: Modifiers::NONE,
        }),
        InputEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Wheel(MouseWheel::Down),
            column: 40,
            row: 20,
            modifiers: Modifiers::ALT,
        }),
        InputEvent::Focus(FocusEvent::Out),
    ];
    base.into_iter().cycle().take(INPUT_EVENTS).collect()
}

criterion_group!(interaction_benches, input_handling, queue_pressure);
criterion_main!(interaction_benches);
