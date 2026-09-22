//! Conformance coverage for SGR colour selection and DECRQSS.
//!
//! These exist because a real defect shipped: fesTerm accepted
//! `ESC[38;2;r;g;bm` on its own but mis-stepped over it when a background
//! colour followed in the same sequence, so GitHub Copilot CLI's inline code
//! spans lost their background. Unit tests that set one colour at a time all
//! passed. The cases below therefore vary the *shape* of the sequence — the
//! delimiter (ITU T.416 colons versus the common semicolons), the presence of
//! the colour-space id, and the colour's position within a longer parameter
//! list — rather than just the colour value.

use festerm_core::{Color, Dimensions, Terminal};

fn screen(bytes: &[u8]) -> Terminal {
    let mut terminal =
        Terminal::new(Dimensions::new(40, 4).expect("dimensions")).expect("terminal");
    terminal.ingest(bytes);
    terminal
}

fn colors(sequence: &str) -> (Color, Color) {
    let terminal = screen(format!("{sequence}X").as_bytes());
    let cell = terminal.cell(0, 0).expect("printed cell");
    assert_eq!(
        cell.character(),
        'X',
        "sequence did not print: {sequence:?}"
    );
    (cell.foreground(), cell.background())
}

const RGB: Color = Color::Rgb {
    red: 38,
    green: 44,
    blue: 54,
};
const OTHER_RGB: Color = Color::Rgb {
    red: 255,
    green: 255,
    blue: 255,
};

#[test]
fn every_spelling_of_a_direct_color_selects_it() {
    for sequence in [
        // Semicolon-delimited, the form nearly every program emits.
        "\x1b[38;2;38;44;54m",
        // ITU T.416 canonical: colons, with an empty colour-space id.
        "\x1b[38:2::38:44:54m",
        // The compact colon form, which omits the colour-space id.
        "\x1b[38:2:38:44:54m",
        // A colour-space id that is present but meaningless here.
        "\x1b[38:2:1:38:44:54m",
    ] {
        assert_eq!(
            colors(sequence).0,
            RGB,
            "foreground not selected by {sequence:?}"
        );
    }

    for sequence in [
        "\x1b[48;2;38;44;54m",
        "\x1b[48:2::38:44:54m",
        "\x1b[48:2:38:44:54m",
        "\x1b[48:2:1:38:44:54m",
    ] {
        assert_eq!(
            colors(sequence).1,
            RGB,
            "background not selected by {sequence:?}"
        );
    }
}

#[test]
fn every_spelling_of_an_indexed_color_selects_it() {
    for sequence in ["\x1b[38;5;208m", "\x1b[38:5:208m"] {
        assert_eq!(
            colors(sequence).0,
            Color::Indexed(208),
            "foreground not selected by {sequence:?}"
        );
    }

    for sequence in ["\x1b[48;5;208m", "\x1b[48:5:208m"] {
        assert_eq!(
            colors(sequence).1,
            Color::Indexed(208),
            "background not selected by {sequence:?}"
        );
    }
}

/// The regression itself: a direct colour has to be stepped over exactly, or
/// whatever follows it in the same sequence is swallowed.
#[test]
fn a_direct_color_does_not_swallow_the_parameters_after_it() {
    for sequence in [
        // Exactly what Copilot CLI emits for an inline code span.
        "\x1b[38;2;255;255;255;48;2;38;44;54m",
        "\x1b[38:2::255:255:255;48:2::38:44:54m",
        "\x1b[38:2:255:255:255;48:2:38:44:54m",
        // Reversed, and mixed with the surrounding attribute codes that make
        // the colour neither first nor last in the list.
        "\x1b[48;2;38;44;54;38;2;255;255;255m",
        "\x1b[1;38;2;255;255;255;48;2;38;44;54;4m",
        "\x1b[1;38:2::255:255:255;48:2::38:44:54;4m",
    ] {
        assert_eq!(
            colors(sequence),
            (OTHER_RGB, RGB),
            "colors not both selected by {sequence:?}"
        );
    }
}

#[test]
fn attributes_after_a_direct_color_are_still_applied() {
    let terminal = screen(b"\x1b[38;2;255;255;255;48;2;38;44;54;1;3mX");
    let cell = terminal.cell(0, 0).expect("printed cell");
    assert!(cell.attributes().contains(festerm_core::Attributes::BOLD));
    assert!(cell.attributes().contains(festerm_core::Attributes::ITALIC));
}

