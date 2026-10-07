# fesTerm GUI SFTP — Product/UI specification

**Status:** design proposal for review
**Scope:** first release of a graphical SFTP application surface

**Interactive workflow mockup:**
[`images/gui-mockups/sftp-workflow.html`](images/gui-mockups/sftp-workflow.html)
steps through opening SFTP, browsing, transferring, resolving a collision,
completion, and recoverable failure. Open the downloaded file in a browser;
the numbered states are local and require no network access.

## Product decision

SFTP opens as a first-class **application surface** in the existing fesTerm chip row, beside the SSH terminal rather than inside or over it. Its chip is labeled `SFTP · <stable session/profile name>` and uses a neutral remote-files icon plus a compact connection-status badge. Closing SFTP does not close the terminal; closing the terminal leaves an independently connected SFTP surface usable. When both were opened from one live SSH session, they may share verified host/profile metadata and credentials through application-owned connection policy, but they remain separate typed lifecycles.

Entry points:

- **Live SSH session:** command palette and Session Inspector action, **Open SFTP**.
- **Saved SSH profile:** Profiles row context action, **Open SFTP**, using the ordinary host-key and authentication flows before showing files.
- **Saved SFTP profile:** Profiles can create and save a reusable destination. Its **Use graphical file manager** option is on by default; turning it off launches the terminal SFTP command surface.
- **Launcher:** saved SFTP profiles launch in their configured mode. Ordinary SSH profiles may also expose **Open SFTP** as a secondary action.

The SFTP surface uses the requested two-pane file-manager model. **Local is left; Remote is right by default.** This matches the established convention of graphical FTP clients, gives left-to-right upload a natural reading direction, and keeps the lower-risk local filesystem as the stable starting context. A global Settings preference, **SFTP pane order: Local left / Remote left**, should swap the visual order because two-pane habits are unusually strong. Commands and accessibility names always say Local/Remote rather than Left/Right, so behavior remains stable when swapped.

## Layout and navigation

Each pane has the same hierarchy:

1. Explicit identity line: **LOCAL · This computer** or **REMOTE · user@host**; remote includes connection state.
2. Compact toolbar: Back, Up, Home, Refresh, followed by a breadcrumb/path bar.
3. Breadcrumb segments are individually clickable. The final segment is current. `Ctrl/Cmd+L` converts the bar to an editable path field; Enter navigates, Escape restores the breadcrumb.
4. Lightweight **Filter this folder** field. Filtering is immediate, case-insensitive by default, scoped to the loaded directory, and never implies an expensive recursive remote search. The filter remains active while navigating folders.
5. File table with Name, Size, Modified, and Type. Clicking a heading sorts; the active sort and direction are visible and announced. Default is folders first, then natural Name order. Sorting/filtering is independent per pane.

Entering a folder starts its table at the top. Back, Up, and an ancestor breadcrumb retrace only the active navigation stack and restore the retained ancestor's scroll position; popped descendants are discarded, so entering one again starts at the top rather than using durable per-directory scroll memory. Refresh preserves the current position where possible.

Rows use first-party semantic line icons at compact sizes: folder, generic file, text/code, image, archive, executable, and symlink. Extension/type text remains visible so color or icon shape is never the only cue. Hidden files follow a per-pane **Show hidden files** overflow setting.

The center transfer rail contains labeled actions that adapt to pane order:

- **Upload to Remote** transfers the Local selection to the open Remote folder.
- **Download to Local** transfers the Remote selection to the open Local folder.

Actions are disabled with an explanation when there is no selection, the destination is not writable, or the remote connection is unavailable. Double-click/Enter opens folders and uses the OS default action for local files only; remote files are not implicitly downloaded/opened in v1.

## Transfer behavior

Transfers are always **copies**, never moves. Selection remains after starting so the user can verify what was queued. A bottom transfer drawer appears while it has active work or retained history and shows aggregate progress, the current item, byte progress where known, destination, and per-item state. Users may cancel pending/current work; finished items can be cleared. A retained failed item shows a concise reason plus **Retry** and **Details** without stopping unrelated queued items.

Refresh the affected destination directory after each committed item while preserving selection and scroll position where possible. Partial files use a temporary sibling name and are renamed only after successful completion where the backend supports it; a canceled/failed temporary is cleaned up when safe and otherwise reported explicitly.

### Remote-to-local name admission

Remote names are untrusted even when they identify the selected top-level
file or directory, not just a recursively enumerated child. Before deriving
a local destination from a remote basename, `festerm-ssh` requires one safe
filename and verifies that the joined path is an immediate child of the
requested local directory. GUI buttons, shortcuts, cross-pane drops and Retry
share this transfer-manager check; text-mode `get` uses the same backend helper
for its default or existing-directory destination. Widgets do not implement
their own filename policy.

