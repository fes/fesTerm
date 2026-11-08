use festerm_core::{Dimensions, Terminal, MAX_RECORDING_CHECKPOINT_BYTES};
use festerm_ui_egui::{
    RecordingFrameSize, RecordingPalette, RecordingRenderer, TerminalFontFamily,
};

#[test]
fn invisible_unicode_cannot_enter_the_initial_checkpoint() {
    let size = RecordingFrameSize::new(240, 80).unwrap();
    let render = |text: &str| {
        let mut terminal = Terminal::new(Dimensions::new(20, 2).unwrap()).unwrap();
        terminal.ingest(b"\x1b[?25l");
        terminal.ingest(text.as_bytes());
        let pixels = RecordingRenderer::new(size, 8.0)
            .unwrap()
            .render(Some(&terminal))
            .unwrap()
            .to_vec();
        (terminal, pixels)
    };
    for (text, baseline) in [("\u{00a0}", " "), ("X\u{034f}", "X")] {
        let (terminal, pixels) = render(text);
        assert_eq!(pixels, render(baseline).1, "{text:?} unexpectedly has ink");
        assert!(terminal
            .recording_checkpoint(MAX_RECORDING_CHECKPOINT_BYTES)
            .is_err());
    }
}

#[test]
fn blank_braille_cannot_enter_a_checkpoint_when_a_supported_font_draws_no_ink() {
    let fonts =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts/iosevka-term");
    let faces = [
        "FesTermIosevka-Regular.ttf",
        "FesTermIosevka-Bold.ttf",
        "FesTermIosevka-Italic.ttf",
        "FesTermIosevka-BoldItalic.ttf",
    ]
    .map(|name| std::fs::read(fonts.join(name)).unwrap());
    let size = RecordingFrameSize::new(240, 80).unwrap();
    let render = |text: &str| {
        let mut terminal = Terminal::new(Dimensions::new(20, 2).unwrap()).unwrap();
        terminal.ingest(b"\x1b[?25l");
        terminal.ingest(text.as_bytes());
        let pixels = RecordingRenderer::with_appearance(
            size,
            8.0,
            TerminalFontFamily::IosevkaTerm,
            false,
            Some(faces.clone()),
            RecordingPalette::Dark,
        )
        .unwrap()
        .render(Some(&terminal))
        .unwrap()
        .to_vec();
        (terminal, pixels)
    };
    let (terminal, pixels) = render("\u{2800}");
    assert_eq!(pixels, render(" ").1);
    assert!(terminal
        .recording_checkpoint(MAX_RECORDING_CHECKPOINT_BYTES)
        .is_err());
}
