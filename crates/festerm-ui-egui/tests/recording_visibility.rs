use festerm_core::{Dimensions, Terminal, MAX_RECORDING_CHECKPOINT_BYTES};
use festerm_ui_egui::{RecordingFrameSize, RecordingRenderer};

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
