//! Shared access to the captured-program corpus.
//!
//! The fixtures under `crates/festerm-core/tests/fixtures/tui/` are the
//! verbatim byte streams real programs wrote to a 120x40 pseudoterminal,
//! recorded by `scripts/capture-tui.py`. They are replayed in two places that
//! ask different questions of them:
//!
//! * `festerm-core`'s `tui_capture.rs` asks what the *parser* made of them -
//!   which cells carry which colours and attributes.
//! * `festerm-ui-egui`'s snapshot tests ask what the *renderer* drew - which
//!   is the step a cell-grid assertion cannot see.
//!
//! The cut that finds the interesting frame is subtle enough that having two
//! copies of it would be a liability, so it lives here.

/// The pseudoterminal geometry every capture was recorded at. Replaying into
/// a different width reflows the content and invalidates any assertion about
/// where things sit on the screen.
pub const COLUMNS: usize = 120;
/// See [`COLUMNS`].
pub const ROWS: usize = 40;

pub const VIM: &[u8] = include_bytes!("../../festerm-core/tests/fixtures/tui/vim.raw");
pub const HTOP: &[u8] = include_bytes!("../../festerm-core/tests/fixtures/tui/htop.raw");
pub const LESS: &[u8] = include_bytes!("../../festerm-core/tests/fixtures/tui/less.raw");
pub const NANO: &[u8] = include_bytes!("../../festerm-core/tests/fixtures/tui/nano.raw");
pub const TMUX: &[u8] = include_bytes!("../../festerm-core/tests/fixtures/tui/tmux.raw");
pub const FZF: &[u8] = include_bytes!("../../festerm-core/tests/fixtures/tui/fzf.raw");

/// A real Copilot CLI session, which is where the compact-colon SGR defect
/// fixed in #188 was first seen. Unlike the corpus above it never takes the
/// alternate screen - it draws inline and scrolls - so it is replayed whole.
pub const COPILOT: &[u8] =
    include_bytes!("../../festerm-core/tests/fixtures/copilot-cli-session.raw");

pub const EVERY_CAPTURE: [(&str, &[u8]); 6] = [
    ("vim", VIM),
    ("htop", HTOP),
    ("less", LESS),
    ("nano", NANO),
    ("tmux", TMUX),
    ("fzf", FZF),
];

/// Everything up to the point the program starts handing the screen back.
///
/// The interesting state - the status line, the highlighted match, the pane
/// divider - only exists while the program owns the alternate screen. Once it
/// leaves, all of that is gone by design, so both the replay assertions and
/// the rendered snapshots need the prefix rather than the whole stream.
///
/// Cutting at the final `ESC[?1049l` is not enough, because a program empties
/// the screen and turns its modes off *before* it gets there: tmux has already
/// disabled mouse reporting and cleared by then, so the prefix would be a
/// blank screen. The teardown is emitted as one short burst, so the cut is the
/// first of those sequences within the burst at the end of the stream.
pub fn prefix_while_on_the_alternate_screen(capture: &[u8]) -> &[u8] {
    const LEAVE: &[u8] = b"\x1b[?1049l";
    // Only sequences near the end count: tmux toggles mouse reporting dozens of
    // times mid-session as focus moves between panes.
    const TEARDOWN_BURST: usize = 512;
    const HANDING_BACK: [&[u8]; 6] = [
        LEAVE,
        // The erase comes first in the burst - tmux blanks the screen before
        // it turns the modes off - so without it the prefix is a clean screen
        // and there is nothing left to assert about.
        b"\x1b[2J",
        b"\x1b[?1000l",
        b"\x1b[?1002l",
        b"\x1b[?1006l",
        b"\x1b[?2004l",
    ];

    let leave = capture
        .windows(LEAVE.len())
        .rposition(|window| window == LEAVE)
        .expect("the capture never leaves the alternate screen");
    let burst_begins = leave.saturating_sub(TEARDOWN_BURST);
    let end = HANDING_BACK
        .iter()
        .filter_map(|marker| {
            capture[burst_begins..leave + LEAVE.len()]
                .windows(marker.len())
                .position(|window| window == *marker)
                .map(|offset| burst_begins + offset)
        })
        .min()
        .expect("the teardown burst contains at least the leave sequence");

    &capture[..end]
}

/// The bytes worth drawing, for captures of either shape.
///
/// Full-screen programs are cut at the teardown burst; a capture that never
/// took the alternate screen (Copilot CLI draws inline and scrolls) is drawn
/// whole, because for those the final frame *is* the interesting one.
pub fn frame_worth_rendering(capture: &[u8]) -> &[u8] {
    const LEAVE: &[u8] = b"\x1b[?1049l";
    if capture.windows(LEAVE.len()).any(|window| window == LEAVE) {
        prefix_while_on_the_alternate_screen(capture)
    } else {
        capture
    }
}
