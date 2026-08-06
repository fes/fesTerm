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

## Session and SSH Implementation Notes

### Local PTYs

- Give each Unix child a controlling terminal, propagate dimensions using
  `TIOCSWINSZ`, and reliably reap the child on shutdown.
- On Windows, use ConPTY and `ResizePseudoConsole`; detect unsupported Windows
  versions and surface a clear error.
- Session I/O owns transport/process lifecycles only. It produces bytes and
  resize/lifecycle events; it does not mutate the terminal core.

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
4. Start local PTY work with `TERM=xterm-256color` only while its advertised
   subset is accurately supported. At Milestone 6, ship a precise `festerm`
   terminfo entry, based on a maintained reference such as
   [Alacritty's entry](https://raw.githubusercontent.com/alacritty/alacritty/master/extra/alacritty.info),
   and validate it with [tack](https://invisible-island.net/ncurses/tack.html).
5. Test SGR mouse at coordinates beyond legacy limits, bracketed paste in both
   states, focus changes, right-margin behavior, wide-cell mutations, alternate
   screen restoration, and high-output flow control.

## Deferred or Deliberate Decisions

These require a focused design decision before implementation:

- Exact maximum limits for protocol strings, CSI parameters, queue watermarks,
  clipboard payloads, and repeat counts.
- Unicode data source, version update policy, emoji tailoring, private-use
  width option, and complex-script scope.
- Back-color erase behavior, which determines whether the future terminfo entry
  may advertise `bce`.
- Reflow semantics and how saved cursor state maps across reflow.
- Final `TERM` identity and the `festerm` terminfo distribution mechanism.
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
