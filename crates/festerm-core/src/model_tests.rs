use crate::{Cell, CellWidth, Dimensions, Terminal};

const MODEL_SCROLLBACK_LIMIT: usize = 8 * 1024 * 1024;
const MODEL_STEPS: usize = 160;
const BOUND_STEPS: usize = 220;

#[derive(Clone, Debug, Eq, PartialEq)]
struct LogicalSnapshotLine {
    cells: Vec<Cell>,
    hard_break: bool,
}

#[derive(Clone, Copy, Debug)]
struct DeterministicRng {
    state: u64,
}

impl DeterministicRng {
    fn new(seed: u64) -> Self {
        Self {
            state: seed ^ 0x9e37_79b9_7f4a_7c15,
        }
    }

    fn next_u64(&mut self) -> u64 {
        let mut value = self.state;
        value ^= value >> 12;
        value ^= value << 25;
        value ^= value >> 27;
        self.state = value;
        value.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn range(&mut self, upper: usize) -> usize {
        (self.next_u64() as usize) % upper
    }

    fn chance(&mut self, numerator: usize, denominator: usize) -> bool {
        self.range(denominator) < numerator
    }
}

#[test]
fn generated_resize_sequences_match_an_unreflowed_logical_model() {
    for seed in 0..24 {
        let initial = Dimensions::new(12, 4).unwrap();
        let mut terminal =
            Terminal::with_scrollback_limit(initial, MODEL_SCROLLBACK_LIMIT).unwrap();
        let mut model = Terminal::with_scrollback_limit(
            Dimensions::new(512, MODEL_STEPS + 2).unwrap(),
            MODEL_SCROLLBACK_LIMIT,
        )
        .unwrap();
        let mut rng = DeterministicRng::new(seed);

        for step in 0..MODEL_STEPS {
            if step == 0 || rng.chance(3, 5) {
                let output = generated_output(&mut rng, true, false);
                terminal.ingest(&output);
                model.ingest(&output);
            } else {
                let dimensions = Dimensions::new(2 + rng.range(47), 1 + rng.range(9)).unwrap();
                terminal.resize(dimensions).unwrap();
            }

            assert_logical_snapshots_equal(
                &logical_snapshot(&terminal),
                &logical_snapshot(&model),
                seed,
                step,
                terminal.dimensions(),
            );
            assert_terminal_invariants(&terminal, seed, step);
        }
    }
}

#[test]
fn generated_output_resize_and_clear_sequences_keep_scrollback_strictly_bounded() {
    const LIMITS: [usize; 8] = [0, 96, 160, 256, 384, 768, 1_536, 4_096];

    for seed in 0..32 {
        let limit = LIMITS[seed as usize % LIMITS.len()];
        let dimensions = Dimensions::new(2 + seed as usize % 17, 1 + seed as usize % 6).unwrap();
        let mut terminal = Terminal::with_scrollback_limit(dimensions, limit).unwrap();
        let mut rng = DeterministicRng::new(seed ^ 0xa5a5_5a5a_d3c4_b2e1);
        let mut prior_evicted = 0;
        let mut prior_oversize = 0;

        for step in 0..BOUND_STEPS {
            match rng.range(10) {
                0..=6 => {
                    let hard_break = rng.chance(3, 4);
                    terminal.ingest(&generated_output(&mut rng, hard_break, true));
                }
                7 | 8 => {
                    let dimensions = Dimensions::new(2 + rng.range(31), 1 + rng.range(8)).unwrap();
                    terminal.resize(dimensions).unwrap();
                }
                _ => {
                    let screen_before = terminal.primary_screen().clone();
                    let cursor_before = terminal.cursor();
                    if rng.chance(1, 2) {
                        terminal.clear_scrollback();
                    } else {
                        terminal.ingest(b"\x1b[3J");
                    }
                    assert_eq!(terminal.primary_screen(), &screen_before);
                    assert_eq!(terminal.cursor(), cursor_before);
                }
            }

            let stats = terminal.scrollback_stats();
            assert!(
                stats.evicted_lines() >= prior_evicted,
                "eviction counter regressed for seed {seed} after step {step}"
            );
            assert!(
                stats.oversize_lines() >= prior_oversize,
                "oversize counter regressed for seed {seed} after step {step}"
            );
            prior_evicted = stats.evicted_lines();
            prior_oversize = stats.oversize_lines();
            assert_terminal_invariants(&terminal, seed, step);
        }

        if limit > 0 && limit <= 768 {
            let stats = terminal.scrollback_stats();
            assert!(
                stats.evicted_lines() > 0 || stats.oversize_lines() > 0,
                "seed {seed} with limit {limit} never exercised bound pressure"
            );
        }
    }
}

#[test]
fn wide_character_pre_wrap_does_not_invent_padding_during_reflow() {
    let mut terminal = Terminal::new(Dimensions::new(3, 2).unwrap()).unwrap();
    terminal.ingest("ab界X\r\nz\r\n".as_bytes());

    let line = terminal.scrollback_lines().next().unwrap();
    assert_eq!(
        line.cells()
            .iter()
            .map(|cell| cell.text())
            .collect::<Vec<_>>(),
        vec!["a", "b", "界", "", "X"]
    );
    assert_eq!(line.physical_rows(), 2);

    terminal.resize(Dimensions::new(5, 1).unwrap()).unwrap();
    let line = terminal.scrollback_lines().next().unwrap();
    assert_eq!(line.physical_rows(), 1);
    assert_eq!(
        line.cells()
            .iter()
            .map(|cell| cell.text())
            .collect::<Vec<_>>(),
        vec!["a", "b", "界", "", "X"]
    );
}

fn generated_output(rng: &mut DeterministicRng, hard_break: bool, decorated: bool) -> Vec<u8> {
    let mut output = Vec::new();
    if decorated {
        match rng.range(4) {
            0 => output.extend_from_slice(b"\x1b[0m"),
            1 => output.extend_from_slice(b"\x1b[1;31m"),
            2 => output.extend_from_slice(b"\x1b[4;38;5;33m"),
            _ => output.extend_from_slice(b"\x1b[3;38;2;20;180;90m"),
        }
    }

    let hyperlink = rng.chance(1, 4);
    if hyperlink {
        output.extend_from_slice(b"\x1b]8;;https://example.test/generated\x1b\\");
    }

    for _ in 0..1 + rng.range(48) {
        match rng.range(10) {
            0 => output.extend_from_slice("界".as_bytes()),
            1 => output.extend_from_slice("e\u{301}".as_bytes()),
            2 => output.extend_from_slice("A\u{327}".as_bytes()),
            3 => output.push(b' '),
            4 => output.push(b'-'),
            _ => output.push(b'a' + rng.range(26) as u8),
        }
    }

    if hyperlink {
        output.extend_from_slice(b"\x1b]8;;\x1b\\");
    }
    if decorated {
        output.extend_from_slice(b"\x1b[0m");
    }
    if hard_break {
        output.extend_from_slice(b"\r\n");
    }
    output
}

fn logical_snapshot(terminal: &Terminal) -> Vec<LogicalSnapshotLine> {
    let mut lines = terminal
        .scrollback_lines()
        .map(|line| LogicalSnapshotLine {
            cells: line.cells().to_vec(),
            hard_break: line.has_hard_break(),
        })
        .collect::<Vec<_>>();

    let screen = terminal.primary_screen();
    let content_rows = screen
        .occupied_row_count()
        .max(terminal.cursor().row() + 1)
        .min(screen.dimensions().rows());
    let rows = screen.to_rows();

    for (row_index, row) in rows.into_iter().take(content_rows).enumerate() {
        if lines.last().is_none_or(|line| line.hard_break) {
            lines.push(LogicalSnapshotLine {
                cells: Vec::new(),
                hard_break: false,
            });
        }
        let line = lines.last_mut().expect("current logical line exists");
        line.cells.extend(row.cells);
        if row_index + 1 < content_rows && !row.soft_wrapped {
            line.hard_break = true;
        }
    }

    if lines
        .last()
        .is_some_and(|line| line.cells.is_empty() && !line.hard_break)
    {
        lines.pop();
    }
    lines
}

fn assert_logical_snapshots_equal(
    actual: &[LogicalSnapshotLine],
    expected: &[LogicalSnapshotLine],
    seed: u64,
    step: usize,
    dimensions: Dimensions,
) {
    if actual.len() != expected.len() {
        let actual_shape = actual
            .iter()
            .map(|line| (line.cells.len(), line.hard_break))
            .collect::<Vec<_>>();
        let expected_shape = expected
            .iter()
            .map(|line| (line.cells.len(), line.hard_break))
            .collect::<Vec<_>>();
        panic!(
            "logical-line count diverged for seed {seed} after step {step} at {dimensions:?}: \
             actual {actual_shape:?}, expected {expected_shape:?}"
        );
    }
    for (line_index, (actual, expected)) in actual.iter().zip(expected).enumerate() {
        assert_eq!(
            actual.hard_break, expected.hard_break,
            "hard-break metadata diverged on line {line_index} for seed {seed} after step {step} at {dimensions:?}"
        );
        assert_eq!(
            actual.cells.len(),
            expected.cells.len(),
            "cell count diverged on line {line_index} for seed {seed} after step {step} at {dimensions:?}"
        );
        for (cell_index, (actual, expected)) in actual.cells.iter().zip(&expected.cells).enumerate()
        {
            assert_eq!(
                actual, expected,
                "cell {cell_index} diverged on line {line_index} for seed {seed} after step {step} at {dimensions:?}"
            );
        }
    }
}

fn assert_terminal_invariants(terminal: &Terminal, seed: u64, step: usize) {
    let stats = terminal.scrollback_stats();
    assert!(
        stats.charged_bytes() <= stats.limit_bytes(),
        "scrollback exceeded its byte limit for seed {seed} after step {step}: {stats:?}"
    );

    let lines = terminal.scrollback_lines().collect::<Vec<_>>();
    assert_eq!(
        lines.len(),
        stats.logical_lines(),
        "logical-line accounting mismatch for seed {seed} after step {step}"
    );
    assert_eq!(
        lines.iter().map(|line| line.physical_rows()).sum::<usize>(),
        stats.physical_rows(),
        "physical-row accounting mismatch for seed {seed} after step {step}"
    );

    let columns = terminal.dimensions().columns();
    let mut history_row = 0;
    for line in lines {
        assert!(line.physical_rows() > 0);
        let mut reconstructed = Vec::new();
        for row_index in 0..line.physical_rows() {
            let row = line.physical_row(row_index).expect("history row exists");
            assert!(
                row.len() <= columns,
                "history row exceeds current width for seed {seed} after step {step}"
            );
            assert_cell_width_invariants(row, seed, step);
            assert_eq!(
                terminal.scrollback_physical_row(history_row),
                Some(row),
                "physical-row lookup mismatch for seed {seed} after step {step}"
            );
            reconstructed.extend_from_slice(row);
            history_row += 1;
        }
        assert_eq!(
            reconstructed,
            line.cells(),
            "history row splits changed cell content for seed {seed} after step {step}"
        );
    }
    assert_eq!(history_row, stats.physical_rows());
    assert!(terminal.scrollback_physical_row(history_row).is_none());

    let screen = terminal.primary_screen();
    for row in 0..screen.dimensions().rows() {
        let cells = (0..screen.dimensions().columns())
            .map(|column| {
                screen
                    .cell_ref(column, row)
                    .expect("visible cell exists")
                    .clone()
            })
            .collect::<Vec<_>>();
        assert_cell_width_invariants(&cells, seed, step);
    }

    let cursor = terminal.cursor();
    assert!(cursor.column() < terminal.dimensions().columns());
    assert!(cursor.row() < terminal.dimensions().rows());
}

fn assert_cell_width_invariants(cells: &[Cell], seed: u64, step: usize) {
    for (index, cell) in cells.iter().enumerate() {
        match cell.width() {
            CellWidth::Single => assert!(
                !cell.text().is_empty(),
                "single-width cell has no text for seed {seed} after step {step}"
            ),
            CellWidth::Double => {
                assert!(
                    !cell.text().is_empty(),
                    "double-width cell has no text for seed {seed} after step {step}"
                );
                assert!(
                    cells
                        .get(index + 1)
                        .is_some_and(|next| next.width() == CellWidth::Continuation),
                    "double-width cell lost its continuation for seed {seed} after step {step}"
                );
            }
            CellWidth::Continuation => {
                assert!(
                    cell.text().is_empty(),
                    "continuation cell retained text for seed {seed} after step {step}"
                );
                assert!(
                    index > 0 && cells[index - 1].width() == CellWidth::Double,
                    "orphan continuation cell for seed {seed} after step {step}"
                );
            }
        }
    }
}
