use std::{env, hint::black_box, mem::size_of};

use festerm_core::{Cell, Dimensions, Terminal};

fn main() {
    let mut args = env::args().skip(1);
    let rows: usize = args.next().map_or(150_000, |value| {
        value.parse().expect("expected a row count")
    });
    let columns: usize = args
        .next()
        .map_or(80, |value| value.parse().expect("expected a column count"));
    let workload = args.next().unwrap_or_else(|| "density".to_owned());
    assert!(args.next().is_none() && columns >= 4);
    let history_limit = if workload == "density" {
        32 * 1024 * 1024
    } else {
        256 * 1024
    };
    let mut terminal =
        Terminal::with_scrollback_limit(Dimensions::new(columns, 24).unwrap(), history_limit)
            .unwrap();
    let mut row = vec![b'x'; columns - 3];
    row.extend_from_slice(b"\r\n");
    let input: &[u8] = match workload.as_str() {
        "density" => &row,
        "styled_utf8" => {
            "\x1b[1;38;5;33mstyled \u{754c} \u{1f642} e\u{301}\x1b[0m repeated output\r\n"
                .as_bytes()
        }
        "osc8_links" => {
            b"\x1b]8;;https://example.invalid/a\x1b\\linked output\x1b]8;;\x1b\\ tail\r\n"
        }
        "tui_overwrite" => {
            terminal.ingest(b"\x1b[?1049h");
            b"\x1b[12;11H\x1b[1;32mstatus: 0123456789\x1b[0m"
        }
        _ => panic!("unknown workload"),
    };
    if workload != "density" {
        for _ in 0..4096 {
            terminal.ingest(input);
        }
        if workload != "tui_overwrite" {
            assert!(terminal.scrollback_stats().evicted_lines() > 0);
        }
    }
    for _ in 0..rows {
        terminal.ingest(black_box(input));
    }
    let stats = terminal.scrollback_stats();
    println!(
        "workload={} cell={} lines={} rows={} charged={}",
        workload,
        size_of::<Cell>(),
        stats.logical_lines(),
        stats.physical_rows(),
        stats.charged_bytes()
    );
    black_box(terminal);
}
