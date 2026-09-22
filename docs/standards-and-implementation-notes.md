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
| OSC 8 hyperlinks | Preserve only normalized absolute ASCII HTTP/HTTPS URLs with a host after the first parameter separator; bound URI and link-run length; require explicit user action and repeat validation in the application before opening. |
| OSC 7 working directory | Never resolve an untrusted hostname or interpolate it into a shell command. |
| Titles and reporting | Sanitize control characters and bound lengths. Do not enable title-report queries by default. |
| DCS/graphics/passthrough | Keep unsupported protocols disabled; cap all payload, repeat, and allocation sizes. |

The query-reply rules mitigate a recurring escape-sequence injection class,
including [kitty GHSA-5gmr-9gwg-hhq6](https://github.com/kovidgoyal/kitty/security/advisories/GHSA-5gmr-9gwg-hhq6)
and the historical DECRQSS issues documented in
[ANSI Terminal Security](https://dgl.cx/2023/09/ansi-terminal-security).
OSC 8 parsing must split only the first parameter separator, as illustrated by
[xterm.js #4944](https://github.com/xtermjs/xterm.js/issues/4944).
Allowlisted text is parsed with the WHATWG URL parser rather than accepted by
scheme prefix, preventing malformed web-looking strings from falling through
an OS opener's local-file path behavior. Non-ASCII targets are rejected;
internationalized hosts remain available through their Punycode form and
other characters through percent encoding.

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
and semicolon true color (`38;2;r;g;b` and `48;2;r;g;b`). Colon extended-color
parameters are structurally retained; true color accepts both canonical
`38:2::r:g:b` / `48:2::r:g:b` and widespread compact
`38:2:r:g:b` / `48:2:r:g:b` forms. The only M2 replies are `CSI 5 n`
(`CSI 0 n`) and `CSI 6 n` (cursor position); device attributes and terminal
identity remain unsupported.

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
[`unicode-segmentation` 1.13.3](https://crates.io/crates/unicode-segmentation/1.13.3).
Both versions are explicit: updating either Unicode data source requires
compatibility review and fixture coverage. East Asian Ambiguous code points
use the width crate's non-CJK policy. A grapheme of width one occupies one
leading cell; width two occupies a leading `Double` cell plus an empty
`Continuation` cell.

The compatibility oracle vendors Unicode Emoji 15.1's `emoji-test.txt`,
`emoji-sequences.txt`, and `emoji-zwj-sequences.txt` with checksummed
provenance. All fully-qualified entries are exercised against fragmented core
input and renderer fallback. ICU may contain newer property data; support is
not claimed beyond 15.1 until width tables, corpus, fonts, and snapshots are
advanced together.

UTF-8 decoding is strict and incremental across `ingest` calls. At most four
bytes are retained; invalid starts, invalid continuations, overlong forms,
surrogates, and values above U+10FFFF emit U+FFFD without becoming controls.
An incomplete final sequence remains pending for the next call. Raw C1 bytes
are therefore invalid UTF-8 and become U+FFFD, rather than being interpreted
as C1 controls.

The core applies UAX #29 extended-grapheme boundaries incrementally to the
most recently written leading cell and recomputes the whole sequence with
`UnicodeWidthStr`. This covers variation selectors, ZWJ sequences, modifiers,
keycaps, and regional-indicator flags even across `ingest` calls. A sequence
may promote from one to two cells; at the right margin it wraps before
promotion with DECAWM enabled or becomes U+FFFD when promotion cannot fit with
DECAWM disabled. A grapheme is capped at 256 UTF-8 bytes; an extending scalar
beyond the cap replaces it with U+FFFD and clears the extension anchor. Orphan
zero-width text is ignored. Complex-script shaping and font fallback belong to
the renderer. Every M2 edit, erase, scroll, and resize operation repairs
invalid leading/continuation relationships. A repair clears an orphan or
clipped half to a blank cell rather than exposing an invalid grid.

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
uses generation-keyed cached one-cell layout jobs over egui's glyph atlas by
default. The clipped cell-run shaping path is now an explicit, persisted,
default-off preference. It groups only compatible ASCII cells and preserves
immutable cursor, selection, hit-test, hyperlink, fallback, and wide-cell
boundaries under ADR 0012.

The renderer prepends pinned Noto Emoji monochrome fallback to each terminal
font chain. Emoji-presentation graphemes use a bounded Swash path over pinned
Noto Color Emoji bitmap glyphs; layered glyphs are composited, centered, and
clipped inside the core-owned span. VS15 remains monochrome, and any failed or
oversized color rasterization falls back to the ordinary layout path.
Versioned interface settings select color (the compatibility-preserving
default) or monochrome presentation. Monochrome bypasses the color texture
path while retaining the same core cells and owned Noto Emoji fallback.
Per-frame diagnostics expose only aggregate emoji paint/cache hit/cache miss
and raster attempt/failure/negative-hit counts. While the visible working set
fits the documented cache bounds, tests enforce one cold attempt per unique
text-and-size key, zero warm attempts, and one failed attempt before
negative-cache reuse; Criterion tracks cold population and warm reuse timing
independently.

Egui keyboard/text, paste, focus, pointer, wheel, selection, and copy events
become M3 `InputEvent` values. The core alone selects mode-aware byte
encodings. Mouse events enable local selection only after
`SelectionAllowed`; when an application mouse mode claims an event, UI
selection is cleared. Copy calls egui's native clipboard output API and never
uses OSC 52. Before M5, the app observes content-free metadata for drained
input rather than using a session transport.

M4 reports frame time, calculated dimensions, changed rows, latest input
outcome and input queue depth, content-free no-session input counters,
aggregate emoji cache work, and input-to-paint-submission time. This ends
after grid paint shapes are submitted to egui, not when pixels are presented.
It does not retain terminal content in diagnostics. The core still has no scrollback, so M4
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

Windows startup additionally selects ConPTY before `portable-pty` allocates its
first pseudoconsole. The only optional sidecar location is the install-relative
layout in [`third_party/conpty/README.md`](../third_party/conpty/README.md);
its DLL and host executable must both match the pinned SHA-512 file hashes.
The process DLL search is reduced to System32, then the verified DLL is loaded
with its absolute path. Missing or invalid sidecars use inbox ConPTY. This
prevents a DLL in the executable, current, or `PATH` directory from changing
the fallback runtime.

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
   `festerm` terminfo entry is deferred until M10 packaging can install it
   reliably on Windows, macOS, and Linux; validate that entry with
   [tack](https://invisible-island.net/ncurses/tack.html) before making it the
   default.
5. Test SGR mouse at coordinates beyond legacy limits, bracketed paste in both
   states, focus changes, right-margin behavior, wide-cell mutations, alternate
   screen restoration, and high-output flow control.

### SGR conformance and DECRQSS

Milestone 2's SGR notes above describe what the parser accepts. What was
missing was a way to *check* it, and the gap shipped a defect: inline code
backgrounds from GitHub Copilot CLI were lost in a release while every
synthetic SGR unit test passed, because each test set one colour at a time and
no test set a foreground and a background in the same sequence.

None of the usual suites close that gap on their own.
[vttest](https://invisible-island.net/vttest/vttest.html) is menu-driven and
predates ITU T.416 extended colour entirely.
[esctest2](https://github.com/ThomasDickey/esctest2) is automatable but asserts
state a terminal *reports*, not the colour a cell ends up with.
[termstandard/colors](https://github.com/termstandard/colors) is the authority
on the delimiter question and defines the one machine-checkable probe: set a
direct colour, ask the terminal to report the setting back, and compare.

fesTerm therefore owns the coverage, in two parts:

- `crates/festerm-core/tests/sgr_conformance.rs` varies the *shape* of each
  sequence rather than the colour value — semicolon versus canonical colon
  (`38:2::r:g:b`) versus compact colon (`38:2:r:g:b`) versus a populated
  colour-space id, each of those alone, combined with the other colour, and
  surrounded by attribute codes — plus truncated and nonsensical forms that
  must be survived without disturbing the parameters after them.
- `crates/festerm-core/tests/copilot_cli_capture.rs` replays a committed,
  scrubbed byte-for-byte capture of a real Copilot CLI session (see the file's
  own provenance comment) and asserts the resulting cell colours. Synthetic
  cases only cover shapes someone thought of; the capture covers what a program
  actually emits.

The probe needs DECRQSS, which fesTerm now answers: `DCS $ q m ST` reports the
current pen as `DCS 1 $ r <parameters> m ST`, always beginning with `0` so the
report is a complete reconstruction rather than a delta. Direct colours are
reported in the canonical colon form xterm uses (`38:2::r:g:b`), which is also
what tells the caller that colons are accepted. Any other selector is answered
`DCS 0 $ r ST` — "not recognized" — rather than left unanswered, so a caller
never waits on a reply that will not come. Device-control strings that are not
DECRQSS stay ignored and bounded exactly as before.

### The captured-program corpus

One capture is not a corpus. `scripts/capture-tui.py` records the verbatim byte
stream a real program writes to a 120x40 pseudoterminal, and
`crates/festerm-core/tests/tui_capture.rs` replays the results. Six programs are
committed, chosen because each one is the shortest path to a different
escape-sequence class rather than because it is popular:

| Fixture | What only this one covers |
| --- | --- |
| `vim` | alternate screen save and restore, a status line cleared to the margin, indexed-palette end-of-buffer markers |
| `htop` | a full-width highlighted header, bracketed meter bars, periodic whole-screen repaint |
| `less` | a search highlight that has to cover the match and nothing either side of it |
| `nano` | a modal prompt drawn over text that must survive underneath it |
| `tmux` | SGR mouse reporting, bracketed paste, DEC special graphics pane dividers, and putting every mode back on exit |
| `fzf` | incremental full-list redraw, the extended palette, and `ESC[;38;5;108m` — a leading *empty* parameter |

That last one is the argument for the whole approach: fzf spells its colours
with an implicit leading zero several hundred times in one short session, and no
hand-written test in this repository had ever produced the form.

Two rules keep the corpus honest.

**Assertions key off structure, not values.** A recording is one moment on one
machine: every percentage, PID, load average and clock reading differs on the
next one. Asserting that a meter is drawn as a bracketed bar is durable;
asserting that it reads 42.7% is a trap for whoever re-records.

**Identity never reaches the fixture.** Programs are run inside a throwaway
`HOME` at a fixed path with a fixed user name and no user configuration, and
tmux and htop are additionally configured to keep the host name and the
machine's process list off the screen. The harness then greps the capture for
the real user name, host name and home directory and refuses to write the file
if it finds them. Scrubbing afterwards is not an option, because changing byte
lengths corrupts the column alignment the replay depends on.

The corpus was checked against deliberate mutations rather than assumed to
work. Breaking the DEC graphics translation, the alternate-screen mode flag, or
the pen used to fill erased cells each fails the tests that claim to cover it.
That exercise found a real hole: *no* test in `festerm-core` noticed when erased
cells stopped carrying the pen's attributes, because no program in the corpus
sets an attribute before clearing. That case is now asserted directly in
`sgr_conformance.rs`, which is where standards-derived cases belong - the corpus
covers what programs do, the conformance file covers what the standard
requires.

Recording is deliberately not part of any test run or CI job. The fixtures are
the evidence the assertions were written against, so replacing them is a
decision to be made and reviewed, not a side effect of running the suite.

### Property tests and fuzzing for the parser

Every other test of the parser feeds it sequences a human wrote, which is the
opposite of the input that finds panics. The parser consumes bytes chosen
freely by whatever program the user runs, so it is the one place in fesTerm
where the input is genuinely adversarial.

`crates/festerm-core/tests/parser_properties.rs` states the invariants as
properties rather than asserting particular renderings: what a terminal *does*
with `CSI 999999999999 m` is a judgement call, but that it neither panics nor
leaves the cursor outside the screen is not. The properties cover arbitrary
input, equivalence across chunk boundaries (bytes arrive from a pseudoterminal
in whatever sized pieces the kernel hands over, so a parser that only works on
whole sequences works by luck), resizing part-way through a stream, the
well-formedness of every reply, and an SGR round trip through `DECRQSS`.

The generators mix random bytes with well-formed sequence shapes on purpose.
Uniformly random bytes almost never form a valid CSI, so a purely random
generator spends its whole budget on the printable-text path and never reaches
the parameter handling that is actually delicate.

This found two reachable panics. `CSI L` and `CSI M` (insert and delete lines)
and `CSI S` and `CSI T` (scroll) all clamp their count to the scroll region and
then computed the last row to copy as `bottom - count`, which underflows the
moment the count covers the whole region. `CSI 6 L` on a six-row screen was
enough to take the process down, and any program can send it. Both are fixed in
`screen.rs` and pinned by name as regression cases.

The properties were themselves checked against deliberate mutations rather than
assumed to be load-bearing: reverting either underflow fix fails two or three
named properties.

`fuzz/` holds two `cargo-fuzz` targets covering the same surface without a
grammar: `parser` asserts the invariants on one buffer, and `parser_chunked`
feeds the same bytes whole and cut at fuzzer-chosen offsets and requires the two
terminals to agree. `scripts/seed-fuzz-corpus.sh` seeds them from the capture
fixtures, which hands libFuzzer a population that already reaches alternate
screen, DEC special graphics, `DECRQSS`, OSC hyperlinks and truecolour SGR
rather than making it rediscover the shape of a CSI sequence first.

The fuzz package is deliberately outside the workspace. The workspace forbids
`unsafe_code` and `libfuzzer-sys` generates an `unsafe extern "C"` entry point,
and keeping it separate also means a stable-toolchain `cargo build --workspace`
never tries to build a target that requires nightly.

Fuzzing runs on a nightly schedule (`.github/workflows/fuzz.yml`), not on pull
requests: a short run on a PR finds almost nothing, and a long one would make
the PR unmergeable for an hour. The property tests are the per-PR coverage; the
scheduled job is what keeps looking after they stop. A crash there is a real
bug, and its fix belongs back in the property tests as a named regression case.

### Over-long string sequences

A string control (`OSC`, `DCS`, `APC`, `PM`) whose payload passes
`MAX_STRING_BYTES` returns to ground immediately rather than waiting for a
terminator that may never arrive. The cost is that the remainder of an
over-long payload prints as text. That is the deliberate trade: the alternative
is to keep consuming until a terminator, which lets any program wedge the
terminal permanently with an unterminated `OSC`, and because `ESC c` would be
swallowed along with everything else, not even a reset would recover it. A
screenful of garbage is recoverable; a silently dead terminal is not.

What the bound must still guarantee, and what is asserted, is that a truncated
payload is never *acted on* - no half-read title is applied - and that the
terminal is usable immediately afterwards.

### Unterminated string sequences

`ESC` inside a string control leaves the string. Only `ESC \` (ST) ends it
normally; `ESC` followed by anything else abandons the payload, and that byte
begins a fresh escape sequence, as in Williams' state machine and in xterm,
VTE and kitty.

fesTerm previously appended the `ESC` and the byte after it to the payload and
kept consuming. That made the length bound above the *only* escape from an
unterminated string, so a truncated title write - or a program that died
mid-sequence - swallowed every following byte up to `MAX_STRING_BYTES`,
including the `ESC[...m` or `ESC[H` that would have restored the screen. The
observable symptom was a terminal that stopped drawing for no visible reason.

The abandoned payload is discarded rather than dispatched: a string that never
reached its terminator was never a complete request, and acting on a truncated
one turns a half-written title or hyperlink into a state change nobody asked
for. The length bound remains, now as a second line of defence for a string
that is never interrupted at all rather than as the primary one.

### Third-party conformance: esctest2

Every test above checks a sequence we thought to write down, which means the
gaps are exactly the sequences we did not think of. That is how the SGR
compact-colon gap fixed in #188 reached a release.
[esctest2](https://github.com/ThomasDickey/esctest2) is xterm's maintainer's
own suite, pinned at `2798f12`, and it checks what he thought to write down
instead.

It cannot be pointed at a parser. `escio.Init()` puts esctest's own stdin into
raw mode and drives the terminal it is *running inside*: sequences go to
stdout, and the terminal's replies come back on stdin. So conformance-testing
`festerm-core` means being the terminal at the other end of a pty.
`crates/festerm-core/examples/esctest-host.rs` is that: it spawns
`python3 esctest.py` on a pty, feeds the child's output into a `Terminal`, and
writes `drain_replies()` back into the pty. `scripts/run-esctest2.sh` fetches
the pinned commit and runs it.

We pass 110 of the suite's 559 test methods today, so running all of it would
produce a wall of red that everyone learns to ignore. Instead
`validation/esctest2-allow.txt` names what we are held to - cursor addressing,
tab stops, and save/restore cursor, 64 tests - and CI fails if any of it
regresses. `validation/esctest2-skip.txt` carries the exclusions *within* those
families, one reason per line, so each skip is an admission rather than a
silence. `scripts/run-esctest2.sh --everything` surveys the whole suite without
gating, which is how to see what the next phase buys.

The large remaining blocker is DECRQCRA: 316 of the 559 methods assert screen
contents, and `AssertScreenCharsInRectEqual` can only read the screen by asking
for a rectangle's checksum. Until that is answered those tests cannot observe
anything at all - not pass, not fail. #193 tracks the phases.

Two window operations are implemented for this reason and no other: `CSI 18 t`
and `CSI 19 t` report the screen size, which esctest asks for before every
single test. They are questions about the grid, which we can answer exactly.
The rest of `CSI ... t` moves, resizes, raises and iconifies a window, which
belongs to the embedder, and is ignored.

### Resets: DECSTR, and what a save actually saves

`CSI ! p` (DECSTR) is a soft reset: it returns the modes, the scroll region
and the saved cursor to their power-on values and leaves the screen's
contents, the scrollback, the tab stops and the title alone. It is what a
program sends to get a predictable terminal without throwing away what the
user is looking at. Two details are easy to get wrong and are worth stating:
the cursor itself does not move (only the *saved* cursor is reset, to home),
and autowrap comes back on. DEC STD 070 says autowrap should be off, but xterm
restores it to the resource default and records that it deviates to avoid
breaking applications that rely on wrapping; ours defaults on, so that is where
a soft reset leaves it. Character sets are RIS's business, not DECSTR's.

Restoring when nothing has been saved is defined as restoring the power-on
state - home the cursor, drop origin mode, reset the pen - rather than as
doing nothing. The difference matters because "do nothing" makes a restore's
effect depend on history the caller cannot see, and because with nothing able
to clear the saved cursor it outlives whatever wrote it. That was visible in
the conformance run before DECSTR existed: a saved cursor leaked from one
esctest2 test into the next, so whether a test passed depended on what ran
before it.

DECSC saves the cursor, the pen, the character sets and the last-column flag.
It does **not** save DECAWM, which fesTerm previously did. A program turns
autowrap off precisely to stop a wrap from happening - drawing a box, filling
the last column of a status line - so an unrelated save/restore that turns it
back on hands it exactly the wrap it was avoiding, and the damage shows up as
a scrolled screen rather than as one misplaced character.

### Relative vertical motion and the scroll region

`CUU` and `CUD` are bound by the scroll region whenever the cursor *starts
inside* it, whether or not origin mode is set. A cursor that starts outside
the region is bound by the screen instead: it was never in that pane, so the
margin is not its boundary.

This is a different rule from the one absolute addressing follows. `CUP` and
`VPA` are measured from the region only in origin mode, because origin mode is
exactly what redefines where row 1 is. fesTerm applied the absolute rule to
both, so a program that reserved rows 2..4 and then moved down from row 3
landed on row 25 - outside the pane it had reserved, with its next write
appearing in someone else's.

## Deferred or Deliberate Decisions

These require a focused design decision before implementation:

- Exact maximum limits for clipboard payloads and repeat counts.
- Unicode data source, version update policy, emoji tailoring, private-use
  width option, and complex-script scope.
- Back-color erase behavior, which determines whether the future terminfo entry
  may advertise `bce`.
- Reflow semantics and how saved cursor state maps across reflow.
- Final `TERM` identity and the `festerm` terminfo distribution mechanism
  remain M10 packaging work; M6 uses the documented `xterm-256color`
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
- [termstandard/colors](https://github.com/termstandard/colors), the reference
  for direct-colour delimiters and the DECRQSS detection probe
- [esctest2](https://github.com/ThomasDickey/esctest2)
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