The policy is deliberately portable: it rejects empty/dot names, either path
separator, drive/stream colons, Windows-invalid characters and control bytes,
trailing dots/spaces and reserved Windows device names (including extensions).
It rejects rather than sanitizes; ordinary Unicode filenames remain unchanged.
An explicitly requested exact local path is not derived from the remote name
and retains its existing semantics, so a single-file download may choose a
safe local name instead. Recursive descendants still require admission.

A name refusal becomes an actionable existing SFTP operation error before
collision handling, recursive enumeration or destination writes. It produces
no partial output and does not disconnect browsing or stop unrelated queued
items. This is lexical path admission, not a new claim of race-free filesystem
containment against local ancestor replacement; the existing collision,
commit and owner-cleanup policies still apply.

### Bounded GUI backlog and history

Each GUI SFTP tab admits at most **64 pending commands** and retains at most
**128 pending worker/local-directory events**. Remote operations await event
capacity asynchronously; only the dedicated local-loader thread may block on
delivery. Each frontend poll and worker transfer-event batch handles at most
**64 events**, with another repaint requested for remaining frontend work.
Adjacent progress for the same batch and transfer may be replaced by its newest
value; collision, completion, failure, cancellation, skip, cleanup and
destination-refresh barriers remain ordered and are not coalesced.
Observed cleanup notices enter the separate bounded cleanup reporter before
any GUI-capacity wait; a retained barrier prevents progress folding across
the notice, and closing the owner cannot discard an already-observed report.

A full or closed command bridge reports a nonfatal refusal; it does not imply
that the action succeeded. An unadmitted remote navigation leaves path, history,
scroll and loading state unchanged; an unadmitted reconnect does not change
connection/spinner state; an unadmitted Markdown read preserves the
previous pending request. Collision decisions stay open until admitted, and
rejected external drops return failure rather than a successful item count.
Successful backend admission publishes queued row metadata before collision
or terminal events, so Skip and pre-copy failure remain visible even when
`ItemStarted` never occurs. Backend refusal creates no phantom drawer rows.
The drawer's **Cancel** action is one ordered bulk-cancel command through both
bridges, not one command per row: full admission refuses the whole action with
the existing error treatment, and one free slot is sufficient to retry.
It cancels existing work preceding that command, including paused collisions;
work queued afterward is not canceled.
An already-delivered collision prompt retires when its row becomes terminal,
rather than offering decisions for work that bulk Cancel has ended.
The backend's existing **256-item batch limit** is checked before the batch
enters the GUI command bridge. Queue counts are not total payload-byte budgets;
recursive-plan admission has its separate shared allowance below.

The drawer retains the **128 most recently finished records**, including
failures, ordered by completion rather than start time. Oldest finished records
retire automatically with a visible cumulative count; a long-running item that
finishes late is still recent. Active and collision-paused work is not evicted.
Retirement removes the row's request, detail and collision storage, keeps an
indexed transfer-ID lookup, and releases exceptional row/index capacity.
Row controls are scoped by transfer ID, so retiring an earlier row cannot
reuse another transfer's action/focus identity.
The existing Clear action removes successful, skipped and canceled rows but
leaves failures available while retained. Retirement is tab-local and does not
persist history or change the filesystem recovery-output policy.

History rows occupy a vertically scrollable area capped at 240 logical pixels.
The retained rows are rendered normally, not virtualized; the finished-record
cap bounds historical rendering work. Owner cancellation remains independent
of command/event capacity. Native drawer reachability, retirement-notice
readability and assistive-technology behavior remain `SFTP-03` evidence.

### Bounded recursive planning

Each live SFTP transfer worker shares **65,536 owned planning items** and
**64 MiB of conservative metadata accounting** across directory enumeration,
queued/active units and collision-paused plans. These are not separate
allowances for each plan. Existing work is not evicted to admit another plan;
exhaustion fails the new planning item visibly before its destination is
materialized. Retry becomes eligible as work progresses, finishes or is
canceled. Already committed copies are never rolled back by this refusal.

Reservations follow actual owned rows, units and pending collisions. An
enumerated row transfers its item reservation into its planned unit, rather
than being counted twice. Capacity, old/new container overlap during growth,
directory-stack paths and each decoded remote page are also charged. The
metadata proxy includes a 512-byte per-item overhead, owned text capacities
and a conservative destination/collision expansion envelope, including the
longest Keep Both suffix and a collision requeue slot. Resolving an admitted
collision therefore does not request new path credit after earlier files may
have committed. It is not an allocator/RSS measurement. Units release their
credits after their data retires, including in-flight copies and paused
decisions. Replacement queues charge their allocator-reported capacity before
retiring the old allocation. Exceptional empty slots remain charged until
their allocation retires; sparse queues copy at most 128 live entries into
fixed staging, free the old allocation before rebuilding, and keep a slot for
requeuing a collision.

