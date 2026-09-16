<!-- GENERATED FILE - do not edit directly.
     Screenshots:      scripts/capture-ui-state.sh
     Narrative source: docs/ui-state-narrative.md
     Rebuild:          python3 scripts/build_ui_state_doc.py -->

# State of the UI

## How to read this document

This is a snapshot of every significant fesTerm surface as it currently
behaves, intended for design and workflow review rather than as a manual.
It is regenerated, not written by hand: the screenshots come from the real
UI rendered against repository-owned fixture data, so the document cannot
drift from the product without the drift showing up as a changed picture.

Everything here is synthetic. Hosts are `example.com`/`example.net`
subdomains, addresses come from the documentation-reserved ranges in
RFC 5737, and users are named `devuser`, `builder` and `operator`. No real
configuration, credential, host key, clipboard content or shell history is
reachable from the capture path.

Two things this document deliberately does *not* do. It does not enumerate
every state of every control — it shows each surface doing representative
work, plus the edge states that are actually interesting. And it does not
assert correctness; the automated suites and the validation gates do that.
What it captures is how the product currently *presents* itself.

## Starting a session

The New Session tab is the application's front door, and it answers three
different questions at once: what kinds of session can I start, what have I
saved, and what is already running that I could rejoin.

Launch cards across the top cover the five entry points — local shell, SSH,
SFTP, serial and Markdown. Below them the view splits: saved profiles on the
left with search and sort, running sessions on the right grouped by the
provider that owns them. That right-hand panel is the part that distinguishes
fesTerm from a plain terminal: native `festerm-sessiond` sessions, tmux and
screen are listed side by side under one Reattach affordance, so reattaching
does not require knowing which multiplexer produced the session.

The compact variant exists because the launch cards are the least valuable
part of the view once a user has profiles. It shrinks the cards so the
profile and running-session panels start higher, while keeping the card
descriptions.

### New Session in the compact launcher-grid layout

The 'Compact New Session layout' preference shrinks the launch cards so saved profiles and running sessions start higher up the window.

![New Session in the compact launcher-grid layout](images/ui-state/launcher-compact.png)

### New Session on first run, before any profile exists

The empty/first-run state: only the fixed launch cards, with no saved profiles or resumable sessions panels to show yet.

![New Session on first run, before any profile exists](images/ui-state/launcher-empty.png)

### New Session at a narrow, responsive width

The same populated launcher reflowing at a narrow window: cards and panels stack instead of sitting side by side.

![New Session at a narrow, responsive width](images/ui-state/launcher-narrow.png)

### New Session with saved profiles and running sessions

Launch cards sit above saved profiles of every kind plus resumable local, tmux, and screen sessions, at a normal desktop width.

![New Session with saved profiles and running sessions](images/ui-state/launcher-populated.png)

## Connecting

Each transport has its own form, and they share a deliberate shape: the
minimum needed to connect is visible immediately, and everything else is
behind *Advanced settings*. The SSH form separates that minimum into a
**Connection** section (host, port, username) and an **Authentication**
section (password, private-key or certificate), with the durable-session
toggle on its own band beneath them; only port forwards remain folded away.
A first-time SSH connection therefore needs a username and a host, and
nothing else is in the way.

Two details worth noticing in review. Saving a password requires a saved
profile, and the form says so at the point of decision rather than failing
later. And the durable-session toggle explains what it will do — attach to a
named remote tmux or screen session, creating it when needed — because the
difference between a durable and an ordinary remote session is otherwise
invisible until something disconnects.

### Serial connect form

Serial launches ask only for a device path and line settings; there is no host/credential concept for a local serial device.

![Serial connect form](images/ui-state/serial-connect-form.png)

### SFTP connect form

The SFTP launch surface defaults to opening the graphical two-pane file manager once connected.

![SFTP connect form](images/ui-state/sftp-connect-form.png)

### SSH connect form with Advanced settings expanded

Expanding 'Advanced settings' reveals the port-forwarding controls beneath the always-visible connection, authentication and durable-session sections.

![SSH connect form with Advanced settings expanded](images/ui-state/ssh-connect-advanced.png)

### SSH connect form

The default SSH launch surface: a Connection section for host, port and username, an Authentication section for the credential method, and the durable-remote-session toggle, with Advanced settings collapsed.

![SSH connect form](images/ui-state/ssh-connect-collapsed.png)

## Working in a session

A connected session is the point of the product, and the terminal surface is
deliberately the least decorated thing in it: no gutter, no persistent
sidebar, no ornament competing with the program's own output. Chrome gets out
of the way once a session is live.

The two captures here show the same surface serving different kinds of work —
an interactive remote shell, and the line-oriented `sftp` client. The second
is worth including precisely because fesTerm also ships a graphical SFTP
workspace: the command-line client remains available and unmodified, and
choosing the GUI is a preference rather than a replacement. Users with muscle
memory for `get` and `put` keep it.

