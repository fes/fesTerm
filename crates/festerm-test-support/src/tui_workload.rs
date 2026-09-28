//! Fixed-size, synthetic TUI updates shared by native and render-stage probes.

use std::fmt::Write as _;

pub const COLUMNS: usize = 120;
pub const ROWS: usize = 40;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Workload {
    Quiet,
    Localized,
    Streaming,
    FullRedraw,
}

impl Workload {
    pub const ALL: [Self; 4] = [
        Self::Quiet,
        Self::Localized,
        Self::Streaming,
        Self::FullRedraw,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Self::Quiet => "quiet",
            Self::Localized => "localized",
            Self::Streaming => "streaming",
            Self::FullRedraw => "full-redraw",
        }
    }

    pub fn parse(name: &str) -> Result<Self, String> {
        Self::ALL
            .into_iter()
            .find(|workload| workload.name() == name)
            .ok_or_else(|| format!("unknown TUI workload: {name}"))
    }

    pub fn setup(self) -> Vec<u8> {
        let mut output = String::from("\x1b[0m\x1b[?25l");
        if self != Self::Streaming {
            output.push_str("\x1b[?1049h");
        }
        output.push_str("\x1b[2J\x1b[H");
        output.push_str(&screen(0));
        output.into_bytes()
    }

    pub fn update(self, frame: usize) -> Vec<u8> {
        let spinner = ['|', '/', '-', '\\'][frame % 4];
        match self {
            Self::Quiet => Vec::new(),
            Self::Localized => format!(
                "\x1b[38;1H\x1b[0;38;5;81mWorking {spinner} frame {frame:06}\x1b[K\
                 \x1b[39;1H\x1b[0;48;5;24m Status: active frame {frame:06} \x1b[K\x1b[0m"
            )
            .into_bytes(),
            Self::Streaming => format!(
                "\x1b[40;1H\x1b[0m\x1b[K\r\n\
                 \x1b[38;5;81mStep {frame:06}\x1b[0m: inspecting synthetic input, preserving output order\r\n\
                 Working {spinner} frame {frame:06}\x1b[K"
            )
            .into_bytes(),
            Self::FullRedraw => screen(frame).into_bytes(),
        }
    }

    pub fn finish(self) -> &'static [u8] {
        if self == Self::Streaming {
            b"\x1b[0m\x1b[?25h"
        } else {
            b"\x1b[0m\x1b[?1049l\x1b[?25h"
        }
    }
}

fn screen(frame: usize) -> String {
    let mut output = String::new();
    for row in 1..=ROWS {
        let color = match row {
            1 | 39 => "\x1b[0;38;5;255;48;5;24m",
            2 | 40 => "\x1b[0;38;5;81m",
            _ => "\x1b[0m",
        };
        write!(
            output,
            "\x1b[{row};1H{color}TUI row {row:02} frame {frame:06}  \
             synthetic task output ABCDEFGHIJKLMNOPQRSTUVWXYZ 0123456789\x1b[K"
        )
        .expect("writing to a String cannot fail");
    }
    output.push_str("\x1b[0m");
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use festerm_core::{Dimensions, Terminal};

    fn terminal() -> Terminal {
        Terminal::new(Dimensions::new(COLUMNS, ROWS).unwrap()).unwrap()
    }

    #[test]
    fn localized_tui_updates_leave_other_rows_unchanged() {
        let mut terminal = terminal();
        terminal.ingest(&Workload::Localized.setup());
        let before: Vec<_> = (0..ROWS)
            .map(|row| terminal.row_text(row).unwrap())
            .collect();
        terminal.take_dirty_rows();
        terminal.ingest(&Workload::Localized.update(17));
        let dirty = terminal.take_dirty_rows();
        assert!(dirty.iter().all(|row| [37, 38].contains(row)), "{dirty:?}");
        for (row, previous) in before.iter().enumerate() {
            let actual = terminal.row_text(row).unwrap();
            if [37, 38].contains(&row) {
                assert!(actual.contains("frame 000017"), "{actual}");
            } else {
                assert_eq!(&actual, previous, "row {row}");
            }
        }
    }

    #[test]
    fn tui_workloads_preserve_final_state_when_bytes_are_fragmented() {
        for workload in Workload::ALL {
            let mut bytes = workload.setup();
            for frame in 1..=17 {
                bytes.extend(workload.update(frame));
            }
            let mut whole = terminal();
            whole.ingest(&bytes);
            let mut fragmented = terminal();
            for chunk in bytes.chunks(7) {
                fragmented.ingest(chunk);
            }
            assert_eq!(
                whole.modes().alternate_screen(),
                workload != Workload::Streaming
            );
            for row in 0..ROWS {
                for column in 0..COLUMNS {
                    assert_eq!(whole.cell(column, row), fragmented.cell(column, row));
                }
            }
            if workload != Workload::Quiet {
                assert!((0..ROWS).any(|row| whole.row_text(row).unwrap().contains("frame 000017")));
                assert!(!(0..ROWS).any(|row| whole
                    .row_text(row)
                    .unwrap()
                    .contains("Working | frame 000016")));
            }
            whole.ingest(workload.finish());
            assert!(!whole.modes().alternate_screen());
            assert!(whole.modes().cursor_visible());
        }
        assert!(Workload::parse("not-a-workload").is_err());
        assert!(Workload::Quiet.update(99).is_empty());
    }
}