#[test]
fn an_indexed_color_does_not_swallow_the_parameters_after_it() {
    for sequence in [
        "\x1b[38;5;208;48;5;17m",
        "\x1b[38:5:208;48:5:17m",
        "\x1b[38;5;208;48;2;38;44;54m",
    ] {
        let (foreground, background) = colors(sequence);
        assert_eq!(
            foreground,
            Color::Indexed(208),
            "foreground not selected by {sequence:?}"
        );
        assert_ne!(
            background,
            Color::Default,
            "background not selected by {sequence:?}"
        );
    }
}

/// A truncated or nonsensical colour must not take the parameters after it
/// with it, and must never panic.
#[test]
fn a_malformed_direct_color_is_survived() {
    for sequence in [
        "\x1b[38;2m",
        "\x1b[38;2;38m",
        "\x1b[38;2;38;44m",
        "\x1b[38;5m",
        "\x1b[38m",
        "\x1b[48;2;m",
        "\x1b[38:2m",
        "\x1b[38:m",
        "\x1b[38;9;1m",
    ] {
        let terminal = screen(format!("{sequence}X").as_bytes());
        assert_eq!(
            terminal.cell(0, 0).expect("printed cell").character(),
            'X',
            "sequence did not print: {sequence:?}"
        );
    }
}

#[test]
fn a_reset_clears_a_direct_color() {
    let (foreground, background) = colors("\x1b[38;2;255;255;255;48;2;38;44;54m\x1b[0m");
    assert_eq!(foreground, Color::Default);
    assert_eq!(background, Color::Default);
}

/// Copilot CLI ends an inline code span this way: a reset combined with the
/// body foreground in a single sequence.
#[test]
fn a_reset_combined_with_a_direct_color_keeps_the_color() {
    let (foreground, background) =
        colors("\x1b[38;2;255;255;255;48;2;38;44;54m\x1b[0;38;2;211;217;223m");
    assert_eq!(
        foreground,
        Color::Rgb {
            red: 211,
            green: 217,
            blue: 223
        }
    );
    assert_eq!(background, Color::Default);
}

fn status_string(sequence: &str) -> String {
    let mut terminal =
        Terminal::new(Dimensions::new(40, 4).expect("dimensions")).expect("terminal");
    terminal.ingest(sequence.as_bytes());
    terminal.ingest(b"\x1bP$qm\x1b\\");
    String::from_utf8(terminal.drain_replies()).expect("reply is text")
}

/// The probe `termstandard/colors` documents for detecting truecolor: set a
/// colour, read the setting back, and compare. A terminal that drops the
/// colour answers with the indexed fallback instead.
#[test]
fn decrqss_reports_the_direct_color_that_was_set() {
    assert_eq!(
        status_string("\x1b[48:2:1:2:3m"),
        "\x1bP1$r0;48:2::1:2:3m\x1b\\"
    );
    assert_eq!(
        status_string("\x1b[48;2;1;2;3m"),
        "\x1bP1$r0;48:2::1:2:3m\x1b\\"
    );
    assert_eq!(
        status_string("\x1b[38;2;255;255;255;48;2;38;44;54m"),
        "\x1bP1$r0;38:2::255:255:255;48:2::38:44:54m\x1b\\"
    );
}

#[test]
fn decrqss_reports_attributes_and_indexed_colors() {
    assert_eq!(status_string(""), "\x1bP1$r0m\x1b\\");
    assert_eq!(status_string("\x1b[1;4m"), "\x1bP1$r0;1;4m\x1b\\");
    assert_eq!(status_string("\x1b[31;44m"), "\x1bP1$r0;31;44m\x1b\\");
    assert_eq!(status_string("\x1b[91;104m"), "\x1bP1$r0;91;104m\x1b\\");
    assert_eq!(status_string("\x1b[38;5;208m"), "\x1bP1$r0;38:5:208m\x1b\\");
}

#[test]
fn decrqss_refuses_a_selector_it_cannot_report() {
    let mut terminal =
        Terminal::new(Dimensions::new(40, 4).expect("dimensions")).expect("terminal");
    terminal.ingest(b"\x1bP$q\"p\x1b\\");
    assert_eq!(terminal.drain_replies(), b"\x1bP0$r\x1b\\".to_vec());
}

/// A device-control string that is not DECRQSS stays silently ignored, and
/// its payload must not reach the screen.
#[test]
fn an_unrelated_device_control_string_is_ignored() {
    let mut terminal =
        Terminal::new(Dimensions::new(40, 4).expect("dimensions")).expect("terminal");
    terminal.ingest(b"\x1bP+q544e\x1b\\X");
    assert!(terminal.drain_replies().is_empty());
    assert_eq!(terminal.cell(0, 0).expect("printed cell").character(), 'X');
}