### The sftp command-line interface

The terminal-driven `sftp` client, distinct from the graphical file manager: directory listing, a `get`, and its transfer-progress line.

![The sftp command-line interface](images/ui-state/terminal-sftp-cli.png)

### A connected SSH terminal session

A remote shell mid-use: service status, a log tail, and the resting prompt, rendered by the same grid the terminal view paints for a real PTY.

![A connected SSH terminal session](images/ui-state/terminal-ssh-session.png)

## The SFTP workspace

The graphical SFTP workspace is a two-pane browser: local on the left, remote
on the right, with the transfer direction made explicit by the two buttons
between them rather than implied by drag direction alone.

Several choices here are worth review. Each pane carries its own breadcrumb,
filter box and sort state, so the two sides are navigated independently. Each
pane's footer reports item count, selection count and selected size, which is
the information needed immediately before a transfer. The local pane is
labelled *This computer* and the remote pane carries the account and host it
is actually connected to, because the single most costly mistake in a file
manager is acting on the wrong side.

The pane order is configurable in Settings, for users who think of the remote
side as the primary one.

### The SFTP graphical file-manager workspace

Local and remote panes browsed side by side; the local pane lists an invented project directory, never the real filesystem.

![The SFTP graphical file-manager workspace](images/ui-state/sftp-workspace-browser.png)

## Markdown workspaces

Markdown is a first-class surface rather than a preview pane, which follows
from where these files usually live: a README or runbook on the far end of an
SSH connection, reached over SFTP. Opening one should not require copying it
locally, and the header here shows a document opened directly from a remote
path.

Preview and Source are peers rather than a mode and its escape hatch, and the
outline is a navigation control — selecting a heading moves the document and
the outline tracks position. For a runbook consulted while something is
broken, that navigation matters more than the rendering does.

### Markdown workspace with the outline open

The heading outline docked alongside the document, letting a reader jump straight to a section of a longer file.

![Markdown workspace with the outline open](images/ui-state/markdown-outline.png)

### Markdown workspace, rendered preview

A fictional project's Markdown fetched over an already-authenticated SFTP session, shown in rendered preview mode with headings, a list, and a code block.

![Markdown workspace, rendered preview](images/ui-state/markdown-preview.png)

### Markdown workspace, source mode

The same document toggled to raw source, for readers who want to see the Markdown itself rather than its rendering.

![Markdown workspace, source mode](images/ui-state/markdown-source.png)

## Session diagnostics

The Session Inspector answers "what exactly am I connected to, and how?" — a
question that becomes urgent precisely when something is wrong and least
convenient to answer by reading scrollback.

It opens beside the session rather than over it, so the terminal stays visible
and readable while the facts are consulted. Content is grouped by what is
being asserted: session identity and grid geometry, the connection's
destination, username and transport, and trust — the key fingerprint and
whether verification is pending or settled. Grouping trust separately is
deliberate, since it is the part a user may need to compare against an
out-of-band source.

The second capture shows a durable multiplexer-backed session, where the facts
on offer differ: what matters is the multiplexer and the session name it can
be rejoined by.

### Session Inspector over a durable tmux session

The same overlay for a session attached through fesTerm's durable persistence provider, showing the extra 'Durable session' facts and 'Resume' (rather than 'Reconnect') action.

![Session Inspector over a durable tmux session](images/ui-state/diagnostics-durable-session.png)

### Session Inspector over an SSH session

The inspector overlay reporting connection facts and a pending host-key fingerprint for a plain SSH shell, without covering the terminal it describes.

![Session Inspector over an SSH session](images/ui-state/diagnostics-ssh-session.png)

## Session chips

Chips are the persistent record of what is open. Each carries a status dot, a
name, and — in the verbose form — a second line of detail: the host for a
remote session, the multiplexer for a durable one, the file type for a
document. Only the active chip shows a close control, so a row of chips
presents one destructive affordance rather than one per session.

The two captures are the same five sessions under the two settings, which is
the only useful way to view this choice. Verbose chips carry the second line;
compact chips drop it and shrink to roughly half the height. The trade is
worth judging directly from the pair: the second line is what distinguishes
two sessions with similar names on different hosts, and it is also the thing
you stop reading once you know which chip is which.

Note what compact mode does *not* do — it does not abbreviate names, and it
does not drop the status dot. Reconnecting and healthy sessions stay
distinguishable at both densities.

### Session chips in compact mode

The identical five sessions with session details turned off: chips shrink to a single line, fitting more of them in the same row.

![Session chips in compact mode](images/ui-state/chips-compact.png)

### Session chips with details shown

The same five sessions with 'Show session details in chips' enabled: each chip carries a secondary line under its title.