Local enumeration admits each row before retaining it. Remote planning reads
one protocol page at a time over the same authenticated subsystem, checks
admission before growing the application-owned collection, and closes its
directory handle on completion/refusal or requests closure on cancellation.
It does not use the library's eager, repeatedly recopied whole-directory
listing. Eight consecutive replies containing no usable directory entry are
refused as a non-progressing server instead of keeping planning alive
indefinitely. Sorting the bounded planning rows is heap-free. The pinned
`russh-sftp` patch exposes only its existing paged client; provenance is in
`vendor/russh-sftp/PATCHES.md`.

Backend snapshot publication updates indexed changed rows without cloning
unchanged requests or sorting the whole inventory after each unit.
Membership changes still compact/reindex the bounded active vector;
explicit public snapshot queries still clone their requested snapshot.

The allowance is per worker, not app-wide or configurable. Ordinary browsing
and file-picker snapshots, queued request/event/public-snapshot payloads,
library wire/parser and blocking-I/O private allocations, allocator
fragmentation and fixed staging stack space remain outside this accounting.
One protocol reply has already been decoded before its page admission check;
this is not a pre-decode wire-buffer bound or a total-process memory promise.
Peer-side handle cleanup cannot be guaranteed after teardown, consistent with
the owner-cancellation policy below. Native refusal/retry readability,
keyboard interaction and accessibility remain `SFTP-03` evidence.

### Owner cancellation and cleanup

Closing an SFTP tab or its owning window cancels pending connection, trust,
browsing, metadata, Markdown-read and transfer work without waiting for another
command or progress callback. The sibling shell remains independent. Text-mode
SFTP shutdown also interrupts an awaited command without consuming queued input.

After dropping the interrupted operation and its file handles, cleanup gets at
most **two seconds**, then teardown proceeds with an explicit incomplete-cleanup
result. Exclusive creation acknowledges ownership at creation, not the current
identity of a mutable local or remote pathname. A metadata check followed by
unlink is not race-free. The current filesystem/SFTP APIs cannot establish safe
conditional deletion, so canceled/failed partial output is preserved and its
path reported for inspection and manual cleanup, even after acknowledged
creation. No cleanup operation follows a replaced ancestor or deletes a
replacement leaf. Already-started blocking filesystem calls cannot be forcibly
aborted; GUI and text runtime retirement do not wait indefinitely for those
calls. A pending creation with unconfirmed ownership is also reported, not
deleted. If an approved Replace is interrupted after its destructive commit
starts, the completed temporary is not removed: inspect both temporary and
destination because an already-sent rename may still complete. Reported recovery
files do not prevent unrelated transfers from proceeding.

Preserving a temporary during a late destination collision does not abandon
the collision workflow. Its incomplete-cleanup notice is separate from transfer
state: Replace, Skip and Keep Both remain available, and cancellation remains
cancellation rather than failing solely because a partial was retained.
The transfer manager owns cleanup reporting separately from copy errors;
text transfer failures report both the original cause and incomplete cleanup.

Cleanup failures use the existing dismissible application error dialog in the
tab's current window, even after the tab is gone. Its notification queue is
bounded and never delays teardown. A full queue or closed window falls back to
diagnostics; durable warnings are content-free, while console details include
recovery paths. Final process exit can interrupt the asynchronous grace period:
a cleanup-request record is not proof of completion, and neither cleanup nor
server-side rollback is guaranteed after process termination. Native packaged
close/quit and notice accessibility remain `SFTP-01` manual evidence.

### Collision and overwrite policy

Never silently overwrite an existing file. A collision pauses only the affected transfer and opens a decision dialog showing source and destination names, locations, sizes, and modified times:

- **Replace** — overwrite the destination only after this explicit choice.
- **Skip** — leave the destination unchanged; this is the initially focused safest action.
- **Keep Both** — choose a non-conflicting sibling name such as `report (copy).csv`, incrementing deterministically if needed.
- **Apply to all conflicts in this batch** — shown only when more than one possible conflict remains. It applies the chosen action to the current batch only and is never saved as a global preference.

Escape/cancel returns the item to a paused state without choosing. For a same-name folder, use a distinct **Merge folders** decision: merging adds/updates descendants but never deletes destination-only content, and each descendant file collision still follows the policy above. If the destination changes after the prompt, revalidate immediately before commit and prompt again rather than using stale approval.

