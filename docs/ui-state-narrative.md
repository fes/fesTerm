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
behind *Show advanced settings*. A first-time SSH connection needs a
username and a host; durable sessions, port forwards and the choice between
password, private-key and certificate authentication are all available but
never in the way.

Two details worth noticing in review. Saving a password requires a saved
profile, and the form says so at the point of decision rather than failing
later. And the durable-session toggle explains what it will do — attach to a
named remote tmux or screen session, creating it when needed — because the
difference between a durable and an ordinary remote session is otherwise
invisible until something disconnects.

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
**Press keys** and then pressing the combination; capture takes the frame's
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
