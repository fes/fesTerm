# M6 Manual Evidence Instructions

**Status:** Active protocol

This document is the explicit, step-by-step protocol for the M6 evidence that
has no automated oracle and must remain human-judged. Everything that
*could* be scripted has been: see
[`m6-evidence-collection.md`](m6-evidence-collection.md) and run
`scripts/collect-m6-evidence.sh` / `scripts/collect-m6-evidence.ps1` first.
Only run this protocol after that script reports `overall_status=pass` (or
every `fail`/`skipped` line is explained), so the manual pass is judging a
build that already cleared the deterministic bar.

Record every result using the evidence-record schema in
[`manual-validation.md`](manual-validation.md#evidence-record): commit SHA,
OS/version/architecture, desktop environment/compositor/display protocol,
display scale/DPI, exact scenario ID, pass/fail/not-run, and a concise
content-free observation. Never record terminal content, clipboard values,
credentials, hostnames, or filesystem paths.

## Blocked (do not attempt)

- **`tack`** — fesTerm ships no `festerm` terminfo entry yet (deferred until
  packaging can install one reliably; see
  [#27](https://github.com/fes/fesTerm/issues/27)). Record this scenario as
  `not run reason=blocked-no-terminfo-entry`, not as a manual failure, and do
  not spend time attempting it until #27 lands.

## Prerequisites

Build the exact candidate commit first:

```sh
cargo build --release -p festerm
```

```powershell
cargo build --release -p festerm
```

Install the reference applications you don't already have. Use whichever of
these matches your platform/package manager — all are common distribution
packages, not anything project-specific:

- **`less`, `vim`/`neovim`, `tmux`, `htop`**: macOS (`brew install neovim tmux htop`,
  `less`/`vim` are preinstalled), Linux (`apt install less vim neovim tmux htop`
  or your distribution's equivalent), Windows (`less`/`vim`/`tmux` typically via
  WSL; `htop` has no native Windows build — substitute Task Manager or skip
  and record `not run reason=unavailable`).
- **GitHub Copilot CLI**: `npm install -g @githubnext/github-copilot-cli` (or
  the current official install method) then `gh auth login` if not already
  authenticated.
- **`vttest`**: macOS (`brew install vttest`), Linux
  (`apt install vttest` or build from source), Windows (run it inside WSL;
  there is no native Windows build).

## Protocol

Run each scenario below inside a real fesTerm window (`cargo run --release -p
festerm` or the packaged build) at a normal terminal size, then resize the
window at least once mid-scenario. For each scenario, record every listed
observation, not just overall pass/fail — a partial failure (e.g., resize
works but alternate-screen restoration doesn't) is more useful evidence than
a single verdict.

### 1. Shell line editor

Use command history (up/down arrows), tab completion, a Unicode character in
a command argument, and a command containing a literal tab character.

**Observe:** tab stops land correctly; Unicode characters occupy the correct
cell width; arrow/history keys do not leak escape sequences into the prompt;
paste of a multi-line command behaves safely; the prompt redraws correctly
after resize.

### 2. `less`

Open a local text file of at least 200 lines: `less <file>`. Page down/up,
search with `/`, jump to end with `G`, then quit with `q`.

**Observe:** alternate screen is entered and restored cleanly on quit (no
leftover `less` content after exit); scrolling and search highlighting work;
resizing mid-session reflows `less`'s own display without corrupting
fesTerm's grid.

### 3. `vim` or `nvim`

Edit a scratch file: type text, move with arrow keys, enter visual mode and
yank/paste, change the cursor shape (e.g., enter/exit insert mode if your
config changes cursor style), then `:wq` or `:q!`.

**Observe:** cursor style changes between modes if configured; colors render
correctly; mouse/selection policy matches expectations (does fesTerm's own
selection yield to the application, or does the app own the click?); the
window title updates if the app sets one; alternate-screen restoration on
quit leaves no artifacts.

### 4. `htop` (or Task Manager on Windows if `htop` is unavailable)

Start `htop`, resize the window repeatedly while it's running, and quit with
`q`.

**Observe:** high-frequency redraw stays responsive without visible tearing
or stale rows; mouse clicks used to sort/select columns do not create a
local fesTerm text selection; resize is reflected promptly.

### 5. `tmux`

Start a nested session: `tmux`. Split panes (`Ctrl-b %` / `Ctrl-b "`),
switch panes, resize the fesTerm window, then detach (`Ctrl-b d`) and
reattach (`tmux attach`), then exit.

**Observe:** `tmux`'s own status line and terminal identification behave
correctly; mouse/focus reporting doesn't conflict between `tmux` and
fesTerm; resize reaches `tmux` and its panes; nested alternate-screen
programs inside a pane still restore correctly; detach/reattach doesn't
corrupt the grid.

### 6. GitHub Copilot CLI

Start an interactive session, type a prompt, paste a multi-line snippet
into it, resize the window mid-response, use arrow keys to navigate any
suggestion list, and exit.

**Observe:** alternate screen (if used), focus, bracketed paste, cursor-key
navigation, resize, and restoration on exit all work together — this is the
scenario most likely to surface a compounding defect that a single-feature
test wouldn't.

### 7. `vttest`

Run `vttest` and work through the menu, exercising at minimum: cursor
movement, screen-clearing, character sets, and the scrolling-region tests.
Follow `vttest`'s own on-screen pass/fail prompts for each sub-test.

**Observe:** record each `vttest` sub-test you ran as its own line (fesTerm
does not claim full VT100/xterm compatibility — see the baseline note in
[`m6-compatibility-checklist.md`](m6-compatibility-checklist.md)); note which
sub-tests are out of scope for fesTerm's currently supported subset rather
than marking them a failure.

## Native compositor/DPI/focus judgment (P4 complement)

While running `scripts/collect-m6-evidence.{sh,ps1}`'s native-window and
OS-input smoke on this machine, additionally observe by eye — these are
exactly the things the automated smoke cannot self-certify:

- The window genuinely has OS focus (not just process-level activity) before
  and after each resize.
- Resizing across a display's DPI/scale boundary (e.g., dragging between two
  monitors with different scale factors, or toggling a display's scale in
  System Settings) does not leave stale or blurry glyph rendering.
- The compositor presents frames smoothly during a rapid resize drag (no
  visible tearing, flashing, or multi-second stalls).

If you don't have real hardware for a given platform, use the manually
operated VM lab in [`vm-evidence-framework.md`](vm-evidence-framework.md)
instead of skipping the platform.

## Recording results

Append your results to the relevant row(s) in
[`manual-validation.md`](manual-validation.md)'s active registry and to the
P5 evidence note in
[`milestone-acceptance-record.md`](milestone-acceptance-record.md), and link
any reproducible failure as the smallest possible fixture, controlled-PTY
test, or focused defect issue per
[`m6-compatibility-checklist.md`](m6-compatibility-checklist.md)'s regression
triage rules before considering it fixed.
