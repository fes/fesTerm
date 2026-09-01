# fesTerm GUI Design

**Status:** Normative product target with explicitly labeled implementation
notes. Unqualified requirements describe the approved target; paragraphs that
begin with “Current implementation” describe temporary repository state and
must not override the target. Historical examples are labeled as such.

This document defines the interaction model, visual hierarchy, and product-level GUI principles for fesTerm. It complements `ARCHITECTURE.md` and `docs/ui-test-plan.md`: architecture defines ownership and dependencies, the UI test plan defines validation, and this document defines the intended user experience.

## Canonical Wireframe

![fesTerm GUI wireframes v1.2](images/festerm-gui-wireframe-v1.2.png)

The v1.2 wireframe is the current visual reference for application chrome, independent session chips, launcher and settings surfaces, the session inspector, reconnecting state, and optional wrapped chip rows. It communicates structure and interaction hierarchy rather than final colors, typography, or platform-specific window controls.

The detailed PNGs linked throughout this document are target-state review
assets, not application screenshots. They use the approved `752 × 516`
baseline viewport, blue-graphite semantic colors, integrated chrome, compact
status bar, and representative non-sensitive content. The narrow-window asset
uses the `360 × 516` minimum-width case. Their stable filenames are intended
for automated or agent-assisted comparisons against native Windows, macOS, and
Linux captures.

Editable SVG sources for mockups introduced or revised after this review live
under `images/gui-mockups/source/`; the adjacent PNG remains the comparison
artifact. Rasterization must preserve the baseline logical viewport and may
not silently change prose-owned behavior.

## Product Posture

fesTerm should feel like a restrained, native-feeling terminal workstation:

- denser than a consumer application;
- quieter than a developer dashboard;
- more integrated than a minimal terminal window;
- less workflow-opinionated than command-block or cloud-dependent products; and
- always centered on terminal interaction.

The terminal viewport is the primary surface. Application chrome exists to help users create, identify, switch, restore, and diagnose sessions without competing with terminal content.

## Core UX Principles

### Stable application identity, dynamic terminal content

The application should optimize for persistent identity while the terminal remains dynamic.

Stable concepts include:

- session identity;
- profile identity;
- workspace identity;
- host identity; and
- user-assigned names.

Transient concepts include:

- terminal-provided titles;
- current directory;
- foreground process;
- alternate-screen content;
- cursor state; and
- connection transport details.

Transient terminal state must not silently replace stable application identity.

### One primary action per screen

Each major screen should have one dominant purpose:

- **Launcher:** start or connect to a session.
- **Terminal tab:** interact with the terminal.
- **Settings:** configure the application.
- **Diagnostics:** inspect or troubleshoot state.

Secondary actions should remain available but visually subordinate.

### Quiet by default

Normal operation should avoid persistent telemetry, cell grids, verbose status strings, or developer instrumentation.

Diagnostics should be available through an explicit overlay, panel, or command. Errors should be concise and contextual, with details available on demand.

### Information must earn its space

Every persistent UI element must present reliable information that supports a
user decision. Unknown, weak, duplicated, speculative, or merely decorative
data is omitted; empty space is acceptable. Each visible value must have an
identified source of truth in the terminal model, session controller,
transport, profile/configuration, operating system, or application state.

In particular, fesTerm must not infer semantic command boundaries from an
undifferentiated terminal byte stream. Current time, last-input time, and
last-output time do not earn permanent status-bar space. If future shell
integration provides reliable command markers, contextual facts such as exit
status or command duration may appear only when genuinely available.

### Keyboard-first, mouse-friendly

All primary workflows should be efficient from the keyboard while remaining discoverable and usable with a mouse.

### Local-first

The launcher, profiles, sessions, workspace restore, and settings must work
without sign-in. Any future cross-machine portability should be explicit
export/import, not a required account or cloud-sync prerequisite.

## Root Application States

![Empty-window Launcher target](images/gui-mockups/launcher-empty.png)

The application should never present an undefined empty state. The root state is always one of:

1. **Session launcher**
2. **One or more open sessions**
3. **One or more application surfaces such as Settings**
4. **Workspace restoration in progress**

When the last session or application surface closes, fesTerm returns to the
launcher instead of showing a blank window or exiting unexpectedly. A possible
future close-window-with-last-session preference remains a usability-testing
question, not current configurable behavior.

### One viewport per chip

One chip owns one session lifecycle and one terminal viewport. Split panes are
not planned: fesTerm adds no split controls, pane keybindings, workspace pane
trees, or speculative architecture for them. Terminal-native multiplexers are
the appropriate composition mechanism unless a compelling unmet need later
justifies reopening this product decision.

### Window ownership

Initial session management is single-window. One fesTerm process/window owns
its chip collection. There is no detach-to-window, cross-window chip drag,
live-session migration, secondary inspector, or speculative IPC/window-manager
layer. Separate ordinary fesTerm instances may exist when the platform permits,
but they own independent sessions. A future workspace may open fresh restored
sessions in a new window; that is launch/restoration, never migration of a live
PTY, SSH, or serial lifecycle.

## Session Launcher

### Launch behavior

When fesTerm starts without a workspace to restore, it should open a lightweight session launcher rather than immediately launching the platform default shell.

The launcher is not a wizard, dashboard, or onboarding flow. It should be fast, compact, and usable repeatedly.

First run is the same ordinary Launcher used on every later invocation. There
is no onboarding wizard, telemetry consent prompt when no telemetry is
collected, default-terminal solicitation, automatic shell-discovery report,
tips carousel, release-note interruption, or mandatory profile setup. Local
Shell, SSH, and each other implemented transport appear as normal launch
choices; unimplemented transports remain absent.

A one-time command-palette hint may be considered only if discoverability
testing demonstrates a real problem, and must then remain dismissible,
nonmodal, and sourced from the actual platform binding. Startup/configuration
failures use the normal actionable application-error treatment rather than
diverting the user into a special first-run recovery flow.

### Launcher content

The launcher may contain:

- **Local shell** — platform default and saved local profiles.
- **SSH** — connect to a host or select a saved profile.
- **Serial** — open a local serial device with explicit line settings.
- **Recent sessions or profiles**.
- **Recent workspaces**.
- **Settings** and limited help/documentation access.

Unavailable categories may be omitted until implemented rather than shown as disabled clutter.

