use std::{env, hint::black_box};

use egui::{Context, RawInput, Rect};
use festerm_core::{Dimensions, Terminal};
use festerm_ui_egui::{install_terminal_fonts, EncodedInputSink, TerminalView};

fn main() {
    let mut args = env::args().skip(1).filter(|argument| argument != "--bench");
    let workload = args.next().unwrap_or_else(|| "repeated_scroll".to_owned());
    let (workload, frames): (&str, usize) = if workload == "--test" {
        ("repeated_scroll", 1)
    } else {
        let frames = args
            .next()
            .map(|value| value.parse().expect("frame count must be an integer"))
            .unwrap_or(4000);
        (workload.as_str(), frames)
    };
    assert!(args.next().is_none());
    let inputs: Vec<Vec<u8>> = (0..64)
        .map(|index| match workload {
            "repeated_scroll" => b"row\r\n".to_vec(),
            "linked_repeated_scroll" => format!(
                "\x1b]8;;https://example.invalid/{}\x1b\\row\x1b]8;;\x1b\\\r\n",
                "a".repeat(1024)
            )
            .into_bytes(),
            "linked_wide_repeated_scroll" => format!(
                "\x1b]8;;https://example.invalid/{}\x1b\\{}\x1b]8;;\x1b\\\r\n",
                "a".repeat(2000),
                "x".repeat(114)
            )
            .into_bytes(),
            "linked_wide_changing_scroll" => format!(
                "\x1b]8;;https://example.invalid/{}{index:02}\x1b\\{}\x1b]8;;\x1b\\\r\n",
                "a".repeat(2000),
                "x".repeat(114)
            )
            .into_bytes(),
            "linked_multi_wide_repeated_scroll" => format!(
                "\x1b]8;;https://example.invalid/{}\x1b\\{}\
                 \x1b]8;;https://example.invalid/{}\x1b\\{}\x1b]8;;\x1b\\\r\n",
                "a".repeat(900),
                "x".repeat(57),
                "b".repeat(900),
                "x".repeat(57)
            )
            .into_bytes(),
            "wide_repeated_scroll" => format!("{}\r\n", "x".repeat(114)).into_bytes(),
            "mostly_repeated_scroll" => {
                if index % 16 == 0 {
                    format!("different {index:02}\r\n").into_bytes()
                } else {
                    b"row\r\n".to_vec()
                }
            }
            "eighty_percent_repeated_scroll" => {
                if index % 16 < 14 {
                    b"row\r\n".to_vec()
                } else {
                    format!("different {index:02}\r\n").into_bytes()
                }
            }
            "three_quarter_repeated_scroll" => {
                if index % 16 < 12 {
                    b"row\r\n".to_vec()
                } else {
                    format!("different {index:02}\r\n").into_bytes()
                }
            }
            "half_repeated_scroll" => {
                if index % 16 < 8 {
                    b"row\r\n".to_vec()
                } else {
                    format!("different {index:02}\r\n").into_bytes()
                }
            }
            "mostly_changing_scroll" => {
                if index % 16 < 3 {
                    b"row\r\n".to_vec()
                } else {
                    format!("different {index:02}\r\n").into_bytes()
                }
            }
            "linked_late_change_scroll" => format!(
                "\x1b]8;;https://example.invalid/{}{index:02}\x1b\\row\x1b]8;;\x1b\\\r\n",
                "a".repeat(1024)
            )
            .into_bytes(),
            "interior_change_scroll" => {
                format!("{}{index:02}{}\r\n", "x".repeat(55), "x".repeat(57)).into_bytes()
            }
            "interior_mixed_scroll" => {
                let marker = if index % 16 == 0 { index } else { 0 };
                format!("{}{marker:02}{}\r\n", "x".repeat(55), "x".repeat(57)).into_bytes()
            }
            "changing_scroll" => format!("{index:02} changing output\r\n").into_bytes(),
            "late_change_scroll" => format!("{}{index:02}\r\n", "x".repeat(114)).into_bytes(),
            "late_change_tui" => format!("\x1b[12;1H{}{index:02}", "x".repeat(114)).into_bytes(),
            _ => panic!("unknown workload"),
        })
        .collect();
    let mut terminal = Terminal::new(Dimensions::new(120, 40).unwrap()).unwrap();
    for index in 0..128 {
        terminal.ingest(&inputs[index % inputs.len()]);
    }

    let context = Context::default();
    install_terminal_fonts(&context);
    let mut view = TerminalView::default();
    let mut sink = Sink;
    let mut reused = 0usize;
    let mut rebuilt = 0usize;
    let mut primitives = 0usize;
    let mut final_vertices = 0usize;
    for index in 0..(frames + 16) {
        terminal.ingest(black_box(&inputs[index % inputs.len()]));
        let mut output = context.run_ui(
            RawInput {
                screen_rect: Some(Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1280.0, 800.0),
                )),
                ..RawInput::default()
            },
            |ui| {
                view.show_in_ui(ui, &mut terminal, &mut sink);
            },
        );
        let rendered = context.tessellate(output.shapes, output.pixels_per_point);
        output.textures_delta.clear();
        if index >= 16 {
            reused += view.diagnostics().retained_rows_reused;
            rebuilt += view.diagnostics().retained_rows_rebuilt;
            primitives += rendered.len();
        }
        if index + 1 == frames + 16 {
            final_vertices = rendered
                .iter()
                .filter_map(|primitive| match &primitive.primitive {
                    egui::epaint::Primitive::Mesh(mesh) => Some(mesh.vertices.len()),
                    egui::epaint::Primitive::Callback(_) => None,
                })
                .sum();
        }
        black_box(rendered);
    }
    let dimensions = terminal.dimensions();
    if workload.starts_with("linked_") {
        assert!(
            terminal
                .cell(0, 0)
                .is_some_and(|cell| cell.hyperlink().is_some()),
            "benchmark did not retain the hyperlink"
        );
    }
    let nonblank_rows = (0..dimensions.rows())
        .filter(|row| {
            terminal
                .cell(0, *row)
                .is_some_and(|cell| cell.text() != " ")
        })
        .count();
    assert!(nonblank_rows > 0, "benchmark produced no visible text");
    println!(
        "workload={workload} frames={frames} columns={} rows={} reused={reused} rebuilt={rebuilt} primitives={primitives} nonblank_rows={nonblank_rows} final_vertices={final_vertices}",
        dimensions.columns(),
        dimensions.rows()
    );
}

struct Sink;

impl EncodedInputSink for Sink {
    fn record_encoded_input(&mut self, _bytes: &[u8]) {}
}
