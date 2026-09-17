Narrative source for `docs/state-of-the-ui.md`.

This file is hand-edited and is the only prose input to the generated
document. `scripts/build_ui_state_doc.py` reads the section markers below,
emits them in this order, and files each captured screenshot under its
section. Screenshot captions live next to their fixtures in
`app/festerm/src/ui_gallery.rs` so they stay correct when a scenario changes.

To add a section, add a marker here and give the matching `section` id to the
scenarios in the gallery. A screenshot whose section has no marker is a build
error, so captures can never appear in the document unexplained.

<!-- section: overview title: How to read this document -->

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

<!-- section: new-session title: Starting a session -->

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

<!-- section: connection-forms title: Connecting -->

Each transport has its own form, and they share a deliberate shape: the
minimum needed to connect is visible immediately, and everything else is
behind *Advanced settings*. The SSH form separates that minimum into a
**Connection** section and an **Authentication** section (password,
private-key or certificate), with the durable-session toggle on its own band
beneath them; only port forwards remain folded away. Connection shows the destination in
one of two notations -- a single squashed `user@host:port` field, or separate
Username, Host and Port fields -- with a toggle on the section heading that
switches between them and carries the value across. The squashed form is the
fast path and the separate fields are the legible one; showing both at once,
as an earlier pass did, only left the reader wondering which one the
connection would actually use.
A first-time SSH connection therefore needs a username and a host, and
nothing else is in the way.

Two details worth noticing in review. Saving a password requires a saved
profile, and the form says so at the point of decision rather than failing
later. And the durable-session toggle explains what it will do — attach to a
named remote tmux or screen session, creating it when needed — because the
difference between a durable and an ordinary remote session is otherwise
invisible until something disconnects.

<!-- section: terminal-sessions title: Working in a session -->

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

<!-- section: sftp-workspace title: The SFTP workspace -->

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

<!-- section: markdown title: Markdown workspaces -->

Markdown is a first-class surface rather than a preview pane, which follows
from where these files usually live: a README or runbook on the far end of an
SSH connection, reached over SFTP. Opening one should not require copying it
locally, and the header here shows a document opened directly from a remote
path.

Preview and Source are peers rather than a mode and its escape hatch, and the
outline is a navigation control — selecting a heading moves the document and
the outline tracks position. For a runbook consulted while something is
broken, that navigation matters more than the rendering does.

<!-- section: diagnostics title: Session diagnostics -->

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

<!-- section: chips title: Session chips -->

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

<!-- section: settings title: Settings -->

Settings is a single scrolling column of cards, each grouping one concern:
Interface, Scrolling, Terminal typography, Quick switch, SFTP and Keyboard
bindings. Every row carries a one-line explanation of what the setting
actually does, on the principle that a preference nobody can predict the
effect of is worse than no preference.

The cards are uniform width and the page owns the only scrollbar. This
matters more than it sounds: an earlier revision nested a scroll area inside
the page, which left the keyboard list showing three of thirty-five rows
inside a page that also scrolled.

<!-- section: keyboard title: Keyboard bindings -->

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

<!-- section: profiles title: Profiles -->

Profiles are the saved form of everything the connection forms can express,
and the editor is per-transport for the same reason the forms are: a serial
adapter and an SSH bastion have almost nothing in common to configure.

The review question here is less about the individual fields than about the
round trip — whether what a user builds in a connection form can be saved
without re-entering it, and whether a saved profile makes clear what it will
do before it is launched.
