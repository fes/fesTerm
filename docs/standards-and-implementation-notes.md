# Terminal Standards and Implementation Notes

**Status:** Research reference, not an ADR
**Reviewed:** 2026-08-05

This document collects standards, implementation references, and lessons from
other terminal projects that affect fesTerm. It supports the roadmap and does
not independently expand product scope. A proposed behavior becomes binding
only when it is covered by the requirements, compatibility plan, or an ADR.

## Normative and Compatibility References

| Area | Primary reference | fesTerm implication |
| --- | --- | --- |
| Terminal controls | [ECMA-48](https://ecma-international.org/publications-and-standards/standards/ecma-48/), [DEC VT510 reference](https://vt100.net/docs/vt510-rm/) | Use these for control-sequence structure and DEC behavior. |
| Compatibility behavior | [xterm control sequences](https://invisible-island.net/xterm/ctlseqs/ctlseqs.html) | Use as the project’s xterm-compatible behavioral baseline. |
| Parser states | [Paul Williams' DEC ANSI parser](https://vt100.net/emu/dec_ansi_parser) | Use the state-machine model for CSI, DCS, OSC, and error recovery. |
| UTF-8 | [RFC 3629](https://www.rfc-editor.org/rfc/rfc3629) | Strictly decode at most four bytes; reject overlong, surrogate, and out-of-range sequences. |
| Unicode width | [UAX #11](https://www.unicode.org/reports/tr11/) | Use a version-pinned width table, tailored for terminal cells. |
| Grapheme boundaries | [UAX #29](https://www.unicode.org/reports/tr29/) | Segment extended grapheme clusters before allocating cells. |
| Terminal capability advertisement | [terminfo(5)](https://man7.org/linux/man-pages/man5/terminfo.5.html) | `TERM` and the shipped terminfo entry must accurately describe implemented behavior. |
| SSH transport and sessions | [RFC 4251](https://www.rfc-editor.org/rfc/rfc4251), [RFC 4252](https://www.rfc-editor.org/rfc/rfc4252), [RFC 4253](https://www.rfc-editor.org/rfc/rfc4253), [RFC 4254](https://www.rfc-editor.org/rfc/rfc4254) | Define SSH architecture, authentication, transport, channels, PTYs, and resize behavior. |
| SSH extensions and algorithms | [RFC 4256](https://www.rfc-editor.org/rfc/rfc4256), [RFC 8308](https://www.rfc-editor.org/rfc/rfc8308), [RFC 8332](https://www.rfc-editor.org/rfc/rfc8332), [RFC 9142](https://www.rfc-editor.org/rfc/rfc9142) | Guide keyboard-interactive auth, extension negotiation, RSA SHA-2, and secure algorithm policy. |
| Local PTYs | [pty(7)](https://man7.org/linux/man-pages/man7/pty.7.html), [termios(3)](https://man7.org/linux/man-pages/man3/termios.3.html), [Windows ConPTY](https://learn.microsoft.com/en-us/windows/console/creating-a-pseudoconsole-session) | Define platform session and resize boundaries. |

## Terminal-Core Decisions to Make Before Milestone 2

### Parser and resource limits

- Implement the Williams parser state machine rather than adding CSI cases to a
  flat byte matcher. It gives explicit states for malformed sequences, C0
  controls, and string commands.
- In UTF-8 mode, default to 7-bit control interpretation. Raw C1 bytes overlap
  with UTF-8 continuation bytes; accepting them by default introduces ambiguity.
- Bound every protocol accumulator. This includes CSI parameter count and
  length, OSC/DCS/APC/PM/SOS payloads, and repeat/fill counts. On a limit
  violation, discard the active sequence and return to ground state.
- Keep parser work and session-to-core queues bounded. Resume upstream PTY/SSH
  reads below a low watermark after pausing at a high watermark.

M2's input and reply transport queues each have a 65,536-byte high watermark.
Writes are accepted atomically or rejected, preserving the exact order of
accepted bytes. `QueuePushResult` and the terminal's sticky, take-and-clear
overflow indicators make rejection observable to the session owner; this
includes automatically generated DSR replies.

The limits above are security requirements, not just optimizations. Unbounded
terminal writes and parameters have caused memory exhaustion in
[xterm.js #2108](https://github.com/xtermjs/xterm.js/issues/2108) and
[CVE-2023-40216](https://nvd.nist.gov/vuln/detail/CVE-2023-40216). Repeated
output and DCS graphics operators also require bounds checks; see
[CVE-2022-24130](https://nvd.nist.gov/vuln/detail/CVE-2022-24130).

### Grid, Unicode, and resize model

- Store an extended grapheme cluster and its display width in the leading cell
  and mark the second cell of a two-cell cluster as a continuation. Every erase,
  insert, delete, resize, and cursor operation must preserve or clear both
  halves together.
- Pin the Unicode version used by the width and segmentation dependencies.
  Default East Asian Ambiguous characters to one cell and make a two-cell
  policy configurable. Font glyph advance must not change cell allocation.
- Enforce a minimum width of two columns. A two-cell cluster cannot be placed
  safely in a one-column grid.
- Do not promise reflow until the model retains logical lines, maps saved
  cursors through reflow, and anchors the scrollback viewport. Until then, keep
  resize behavior deliberately simple as specified by Milestone 2.
- The alternate screen has no scrollback. Switching back must restore the
  primary screen and its scroll position exactly.

These practices address concrete defects in
[xterm.js #1779](https://github.com/xtermjs/xterm.js/issues/1779),
[xterm.js #5213](https://github.com/xtermjs/xterm.js/issues/5213),
[Alacritty #7697](https://github.com/alacritty/alacritty/issues/7697), and
[WezTerm #6669](https://github.com/wez/wezterm/issues/6669).

### Control-sequence behavior

- Treat pending wrap as explicit state: a character written in the last column
  leaves the cursor pending until the next printable character causes wrapping.
- Track both semicolon and colon CSI subparameter separators from the beginning.
  True-color and extended underline SGR forms use both conventions in deployed
  software.
- Support the Tier 1 DEC modes and operations identified in
  `COMPATIBILITY.md` before advertising them in terminfo. In particular,
  alternate-screen (`?1049`), autowrap (`?7`), cursor keys (`?1`), cursor
  visibility (`?25`), bracketed paste (`?2004`), focus (`?1004`), and SGR
  mouse (`?1006`) must have mode state and fixture coverage.
- Implement SGR 21 as doubly underlined and use SGR 22 to reset bold/faint,
  consistent with ECMA-48 and current xterm behavior.

## Security Boundaries

Terminal output is untrusted. The parser must never turn output into implicit
user input, clipboard disclosure, network activity, or process invocation.

| Feature | Safe default |
| --- | --- |
| Query replies (DECRQSS, title, color, font) | Reply only to recognized requests; never echo attacker-controlled payloads; strip C0 controls from every reply sent to a child session. |
| OSC 52 clipboard | Disable reads. If writes are implemented, require explicit opt-in or confirmation and cap decoded payload size. |
| OSC 8 hyperlinks | Preserve the URI after only the first parameter separator, allowlist schemes, bound URI length, and require user action before opening. |
| OSC 7 working directory | Never resolve an untrusted hostname or interpolate it into a shell command. |
| Titles and reporting | Sanitize control characters and bound lengths. Do not enable title-report queries by default. |
| DCS/graphics/passthrough | Keep unsupported protocols disabled; cap all payload, repeat, and allocation sizes. |

The query-reply rules mitigate a recurring escape-sequence injection class,
including [kitty GHSA-5gmr-9gwg-hhq6](https://github.com/kovidgoyal/kitty/security/advisories/GHSA-5gmr-9gwg-hhq6)
and the historical DECRQSS issues documented in
[ANSI Terminal Security](https://dgl.cx/2023/09/ansi-terminal-security).
OSC 8 parsing must split only the first parameter separator, as illustrated by
[xterm.js #4944](https://github.com/xtermjs/xterm.js/issues/4944).

## Milestone 2 Implemented Behavior

M2 is intentionally an ASCII/C0 terminal core. Printable bytes are accepted
only in the ASCII `0x20..=0x7e` range. Raw C1 bytes, including `0x9b`, are
ignored rather than interpreted as controls, so malformed or partial UTF-8
cannot become an escape sequence. The explicit parser has ground, ESC, CSI,
CSI-ignore, and discard-string states. CSI retains at most 32 parameters
(five decimal digits each) and two intermediate bytes. Unsupported OSC, DCS,
APC, PM, and SOS payloads are never stored; they are discarded through their
terminator or after 4096 bytes, at which point parsing returns to ground.
CR, LF, BS, and TAB still execute while a string is being discarded and leave
the parser in that string state.

CSI coordinates are one based. CUP/HVP and VPA apply relative to the top
margin while DECOM is set; their vertical range, and CUU/CUD/CNL/CPL under
DECOM, is the scrolling region. ED, EL, ECH, ICH, DCH, IL, DL, SU, and SD
operate on the active buffer. Erasure and newly exposed scroll rows use a
space with the current SGR rendition. `CSI r` accepts only a valid increasing
region and homes the cursor; index/reverse-index scroll only at that region's
boundary.

Pending wrap is explicit. A printable byte in the final column sets it only
when DECAWM is enabled; the next printable byte indexes and writes at column
one. SGR does not cancel pending wrap, which permits an application to change
rendition between a right-margin character and its continuation. CR, LF, BS,
TAB, cursor movement, home/margin changes, and disabling DECAWM cancel it.

M2 supports `ESC 7`/`ESC 8` and DEC private `?1048` save/restore of the
cursor, pending-wrap state, rendition, DECOM, and DECAWM for the saved buffer.
`CSI s`/`CSI u` save/restore cursor position only. `?47` switches to a
retained alternate buffer; `?1047` switches to and clears it; `?1049` saves
DEC state, enters a cleared alternate buffer, then returns to primary and
restores DEC state. Exiting either `?1047` or `?1049` resets the alternate
buffer, so a later `?47` cannot reveal its prior content. Primary and
alternate buffers each retain their cursor, scrolling region, and independent
DEC/ANSI saved cursor slots. A restored DECOM cursor is clamped to the active
buffer's current margins. Switching to a buffer dirties all of its rows. M2 also
tracks DECTCEM (`?25`) visibility but has no renderer.

SGR supports reset, the standard text flags (including double underline for
21 and bold/faint reset for 22), ANSI 16-color palettes, 256 indexed color,
and semicolon true color (`38;2;r;g;b` and `48;2;r;g;b`). Canonical colon
extended-color parameters are structurally retained and accepted with an
empty color-space subparameter. The only M2 replies are `CSI 5 n` (`CSI 0 n`)
and `CSI 6 n` (cursor position); device attributes and terminal identity
remain unsupported.

Resize does not reflow. It preserves the upper-left rectangular intersection
of both allocated buffers, initializes newly exposed cells to default blank
cells, clamps cursors and saved cursors, clamps margins (resetting a collapsed
multi-row region to full screen), adjusts pending wrap to the new right
margin, and marks every row dirty. M2 deliberately has no scrollback.

## Milestone 3 Implemented Behavior

M3 retains M2's bounded parser and transport queues while adding the input and
initial Unicode behavior below. Kitty keyboard, OSC/DCS handling, rendering,
shaping, scrollback, and reflow remain unsupported.

### Typed input and mouse policy

`festerm-core` owns `InputEvent` handling for `Key`, `Paste`, `Focus`, and
`Mouse` values. `InputEventOutcome` is the UI boundary: it distinguishes
encoded bytes, `SelectionAllowed`, `SelectionClaimed`, queue overflow, and
rejection. A UI starts local selection only after `SelectionAllowed`; any
enabled application mouse tracking mode claims pointer events, including an
event that its reporting level intentionally does not send.

`MouseEvent.column` and `.row` are zero-based terminal-cell coordinates.
SGR reports add one to both coordinates (`CSI < Cb ; Cx ; Cy M` or `m`) and
use unbounded decimal values apart from `usize` overflow. In legacy encoding,
the zero-based coordinates must fit `0..=222`; a larger coordinate is rejected
instead of truncating or wrapping. SGR releases use the actual button code and
final `m`; legacy releases use code 3 and final `M`. Shift, Alt, and Control
set bits 4, 8, and 16. Wheel up/down set 64/65 and motion sets 32.

DECSET `?9`, `?1000`, `?1002`, and `?1003` select respectively X10,
button-event, button-motion, and any-motion tracking. Enabling one replaces
the previous tracking level; resetting only the currently active level
disables it. `?1006` independently selects SGR rather than legacy encoding.
`?1` selects application cursor keys, and `ESC =`/`ESC >` select/reset
application keypad. `?2004` wraps a paste in `CSI 200~` and `CSI 201~`;
the wrapper and complete payload are one atomic bounded-queue write, so a
rejected paste cannot leave an unmatched marker. Literal marker-looking bytes
in the pasted payload are preserved. `?1004` emits `CSI I` and `CSI O` only
while enabled. Unknown DEC modes remain inert.

### Unicode policy

The core pins [`unicode-width` 0.2.2](https://crates.io/crates/unicode-width/0.2.2),
whose generated tables declare Unicode **15.1.0**, and
[`icu_properties` 2.2.0](https://crates.io/crates/icu_properties) with
compiled data for the `Grapheme_Extend` property. Both versions are explicit:
updating either Unicode data source requires compatibility review and fixture
coverage. It uses `UnicodeWidthChar::width`, so East Asian Ambiguous code
points use the crate's non-CJK one-cell policy. A printable width-one
character occupies one leading cell; a width-two character occupies a leading
`Double` cell plus an empty `Continuation` cell. Width-two output at the right
margin wraps before writing with DECAWM enabled; with DECAWM disabled it is
conservatively replaced with U+FFFD.

UTF-8 decoding is strict and incremental across `ingest` calls. At most four
bytes are retained; invalid starts, invalid continuations, overlong forms,
surrogates, and values above U+10FFFF emit U+FFFD without becoming controls.
An incomplete final sequence remains pending for the next call. Raw C1 bytes
are therefore invalid UTF-8 and become U+FFFD, rather than being interpreted
as C1 controls.

This is intentionally not a claim of full UAX #29 extended-grapheme
segmentation. Every `Grapheme_Extend` character, including variation
selectors and Bengali sign nukta (U+09BC), plus U+200D attaches to the most
recently written leading cell in that buffer; orphan marks are ignored. Other
zero-width controls do not attach. Simple Unicode-width-table emoji occupy
their reported two cells, but a multi-code-point ZWJ emoji sequence is
allocated by code point and may consume more than two cells. Complex-script
shaping and font fallback belong to the renderer. Every M2 edit, erase,
scroll, and resize operation repairs invalid leading/continuation
relationships. A repair clears an orphan or clipped half to a blank cell
rather than exposing an invalid grid.

## Milestone 4 Implemented Behavior

`festerm-ui-egui` is the GUI boundary and depends on `festerm-core`; the core
does not depend on egui, eframe, fonts, pixels, clipboard APIs, or session I/O.
`TerminalSnapshot` borrows the active `Screen`, cursor, modes, and cells for a
render pass. The UI's `TerminalRenderCache` copies only the rows returned by
`Terminal::take_dirty_rows`, except for its required initial/resize refresh.
This preserves core width-two leading cells and continuation cells without a
per-frame complete-grid clone.

The initial renderer measures an egui monospace font to calculate rows and
columns, clamps requests to valid core dimensions, and avoids redundant
resizes. It resolves ANSI, indexed, and RGB colors; handles inverse,
concealed, faint/bold fallback, italic, underline, double underline, and
strikethrough as egui permits; and draws a visibility-controlled cursor. It
uses cached one-cell layout jobs over egui's glyph atlas. It deliberately does
not claim ligature or cell-run shaping: preserving that mapping is M6 work.

Egui keyboard/text, paste, focus, pointer, wheel, selection, and copy events
become M3 `InputEvent` values. The core alone selects mode-aware byte
encodings. Mouse events enable local selection only after
`SelectionAllowed`; when an application mouse mode claims an event, UI
selection is cleared. Copy calls egui's native clipboard output API and never
uses OSC 52. Before M5, the app observes content-free metadata for drained
input rather than using a session transport.

M4 reports frame time, calculated dimensions, changed rows, latest input
outcome and input queue depth, content-free no-session input counters, and
input-to-paint-submission time. This ends after grid paint shapes are submitted
to egui, not when pixels are presented. It does not retain terminal content in
diagnostics. The core still has no scrollback, so M4
supports output-driven terminal scrolling and mode-aware wheel reporting but
does not claim local history scrolling.

## Session and SSH Implementation Notes

### Local PTYs

- Give each Unix child a controlling terminal, propagate dimensions using
  `TIOCSWINSZ`, and reliably reap the child on shutdown.
- On Windows, use ConPTY and `ResizePseudoConsole`; detect unsupported Windows
  versions and surface a clear error.
- Session I/O owns transport/process lifecycles only. It produces bytes and
  resize/lifecycle events; it does not mutate the terminal core.

M5 implements this boundary with `portable-pty` 0.9. Its native selector uses
Unix PTYs on Unix and ConPTY on Windows, including the crate's size propagation
to `TIOCSWINSZ`/ConPTY resize. `festerm-pty` accepts direct executable/argument
profiles, not shell command strings. It uses a 64-command input/resize/shutdown
queue and a 128-event output queue; reader pressure pauses further PTY reads
instead of retaining unbounded output. Each enqueued event calls an
application-owned notifier; the egui app supplies `Context::request_repaint`,
which is safe from the reader thread and avoids idle polling. Core input and
replies that meet session-queue backpressure remain ordered in a bounded 4 MiB
application pending buffer; an exhausted buffer or permanent rejection is
application-visible. Lifecycle, queue pressure, byte counts, resize results,
errors, and exit state are application-visible. Shutdown wakes the workers and
terminates the owned process tree: the Unix PTY session process group receives
`SIGTERM`, while Windows uses a kill-on-close Job Object. It waits only for a
caller-supplied finite interval; a timeout is reported rather than silently
ignored.

### Native SSH

- Use an established Rust SSH library and maintain an explicit state machine:
  transport/authentication, channel open, `pty-req`, optional approved
  environment variables, shell/exec, data flow, EOF, and bidirectional close.
- Send `pty-req` before `shell` or `exec`, request a reply, and set `TERM` to
  the capability actually provided. RFC 4254 specifies character dimensions
  before pixel dimensions.
- Send `window-change` on each terminal resize with `want_reply` false. Keep
  channel flow control bounded and send window adjustments as data is consumed.
- Verify host keys by default, use modern algorithm policy, exclude legacy
  SHA-1 `ssh-rsa` and `ssh-dss` defaults, and sanitize server banners before
  display.
- Test host-key changes, keyboard-interactive multi-round authentication,
  channel-window exhaustion, close races, resize, exit status, and rekey during
  data transfer.

RFC 4254 sections 5, 6.2, 6.7, and 8 are the implementation authority for
channel flow control, PTY request, resize, and terminal mode encoding.

## Compatibility and Test Plan

1. Use [vttest](https://invisible-island.net/vttest/vttest.html) interactively
   during Milestones 2 through 6; turn each discovered defect into a
   repository-owned fixture.
2. Use the Williams state machine and
   [alacritty/vte](https://github.com/alacritty/vte) as parser references.
   fesTerm may implement its own parser; compatibility does not require using
   that crate.
3. Extend the fixture format before Milestone 2 closes to assert cell
   attributes, modes, scrollback, dirty rows, and emitted replies/input bytes,
   not only grid text and cursor position.
4. Keep `TERM=xterm-256color` as the M6 interoperability baseline while
   deterministic regressions define fesTerm's supported subset. A precise
   `festerm` terminfo entry is deferred until M9 packaging can install it
   reliably on Windows, macOS, and Linux; validate that entry with
   [tack](https://invisible-island.net/ncurses/tack.html) before making it the
   default.
5. Test SGR mouse at coordinates beyond legacy limits, bracketed paste in both
   states, focus changes, right-margin behavior, wide-cell mutations, alternate
   screen restoration, and high-output flow control.

## Deferred or Deliberate Decisions

These require a focused design decision before implementation:

- Exact maximum limits for clipboard payloads and repeat counts.
- Unicode data source, version update policy, emoji tailoring, private-use
  width option, and complex-script scope.
- Back-color erase behavior, which determines whether the future terminfo entry
  may advertise `bce`.
- Reflow semantics and how saved cursor state maps across reflow.
- Final `TERM` identity and the `festerm` terminfo distribution mechanism
  remain M9 packaging work; M6 uses the documented `xterm-256color`
  interoperability baseline.
- Kitty keyboard protocol and synchronized update support.
- Clipboard, hyperlink, title, working-directory, and graphics protocol consent
  models.

## Further Sources

- [DEC VT100 User Guide](https://vt100.net/docs/vt100-ug/chapter3.html)
- [GraphemeBreakTest.txt](https://www.unicode.org/Public/UCD/latest/ucd/auxiliary/GraphemeBreakTest.txt)
- [OpenSSH `ssh_config(5)`](https://man.openbsd.org/ssh_config.5)
- [OpenSSH `sshd_config(5)`](https://man.openbsd.org/sshd_config.5)
- [Kitty keyboard protocol](https://sw.kovidgoyal.net/kitty/keyboard-protocol/)
- [xterm.js flow control guide](https://xtermjs.org/docs/guides/flowcontrol/)
- [WezTerm #4293](https://github.com/wez/wezterm/issues/4293), a representative
  high-output rendering latency report

## Milestone 6 Implemented Behavior

M6 adds DEC tab-stop state: default stops are every eight columns, `ESC H`
sets a stop, and `CSI Ps g` clears the current (`Ps=0`) or all (`Ps=3`) stops.
Stops preserve their overlap on resize and newly exposed columns receive the
standard every-eight-columns default.

`DECSCUSR` (`CSI Ps SP q`) records block, underline, and bar cursor shapes;
the egui renderer maps the shape without changing cell geometry. It does not
schedule a blink timer, so blinking and steady variants intentionally share
their static shape until a presentation-timing policy exists.

The bounded OSC parser retains only OSC 0/2 titles and OSC 8 hyperlink
metadata. Titles are UTF-8 validated, stripped of controls, and capped at 256
characters before the application requests a native window-title update. OSC
8 accepts only `http`, `https`, and `mailto` targets up to 2,048 bytes, stores
them on cells, and never opens a target automatically. OSC 52 remains
unsupported. All other string controls remain discard-only and every OSC
payload remains bounded by `MAX_STRING_BYTES`.

Primary (`CSI c`) and secondary (`CSI > c`) device attributes respond with a
conservative VT102 identity and neutral secondary identity. This avoids
advertising xterm extensions that fesTerm has not implemented.