## Drag/drop and input

- Dragging selected rows across panes starts the same copy command as the transfer buttons; the drop target says **Copy to Remote** or **Copy to Local**. No modifier turns it into a move.
- Dropping external OS files onto the Remote pane uploads them after the same collision checks. Dropping onto a visible folder targets that folder; otherwise it targets the open folder.
- Dragging remote files out to the desktop requires platform-specific promised-file support and is deferred if it cannot be made reliable cross-platform. The Download action remains the accessible path.
- Dragging inside one pane only changes selection; it does not reorder or move files.

Keyboard behavior when a file list has focus:

- Arrows navigate; Shift extends selection; Space toggles selection; Enter opens a folder.
- Tab moves between Local and Remote lists; ordinary focus traversal reaches both path bars, filters, transfer actions, and the queue.
- `Ctrl/Cmd+Enter` copies the selection to the opposite pane.
- `Alt+Left`, `Alt+Up`, and `Alt+Home` mean Back, Up, and Home for the focused pane.
- `Ctrl/Cmd+L` focuses its path; `Ctrl/Cmd+F` focuses its filter; `Ctrl/Cmd+R` refreshes it; Escape clears the transient field/dialog first.
- Session switching keeps the existing `Ctrl+Tab` / `Ctrl+Shift+Tab` behavior. Every action is also discoverable through the command palette and dispatches one typed application command.

## States

- **Loading:** retain the path chrome; replace rows with a restrained progress line, not fake skeleton filenames.
- **Empty:** `This folder is empty`, with no decorative illustration.
- **No filter results:** `No items match “…”` and **Clear filter**.
- **Remote disconnected:** keep the last successfully loaded listing visibly stale/read-only, disable transfers, show **Disconnected** with **Reconnect**. Never imply stale data is current.
- **Path/permission error:** keep the prior valid path/listing; show a concise inline error near the path plus Retry/Details as applicable.
- **Initial connection error:** show the ordinary fesTerm SSH error treatment in the SFTP surface with Back to profile, Retry, and Details.

## Deletion decision

**Defer delete in v1 on both panes.** Delete is destructive, remote trash semantics are inconsistent, local/remote asymmetry is confusing, and transfer is the core job to validate first. Do not render disabled Delete buttons, accept a Delete keybinding, or expose drag-to-trash. Users can delete through their terminal or OS file manager. Revisit only with a separately reviewed policy covering confirmation, recursive folders, symlinks, remote trash absence, permissions, partial failure, auditability, and recovery.

## Accessibility and visual fit

Use the approved blue-graphite roles (`surface.terminal #11161e`, `surface.tab.inactive #1a222c`, `surface.tab.active #29333e`, `text.primary #e8edf2`, `accent.primary #42bfd0`) with cyan reserved for focus and high-information accents. Keep 16 px icons inside at least 24 × 24 logical hit targets, compact row density, visible focus outlines, status text paired with color, and accessible names that include pane identity (for example, “Refresh Remote folder”). At narrow widths, keep the two-pane model by allowing a focused-pane mode toggle rather than crushing both tables; the user can switch Local/Remote while the transfer queue remains available.

This narrow-width fallback is the precedent `docs/mobile-layout-design.md`
generalizes into breakpoint-driven Wide/Compact/Minimal tiers for a possible
future mobile client (see ADR 0031, exploratory/not scheduled): Compact
renders this same two-pane model stacked top/bottom instead of left/right,
and Minimal reuses this exact focused-pane toggle unchanged. That document
also reframes `SftpPaneOrderPreference` as orientation-neutral
Local-primary/Remote-primary — this pane order preference remains the one
mechanism controlling which side (or, on a future mobile client, which
top/bottom position) Local and Remote occupy; no separate mobile preference
should be introduced.

## Acceptance sequence

1. From a running `production-db` SSH chip, invoke **Open SFTP**; a sibling SFTP chip appears and reuses trusted host/profile context.
2. Browse Local `/Users/fes/Downloads/release` and Remote `/srv/releases/2026.09` using breadcrumbs, keyboard navigation, sorting, and folder filters.
3. Select `festerm-0.1.0.tar.gz` locally and upload with the center action or `Ctrl/Cmd+Enter`.
4. When the remote file already exists, verify the collision dialog defaults to Skip and presents Replace, Skip, Keep Both, metadata, and batch-only Apply to all.
5. Choose Keep Both; observe queued/running byte progress, the deterministic destination name, completion, refreshed Remote listing, and a concise success state.
6. Simulate one permission failure; verify the affected row offers Retry/Details, other work continues, and no existing destination was silently overwritten.