The launcher's fixed action rows form a compact keyboard-navigable list
(`app/festerm/src/screens.rs`'s `show_launcher`): Up/Down moves a highlighted
selection and Enter launches whichever option is currently highlighted,
without requiring the mouse. Local Shell has initial focus. Each row uses its
semantic icon, a short primary label, and one factual secondary line: **Local
Shell** / “Default shell on this computer,” **SSH** / “Connect to a remote
host,” and **Serial** / “Open a local serial device.” Saved profiles remain in
list order by default, but the Settings toggle may render them in a responsive
multi-column grid when width allows. When the selected local shell or profile
is reliably known, that identity may replace the generic Local Shell
secondary line.

The initial target launcher shows the usable choices Local Shell, SSH, and
Serial. A choice whose transport is not yet implemented remains absent from a
shipped build rather than appearing as a disabled promise. Empty
Recent, Profiles, and Workspaces sections remain absent until their underlying
models exist and contain real entries. Local Shell launches immediately; SSH
and Serial navigate within the same launcher tab to focused connection forms.
Back or Escape returns to the launcher without creating another chip. Escape
closes a Launcher opened from another session and restores that session, but
does nothing when Launcher is the window's only surface. Partially entered
non-secret connection fields live only for that Launcher lifetime.

The launcher uses no welcome copy, promotional cards, tips carousel, version
number, or decorative empty-state content. Its job is simply to select a real
session type.

The current implementation still expands a compact one-off password form
under the launcher choices. The approved target separates destination,
host-key verification, and authentication into the focused stages specified
in [SSH session creation](#ssh-session-creation). Saved profiles, agents,
key-file selection, and OpenSSH-config import controls appear only when their
capabilities are implemented and available, never as disabled placeholders.

Selecting SSH opens **Quick Connect** by default: a `user@host[:port]` field
and Connect button, plus an initially off **Durable remote session** control.
Enabling it exposes the shared provider (`tmux` or GNU screen), validated
session-name, and per-launch automatic-recovery controls, allowing an ad-hoc
connection to attach to or create a durable session without first saving a
profile. A "Show advanced settings" checkbox reveals the full destination and
authentication-method form while reusing those same durable-session controls
(`app/festerm/src/screens.rs`'s `show_ssh_quick_connect`/`show_ssh_form`).
The Launcher list, SSH connection surface, and profile editor use the same
surface-owned scrollbar behavior as Settings: content reserves a right-side
lane, the bar does not overlap controls, and ordinary content hover does not
reveal it.
Submitting with no password (from either surface) starts the session
immediately with no credential attached (`SshAuthentication::Interactive`)
rather than collecting one blind beforehand, mirroring `ssh`'s own ordering:
the worker connects and verifies the host key first (see
[Host-key verification](#host-key-verification)), and only once
authentication is actually needed does an in-terminal, pty-styled password
prompt appear (`user@host's password:`, `show_ssh_live_password_prompt`),
rendered in the bundled terminal font in place of the terminal view with no
textbox or button — typed characters are never echoed, matching real `ssh`,
and Enter submits. A rejected password appends "Permission denied, please try
again." plus a fresh prompt line below (matching `ssh`'s own
`NumberOfPasswordPrompts` default of three in-connection attempts,
`festerm_ssh::MAX_INTERACTIVE_PASSWORD_ATTEMPTS`) before falling back to the
ordinary failed-session presentation. This deliberately does not implement
server-driven keyboard-interactive prompts or 2FA; that remains a separate,
larger feature (see the SSH keyboard-interactive/2FA follow-up issue).

### Launcher as a tab

The launcher should use the same tab model as sessions.

Consequences:

- Closing the final session returns to the launcher.
- The New Session command opens the singleton launcher or focuses it when it
  already exists.
- Users can keep a launcher tab open while other sessions run.
- The application does not need a separate home-window architecture.
- Starting a session *from* the active launcher tab (e.g. "Local Shell") replaces that launcher tab in place with the new session tab, keeping the same position and chip identity, rather than leaving the spent launcher behind alongside a separate new tab (`app/festerm/src/tabs.rs`'s `start_local_session`). Starting a session while some other tab is active (e.g. via a keyboard shortcut) still opens a new tab rather than replacing whatever is currently active.

### New Tab behavior

The primary New Tab action should open the launcher rather than immediately creating one predetermined session type.

The semantic Start Local Shell action opens the default local profile directly.
It is initially unbound, remains available from the command palette, and may
become user-bindable when shortcut customization exists.

## Primary Layout

![Active terminal and integrated chrome target](images/gui-mockups/terminal-active.png)

![Optional compact single-line session chrome target](images/gui-mockups/compact-session-chrome.png)

The default window has three conceptual zones:

```text
+------------------------------------------------------+
| Integrated chrome: session chips + global actions   |
+------------------------------------------------------+
|                                                      |
| Terminal viewport or launcher content                |
|                                                      |
+------------------------------------------------------+
| Contextual status only when needed                   |
+------------------------------------------------------+
```

### Tab strip

The chip row provides session switching and identity. It is part of the upper application chrome, sharing the top-of-window band with New Tab and compact global controls. The chips remain visually independent through spacing, shape, border, and elevation—not by moving them onto a separate shelf.

### Main content

The main content is either a terminal viewport, launcher, or Settings page.
Per-session diagnostics live in the temporary session inspector unless a
future cross-session support workflow establishes a real need for a separate
application surface. Terminal tabs should maximize terminal area.

### Launcher lifecycle

Launcher is a singleton task surface and the window's stable empty state, not a
permanently pinned destination. New Session activates an existing Launcher chip
or creates one at the end of the chip row. Choosing a local shell, or completing
the SSH setup flow, converts that Launcher chip into the new session in place so
its position remains stable and no redundant Launcher is left behind. Multiple
Launcher chips and launcher pinning are not initial features.

Closing the final terminal session does not close the fesTerm window or quit the
application. If no other application surface remains, it returns the window to
Launcher. If Settings or another valid application surface is still open, that
surface remains visible and New Session remains available from the chrome.
Closing the final remaining application surface likewise returns to Launcher,
so an open window is never blank.

This keeps three lifecycles explicit and consistent across every invocation
path: **Close session** removes a session, **Close window** closes the native
window, and **Quit fesTerm** terminates the application using the aggregate
live-session confirmation rules. A future preference to close the window with
its last session may be evaluated through usability testing, but it is not the
default contract.

### Application chrome and session context

Global application actions and session-specific context are separate concerns.

- The upper application chrome owns compact global actions: New Tab/Launcher,
  command palette, session-inspector toggle, context-sensitive terminal search
  when space permits, and a deliberately small overflow menu.
- Global controls are icon-only, using the canonical first-party forms in [the icon system](icon-system.md), with accessible labels and hover text carrying their meaning. Painter-drawn implementations may remain while runtime SVG integration is incremental, but their geometry should converge on the canonical sources rather than creating a second icon vocabulary.
- The session chips live inside the upper application chrome, in the same top-of-window band as New Tab and compact global controls. They are independent because each lozenge has visible space around it—not because the row is separated from the chrome.
- The right-side session inspector is normally hidden and overlays rather than
  resizes the terminal viewport. It shows context for the active session:
  connection state, host/profile metadata, diagnostics, and relevant actions.
- Global Settings do not live in the inspector. Settings open as an application surface represented by their own chip, and remain reachable from the command palette and compact overflow menu.

The overflow menu should stay intentionally small. Growth into a general-purpose action list is a design smell; less common actions belong in the command palette or their relevant application surface.

For a normal terminal session, its initial contents are **Find in terminal** and
**Focus mode**, followed by a separator, **Settings**, and **Command palette**.
When narrow-window collapse hides a dedicated control, **Session inspector**
may also move into this menu. Find and Inspector should not appear in both the
visible icon controls and the overflow menu merely to duplicate access.

The menu is not a second session switcher and does not contain Copy, Paste,
Close session, Disconnect, Reconnect, host-trust, authentication, or appearance
toggles. Those actions remain in their owned surface, direct keyboard path, or
the command palette. Launcher and Settings omit terminal-only entries, and an
unavailable action is omitted rather than shown as unexplained disabled text.
Entries use text labels rather than another icon-only vocabulary. The current
chip-layout and status-bar toggles in the implementation are transitional;
they belong in Settings as the corresponding settings surface matures.

### Terminal context menu

![Terminal and session-chip context-menu targets](images/gui-mockups/context-menus.png)

The terminal viewport's local context menu is deliberately about the text and
target under the pointer, not application or session administration. An
explicit OSC 8 link contributes **Open link** and **Copy link**. A non-empty
terminal selection contributes **Copy**. A live session that currently accepts
input contributes **Paste**. **Find in terminal** remains available in every
retained terminal viewport, including an exited or disconnected read-only
history. Unavailable entries are omitted; in particular, Paste is absent for a
read-only history.

Clear, Close session, Disconnect, Reconnect, Settings, and appearance actions
do not belong in this menu. **Select all** is also omitted initially because it
can unexpectedly capture very large retained scrollback; it may be revisited
only if usability testing establishes a concrete need. Opening the menu does
not send terminal input or destroy the current selection.

When terminal mouse reporting is inactive, right-click opens the local context
menu. When an application in the terminal has mouse reporting enabled, ordinary
right-click remains available to that application and **Shift+right-click**
always invokes the local context menu. This is the same local-override principle
used for Shift-modified selection and ensures Copy and Paste remain reachable
without permanently taking mouse input away from terminal applications.

### Session-chip context menu and rename

A terminal session chip's context menu contains **Rename session**, applicable
**Move left** and **Move right** entries (omitted at the respective edge), a
separator, and **Close session**. The menu targets the clicked chip without
activating it. Close uses the same live-session consequence and confirmation
rules as every other close path. Reconnect, Disconnect, diagnostics, and trust
or authentication actions remain in the session inspector or relevant state
overlay rather than being duplicated here.

Application-surface chips such as Launcher and Settings do not offer Rename;
they expose only movement and closing actions that actually apply. Duplicate,
Pin, Save as Profile, per-chip color labels, and other organizational features
are omitted until they have a defined product behavior.

Double-clicking specifically on the stable primary session name starts an
in-place rename. The text field occupies the existing label region so entering
rename does not resize the row or chip. It selects the current user-controlled
name, **Enter** commits, and **Escape** cancels; an empty or whitespace-only
value does not replace the existing name. Moving focus elsewhere commits a
valid value and otherwise restores the previous name. The resulting display text is
sanitized and bounded by the same rules as other chip identity text. Rename
never edits the terminal-provided secondary title.

The name hit target consumes the double-click so it cannot become a custom
title-bar maximize gesture or terminal input. Since the first click follows the
normal chip activation behavior, double-clicking an inactive chip activates it
before entering rename. Committing or cancelling returns keyboard focus to that
session's terminal viewport. Rename remains available from the context menu for
discoverability and non-pointer access.

### Platform application menus

macOS uses its native application menu bar without adding another visible menu
row inside the fesTerm window. The implemented first slice is deliberately
capability-truthful:

- **fesTerm** contains Settings… (`Cmd+,`) and the standard native Services,
  Hide, Hide Others, Show All, and Quit entries.
- **File** contains New Session…, Start Local Shell, the active-surface close
  action (`Cmd+W`), and Close Window (`Cmd+Shift+W`). The close label names its
  result—Close Session, Close Launcher, or Close Settings—rather than pretending
  every surface is the same object.
- **Edit** contains focus-aware Copy and Paste on AppKit's responder chain.
- **View** contains Command Palette and the dynamically enabled Session
  Inspector action.
- **Window** contains native Minimize and Zoom.
- **Help** remains absent until fesTerm has real help content rather than an
  empty or placeholder destination.

Find in Terminal remains target behavior, not an inert menu placeholder. The
implemented About, Focus Mode, and zoom commands must still pass native menu
validation: confirm keyboard equivalents, dynamic Close/Inspector state, and
that responder-chain Copy and Paste preserve the same terminal selection and
paste-safety policy as the in-window routes.

Fixed native menu entries may remain visible but disabled when their context
does not apply, following platform menu stability conventions; their labels and
enabled state still come from the same semantic application-command model as
every other invocation surface. Trust decisions, authentication responses,
reconnect detail, and other owned workflow actions do not move into the menu
bar merely for completeness.

Windows and Linux do not gain a persistent in-window menu bar. Overflow,
command palette, shortcuts, and Settings provide those routes without consuming
terminal space. Custom decorations must preserve native system-window access,
including the expected `Alt+Space` system menu on Windows, alongside snapping,
keyboard movement/resizing, and accessibility work tracked in issue #29.

### About and updates

![About fesTerm dialog target](images/gui-mockups/about-festerm.png)

**About fesTerm** is a compact native-style dialog containing the same branded
fesTerm application icon used by the native window, dock, and task switcher,
`fesTerm`, exact application version, the description “A compact local,
SSH, and serial terminal.”, the explicit disclosure “Entirely AI-written with
human guidance.” plus concise GitHub Copilot/project-owner attribution,
**Copy Version Information**, **Licenses**, the canonical project/source link,
and Close. It does not include live session counts, decorative slogans,
donation/promotional content, or an update control without an implemented
update capability.

Copy Version Information produces a bounded support-oriented summary: exact
version, build/commit identifier when available, OS and architecture, and
relevant renderer/backend versions, plus the same bounded authorship summary.
It excludes usernames, hostnames, filesystem paths, serial-device identifiers,
environment values, settings, profiles, workspace names, terminal content,
and session diagnostics. Licenses shows the repository license and bundled
dependency/asset notices, including the exact Inter font license once the font
is shipped.

**Check for Updates** is present only in packaged builds carrying the updater
verification public key. It contacts the fixed public fesTerm GitHub Releases
`latest/download/festerm-update.json` endpoint only after the user presses the
button; it sends no profile, session, terminal, device, or configuration data.
Checking, verified download, and installation are separate user actions.
Package-manager installations may report availability but defer installation
to their package manager. Release notes never interrupt startup. Update state
is quiet, factual, dismissible, and cannot block the Launcher or a terminal
session. Developer and incompletely signed builds expose no network action.

### Deferred native Markdown viewing

Native Markdown viewing is pinned for an explicit product-design discussion
only after Local, SSH, and Serial capabilities and their UI components are
complete. Markdown is unusually valuable in AI-assisted development and
technical terminal workflows, but that motivation does not yet establish that
fesTerm should own a document renderer. No viewer control, disabled menu item,
file association, or implementation issue should be added before the gate.

The review must first answer **whether** native viewing belongs in a restrained
terminal workstation. If it does, it must then choose and name one primary
quality target:

- **Readability-first:** dependable structure, typography, code blocks,
  tables, links, search, selection, and Copy without claiming browser- or
  editor-identical layout.
- **Visual-fidelity-oriented:** a broader, precisely specified Markdown and
  styling model with deterministic rendering/reference evidence and the
  additional complexity that entails.

A vague promise to “render Markdown accurately” is not sufficient. The design
review must bound at least:

- Markdown dialect and extensions: CommonMark/GFM, tables, task lists,
  footnotes, syntax highlighting, math, diagrams, embedded HTML, and any
  unsupported construct treatment;
- document ownership and entry routes: explicitly selected local files, files
  referenced by local terminal output, and SSH-remote files. A remote path is
  never treated as a local path, and Serial has no implied filesystem;
- resource policy: scripts never execute, raw HTML is disabled or sanitized by
  an explicit contract, and local files, remote images, network URLs, data
  URLs, fonts, and includes do not load implicitly;
- privacy and trust: opening a document cannot silently create network traffic,
  expose a remote path locally, upload content, or place document data in
  diagnostics, workspace metadata, or logs;
- bounded behavior: file/line/image/resource limits, large-document
  responsiveness, cancellation, error recovery, encoding, and binary-file
  rejection;
- navigation and accessibility: headings, landmarks, link destinations,
  tables, code blocks, keyboard traversal, screen-reader semantics, text
  selection, Copy, Find, source/rendered switching, and scroll restoration;
- freshness and lifecycle: read-only versus editing, manual reload versus file
  watching, conflict/deletion behavior, SSH disconnect handling, and whether a
  viewer is a singleton task surface or one surface per document; and
- product boundary: whether it remains a read-only companion surface rather
  than expanding into an editor, preview server, web browser, notebook, or
  general IDE.

Any accepted viewer must follow the existing application-surface model: it
cannot resize or take ownership of a live terminal session, terminal escape
sequences cannot open it without an explicit user action, Escape/Close returns
to a known prior surface, and closing the final viewer returns to Launcher.
Workspace persistence, recent-document history, file watching, remote fetch,
and external-resource loading remain absent unless separately designed.

### Contextual status region

The bottom status region should normally be absent. It may appear for actionable or transitional states such as:

- reconnecting;
- authentication required;
- host-key decision required;
- session startup failure;
- transport failure; or
- an available diagnostic detail.

It should not display continuous byte counts, queue metrics, dimensions, or frame statistics during normal use.

### Bottom status bar

Distinct from the contextual status region above, a persistent 24 px bottom
status bar (`crates/festerm-ui-egui/src/statusbar.rs`) runs along the bottom of
the window by default and remains user-configurable on/off
(`AppCommand::ToggleStatusBar`). It earns that permanent space by remaining
strictly factual and nonduplicative:

```text
| 80×25   Local · Linux                                      ● Running |
| 80×25   Remote · Linux                                   ● Connected |
| 80×25   Serial · COM3                                         ● Open |
| 80×25   Local · macOS   cargo test — fesTerm                ● Running |
```

The fourth row is the compact-chip variant; the first three show the default
two-line-chip mode.

- The left side shows actual measured terminal grid dimensions and locality.
- The platform is concrete when reliably known: `Windows`, `Linux`, or
  `macOS`. A remote session shows `Remote` alone until its host platform is
  reliably detected; it is never inferred from shell syntax or line endings.
- The right side shows the session state using a colored indicator plus an
  accessible text label. Local sessions use `Running`, SSH sessions use
  `Connected`, and serial sessions use `Open`; each term describes the fact
  the owning transport can establish without implying a responsive peer.
- Stable session identity and terminal title remain in the active chip and are
  not repeated in the bar while session details are shown in chips. When the
  **Show session details in chips** preference is off, the active session's
  same sanitized secondary display value moves into the status bar instead;
  this is the only intentional identity/title exception.
- Client clock/date, shell version, encoding, line-ending convention, command
  timing, and last-input/output timestamps are absent.

The implementation follows this content contract. Terminal ANSI/indexed and
explicit RGB colors remain unrelated to these application status roles.

When Launcher or Settings is active, preserve the same 24 px footer geometry
but leave its content empty. The stable footprint prevents vertical layout
jumps when switching surface types; it does not justify invented `Ready`, app
identity, time, or dimensions. The subtle top rule may remain. Disabling the
status-bar preference removes the footer consistently on every surface.

The relocated compact-mode detail sits after dimensions/locality and before
the right-aligned state. It uses `text.muted`, never scrolls or animates, and
ellipsizes before dimensions, session type, or lifecycle state are removed.
Its complete sanitized value remains available to accessibility and hover
presentation. It follows the same source precedence as chip secondary text:
terminal-provided title first, then factual launch metadata. It is never named
or inferred as the “current command,” “last command,” or “last response.” Only
the active session contributes this value; Launcher and Settings continue to
have empty footer content.

If both session details and the status bar are disabled, fesTerm does not force
either surface back on. The secondary value remains available through command
palette session results, chip hover/accessibility, and Session Inspector. This
is deliberate user-selected density, not loss of stable session identity.

The terminal viewport itself (`crates/festerm-ui-egui/src/view.rs`) no longer draws its own inline "fesTerm / Diagnostics" header or expandable per-frame diagnostics footer — that duplicated chrome the application already owns and only its grid-dimensions field was in regular use, which now lives in the status bar instead. `TerminalView::show` fills all available space with just the terminal grid; `TerminalView::diagnostics()` remains available for tests and future tooling that need the raw per-frame `FrameDiagnostics`.

## Tab Model

### Independent session chips

Session tabs are independent lozenges or chips, not connected browser-style or rolodex tabs. This is an interaction principle, not merely a corner-radius choice: every chip represents a persistent, independently managed session object.

Implementation requirements:

- Place the chip row inside the top window-chrome band; terminal, launcher, or settings content begins immediately below that integrated chrome.
- Preserve visible space around every chip; adjacent chips must never merge into a continuous tab strip.
- Keep chip surfaces neutral. Connection color belongs in a small status indicator, not across the whole chip.
- Indicate the active chip with a slightly brighter surface, stronger border, subtle elevation, and optionally a small height difference. Do not depend on a saturated accent fill.
- Keep stable session identity as the primary label and transient terminal state as secondary metadata.
- Make Launcher and non-session application surfaces such as Settings visually distinct while keeping them in the same switching model.
- Drag-and-drop reorders independent session objects and should preserve their identity and state.
- Design chip layout for both horizontal overflow and an optional multi-row wrapping mode. Wrapping must remain user-configurable because some users will prefer a single scrolling row.
- Preserve semantic tab roles, keyboard order, accessible names, focus indication, and close behavior even when the visual treatment is chip-like.

The wireframe in [Canonical Wireframe](#canonical-wireframe) is the visual contract for these requirements. A detached chip shelf below a separate title bar, connected browser tabs, file-folder tabs, full-chip status colors, and constantly changing primary labels are non-conforming implementations.

### Window chrome (integrated title-bar controls)

The chip row and global actions occupy one integrated top band rather than a
detached shelf. On Windows and Linux, the window disables native decorations
(`app/festerm/src/main.rs`, `ViewportBuilder::with_decorations(false)`) and
renders custom minimize/maximize/close controls in that band. On macOS,
eframe's full-size content view keeps the native close/minimize/zoom traffic
lights on the left over the integrated chip band; the application reserves
their hit-test area, offsets them to the chip row's centered baseline, and
does not render duplicate right-side controls.
Implementation (`crates/festerm-ui-egui/src/chrome.rs::show`):

- On Windows and Linux, the trailing icon block (right-to-left: close, maximize/restore, minimize, overflow menu, panel toggle, command palette) is painted in the same row as the chips through the semantic first-party [`Icon`](icon-system.md) layer. Each window-control icon calls `ui.ctx().send_viewport_cmd(ViewportCommand::Close/Maximized/Minimized)` directly rather than going through `ChromeAction`/`AppCommand`, since these are OS-window-level actions with no application-state implications.
- The maximize/restore icon reads the viewport's real current state (`ui.input(|i| i.viewport().maximized)`) and paints a single square (maximize) or two overlapping squares (restore) accordingly, so the icon's own shape communicates state rather than a text label.
- A background drag-to-move region is registered across the row's own compact content band (not the full remaining panel height, which - before any content is laid out - would otherwise swallow pointer events meant for the terminal view painted below) *before* the chips/icons are added, so those widgets' own click handling still takes priority over this catch-all background sense wherever they visually sit on top of it. Starting a primary-button drag on that background sends `ViewportCommand::StartDrag`; double-clicking it toggles `ViewportCommand::Maximized`.
- `TRAILING_CONTROLS_RESERVED_WIDTH` accounts for platform-specific trailing icons so the chip row never overlaps them, extending the existing narrow-window overlap fix.

### Tab anatomy

A session tab conceptually contains:

```text
[session icon + status badge] [primary identity] [close]
                             [secondary title or state]
```

Not every element must be visible at every density. The primary identity and active state take precedence.

The semantic session-type icon renders at 16 px. A separate 6–7 px corner
badge carries lifecycle/connection state without consuming another horizontal
slot or tinting the icon or chip. The icon identifies durable type; the badge
identifies transient state. The accessible chip name exposes both. The close
control appears only on the active chip.

### Identity precedence

The primary tab label should follow this order:

1. User-assigned tab name.
2. Profile or connection name.
3. Remote host alias for SSH.
4. Local shell name or session type.
5. Generic fallback such as `Local Shell` or `SSH Session`.

The terminal-provided dynamic title must not replace the primary identity.

### Dynamic terminal title

A terminal-provided title may appear as secondary metadata when space permits, for example:

```text
production-db — nvim server.rs
```

The stable identity remains first. In compact layouts, the secondary title should be omitted before truncating the primary identity beyond usefulness.

Inactive chips retain their secondary line when space permits because it helps
distinguish similar sessions, but it remains visually quiet and disappears
before stable identity, session type, or state. The secondary line follows
this precedence:

1. Actionable exception: host-key verification, authentication required,
   disconnected, or failed to start.
2. Transitional state: starting or reconnecting.
3. Completed state: exited, including a known exit code.
4. Otherwise, the sanitized terminal-provided title when nonempty and
   meaningfully different from the primary identity.

Terminal titles are untrusted protocol output. Strip control characters,
normalize to one line, bound retained/displayed length, and coalesce rapid
changes. A title cannot replace stable identity, impersonate state, or
automatically become a profile/workspace name, notification, window title, or
diagnostic field. A compact chip may reduce a path-like title to its useful
final component while the inspector exposes the bounded sanitized value.

Working directory requires an explicitly supported metadata protocol such as
OSC 7 with strict URI and host validation; never parse prompts, titles, or
process-list guesses. A local file URI may identify a client path. An SSH path
remains remote metadata and is never opened through the local OS. Runtime
directories are private in screenshots, notifications, copied diagnostics,
and workspace metadata and are not persisted as though they were a profile's
reliable initial directory.

### Session-type identity

Icons may help distinguish local shell, SSH, serial, launcher, settings, or
other future session types. Icons should aid recognition rather than decorate
every control.

The repository-owned source set and its semantic Rust names are defined in [Icon System](icon-system.md). Assets are monochrome and receive semantic color from UI code; status remains a separate accessible indicator instead of tinting the entire session icon or chip.

Remote operating-system icons may be shown when reliable metadata is available, but the UI must always use `SshRemote` as a coherent generic remote-host identity. OS detection must not be required, and branded OS logos are outside the first-party set.

### Icon systems and usage

fesTerm has three related but deliberately distinct icon treatments:

| System | Source and use | Color and sizing contract |
| --- | --- | --- |
| Application icon | The branded launch asset in [`assets/app-icon`](../assets/app-icon), used by the executable, installer, dock, taskbar, application switcher, and store/package metadata. The `AppMark` may also identify fesTerm in an About surface. | A fixed graphite, white, and cyan composition with platform-sized raster exports. It is not recolored by the application theme and is not used as a generic terminal or command icon. |
| Semantic UI icons | The canonical first-party SVGs in [`assets/icons/source`](../assets/icons/source), requested by semantic Rust name rather than file path. They identify session types, actions, settings categories, connection/trust concepts, and window controls. | Monochrome `currentColor`, drawn on a 24 px source grid with a nominal 1.75 px stroke. Render at 16 px for compact chrome and 20 px where the surface has more room, normally inside a hit target of at least 24 × 24 logical px. |
| Status indicators | A compact badge on the semantic session icon plus an accessible state label in chips; a small dot plus label in the status bar. Warning, error, authentication, and host-key icons may supplement a larger message where their shapes add meaning. | State colors come from the semantic `status.*` roles. Badges/dots remain separate from the neutral session-type icon and never tint the whole chip. Shape, text, or an accessible name must carry the state when color cannot. |

Painter-drawn controls in the current egui chrome are a rendering technique,
not a fourth icon set. They may migrate incrementally to the semantic SVG asset
layer, but their geometry and naming must converge on the canonical source
set. Platform-native controls remain native: macOS uses its traffic lights;
Windows and Linux use fesTerm's `Minimize`, `Maximize`/`Restore`, and `Close`
forms.

Use the semantic name that describes the action or object, never the icon's
location or construction. The main mappings are:

| Surface | Semantic icons |
| --- | --- |
| Session chips and launcher | `LocalTerminal`, `SshRemote`, `Serial`, `NewSession`, `Settings`, `Workspace`, `Profile` |
| Upper chrome | `NewSession`, `CommandPalette`, `SessionInspector`, `Overflow`; reserve `Search` for literal search/filter UI rather than the command-palette trigger |
| Window controls | `Close`, `Minimize`, and state-dependent `Maximize` or `Restore` on Windows/Linux; native traffic lights on macOS |
| Connection and trust UI | `Reconnect`, `Disconnect`, `AuthRequired`, `HostKeyVerification`, `Warning`, `Error` |
| Terminal and diagnostics actions | `Copy`, `Paste`, `Clear`, `Diagnostics` |
| Settings categories | `KeyboardShortcuts`, `ThemeAppearance`, `TypographyFont`, `SecretStorage` |

An icon-only action owns a localized accessible label and matching hover text;
the SVG itself carries no fixed title because its meaning depends on the use
site. Decorative icons next to complete visible text are hidden from the
accessibility tree. See [Icon System](icon-system.md) for the full inventory,
source rules, validation pipeline, and proposed Rust-facing `Icon` API.

### Connection states

The visual state vocabulary should include at least:

- connected, running, or open as appropriate to the transport;
- starting;
- reconnecting;
- disconnected;
- authentication required;
- failed; and
- exited.

State indicators should be compact, accessible, and not rely on color alone.

### Active and inactive tabs

The active tab must remain immediately distinguishable in the shipped dark
theme and every future validated theme variant. Inactive tabs should be
readable without competing visually with the active terminal. The application
defaults to the blue-graphite semantic palette in
`crates/festerm-ui-egui/src/theme.rs`, applied to egui in
`app/festerm/src/app.rs`. Chrome uses those same roles explicitly rather than
asking generic widget-state helpers such as `strong_text_color()` to decide
static chip identity; those helpers describe momentary button interaction, not
the chip hierarchy.

- `CHIP_ACTIVE_OUTLINE` maps to `border.active`; the active chip and its close control share this cool light outline.
- `CHIP_INACTIVE_OUTLINE` maps to `border.subtle`; inactive chips, the New Session control, and drag preview share it.
- `CHIP_ACTIVE_FILL` maps to `surface.tab.active`, a restrained step above the inactive chip and shared window well. The active outline remains the stronger selection cue. The same quiet fill is the hover treatment for inactive chips and New Session.
- `CHIP_INACTIVE_FILL` maps to `surface.tab.inactive`.
- `CHIP_PRIMARY_TEXT` and `CHIP_SECONDARY_TEXT` map to `text.primary` and `text.secondary`.
- `CHROME_ICON_COLOR` / `CHROME_ICON_COLOR_HOVERED` map to `text.secondary` / `text.primary`.
- `CHROME_CLOSE_HOVER` maps to `status.error`.

All chips sit on top of the terminal viewport's own near-black chrome-band
background (`crates/festerm-ui-egui/src/lib.rs`'s `DEFAULT_BACKGROUND`, mapped
to `surface.terminal`); against that shared dark surface, the
active/selected chip is indicated by both its brighter `CHIP_ACTIVE_OUTLINE`
border *and* its own lighter `CHIP_ACTIVE_FILL` fill — not by outline alone,
and not by a saturated accent color or by highlighting the label text.
Hovering an *inactive* chip, or the `+` new-chip button, previews that same
`CHIP_ACTIVE_FILL` fill without changing its outline color at all: the
outline stays fixed (`CHIP_INACTIVE_OUTLINE`) regardless of hover state, so
hover and selection read as two different, non-conflicting signals — a
consistent hover language shared by every chip-shaped control in the row
(`paint_chip`'s `hovered` parameter and `paint_new_chip_button`, both in
`crates/festerm-ui-egui/src/chrome.rs`).

The top chrome band and terminal well intentionally share this exact surface
color. There is no additional color border or separator between them. The
chrome consumes only its 8px top inset plus the 34px chip row; the terminal's
own 8px top margin is the single shared breathing space below the chips. This
coalesces what would otherwise be two adjacent 8px gaps and reduces the real
top stack from 50px to 42px without moving the terminal grid toward the chips.

Every chip, active or not, is drawn with a visible border: the active chip's border is `CHIP_ACTIVE_OUTLINE` (1.5px) and inactive chips use `CHIP_INACTIVE_OUTLINE` (1.0px), so the distinction reads clearly without color being the only cue. Each chip is two lines and left-aligned. The approved target places a 16 px semantic session icon with its compact state badge before the stable identity; the second, smaller and muted line holds state text or terminal-provided metadata, indented under the primary label. The current implementation still reserves that leading strip for a standalone 8 px status dot and should migrate to the icon-plus-badge arrangement after semantic icon rendering lands. The close control, shown only on the active chip (never on inactive chips, even on hover), is positioned from the chip's own outer corner — evenly inset from the top and right edges — rather than flowing through the label's layout, so its position stays fixed and the label never shifts to accommodate it. Opening a new Launcher tab is done exclusively through the compact icon-only "New tab" control at the end of the chip row (`AGENTS.md`: no duplicate widget-specific copies of the same operation) — an earlier full chip-styled "+ Launcher" button duplicated this control and was removed as redundant.

Chips without a secondary line (an open Launcher-type tab, which has no secondary terminal metadata) claim the chip's full footprint (`ui.set_min_size(outer_rect.size())`) before laying out their top-down content, rather than letting the shrink-wrapped one-line content block get vertically centered by the surrounding chip-row layout; without this, a one-line chip would render with visibly larger top/bottom padding than its two-line neighbors even though every chip in the row shares the same height (`CHIP_HEIGHT_FULL` when session details are shown, or the shorter `CHIP_HEIGHT_COMPACT` once that preference is off - see "Show session details in chips").

The chip row is scoped to its own narrower top-aligned sub-layout (`Layout::left_to_right(Align::Min)`), rather than the *whole* top-level chrome row switching to `Align::Min`: an `Align::Min` layout at the very top level was tried and reverted, because it handed the full remaining panel height down to the trailing icon controls' own nested `Align::Center` sub-layout, centering the icons roughly mid-window instead of near the top and (in the real app, where the terminal view is painted immediately after the chrome row in the same `Ui`) starving the terminal view of any remaining vertical space. Only the outermost row keeps plain `Align::Center` (`ui.horizontal`); scoping `Align::Min` to just the chip-row sub-block fixes its top-alignment without that regression (`crates/festerm-ui-egui/src/chrome.rs`'s `chrome_row_stays_a_compact_band_even_with_a_tall_available_area` test guards this).

Until that migration, the current status dot is allocated at the primary label's own text-line height (via `Ui::text_style_height`), not just its own diameter, so the row's `Align::Center` computes one shared center line for both the dot and the label text. The primary row has a one-pixel optical downward adjustment. That pixel is taken from the gap below the primary row, so the smaller secondary line retains its existing position.

### Tab overflow and wrapping

The default is one horizontally scrollable row. This preserves terminal height
and resolves the default preference tracked in issue #24. Optional wrapping
remains available for users who value simultaneous session visibility more
than terminal height.

In single-row mode, chips first compact (shrink proportionally toward a
minimum width, mirroring a browser tab strip) as the row gets crowded;
horizontal scrolling only takes over once chips have compacted as far as
they will go and still don't fit (`crates/festerm-ui-egui/src/chrome.rs`'s
`shrink_to_fit_single_row`).

The design must remain usable with many sessions. The implementation should support:

- compact chip width and sensible truncation;
- a default single-row mode that first shrinks chips to fit, then falls back to horizontal scrolling;
- an optional multi-row wrapped-chip mode for wide displays;
- keyboard switching that follows a predictable logical order independent of visual wrapping;
- a searchable session switcher keyed primarily by stable identity; and
- graceful narrow-window collapse without merging chips into a connected strip.

Chips clip overflowing text with an ellipsis rather than growing without bound: both the primary and secondary lines truncate to a fixed chip-width range instead of forcing the whole row layout to widen to fit a long terminal-provided title (for example, a full shell executable path). Where a terminal-provided title looks path-like, the chip's secondary text shows only its final path component (e.g. `cmd.exe`) rather than the full path, so the identity-first chip stays compact; this is purely a display reduction and never mutates the underlying terminal-provided title data.

The chip row and the trailing global icon controls (search, panel toggle, overflow menu) share one horizontal row, so the chip row's own wrap/scroll width budget is capped to leave room for those icons (`TRAILING_CONTROLS_RESERVED_WIDTH` in `crates/festerm-ui-egui/src/chrome.rs`) rather than being computed as if it owned the full row width. This prevents the last wrapped or scrolled chip from rendering underneath the icons on narrow windows.

Activating, creating, restoring, or keyboard-switching to a session scrolls
its chip fully into view. Overflow affordances appear only when needed; wheel
and trackpad input over the row may scroll it horizontally, and dragging near
an edge auto-scrolls during reorder. Overflow menus contain application
actions rather than a duplicate hidden-tab list; the command palette remains
the searchable session switcher.

Chip reduction order is secondary-line removal, reduction toward minimum chip
width, then stable-identity ellipsis. The type icon, state badge, and entire
active identity are never removed merely to retain optional chrome actions.
New Session and platform-required window controls remain visible; Search and
Session Inspector collapse into overflow first. Full text remains available
through accessible naming and hover help. Duplicate visible names are allowed
and are never silently given fabricated numeric suffixes.

Wrapping adds complete 34 px rows plus normal spacing and genuinely resizes
the terminal viewport; it never floats over terminal content. That resize is
acceptable because wrapping is an explicit preference.


## Session Creation Workflow

### Local session

The launcher should allow users to:

- open the platform default local profile;
- choose a saved local profile; and
- access additional options without requiring them for the common path.

Create the chip and viewport immediately in `Starting` state. Fast startup
needs no modal spinner; after a short visual threshold, a restrained
`Starting local session…` message with Cancel may overlay the viewport. Output
received during startup renders immediately, switching tabs does not cancel,
and Cancel uses the bounded backend lifecycle.

Failure before a process lifecycle is established is `Failed`, keeps stable
identity, and offers Details plus Close Session without leaking executable
paths or raw OS errors in the concise message. A successfully started process
that later ends is `Exited` regardless of exit code. Show a known code as
secondary state, but do not call nonzero exit a fesTerm failure. Preserve the
approved read-only history, never auto-close, and expose Relaunch only when a
real command can create a fresh generation.

### SSH session creation

![SSH destination-entry target](images/gui-mockups/ssh-destination.png)

SSH is a first-class session type, not a local shell command presented as an
application feature. A one-off connection proceeds through focused stages in
the same launcher tab:

```text
Destination -> Host-key verification, when required
            -> Authentication, when required
            -> Running terminal
```

The Destination screen collects only host, port, and username. Host and
username are required unless supplied by a real profile; port defaults to 22
but remains explicit and editable. Terminal dimensions and `TERM` do not
belong in this form because the viewport and session layer own them. Continue
starts connection work asynchronously; validation stays beside the relevant
field, and Back returns to the launcher without creating another chip.

Authentication credentials are requested only after host identity has been
accepted. This avoids retaining a password while the user decides whether to
trust an unknown host and leaves room for future agent, key, and
keyboard-interactive methods without redesigning destination entry.

Once connected, SSH differs from a local terminal only where useful: it uses
the generic `SshRemote` icon, `Connected` state language, and `Remote` locality.
Username, host, port, cipher, latency, and key algorithm belong in the session
inspector rather than permanent chrome. A user rename changes only stable
display identity, never the destination or profile.

### Serial session creation

![Serial connection form target](images/gui-mockups/serial-connection.png)

Serial is a first-class session transport, not a local-shell option and not an
SSH variant. Selecting it moves the same Launcher chip to one focused form:

```text
Serial device
  Device       [ discovered device or explicit system identifier ]
  Baud rate    [ 115200 ]
  Data bits    [ 8 ]
  Parity       [ None ]
  Stop bits    [ 1 ]
  Flow control [ None ]

  Back                                      Open
```

Device is required. The picker refreshes discoverable ports and may show a
friendly OS-supplied description, but it always exposes the exact system
identifier (`COM3`, `/dev/ttyUSB0`, `/dev/cu.*`, or the platform equivalent)
needed to distinguish devices. An explicit identifier remains possible when
enumeration is incomplete. The initial line defaults are 115200 baud, 8 data
bits, no parity, 1 stop bit, and no flow control; they are editable choices,
not claims about the attached hardware. Baud accepts supported common values
and a validated positive custom value. Data bits, parity, stop bits, and
software/hardware flow-control choices appear only where the backend supports
them.

Open creates the chip and terminal viewport in an `Opening` state and acquires
the device asynchronously. A busy, missing, unsupported, or permission-denied
device produces a concise failure state with Details, Back/Edit Settings, and
Close Session as applicable; raw OS errors and sensitive paths remain in the
bounded diagnostic detail rather than the primary message. No probe bytes are
sent merely to identify the device.

An open serial session uses the semantic `Serial` icon and `Open` state
language—never `Connected`, which would imply a handshake or responsive peer
that a byte-stream port cannot prove. The default stable identity may use the
exact device identifier or a reliable friendly device name. Port identifier,
line settings, and any reliable hardware metadata belong in the session
inspector rather than permanent chip text. The status bar may show a sourced
`Serial · <device>` locality and `Open` state when space permits.

Serial input, output, selection, Copy, Paste, Find, scrollback, exit handling,
and read-only retained history follow the same terminal contracts as other
sessions. Terminal grid resizing remains a local renderer fact; fesTerm does
not claim to notify a serial peer of rows or columns. Closing releases the port
through the bounded session lifecycle. Reopen is offered only when the backend
can open the same configured identifier again, and never claims that a reused
path names the same physical hardware without a reliable stable identity.

Local echo, newline translation, character pacing, break signaling, modem-line
controls, logging, file transfer, and device flashing are not initial launcher
options. They require separate explicit behavior rather than silently changing
the byte stream.

### Host-key verification

![Host-key verification target](images/gui-mockups/host-key-verification.png)

Host-key verification is an in-tab decision that blocks only the affected
connection, rendered pty-style (bundled terminal font, terminal background)
in place of the terminal view, mimicking `ssh`'s own prompt text rather than
a dialog. It shows the canonical `host:port`, key algorithm, and full SHA-256
fingerprint. It does not claim that the host is safe; it asks the user to
confirm the fingerprint through a trusted source.

For a previously unknown host, the implemented surface
(`app/festerm/src/app.rs`'s `show_host_key_prompt_ui`) shows `ssh`'s own
`Are you sure you want to continue connecting (yes/no)? [y/N] ` line with a
blinking terminal cursor and captures `y`/`n`/`a`/Escape directly from the
keyboard — no Reject/Accept Once buttons, matching the "feel" of answering a
real terminal prompt. `y` accepts the connection for this attempt only;
`a` accepts it and persists the fingerprint non-secretly in configuration
(ADR 0020), so a future connection to this exact host:port skips the prompt
entirely, mirroring `ssh`'s own `known_hosts`; `n`/Escape rejects it and
returns to destination entry with non-secret fields retained.

A changed previously trusted key (`show_changed_host_key_prompt_ui`) is a
separate high-severity state showing both the previously trusted and
newly presented fingerprints. It never offers an ordinary Accept Once: the
only way to proceed is to type the literal word `yes` and press Enter, which
replaces the stored trust record and continues; Escape, or Enter with
anything else typed, cancels and returns to destination entry. Typed
characters are echoed here (unlike the password prompt) since they are not
secret, and this is a deliberate, visible act. Replacing established trust
always requires this explicit typed override; there is no plain
Accept-Once path for a changed key.
Switching tabs remains allowed while either decision is pending; closing the
chip cancels the attempt. Keyboard focus begins on the safe rejection/cancel
action rather than acceptance.

### Authentication

![SSH authentication target](images/gui-mockups/ssh-authentication.png)

Authentication remains in the same connection tab and displays the confirmed
`username@host` target. It shows only methods that are genuinely available.
For password authentication:

- the password is masked, transient, and cleared after every submission,
  successful or failed;
- it never enters a profile, workspace, log, diagnostic, or ordinary config;
- Connect is unavailable when the field is empty, and Enter submits;
- Escape clears a nonempty password before a subsequent Escape returns to
  destination entry;
- an in-progress attempt exposes Cancel and does not accept duplicate submits;
- failure remains on this surface with a concise correction prompt; and
- an existing saved SSH profile may offer an explicit **Remember this password
  in native secure storage** control and a separate **Use stored password**
  action. One-off forms remain transient; private keys, passphrases, agents,
  key files, and host trust are never included in this control.

### Profile editing

![Profile editor target](images/gui-mockups/profile-editor.png)

Profile creation and editing is separate from the immediate launch path. The
common case requires one selection or a small amount of connection information,
not completion of a large settings form.

A profile is a persisted, reusable launch definition, never a running session
or terminal-history container. Its name becomes the default stable chip
identity. Renaming a running chip does not rename its profile; editing a
profile does not mutate existing sessions. Launching uses a validated snapshot
of the current definition.

Local profiles contain name, executable, arguments, initial directory, and an
initially-off durable local-session option. Enabling it selects the local
`festerm-sessiond`, `tmux`, or GNU Screen provider plus a validated session
name; it affects only future launches of that saved profile. The built-in
**Local Shell** has no persistence control and always starts a fresh plain
shell unless the user later resumes an already-running `festerm-sessiond`
session from the Launcher. SSH profiles contain name, host, port, username,
stored-password/private-key reference state, and an authentication
preference. Serial profiles contain name, exact device
identifier, baud rate, data bits, parity, stop bits, and flow control. Profiles
never contain passwords, private-key contents, terminal output, scrollback, or
diagnostics. Authentication may
reference a supported agent, key location, or secure-storage record without
storing the secret itself. Only available authentication choices appear.
Terminal dimensions and inferred remote platform do not belong in profiles.

Multi-field edits are staged behind Save; Cancel discards them and validation
stays beside the relevant field. Delete requires confirmation and reports
workspace references. Duplicate creates a distinct profile with an editable
name. OpenSSH import, if implemented, is a separate explicit operation and
does not imply continued synchronization. Recent one-off destinations are not
silently converted into profiles.

A standalone **Profiles** surface (a singleton chip, opened from the chip-row
overflow menu's "Open Profiles" button or the command palette's "Open
Profiles" entry, mirroring Settings) lists every saved profile with its kind
and a short description, plus "New Local Profile" and "New SSH Profile"
buttons, plus "New Serial Profile" when serial support is available. Each row
offers Edit, Duplicate, and Delete. Local editing currently covers
name/executable/arguments/working directory and explicit local
`festerm-sessiond`/tmux/screen persistence; SSH editing currently covers
name/host/port/username, stored password/private-key replacement and removal,
and tmux/screen durable-session settings. SSH-agent, key-file-path, and
OpenSSH-import UI remain future work. A profile's name is also its
identifier, so it is validated as kebab-case (lowercase letters, digits,
hyphens); an invalid or empty name/executable surfaces a single inline
validation message rather than field-specific errors. Delete asks for
confirmation and, when the profile is still referenced by a saved workspace
tab, reports the reference count and blocks the deletion (the same
`UnknownWorkspaceProfileReference` validation workspaces already enforce)
until the tab is removed or repointed. Rows can be dragged to reorder them;
the new order persists to configuration immediately and the Launcher's saved
profiles follow the same order.

## Settings

![Settings surface target](images/gui-mockups/settings.png)

Settings is a singleton application surface represented by its own chip;
invoking Settings again focuses it. It currently uses compact cards rather
than a category sidebar: **Interface**, **Scrolling**, **Terminal
typography**, and **Keyboard**. Only implemented controls appear.

The current **Interface** card groups the persistent chrome/session toggles as
quiet rows with subtle dividers:

- **Session chip layout** is an exposed two-choice control: **Single scrolling
  row** (the default, with the explanation that it keeps terminal height
  stable) or **Wrap to multiple rows** (which exposes more sessions but may
  reduce terminal rows).
- **Show session details in chips** is an on/off switch, on by default. It shows
  the sanitized terminal title or factual launch context under stable session
  identity. Turning it off makes every session, Launcher, and Settings chip a
  compact single-line chip. When the status bar is visible, only the active
  session's secondary value is shown there under the status-bar rules above.
- **Show status bar** is an on/off switch, on by default, described factually as
  displaying sourced session state, terminal dimensions, and the active
  session detail when compact chips require that relocation.
- **Confirm before closing live sessions** is an on/off switch, on by default.
  It applies to every live-session close route through the central application
  policy. Turning it off closes immediately; it does not alter bounded backend
  shutdown or the separate aggregate window/quit policy.
- **Workspace restore** is an on/off switch, off by default (see
  "Configuration" below): unlike the four controls above, which always apply
  and save immediately, resurrecting a previous run's open tabs is an
  explicit opt-in.
- **Compact multi-column New Session list** is an off-by-default switch that
  lets saved profiles render in a responsive grid when width allows, while the
  original single-column list remains the baseline.
- **Pulse status dot on new background output** is an off-by-default switch
  that animates only background-session chip dots when unseen output arrives.
- **Resume unattached local sessions from New Session** is an off-by-default
  switch that surfaces locally running, unattached `festerm-sessiond`
  sessions as one-click Resume entries in the Launcher.

The separate **Scrolling** card currently exposes a five-step **Scroll
speed** clickstop (`Very slow` through `Very fast`) that scales wheel/trackpad
history motion relative to the original fixed mapping.

The **Terminal typography** card exposes the bundled terminal-family selector
plus the default-off ligature toggle. The **Keyboard** card currently exposes
the off-by-default quick-switch-number overlay preference.

The two layout choices remain visible rather than hiding a binary decision in a
selector. Reversible settings apply immediately and save automatically as they
change; Reset appears beside a changed setting at the narrowest useful scope.

There is no separate **Configuration** card in Settings. Workspace
persistence itself remains the explicit off-by-default **Workspace restore**
toggle: unlike chip layout, status bar, and session-detail visibility (all of
which apply and save immediately with no opt-in), resurrecting a previous
run's open tabs is a bigger behavioral change than a cosmetic preference, so
it stays off until the user turns it on. While off, fesTerm always starts at
the Launcher and neither reads nor writes the open tab list, its order, or
the active tab - any workspace snapshot left over from an earlier run (or an
older fesTerm build that always persisted it) is dropped rather than silently
resurfacing. Turning the toggle off also scrubs any already-saved workspace
from disk immediately, so re-enabling it later starts clean instead of
resurrecting a forgotten snapshot. Once turned on, workspace state (the open
tab list, its order, and the active tab) saves and restores automatically:
every tab-list mutation (opening, closing, reordering, or activating a tab)
triggers a write-through save on the next frame, mirroring how interface
settings and profile CRUD already save immediately on every change. There is
no manual **Reload configuration** or **Save workspace** action for the user
to remember to invoke, and Settings does not describe persistence with
reload/save language; ADR 0015 owns the startup contract that this automatic
behavior relies on. No file watching, configuration editor, or
credential-storage control is implied.

There is still no sidebar, settings search, theme selector, empty category,
or general Reset-all control. Keyboard shortcuts remain mostly fixed; the
single quick-switch overlay preference earns a small dedicated card without
pretending the whole keyboard model is fully user-configurable.

Category icons support scanning but never replace labels. Reset appears only
for a non-default value and at appropriate setting/category scope. Terminal
font changes may alter cell geometry and resize active PTYs, so unusually
disruptive changes require a clear consequence before application. Profile,
credential, and trust-record edits use their own staged and security-aware
flows. Closing Settings returns focus to the previously active session and
never restarts sessions; Settings never lives in the session inspector.

The detail, status-bar, close-confirmation, and workspace switches are
independent. Changing detail visibility
does not rewrite a session name, terminal title, profile, or workspace, and it
does not change the chip width range. A live change produces one coherent
terminal resize after the chrome height settles; it must not resize once per
chip or over multiple intermediate frames. Wrapped rows use the selected chip
height consistently across every row.

### Configuration recovery

![Configuration recovery target](images/gui-mockups/configuration-recovery.png)

Configuration loading is transactional and non-destructive. fesTerm parses and
validates a complete candidate before it becomes active. If the candidate
cannot be used, the error surface says **Configuration needs attention**, gives
one concise source-owned explanation, and states that the user's file was not
changed. It offers **Open Configuration Folder** when supported, **Copy
Details**, and the primary recovery action **Continue with Safe Defaults**.

Safe Defaults keeps the Launcher and terminal access usable for the current
run without overwriting, renaming, moving, or silently repairing the invalid
source. The ordinary Settings UI must not imply those fallback values were
successfully persisted. There is no initial Reset Configuration shortcut.
Exact paths and bounded parser/schema detail appear only through deliberate
expansion or Copy Details; raw parser stacks, secrets, environment values, and
unreviewed source content do not appear in the concise error.

When schema boundaries permit it safely, unsupported fields generate bounded
warnings while valid settings load, and invalid profiles or workspace entries
are quarantined individually with their exact reason rather than invalidating
unrelated configuration. fesTerm never guesses how to reinterpret an invalid
security-, transport-, or identity-relevant value. The next startup and any
explicit save/retry follow the same complete-candidate validation and update
disk state atomically only on success.

A save failure leaves the user's in-memory reversible change visible but marks
it clearly **Not saved**, retains the last successfully persisted configuration
as the disk truth, and offers Retry plus bounded details. Later changes do not
silently erase that state. Closing the application with unsaved configuration
uses a focused consequence prompt; it never claims settings will survive the
next launch. Successful retry clears the warning only after an atomic write has
completed.

## Workspace Workflow

### Restore

![Mixed-result workspace restoration target](images/gui-mockups/workspace-restore.png)

When workspace restore is enabled, startup clearly indicates restoration while
sessions are recreated. A workspace is a recipe for reopening sessions, not a
snapshot of processes. Restore always launches new local processes, new SSH
connections, and new serial-device opens; the UI never calls this process
resumption.

Workspace data may include profile references or validated launch definitions,
tab order, stable names, selected tab, supported window geometry/state, and
applicable layout preferences. It never includes process memory, terminal
content, scrollback, passwords, authentication responses, or private keys. A
one-off SSH launch may contribute host, port, and username only; a one-off
serial launch may contribute its non-secret device identifier and line
settings. An initial directory is saved only from a reliable profile/launch
definition, never guessed from prompt text or terminal title.

Each tab independently enters starting/opening,
authentication/trust-required, failed, or running/connected/open state.
Progress presents exact completed/total counts
only while restoration is active. Selecting an actionable chip opens its
focused decision surface. The previously selected session becomes active when
practical, but required action on it takes precedence.

### Failure handling

One failed session must not block restoration of the rest of the workspace. Failed tabs retain their stable identity and expose retry or diagnostic actions.

Missing profiles or unsupported definitions remain visible as failed entries
with explanations rather than being silently dropped. Exited/failed sessions
are excluded when saving by default but may be explicitly included if their
launch definitions remain valid. Launcher and Settings tabs are not workspace
sessions.

### Restored identity

Workspace restoration should preserve:

- tab order;
- stable tab/session names;
- selected tab;
- window dimensions and supported window state; and
- profile references or launch definitions.

Transient terminal titles should not become the restored primary identity.

Saving and updating are explicit rather than continuous. Deleting a workspace
does not delete referenced profiles. Automatic startup restore is a separate
preference and retains the same fresh-launch semantics.

## Diagnostics and Error UX

### Three levels

1. **Normal mode** — quiet terminal workstation.
2. **Contextual notification** — brief actionable state near the affected session.
3. **Inspector diagnostics** — detailed lifecycle, queue, dimensions, byte
   counts, timing, and error information for the active session.

### Error presentation

Errors should answer:

- What failed?
- Which session is affected?
- Is retry possible?
- What is the primary next action?

Technical details should be expandable or copyable without occupying the main terminal surface permanently.

### Session inspector

![Session Inspector overlay target](images/gui-mockups/session-inspector.png)

The inspector is a hidden-by-default right-side overlay. At ordinary window
widths it is exactly 320 logical px wide and inset 8 px from the top, right, and
bottom of the content viewport between application chrome and the status bar.
At 480 logical px window width or below it becomes a near-full-width overlay
with 16 px left and right margins. It never resizes the terminal, generates a
PTY resize, or reflows a full-screen TUI. There is no scrim; terminal output
remains visible where the overlay does not cover it.

The header remains fixed and contains stable identity, semantic type/state,
and Close. The body scrolls internally rather than growing outside the content
viewport. Its section order is: an actionable state message when one exists,
**Session**, the applicable **Process**, **Connection**, or **Serial device**
section, **Trust** when applicable, **Actions**, then collapsed **Diagnostics**.
The inspector follows the active chip while open. Its heading may repeat stable
identity because that identifies the temporary panel's subject.

Only applicable sections appear. A normal SSH inspector may show destination,
username, connected state, reliably known remote platform, actual grid, full
selectable terminal title, host-key algorithm/fingerprint and trust scope.
Local sessions omit Connection and Trust. Working directory appears only with
reliable shell integration. Unknown rows are omitted unless absence itself is
decision-relevant, such as `Remote platform not reported`.

A serial inspector shows the exact device identifier, open state, baud rate,
data bits, parity, stop bits, and flow control, plus only reliable OS-supplied
hardware identity. It has no remote-platform, host-trust, username, cipher, or
latency rows. Serial line settings describe the configuration fesTerm applied,
not a claim that an attached device recognized or accepted it.

The full fingerprint is selectable and copyable, and trust text states the
actual scope (for example, Accepted once). Diagnostics begins collapsed.
Actions are capability-dependent and transport-specific: **Stop Process** for
a local process, **Disconnect** for SSH, and **Close Port** for serial stop the
live transport while preserving the approved read-only history. **Close
Session** destroys the chip and unsaved history. **Reconnect** or **Reopen**
appears only when the backend can perform the corresponding operation. Settings
and unrelated global actions never appear in the inspector.

Opening the inspector moves keyboard focus into it without changing the active
session. Escape or its Close control dismisses it and restores focus to the
previously focused terminal or application control. Clicking the uncovered
terminal dismisses the inspector but consumes that click so it cannot
accidentally select, paste, report a mouse event, or reposition an application
cursor; a subsequent click interacts normally. Switching chips keeps the
inspector open, replaces its contents with the newly active session's sourced
facts, and focuses its header rather than retaining a stale row or action.

The current implementation uses a foreground `egui::Area` with the specified
desktop and narrow-window geometry, so opening it does not change the terminal
grid or issue a PTY resize. Escape and the first pointer click outside the
panel are consumed before terminal input routing; tests verify that dismissal
does not alter terminal selection. The panel exposes only non-secret metadata
owned by the application tab, keeps Diagnostics collapsed/selectable, restores
focus where the invoking control remains viable, and closes rather than hiding
and resurrecting when a non-session surface becomes active. Stop/Disconnect,
destructive Close Session, and error-event emphasis remain staged behind their
respective read-only-history, confirmation, and diagnostic-event work rather
than appearing as misleading controls.

### Diagnostic content

Details from an error overlay opens the inspector with Diagnostics expanded
and the relevant event emphasized. Diagnostics shows only measurements and
events fesTerm owns: lifecycle state/generation, client-observed event times,
grid and cell geometry, active buffer, bounded scrollback usage, cursor/title,
transport/queue state, reconnect details, renderer timing, and the last error
when available. Timestamps are explicitly client-observed, not remote event
times. Sections remain capability-dependent and raw errors are collapsed and
selectable.

Opening Diagnostics does not pause or resize the session. Copy Details emits a
bounded structured summary with hostnames, usernames, and filesystem paths
redacted by default. Terminal contents, commands, passwords, tokens, private
keys, authentication responses, and environment values are excluded by
default. Any future support bundle follows the same policy and requires a
separate explicit warning before terminal content is included. A dedicated
cross-session diagnostics surface is deferred until a support workflow proves
that it is needed.

### Reconnect presentation

![Reconnecting-session overlay target](images/gui-mockups/reconnecting-session.png)

A reconnecting SSH session should retain its tab and stable identity. The tab may show a compact reconnecting state, while the viewport presents a restrained overlay or message that does not destroy prior terminal content unnecessarily.

The overlay may show attempt count and retry delay only when the reconnect
controller supplies those exact values. Stop Retrying transitions to a stable
Disconnected state; switching tabs does not interrupt retries. Details opens
the inspector at the relevant event. Reconnect appears only when the backend
can actually create a new connection attempt; otherwise the actions are
Details and Close Session.

Reconnection must not imply that remote process state survived. Unless
continuity is guaranteed, the UI describes reconnecting to the host rather
than restoring the shell, and a successful attempt starts a new terminal
lifecycle while retaining user-assigned chip identity.

That new lifecycle remains in the same chip and viewport. fesTerm seals the
previous generation's retained output as read-only history, resets terminal
modes for a clean new terminal model, and presents new output beneath the old
scrollback. A subtle generation boundary distinguishes the two. The boundary
is UI-owned metadata rather than terminal cells: terminal output cannot forge
or erase it, Find does not match it, and Copy never emits its label. A selection
that crosses the boundary receives one structural line break so unrelated old
and new text cannot be concatenated.

There is no initial generation picker or separate history browser in the
inspector. Ordinary scrolling reaches prior output; scroll, selection, Copy,
Find, and explicit links retain their normal read-only behavior. A Find match
cannot span the generation boundary. New terminal output follows the existing
follow/suspend rules, and input always targets only the current live
generation. The chip and status bar report the current transport state even
while the user has scrolled into older output, just as they do during ordinary
live-session scrollback review.

All retained generations share the session's bounded in-memory history budget;
oldest content is evicted predictably and nothing is persisted to disk. Escape
sequences from a fresh generation may clear only that generation's terminal
content, never sealed prior history. Failed connection attempts do not create
empty generations or boundaries; the boundary is committed only once a new
terminal channel or process is actually established. Local Relaunch and serial
Reopen follow the same rule. True continuity is claimed only when a future
multiplexer-aware backend can prove that it reattached to the same remote
terminal lifecycle.

The current floating overlay (`crates/festerm-ui-egui/src/overlay.rs`) already
draws above rather than instead of terminal content, but currently offers only
View Diagnostics and Close Tab because restart capability does not yet exist.

### Disconnected and exited history

![Disconnected read-only history target](images/gui-mockups/disconnected-history.png)

A disconnected or exited session becomes a read-only terminal history surface:

- Preserve exactly the terminal content and bounded scrollback fesTerm already
  holds; do not synthesize missing history.
- Keep scrolling, selection, and copying available. Stop cursor blinking and
  disable typing, paste, and terminal mouse reporting because no process can
  receive input. Local mouse selection/scrolling works without the terminal's
  former mouse-reporting modifiers.
- `Ctrl+Shift+C`, the Copy command, and context-menu Copy copy a selection.
  Plain `Ctrl+C` with no selection does nothing rather than pretending to send
  an interrupt.
- The compact overlay must not capture interaction across the viewport.
- History remains only until the tab is closed and is not automatically saved
  to disk. Any future transcript export is explicit and warns about sensitive
  content.

If reconnection creates a fresh terminal lifecycle, do not inject a fabricated
divider into the terminal byte stream. Seal the prior generation into the
same bounded read-only scrollback and mark its transition to the fresh terminal
model with the UI-owned, non-content boundary specified above.

### Sensitive data

Diagnostic views and exported bundles must avoid exposing secrets, credentials, private keys, tokens, or unreviewed terminal content by default.

## Visual Language

### General style

- Blue-graphite terminal-first default: cool undertones in neutral surfaces,
  with cyan reserved for small, high-information accents.
- Minimal borders and separators.
- Compact controls and tab chrome.
- Purposeful accent color.
- No visible cell grid in normal mode.
- Limited animation, used only for meaningful state changes.
- Terminal content visually dominates application chrome.

The initial product ships only the approved blue-graphite dark application
theme. Do not show a one-option selector or claim light/System support before
complete semantic tokens, accessibility checks, and visual baselines exist.
OS appearance changes do not silently select an unsupported theme. High
contrast is a separate accessibility requirement, and arbitrary UI color
editors remain out of scope. Semantic roles preserve a future path to validated
System/Dark/Light choices without changing widget behavior.

### Density

The default density should support many tabs and long work sessions without feeling cramped. Touch-sized controls are not required as the desktop default, but critical actions must remain usable and accessible.

### Implemented geometry and spacing

The following logical-pixel values describe the current default UI. They are
the baseline for visual review and tests, not an instruction to duplicate
literals across modules; implementation code should centralize shared tokens
as those APIs mature.

| Element | Current value |
| --- | --- |
| Initial viewport | Approximately `752 × 516`, targeting an `80 × 25` terminal at the default font estimate; minimum `360 × 240` |
| Upper chrome | `42` high: `8` top inset plus one `34` chip row; no lower separator or differently colored title band |
| Window/content inset | `8` from the left and right edges in chrome; terminal viewport uses `8` on every side, with its top inset forming the single gap below the chips |
| Session chip | `132..220 × 34`, `6` corner radius, `8` between siblings; `1` inactive outline and `1.5` active outline |
| Chip content | `8` leading inset; `6` icon/text spacing; currently an `8` px standalone status dot, with an approved migration to a `16` px session icon carrying a `6..7` px state badge; primary row shifted down `1` optical px without moving the secondary row |
| Active-chip close control | `16 × 16`, inset `8` from the chip's top-right corner; only shown on the active chip |
| New-session control | `34 × 34`, matching the chip height and `6` corner radius |
| Trailing chrome controls | Currently `22 × 22` control boxes with `8` spacing. Windows/Linux show six; macOS shows the three application controls and reserves `76` on the left for native traffic lights. The current boxes are a known `2` px shortfall from the icon system's `24 × 24` minimum target and should not become the asset-layer default |
| Status bar | `24` high with a `1` top rule, `20` leading inset, `8` between items, `7` px status dot, and `1` px downward optical adjustment |
| Command palette | `420 × 320` when space permits, centered `48` below the top edge with at least `16` side margins when narrow; results scroll region capped at `220` high |
| Connection overlay | Bottom-center, `24` above the bottom anchor, maximum content width `360`, with `6` before its actions |
| Launcher/settings surfaces | `24` top padding and `12` between primary groups; the current SSH text fields request `220` width |

The initial viewport estimate uses a `9 × 18` cell at the default 14 pt
monospace font; actual grid dimensions remain renderer-measured and must not
be forced to those estimates. Chip wrapping and single-row scrolling may
change the chrome's occupied height when explicitly selected, but neither may
cover or fragment the terminal viewport.

When **Show session details in chips** is off, the corresponding compact
geometry is `36` px upper chrome (`8` top inset plus one `28` px chip row),
`132..220 × 28` session/application chips, and a `28 × 28` New Session
control. Chip widths deliberately stay in the default range so toggling detail
does not cause a second horizontal reflow. The 16 px type icon/state badge and
active close control remain, and every interactive target retains at least the
icon system's 24 px target. Trailing controls remain vertically centered in the
occupied band. On macOS the 76 px traffic-light reserve remains unchanged and
must be reviewed optically against both heights.

### Semantic color roles

Use semantic roles rather than scattered literal colors:

```text
surface.window
surface.chrome
surface.terminal
surface.tab.active
surface.tab.inactive
surface.overlay
surface.selection
text.primary
text.secondary
text.muted
border.subtle
border.active
accent.primary
status.running
status.starting
status.reconnecting
status.disconnected
status.error
status.attention
status.exited
```

Themes should map these roles to concrete colors while preserving contrast and state distinctions.

The default mapping is centralized in `crates/festerm-ui-egui/src/theme.rs`:

| Role | Default |
| --- | --- |
| `surface.window` | `#0e1319` |
| `surface.terminal` | `#11161e` |
| `surface.chrome` | `#11161e` (same as terminal) |
| `surface.tab.inactive` | `#1a222c` |
| `surface.tab.active` | `#29333e` |
| `surface.overlay` | `#26313d` |
| `surface.selection` | `#28516b` |
| `text.primary` | `#e8edf2` |
| `text.secondary` | `#a7b2bd` |
| `text.muted` | `#788592` |
| `border.subtle` | `#35414e` |
| `border.active` | `#91a7b8` |
| `accent.primary` | `#42bfd0` |
| `status.running` | `#4fc17d` |
| `status.starting` | `#d2a94b` |
| `status.reconnecting` | `#42bfd0` (same as accent) |
| `status.disconnected` | `#7c8794` |
| `status.attention` | `#b58ad4` |
| `status.error` | `#d96868` |
| `status.exited` | `#657280` |

The accent is not a general surface fill. Use it for links, focus/selection
details, active controls, and reconnecting activity. Terminal ANSI/indexed and
explicit RGB colors remain a separate protocol palette and are never remapped
through these application roles.

The default egui mapping is also intentional: panels use `surface.window`,
popup windows use `surface.overlay` with a `1` px `border.subtle` stroke,
text edits and code blocks use `surface.tab.inactive`, hovered/open widgets use
`surface.tab.active` with `border.active`, pressed widgets use
`surface.selection` with an accent border, and selection uses
`surface.selection` with a `1` px `text.primary` stroke. Warning and error text
map to `status.starting` and `status.error`. Icon color is supplied at the use
site: compact chrome is `text.secondary`, becomes `text.primary` on hover,
and uses `status.error` for destructive close hover. Do not create separate
colored SVG files for any of these states.

### Typography roles

At minimum, define:

- terminal text;
- tab primary identity;
- tab secondary title;
- launcher heading;
- launcher item title;
- launcher item description;
- contextual status text; and
- diagnostic text.

Terminal typography may use separate font and shaping rules from application chrome.

The terminal renderer defaults to bundled **JetBrains Mono NL 2.304** at `14`
pt and lets Settings select JetBrains Mono, Iosevka Term, JuliaMono, or Maple
Mono. Each family includes regular, bold, italic, and bold-italic static
faces; missing glyphs continue through an owned Noto Emoji monochrome face and
then the separate monospace fallback chain. Emoji-presentation graphemes use
the pinned Noto Color Emoji renderer path, while VS15 requests monochrome text
presentation. Both paths remain clipped to core-owned cell spans under ADRs
0012 and 0026.
Changing the primary family rebuilds egui's atlas, advances a font generation,
invalidates terminal glyph caches, recalculates primary-face cell metrics, and
coalesces the resulting PTY resize through the existing viewport path.
The approved application chrome will bundle **Inter** for consistent metrics
across Windows, Linux, and macOS. Inter is an application asset, not a terminal
font default: it serves chips, launcher and Settings content, overlays, menus,
status text, and diagnostics, while terminal shaping and user-selected terminal
fonts remain completely independent.

Application chrome continues to use egui's semantic `Body` role for primary
chip identity and `Small` for secondary chip metadata and status-bar content.
Those named roles and the geometry they produce are the layout contract rather
than scattered widget-specific font sizes. Use regular weight for ordinary
body/description text, medium for compact control and identity labels, and
semibold only for real headings. Unsupported scripts and glyphs fall through
to a platform-appropriate UI fallback without changing the terminal font
fallback chain. The bundled font's license and exact source version must be
recorded with the asset when it is added to the binary.

Do not show an application-font selector while Inter is the only supported
choice. Before the application typography is frozen, usability and visual-
baseline review must compare Inter at supported DPI scales and on every native
platform, with particular attention to the smallest secondary/status text,
long and duplicate chip identities, dense menus, non-Latin fallback, and
letterform ambiguity. Evidence from that review may replace the bundled face
without changing the semantic typography roles or terminal-font contract.

## Terminal Typography

### Goals

- High readability for long sessions.
- Reliable cross-platform glyph coverage.
- Correct width and continuation mapping.
- Deterministic monochrome and color emoji fallback without breaking cell
  geometry.
- Ligature support after the shaping-to-cell mapping contract is validated.

### Ligatures

Ligatures are a planned capability, but they must never alter terminal cell ownership, cursor placement, selection boundaries, or mouse coordinates.

The bundled default deliberately uses JetBrains Mono's `NL` faces and
one-cell layout. A default-off Settings toggle enables shaping only for
contiguous, compatible ASCII cells. JetBrains then uses its ordinary faces;
Iosevka uses the reproducible `calt` build recorded with its assets; JuliaMono
and Maple Mono use their pinned upstream feature sets. Empty, wide,
combining/non-ASCII, fallback, selected, linked, and style-transition cells
remain hard shaping boundaries.

### User control

Users can configure the bundled terminal family and ligature preference
through versioned configuration and GUI Settings. Font size remains
per-session runtime zoom; configurable fallback ordering and line height are
future controls.

Runtime zoom is per session. `Ctrl++`, `Ctrl+-`, and `Ctrl+0` on Windows/Linux
(`Cmd` equivalents on macOS) change only the active terminal's font size.
Each change recalculates cell geometry and emits one coalesced PTY/SSH resize
after layout settles, preserving bottom-follow or the viewed logical region.
A temporary noninteractive overlay reports the factual result, for example
`14 pt · 80×25`, without stealing focus or entering scrollback.

Zoom clamps to a tested readable range. Reset returns to the applicable
profile/application default. Application chrome does not scale with terminal
zoom, and runtime zoom never silently rewrites the source profile. Profiles
may later define an initial size or accept an explicit apply-as-default action.
High-DPI moves preserve point size while recalculating physical pixels and
cell geometry.

### Terminal color schemes

Terminal schemes are independent of application theme. A scheme supplies
default foreground/background, ANSI colors 0–15, and optional cursor/selection
defaults. Explicit RGB and indexed protocol colors remain terminal output and
are rendered faithfully rather than remapped by chrome.

Runtime scheme changes are per session, like zoom, and do not rewrite a source
profile. A profile may reference a stable scheme identifier and later accept
an explicit apply-as-default action. Built-ins are complete tested definitions;
imports require schema validation, bounded names, and safe fallback roles.
Previews show representative text and all ANSI colors without starting a
process or fabricating session output. Cursor/selection visibility receives a
documented accessibility fallback. History can be re-presented under another
scheme without mutating cell data. No automatic scheme changes derive from
remote host, shell, or command, and no selector appears while only one real
scheme exists.

### Cursor appearance

The terminal-correctness spec default cursor style is `BlinkingBlock` (per real xterm/VT100 behavior), which the core terminal model (`crates/festerm-core/src/terminal.rs`) always reports faithfully once a program has requested a style. However, as an additive, GUI-only presentation choice, the renderer (`crates/festerm-ui-egui/src/renderer.rs`) shows a steady vertical bar cursor by default — until the terminal program in the session explicitly requests a style via DECSCUSR (tracked separately via `Terminal::cursor_style_requested_by_program()`), at which point the program's requested style is honored exactly. This avoids the hollow, unfocused-looking empty box appearing by default while never altering the spec-observable `cursor_style()` value itself. When a block-style cursor is in effect and the view is focused, it renders filled (solid) rather than a hollow outline, matching conventional terminal-emulator focus affordance; the character glyph underneath a filled cursor is redrawn in the background color on top so it stays legible.

## Interaction Conventions

### Keyboard

Expected commands include:

- New Tab opens the session launcher.
- **Start Local Shell** opens the default local profile directly with the
  platform shortcut shown below and remains available from the command palette.
- Close Tab closes the current launcher or session tab.
- Next/Previous Tab switch predictably.
- The combined command palette/session switcher finds tabs by stable identity
  and optional secondary content and exposes applicable application commands.
- Terminal-content search is a separate local operation over retained rendered
  text, not a command-palette mode.

Application bindings must respect terminal-native editors. On Windows/Linux,
avoid reserving plain `Ctrl+letter` chords whenever a Shift-modified or
platform-level alternative exists; in particular, plain `Ctrl+T` and
`Ctrl+W` remain available to Vim's tag and window commands. macOS uses Command
for application chrome because it is ordinarily distinct from terminal Ctrl
input. The current platform defaults are:

| Command | Windows/Linux | macOS |
| --- | --- | --- |
| New Session… | `Ctrl+Shift+T` | `Cmd+T` |
| Start Local Shell directly | `Ctrl+Shift+N` | `Cmd+N` |
| Close active tab | `Ctrl+Shift+W` | `Cmd+W` |
| Next / previous tab | `Ctrl+Tab` / `Ctrl+Shift+Tab` | `Ctrl+Tab` / `Ctrl+Shift+Tab` (pending native-convention review) |
| Command palette / session switcher | `Ctrl+Shift+P` | `Cmd+Shift+P` |
| Open Settings | `Ctrl+Shift+S` | `Cmd+Shift+S` (also `Cmd+,`, the macOS "Preferences" convention, via the native app menu) |
| Find in terminal | `Ctrl+Shift+F` | `Cmd+F` target; current implementation remains `Cmd+Shift+F` until search lands |
| Copy / Paste | `Ctrl+Shift+C` / `Ctrl+Shift+V` | `Cmd+C` / `Cmd+V` |
| Clear terminal (display and scrollback) | `Ctrl+Shift+K` | `Cmd+K` |
| Reset terminal display state | `Ctrl+Shift+R` | `Option+Cmd+R` |
| Enter / exit Focus Mode | `Ctrl+Shift+F11` | `Cmd+Shift+F` |
| Zoom in / out / reset | `Ctrl++` / `Ctrl+-` / `Ctrl+0` | `Cmd++` / `Cmd+-` / `Cmd+0` |

The application observes physical GUI modifiers before terminal encoding, so
it can reserve `Ctrl+Shift+T` while forwarding plain `Ctrl+T` even on legacy
terminal protocols that could not distinguish them after encoding. Every
essential command retains a menu/palette route, and configurable bindings may
later free a reserved chord. Before defaults are frozen, validate them against
Vim, Emacs, Readline, tmux, common shells, and extended keyboard protocols.
Bindings dispatch through the semantic application command model rather than
widget-specific paths.

### Command palette and session switcher

![Combined command palette and session switcher target](images/gui-mockups/command-palette.png)

The command palette and session switcher remain one overlay. Session switching
is an application command, and a second nearly identical search surface would
add a shortcut and mental model without adding capability.

The palette opens with `Ctrl+Shift+P` or `CommandPalette`, overlays without
resizing the terminal, and uses a 420 px width when space permits with at least
16 px window margins when narrow. Search matches command names, stable session
identities, and secondary chip content. Session rows reuse type icon, state
badge, stable identity, and secondary precedence. Empty result groups disappear.

Selecting a session activates it; selecting a command routes through the same
application command path as every other invocation surface. Only implemented,
applicable commands and actually bound platform shortcuts appear. Up/Down
moves selection, Enter activates, and Escape closes and restores previous
focus. Query text clears on close and is never logged or persisted. Results
scroll while the search field and group context remain visible.

With an empty query, the palette presents a **Sessions** group in chip order,
with the active surface visibly identified, followed by an applicable
**Commands** group. Session matching and ranking prefer stable identity over
secondary terminal-provided content; dynamic text may help find a session but
never outranks its stable name. Empty groups disappear.

The initial command inventory, subject to actual implementation and active-
surface applicability, is:

- **New Session…** and **Start Local Shell**;
- **Find in Terminal**, **Show Session Inspector** or **Hide Session
  Inspector**, **Rename Session…**, a capability-backed **Reconnect Session**,
  and **Close Session…**;
- **Enter Focus Mode** or **Exit Focus Mode**, **Zoom In**, **Zoom Out**, and
  **Reset Zoom**; and
- **Open Settings**.

Labels name the resulting action rather than exposing implementation language
such as “toggle.” An unfinished command—including Focus mode before it exists—
is absent. Chip-layout and status-bar controls remain in Settings. Copy, Paste,
next/previous session, native window controls, host-trust decisions, and
authentication responses do not clutter the palette; their existing direct or
owned surfaces remain authoritative. The implementation uses **New Session…**
and keeps chip-layout controls in Settings rather than duplicating them in the
palette.

The palette does not search terminal history. Its trigger uses
`CommandPalette`; the ordinary `Search` icon is reserved for terminal-content
search.

### Terminal-content search

![Terminal-content search target](images/gui-mockups/terminal-search.png)

Find overlays the viewport without changing grid dimensions and searches the
terminal's rendered text model rather than raw transport bytes. It covers
retained scrollback and the current primary screen. In alternate-screen mode,
it searches only the retained alternate-screen content and does not pretend
that the inaccessible primary buffer is simultaneously visible. Wrapped rows
are treated as logical text only where the model reliably preserves wrapping;
wide and combining characters retain cell ownership.

The initial behavior is literal, case-insensitive search. Regex, whole-word,
and case toggles remain absent until deliberately implemented. Matches receive
a subtle highlight and the active match the stronger selection treatment.
Enter/Down advances, Shift+Enter/Up reverses, and Escape clears the transient
query/highlights and restores terminal focus. No result uses `No matches`
rather than a fabricated `0 of 0` position.

Search focus prevents input from reaching the PTY. Navigation scrolls locally
and never sends mouse or keyboard reporting. New output may extend the result
set without moving the current match unexpectedly. Search highlighting remains
independent of terminal text selection; Copy operates on explicit selection,
not implicitly on the current match. Queries are never logged or persisted.
Search may collapse into overflow at narrow widths while remaining available
through `Ctrl+Shift+F` and the command palette.

### Reserved local input and clipboard

Copy, Paste, forced local selection, and the local context menu are application
escape hatches intercepted before terminal input encoding. Raw mode,
alternate-screen mode, mouse reporting, or extended keyboard protocols cannot
disable them.

- Windows/Linux reserve `Ctrl+Shift+C` and `Ctrl+Shift+V`; macOS reserves
  `Cmd+C` and `Cmd+V`. Plain `Ctrl+C` in a live terminal remains terminal
  input, normally the interrupt character.
- Shift+drag always forces local selection. Shift+right-click always opens the
  local context menu. Without terminal mouse reporting, ordinary drag selects
  and ordinary right-click opens the menu; with reporting, unmodified mouse
  events go to the terminal application.
- When an application text field owns focus, standard Copy/Paste applies to
  that field instead of the terminal.
- Copy reads selected cells from fesTerm's model and remains available for any
  retained selectable buffer. It joins soft-wrapped rows, preserves real line
  breaks, omits unused trailing cells, and preserves displayed wide/combining
  Unicode. It does not clear selection.
- Paste reads the OS clipboard and uses the ordered bounded session input path.
  It remains available whenever a live session accepts input and compatible
  clipboard text exists. A terminal program cannot disable it.
- Disconnected/exited sessions retain Copy for an existing selection and Find,
  but omit Paste.
- The context menu contains only the applicable terminal-local operations
  specified above: explicit-link actions, Copy, Paste, and Find. Session/global
  actions remain elsewhere. Select All is not an initial action; copy-on-select
  and right-click-to-paste are not defaults.
- Clipboard contents are never logged, persisted, or included in diagnostics.

### IME and composed input

IME pre-edit text is a transient local overlay anchored to the terminal cursor.
It does not mutate the terminal model, enter scrollback, claim terminal cells,
or reach transport. Only committed text enters normal terminal text encoding;
it is typing, not clipboard Paste, and receives no paste warning.

Platform composition/candidate presentation remains visible near viewport
edges and under DPI scaling. Escape is offered to the IME before terminal
encoding. Switching/closing sessions or moving focus to an application field
cancels uncommitted composition rather than sending it to the wrong owner.
Disconnected history rejects composition. Pre-edit content is never logged,
persisted, diagnosed, or exposed as terminal history, while screen readers use
the platform composition state rather than duplicated terminal output.

### Paste safety

![Multiline paste-confirmation target](images/gui-mockups/paste-confirmation.png)

If bracketed-paste mode is enabled, paste uses the protocol markers. Without
it, a single line pastes directly; multiline text requires confirmation because
line breaks may immediately execute commands. Very large pastes require
confirmation even in bracketed-paste mode because they may flood queues. The
initial large-paste threshold remains an implementation constant pending the
usability testing specified below, not a user-facing preference or a fabricated
safety boundary.

The modal title is `Paste <exact line count> into “<stable identity>”?` and its
body explains that a line may execute immediately when bracketed paste is not
active. It shows an exact plain-text preview bounded by both lines and
characters, states the exact total line and character counts, and explicitly
counts omitted content. Spaces and tabs remain faithful; non-tab/newline control
characters use an unambiguous escaped representation. The dialog also exposes
the current transport state so the target cannot be mistaken for a stale or
inactive session. Its actions are **Cancel** and **Paste**.

Cancel has initial focus, Escape cancels, clicking outside does not dismiss,
and Enter cannot submit until the user deliberately focuses Paste. The dialog
captures the original clipboard text and binds it to the exact session identity
and lifecycle generation. A clipboard change, session switch, reconnect,
disconnect, close, or any transition that stops input invalidates and cancels
the operation rather than sending different content or targeting another
session. It never follows activation to a different chip.

Normalize line endings for the session input representation without trimming
whitespace, rewriting shell syntax, or claiming content is safe. A confirmed
paste sends the captured original text—not an escaped or truncated preview—as
one ordered input operation and cannot interleave with ordinary keystrokes. If
the session stops accepting input, submission becomes unavailable. There is no
initial don't-ask-again control.

### Scrollback and follow-output

Output follows the bottom only while the viewport is already at the bottom.
Deliberate scrolling, Page Up, search navigation, or text selection away from
the bottom suspends follow-output; new output enters bounded scrollback without
yanking the reading position. A compact lower-corner `Jump to latest` control
appears only after unseen output arrives. It may show an exact new logical-line
count only when reliably measurable.

Activating the control or `Ctrl+End` returns to the bottom and resumes follow.
Shift+Page Up/Down and Shift-modified wheel behavior are guaranteed local
scroll escape hatches even when a TUI owns normal paging/mouse input. Resize
preserves the viewed logical region where possible, search leaves follow
suspended after navigating backward, and scroll position is per session across
chip switches but not persisted after closure. Disconnected/exited sessions
have no live-follow state or new-output indicator.

If bounded scrollback evicts content above the current view, preserve the
nearest retained position and announce that older history was discarded;
never display stale rows.

The terminal uses a thin right-edge overlay scrollbar that does not consume a
grid column. It appears on hover, scroll, selection drag, or whenever the view
is away from the bottom. Its thumb represents only the bounded retained buffer,
uses a practical minimum size, and becomes full/hidden when nothing can scroll.
Dragging suspends follow; track interaction pages toward a location rather than
claiming one-pixel precision. It remains fully usable for disconnected history
and is application-reserved even when a TUI has mouse reporting enabled.
Keyboard scrolling remains available, and accessibility/high-contrast scaling
may widen the hit target without changing grid geometry. There is no minimap
or search/command/diagnostic decoration on the track.

### Background output hypothesis

The initial design does not mark an inactive chip for ordinary output bytes.
Generic output has no reliable message/read boundary, and log streams would
make an unread badge or row count permanently noisy. Bell attention and
reliable lifecycle transitions remain the signals that change a chip.

This is explicitly a usability-test hypothesis, not an irreversible product
rule. Testing should determine whether users lose important background context.
If a future preference is justified, it must define the truthful event it
represents (for example, activity since last activation) and must not label
bytes/rows as “unread,” infer task completion, or compete with higher-priority
attention and lifecycle states. Ordinary output remains silent by default.

Activating a background session restores its own scroll/follow position. A
session left at live bottom shows the current bottom; one deliberately left in
history returns there with Jump to Latest. Activation does not fabricate a
read acknowledgment.

### Explicit terminal hyperlinks

The initial link implementation supports explicit OSC 8 hyperlinks only.
Automatic URL and path detection is deferred because punctuation, wrapping,
and local-versus-remote ownership are ambiguous.

Explicit links use restrained hover underlining and expose their full target
in a tooltip. `Ctrl+click` on Windows/Linux or `Cmd+click` on macOS opens a
validated target through the OS handler; ordinary clicks retain terminal
selection/mouse behavior. Context actions Open Link and Copy Link Address
appear only over a real link range. Copying terminal text copies visible text,
not the hidden URI. Common explicit schemes such as HTTPS, HTTP, and mailto may
open directly; unfamiliar schemes require confirmation, and malformed/control-
character targets are rejected. Links remain usable in read-only history and
are excluded from diagnostics unless explicitly requested.

A future local path action must first verify a client-local existing path. An
SSH path is remote and must never be passed to the local OS without a separate
deliberate remote-file workflow.

### Bell and attention

A terminal bell requests attention; it does not prove success, failure,
completion, or command identity. For an inactive session, set the
`status.attention` badge and secondary text `Attention requested` until the
chip is activated. Repeated bells coalesce rather than forming a speculative
counter. A higher-priority lifecycle state wins.

For an active session, briefly pulse only its status badge. There is no
whole-window flash, focus stealing, tab switching, scrolling, or cursor
movement. The pulse is rate-limited and becomes a static temporary color
change when reduced motion is requested. Sound and OS notifications are off by
default until persisted preferences and an explicit privacy/rate-limit policy
exist. Disconnected history cannot generate new attention events.

Diagnostics may retain a client-observed bell event without terminal content.
Future visual/audio/background-alert controls appear only with real choices
and configuration persistence.

### Mouse

- Clicking a tab activates it.
- Tab close controls should avoid accidental activation or closure.
- Reordering may be supported through drag-and-drop.
- Session tabs may be renamed by double-clicking the chip's label, editing inline, and committing with Enter or by clicking away; Escape cancels. Launcher and Settings chips are not renamable.
- Terminal mouse reporting and local selection remain governed by terminal mode and modifier policy.

Chip reordering is implemented by making the whole chip press-and-hold draggable (`crates/festerm-ui-egui/src/chrome.rs`) rather than via a dedicated drag-handle glyph: no explicit move icon is shown, and pressing anywhere on the chip body (outside the label, close button, and current status indicator, which keep their own click targets) starts a drag. While dragging, the chip itself floats at the cursor and sibling chips live-shuffle to preview the resulting order as the pointer moves, rather than only showing a static insertion marker.

### Focus

Opening or activating a terminal tab should focus the terminal unless a modal action, authentication prompt, or launcher control explicitly owns focus. Implemented in `crates/festerm-ui-egui/src/view.rs`'s `TerminalView::show_in_ui`: each view claims keyboard focus once automatically on its first rendered frame (tracked via a `has_requested_initial_focus` flag), in addition to the existing click-to-focus behavior, so a freshly started session is immediately typeable without requiring an extra click into the terminal.

### Fullscreen and focus mode

![Focus-mode target](images/gui-mockups/focus-mode.png)

OS fullscreen retains application chrome and the 24 px status bar. Focus mode
is a separate explicit window-level command that hides both for a terminal-only
view; terminal alternate-screen mode affects only terminal buffers and cannot
enter or exit either application mode.

Entering/exiting focus mode preserves the active session and per-session zoom,
recalculates the grid, and emits one coalesced resize. A brief noninteractive
overlay names the mode and its exit shortcut. Exceptional trust,
authentication, disconnect, and failure overlays remain available, and
background attention may use a small non-content-bearing indicator without
switching sessions. Platform window controls remain reachable through native
conventions. Escape alone does not exit, terminal escape sequences cannot
control it, and focus mode is not initially persisted in workspaces.

### Closing sessions and quitting

![Live-session close-confirmation target](images/gui-mockups/close-session-confirmation.png)

Closing Launcher, Settings, or an already exited/disconnected history surface
is immediate. By default, closing a live local, SSH, or serial session requires
confirmation from every invocation path. Settings exposes **Confirm before
closing live sessions** for users who prefer immediate one-click closing; when
off, every close invocation bypasses this session confirmation consistently.
When confirmation is enabled, its title is `Close “<stable
identity>”?`; its body names the exact transport consequence—terminate the
owned local process, disconnect from the remote host, or close the serial
port—and states that the session's unsaved terminal history will be discarded.
The actions are **Cancel** and destructive **Close Session**.

Inspector transport actions preserve the chip and history and therefore use
different language. **Stop Process** says the process will terminate and its
history become read-only; **Disconnect** says the SSH transport will close and
history become read-only; **Close Port** says the serial device will be
released and history become read-only. Their confirmation actions repeat the
specific verb rather than using a generic OK.

Closing the application window presents one aggregate confirmation when live
sessions remain, summarizing exact counts of local processes, SSH connections,
and open serial devices rather than opening one dialog per chip. It states that
unsaved history will be discarded and uses **Close Window**. **Quit fesTerm**
uses the same aggregate model across every window owned by that application
process and names its destructive action **Quit fesTerm**; on macOS, Close
Window and Quit retain their distinct native lifecycle meanings.

All destructive confirmations are modal to their owning window. Cancel has
initial focus, Escape cancels, clicking outside does not dismiss, and Enter
cannot confirm destruction unless the user has explicitly moved focus to the
destructive action. A dialog binds to the exact session identity and lifecycle
generation it describes, then revalidates state at confirmation time; it must
not stop a freshly reconnected process or transport through a stale prompt. If
the consequence disappears while open, the dialog closes safely or updates
before accepting action.

Cleanup follows bounded backend shutdown policy and cannot hang indefinitely.
Workspace definitions survive runtime closure; history does not. No
always-close-without-asking, close-protection, or “don't ask again” preference
appears before persisted configuration exists. Closing the final chip returns
to Launcher under the root-state rule above.

### OS window title

Use `fesTerm` as the native title for every application surface and session.
Stable session identity remains in the active chip rather than being exposed
to task switchers, window managers, screenshots, or screen sharing by default.
Terminal-provided titles, transient state, bells, workspace names, and rapid
output never rewrite the OS title. No dynamic suffix is used initially, and the
app icon remains stable taskbar/dock identity.

Usability testing must include multiple independent fesTerm instances and
task-switcher/window-overview navigation to validate whether a fixed title
causes meaningful identification problems. If evidence establishes that need,
consider an explicit opt-in preference to expose stable active-session identity
in the native title. Never fall back to terminal-provided title text or enable
identity exposure silently.

### Privacy outside the terminal viewport

fesTerm does not claim that terminal use is private merely because sensitive
values are omitted from ordinary chrome. OS screenshot, screen-recording,
window-management, accessibility, and clipboard facilities may capture visible
terminal or application content. The application does not attempt to intercept
those facilities, and Focus mode is not a privacy mode. An initial masking mode
is omitted because it could hide app-owned labels while leaving arbitrary
terminal output exposed, creating false confidence.

OS notifications and sound remain off by default. If notifications are later
implemented behind persisted preferences, their default external text is
generic: application identity plus a factual message such as “A session
requires attention.” They omit stable session names, terminal-provided titles,
hostnames, usernames, paths, commands, output, authentication method, trust
fingerprints, and error details. Activating a notification focuses the affected
session, where the owned UI can present the necessary context. Any future
identity-bearing notification option must be an explicit privacy tradeoff with
rate limiting, not an inferred convenience.

Copied diagnostics are bounded and redact stable/user-provided identity,
hostnames, usernames, filesystem paths, serial-device identifiers, host-key
fingerprints, environment values, and terminal content by default. Dedicated
explicit actions such as Copy Fingerprint or ordinary terminal Copy remain
literal because the user selected that exact content. Logs, crash metadata, and
telemetry follow the diagnostic exclusion rules and never become a second,
less-protected transcript channel.

Workspace and profile files may necessarily contain validated launch metadata,
including host, username, path, or serial-device configuration. That data is
private local configuration: it uses user-scoped storage, is not advertised as
secret storage, and is never automatically synchronized, uploaded, attached to
diagnostics, or shared. Passwords, authentication responses, private-key
contents, environment values, terminal output, and scrollback remain excluded.
Workspace sharing/export is not an initial feature. A future export must show
an exact metadata preview and offer explicit redaction before creating a
shareable artifact; it cannot reuse the ordinary internal workspace file and
call it safe.

### Drag-and-drop input

Plain-text drops follow the same ordering and safety rules as Paste. Local
file-path dropping is an explicit future requirement because it is useful for
interactive CLIs, but it is not implemented until path insertion semantics are
reliable.

For a future local live session, a file drop inserts absolute client paths as
one ordered input operation and never sends Enter, reads file contents, or
uploads automatically. A reliably known profile shell family may supply
PowerShell, POSIX-shell, or `cmd.exe` quoting. Otherwise a bounded preview
offers raw-path insertion or Cancel rather than guessing. Multiple paths retain
drop order; control/newline characters receive escaped warning presentation.

A local file dropped on SSH must not insert a misleading client-local path. A
future transfer workflow may explicitly upload and then insert a verified
remote path, but those remain two visible operations. Application text fields
receive drops targeted to them; disconnected/exited terminals reject input
drops. Paths remain transient input and never enter logs, diagnostics,
profiles, or workspaces.

## Accessibility

The GUI should provide:

- semantic roles and accessible names for tabs and controls;
- non-color state indicators;
- sufficient contrast;
- keyboard traversal for launcher, tabs, settings, and diagnostics;
- clear focus indication outside the terminal viewport;
- meaningful truncation or tooltips for shortened identity text; and
- scalable application chrome independent of terminal font size where practical.

Headless GUI tests should assert semantic names and roles for major controls before pixel snapshots are treated as authoritative.

Application and terminal accessibility remain separate trust domains. Chips
expose tab role, stable name, selected state, session type, and lifecycle;
icon-only controls expose actions rather than picture descriptions. Reliable
exceptional transitions announce once, while ordinary output and repeated
bells do not flood announcements. Terminal escape sequences cannot create
application controls, replace trusted identity, or generate arbitrary
application-level announcements.

The terminal accessibility layer exposes bounded visible logical lines, cursor,
selection, and focused-line changes without mirroring the full changing screen
into generic labels. Alternate-screen transitions expose only the active
buffer. Decision dialogs trap focus; temporary nonmodal surfaces restore the
exact prior focus owner. Chrome focus is visibly non-color-only, while terminal
focus uses the active cursor rather than a bright viewport border. Truncated
text retains full sanitized accessible naming. UI scale affects chrome
independently of terminal zoom; high contrast may strengthen tokens without
changing geometry, and reduced motion removes pulses/fades without hiding
persistent state. No essential action depends on hover, drag, precision, or
color.

### Terminal selection

Drag creates linear selection; double-click selects a Unicode-aware terminal
word, and triple-click selects a logical line by joining soft wraps but not
real breaks. Shift+click extends. Alt+drag (Option on macOS) creates rectangular
cell selection; Shift forces either local mode while mouse reporting is active.
Word rules may recognize common path punctuation but never infer shell syntax.

Wide glyphs are indivisible, combining marks stay with their bases, and
rectangular copy omits continuation cells without duplicating glyphs. Selection
may span retained scrollback, auto-scrolls predictably at edges, persists per
session and across new output, and remains tied to its buffer across alternate-
screen transitions. Overwrite or eviction invalidates only affected content
and never permits stale copying. Search highlight stays visually distinct.
Linear copy joins soft wraps; rectangular copy preserves rows. There is no
initial primary-selection clipboard behavior or dedicated Clear Selection
command.

## Responsive and Narrow-Window Behavior

![Minimum-width responsive target](images/gui-mockups/narrow-window.png)

When horizontal space is constrained, remove or collapse information in this order:

1. Secondary terminal-provided title in the chip; when compact detail has been
   relocated to the status bar, that relocated value ellipsizes first.
2. Chip width toward its minimum, then stable-identity ellipsis.
3. Less-used global actions into overflow, starting with terminal Search and
   Session Inspector.
4. Status-bar locality/platform, retaining grid dimensions and accessible
   state.

The stable primary session identity, type icon, state badge, New Session, and
platform-required window controls should remain visible as long as possible.
The active chip is always scrolled fully into view. The command palette—not an
overflow menu—is the session switcher when many chips are offscreen.

The terminal viewport must never become fragmented or uncovered because chrome, diagnostics, or footer geometry was calculated after terminal dimensions.

## Future Populated Launcher Example

![Future populated Launcher target](images/gui-mockups/launcher-populated.png)

Once persisted profiles and workspaces exist and contain real entries, a
populated launcher may resemble:

```text
fesTerm

New Session
  Local Shell
    PowerShell
    Bash

  SSH
    Connect to host...

Recent
  production-db
  staging
  dev-vm

Workspaces
  Yesterday
  Kubernetes
```

This is information architecture, not a pixel specification. The final implementation should remain compact and terminal-oriented.

## Validation Strategy

The complete traversable state/action model is maintained in
[`gui-action-graph.md`](gui-action-graph.md). It assigns stable edge IDs to
every workflow in this document, including cancellation, inverse, cleanup, and
return-to-known-state paths. Test plans and VM drivers should reference those
edges rather than inventing isolated sequences.

GUI behavior should be validated in layers:

1. Pure layout, geometry, state, and input tests.
2. Headless egui frame tests.
3. Structural viewport and clipping assertions.
4. Stable visual snapshots.
5. Native-window platform smoke tests.
6. Workflow review using screenshots or recordings.

Important GUI states should include:

- launcher with no saved profiles;
- launcher with recent profiles and workspaces;
- multiple local, SSH, and serial tabs when their transports are implemented;
- active, inactive, reconnecting, failed, and exited sessions;
- long and duplicate session names;
- narrow and high-DPI windows;
- diagnostics collapsed and expanded; and
- workspace restoration with mixed success.

## Iteration Process

GUI work should proceed in short design rounds:

1. **Workflow:** define user goal, primary action, keyboard path, failure state, and required information.
2. **Information architecture:** define tabs, menus, panels, and state ownership.
3. **Visual language:** define density, spacing, typography, color roles, and icon policy.
4. **Prototype:** build a narrow vertical slice with fake session metadata where appropriate.
5. **Evaluation:** review screenshots, recordings, structural tests, and interaction behavior.
6. **Specification:** update this document and create focused implementation issues.

Implementation should not attempt the entire future UI at once. The first prototype should prove the launcher, tab identity model, session states, hidden inspector diagnostics, and terminal dominance before broader settings or workspace UI is built.

### Mockup-comparison review process

Step 5 (**Evaluation**) above is deliberately not "eyeball the screenshot and move on." Early GUI rounds relied on a general-purpose coding agent doing its own ad hoc visual comparison against the wireframe in the same context as the implementation work, and that repeatedly produced two failure modes: the agent anchored on whichever detail it had just been told to check (missing unrelated regressions elsewhere in the same screenshot), and its pixel-level size/spacing estimates from the image were unreliable — two independent passes over the same screenshot region gave materially different numbers, and at least one flagged "deviation" (an 18px vs. 24px chip-inset asymmetry) turned out to be a false positive once actually measured.

That motivated splitting mockup analysis into its own persisted, reusable custom agent definition (`~/.copilot/agents/mockup-analyst.md`, user-scoped so it is available across repositories/sessions) whose sole job is deep, disinterested visual/UX comparison — not implementation. Its design settled on:

- **Workflow- and aesthetics-first reading, not pattern matching.** It is instructed to first form an understanding of the mockup's intent (border spacing and offsets, alignment axes, typography, color semantics, navigation paradigms, element grouping, and permanent-vs-transient elements) before diffing against any implementation, and to recognize when a single image is a composite of multiple independent screens/states that must be analyzed separately.
- **A structured Match / Deviation / Ambiguous protocol** for every comparison, rather than free-form prose, so findings are consistently actionable.
- **A negotiated-deviations ledger** (`~/.copilot/agents/mockup-analyst-deviations.md`): some departures from the mockup are conscious product decisions (for example, the status bar deliberately omitting fabricated shell-version/encoding fields). The agent reads this ledger before every review and reports conformance to a negotiated preference as **Match (negotiated)**, distinct from a plain mockup match, instead of re-flagging an accepted decision as a fresh deviation every round.
- **Explicit evidence-sufficiency checks.** The agent must say when a screenshot cannot answer the question being asked (e.g. a single-chip screenshot cannot prove active-vs-inactive contrast, an empty terminal cannot prove viewport padding on all four sides, no hover/focus captured cannot assess transient states) and name the specific follow-up screenshot needed, rather than silently passing or failing on insufficient evidence.
- **Preferring objective pixel measurement over visual estimation for close calls** — script-sampling the actual mockup/screenshot image file (e.g. via a short Python/Pillow snippet run through `powershell`) rather than trusting an LLM's visual read of a rendered image, directly because of the false-positive case above.

The agent is invoked today by pasting its full definition into a fresh general-purpose background-agent context for each review (custom agent definitions are not yet directly selectable as a `task`-tool agent type), so each run is a clean, disinterested pass with no bias carried over from the implementation work in the same session. A comparative run using two different underlying models (Claude and GPT) on the same mockup found they can disagree on the same evidence — for example, only one correctly recognized a status-bar field omission as an already-negotiated decision rather than a fresh deviation — which is the concrete case the negotiated-deviations ledger now exists to prevent.

For screen-specific review, use the nearest linked PNG in this document rather
than cropping the older composite wireframe. Capture the native application at
the corresponding logical viewport and state when practical, retain the OS and
DPI/scale in the evidence record, and compare workflow and semantics before
pixel detail. A mockup is authoritative only for the contract described by its
adjacent section; prose and recorded negotiated deviations win when a static
image cannot express focus, timing, platform convention, accessibility, or
implemented-capability conditions.

## Historical First GUI Prototype Scope

The completed first design prototype was scoped to include:

- a launcher tab;
- two or three mock session tabs;
- local and SSH identities;
- active and inactive states;
- disconnected and reconnecting states;
- stable primary identity plus optional dynamic secondary title;
- a compact New Session action;
- hidden diagnostics within the session inspector; and
- terminal viewport dominance.

It used fake metadata where necessary so interaction and hierarchy could be
settled before M7 and M8 implementation details constrained the design. This
section is retained as design history, not as the current implementation plan.

## Current Implementation Priorities

The command-label/palette alignment, Launcher lifecycle, responsive Session
Inspector overlay, first native macOS application-menu slice, and truthful
terminal/session-chip context menus are implemented. Remaining approved target
behavior should proceed in this order unless an active milestone or regression
requires a different sequence:

1. Add bounded scrollback under the M9 design gate, then represent reconnect,
   relaunch, and reopen boundaries as non-terminal UI metadata within it.
2. **Initial slice implemented:** live-session close and risky-paste
   confirmations use application-owned policy with stable tab identity and
   transport-generation binding. Escape is consumed before terminal routing,
   Cancel owns initial focus, backdrop clicks do not dismiss, and terminal
   input is suppressed while a prompt is open. Paste normalizes only line
   endings, shows exact bounded counts/preview, and submits one ordered
   captured operation after revalidation. Native narrow-window, clipboard,
   threshold, wording, and focus usability remain in `manual-validation.md`;
   aggregate multi-session window quit remains staged.
3. **Initial Focus Mode and zoom slices implemented:** Focus Mode is an
   explicit palette command that hides chrome/footer, preserves the active
   terminal, leaves Escape to terminal content, and exposes its exit route in
   a brief overlay. Runtime zoom uses the documented platform chords and
   palette commands, is bounded and per-session, and never scales application
   chrome. Native DPI, resize-coalescing, anchor, focus, and accessibility
   evidence remains in `manual-validation.md`.
4. **Initial About and update surface implemented:** the compact
   application-owned modal
   exposes exact version/platform information, a bounded privacy-safe Copy
   summary, the canonical source link, and repository/asset license notices;
   configured packaged builds additionally expose the explicit signed-update
   state machine from ADR 0021. Developer/incomplete builds fail closed without
   a network action. Native update, link handoff, accessibility, and visual
   evidence remain in `manual-validation.md`.
5. **Semantic icon layer implemented:** `festerm-ui-egui::icon::Icon` owns the
   complete first-party inventory and dependency-free canonical geometry;
   Launcher session types and persistent chrome controls use it with semantic
   colors and accessible control labels. Remaining one-off sites and native
   cross-platform visual evidence are tracked separately. The terminal now
   bundles four selectable families with real regular/bold/italic/bold-italic
   faces, generation-safe runtime switching, independent fallback chains, and
   default-off ligatures. Bundle Inter through the owned application-font
   layer next, retaining fallback contracts.
6. Implement **Show session details in chips** as the presentation-only live
   preference defined above: grouped Settings row, `34→28` chip geometry,
   `42→36` single-row chrome geometry, active-only status-bar relocation, and
   one coalesced terminal resize. Cover `CHIP-11`, `STATUS-03`, `SET-02`, and
   manual scenario `AS-09`; do not turn it into a general density or theming
   system.
7. Schedule the serial backend as a focused capability track with platform
   discovery, permissions, exclusive ownership, and deterministic loopback
   evidence; its GUI contract is already defined here.

## Open Questions

Manual/native checks and usability hypotheses in this section are inventoried
in [`manual-validation.md`](manual-validation.md) and coordinated through
[issue #43](https://github.com/fes/fesTerm/issues/43). A focused issue is added
only when a check has its own environment, owner, acceptance boundary, or
discovered defect.

- Cross-platform custom-title-bar behavior: drag, double-click maximize/restore,
  snapping, multi-monitor DPI, accessibility, and native convention alignment
  are tracked in [issue #29](https://github.com/fes/fesTerm/issues/29).
- Usability and cross-platform visual-baseline review of bundled Inter for
  application chrome, especially small text, DPI scaling, and non-Latin
  fallback; future configurable theme variants remain separate.
- Usability validation of paste confirmation: multiline and large-paste
  thresholds, exact-preview comprehension at narrow sizes, keyboard focus and
  Enter safety, bracketed-paste explanation, and clear cancellation when the
  clipboard or target session changes.
- Incremental migration of remaining one-off painter-drawn presentation sites
  through the semantic icon layer; revisit raster caching only if measured
  performance or additional asset complexity justifies it.
- Usability validation of the fixed `fesTerm` native title across multiple
  independent instances and OS task-switcher/window-overview workflows; any
  identity-bearing title remains an explicit future opt-in.
- Backend capability and command semantics for a real Reconnect action; the UX
  contract is specified above, while the current overlay correctly omits an
  action it cannot perform.
- Post-transport Markdown-viewing decision: whether it belongs in fesTerm at
  all and, if accepted, whether the primary target is structural readability
  or a deliberately bounded level of visual fidelity. The prerequisite and
  design/security questions are recorded under Deferred native Markdown
  viewing; no implementation is scheduled yet.