![Session chips with details shown](images/ui-state/chips-verbose.png)

## Settings

Settings is a single scrolling column of cards, each grouping one concern:
Interface, Scrolling, Terminal typography, Quick switch, SFTP and Keyboard
bindings. Every row carries a one-line explanation of what the setting
actually does, on the principle that a preference nobody can predict the
effect of is worse than no preference.

The cards are uniform width and the page owns the only scrollbar. This
matters more than it sounds: an earlier revision nested a scroll area inside
the page, which left the keyboard list showing three of thirty-five rows
inside a page that also scrolled.

### Settings: Interface and Scrolling

The top of Settings: chip layout, session-detail, and workspace-restore toggles, followed by scrollback limit and scroll-speed controls.

![Settings: Interface and Scrolling](images/ui-state/settings-interface-scrolling.png)

### Settings: Quick switch

The single toggle controlling whether held quick-switch modifiers overlay chip numbers.

![Settings: Quick switch](images/ui-state/settings-quick-switch.png)

### Settings: SFTP

Pane order and the default local directory used when opening new SFTP tabs.

![Settings: SFTP](images/ui-state/settings-sftp-card.png)

### Settings: Terminal typography

Font family, programming ligatures, and emoji presentation, all scoped to terminal cell rendering only.

![Settings: Terminal typography](images/ui-state/settings-terminal-typography.png)

## Keyboard bindings

The keyboard editor is the most information-dense surface in the product and
had the most iteration, so it is worth reviewing closely.

Actions are grouped by the scope they apply in — global, terminal, Markdown,
document — because scope is what determines whether a chord reaches fesTerm
or the terminal underneath. Each action carries a one-line description, and
search matches both the title and that description. The *Show* filter narrows
to a single scope, or to just the customized or just the unbound actions.

Chords render as keycaps in columns anchored on the **right**, so the key
itself always lands in the same column and each chord stays contiguous. The
alternative — one fixed column per modifier — forces every row to reserve
space for modifiers it does not use and splits short chords across a gap. The
reasoning is the same as right-aligning numbers.

Selecting an action expands its editor inline beneath its own row, and
clicking the row again closes it. A binding is assigned by pressing
**Record shortcut** and then pressing the combination; capture takes the frame's
key events before the shortcut dispatcher sees them, so binding a shortcut
does not also fire it. Recorded modifiers are normalised to the portable
`Primary` spelling, which is Command on macOS and Ctrl elsewhere, so a chord
captured on one platform still validates on another.

Both resets are always present and disabled — with an explanation on hover —
when they would do nothing. This deliberately departs from the house rule
that a reset appears only for a non-default value, because applied literally
that rule answered "is there a way to undo this?" with silence.

### Keyboard bindings editor, collapsed

Actions grouped by scope, each showing its current chord as keycaps; one customized action carries a 'Customized' badge.

![Keyboard bindings editor, collapsed](images/ui-state/keyboard-editor-collapsed.png)

### Keyboard bindings editor filtered by search

Typing into Search narrows the list to matching actions and hides scope groups with no match.

![Keyboard bindings editor filtered by search](images/ui-state/keyboard-editor-filtered.png)

### Keyboard bindings editor capturing a chord

'Record shortcut' arms live chord capture; the editor waits for a chord instead of dispatching whatever is pressed next.

![Keyboard bindings editor capturing a chord](images/ui-state/keyboard-editor-press-keys.png)

### Keyboard bindings editor with an action selected

Selecting an action expands its inline editor directly under its own row, showing scope, default, and current chord as read-only context.

![Keyboard bindings editor with an action selected](images/ui-state/keyboard-editor-selected.png)

## Profiles

Profiles are the saved form of everything the connection forms can express,
and the editor is per-transport for the same reason the forms are: a serial
adapter and an SSH bastion have almost nothing in common to configure.

The review question here is less about the individual fields than about the
round trip — whether what a user builds in a connection form can be saved
without re-entering it, and whether a saved profile makes clear what it will
do before it is launched.

### Profiles list

Every saved local, SSH, SFTP, and serial profile in a single searchable table, each row carrying an overflow menu for connecting, editing, duplicating, and deleting.

![Profiles list](images/ui-state/profiles-list.png)

### Serial profile editor

Device path and line settings (baud, data bits, parity, stop bits, flow control) for a saved serial profile.

![Serial profile editor](images/ui-state/profiles-serial-editor.png)

### SFTP profile editor

The same SSH-family editor in SFTP mode, offering the graphical file-manager toggle instead of a terminal type.

![SFTP profile editor](images/ui-state/profiles-sftp-editor.png)

### SSH profile editor

Editing an existing SSH profile's connection metadata and durable- session settings.

![SSH profile editor](images/ui-state/profiles-ssh-editor.png)
