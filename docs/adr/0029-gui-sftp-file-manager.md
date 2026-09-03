# ADR 0029: GUI SFTP file manager surface

- **Status:** Proposed
- **Date:** 2026-09-03
- **Supersedes:** None

## Context

ADR 0028 deliberately shipped SFTP first as a text-mode transcript surface.
That remains valuable for script-like, keyboard-first file work, and its
transport/authentication boundary is still the right one: `festerm-ssh` owns
the authenticated SSH handle and the `"sftp"` subsystem channel.

The approved GUI SFTP design now asks for a different product surface: a
two-pane file manager that opens beside an SSH terminal in the chip row rather
than inside a terminal transcript. The design also requires a subtle but
important ownership rule: when opened from a live SSH session or a saved SSH
profile, the GUI surface may reuse trusted host/profile/authentication context,
but it must remain a separately typed lifecycle. Closing the GUI must not close
the shell tab; closing the shell tab must not tear down an already-open GUI
file manager.

Existing architecture constrains how to do this honestly.

- ADR 0014 says `SessionTab` owns exactly one terminal viewport and that
  split-pane UI is not part of that model. Forcing a two-pane file manager into
  `TabContent::Session(Box<SessionTab>)` would overload a terminal-specific
  surface with non-terminal ownership and layout concerns.
- ADR 0028's text-mode SFTP path already created
  `ApplicationSession::Sftp`, `SftpTerminalSession`, `WorkspaceTab::SftpSession`,
  and `TabContent::SftpAuthenticationRequired(...)` specifically for a
  transcript UX. Reusing those types directly for the graphical surface would
  blur two intentionally different products.
- The current `SftpSession` backend already provides useful low-level building
  blocks: connect/open/close, remote and local working-directory tracking,
  remote listing, single-file get/put, relative-path resolution, explicit
  overwrite refusal (`create_new` / `EXCLUDE`), and best-effort cleanup of
  failed partial files.

The current backend is still insufficient for the GUI requirements. The design
needs capabilities that do not exist yet:

- a reusable directory-listing snapshot with metadata suitable for a sortable
  table, including modified time and a stable UI-oriented entry model;
- a queued, cancellable, multi-item transfer engine with progress events rather
  than one blocking `get`/`put` result per command line;
- recursive folder-copy planning for the design's merge-folders semantics;
- collision-decision types covering Replace, Skip, Keep Both, and Merge
  folders, including a batch-scoped "apply to all remaining conflicts";
- explicit disconnected/stale-listing state and reconnect behavior for the GUI
  surface; and
- a global pane-order preference in `InterfaceSettings`, following the
  repository's additive settings pattern.

The current egui codebase also provides some guidance about fit and
conventions:

- egui drag-and-drop is already used in the app for profile reordering, so
  cross-pane drag sources/targets are feasible with existing UI primitives.
- application-owned modal confirmation overlays already exist and match the
  collision dialog requirement.
- OS file drops are currently intercepted globally for local terminal-path
  insertion only, so GUI SFTP drop targets must deliberately preempt or bypass
  that path when an SFTP file-manager surface is active.
- the repository does not currently use `egui_extras::TableBuilder` or any
  existing sortable-table/progress-drawer abstraction, so the GUI should be
  composed from existing egui layout/widgets rather than introducing a second
  UI framework. Adding `egui_extras` remains optional, not assumed.

## Decision

fesTerm will add a first-class graphical SFTP file-manager surface that
coexists with, but does not replace or subsume, ADR 0028's text-mode SFTP tab.

### The GUI surface is a distinct application surface, not another `SessionTab`

The two-pane GUI SFTP surface will be modeled as its own `TabContent` variant,
for example `TabContent::SftpFileManager(SftpFileManagerTab)`, with a matching
workspace metadata variant such as
`WorkspaceTab::SftpFileManager(SessionTabConfiguration)`.

It will **not** be represented as `TabContent::Session(Box<SessionTab>)`, and
it will **not** reuse `ApplicationSession::Sftp` directly as its user-visible
surface type. That existing type continues to mean "text-mode SFTP transcript."

This preserves ADR 0014's ownership rule: terminal-backed session tabs still
own one terminal viewport, while the graphical SFTP surface is a sibling
application surface with its own UI state, selection model, sorting, drawer,
and modal dialogs.

Workspace restore for the GUI surface remains metadata-only, just like SSH and
text-mode SFTP restore today. Restoring a saved GUI SFTP tab recreates the
surface in an auth-required or reconnect-needed state using only destination
metadata; it does not resume a live authenticated subsystem channel or a prior
transfer queue.

### GUI and text-mode SFTP share connection context, not tab identity

The GUI file manager and text-mode SFTP features share the same SSH destination
identity, host-key trust policy, and authentication/profile sources. There is
still only one SSH profile kind; the GUI feature does not introduce a second
SFTP-only host-profile schema.

When the GUI surface is opened from:

- a saved SSH profile,
- a launcher SFTP form, or
- a live SSH shell tab,

the app may seed the new GUI tab from application-owned connection context
containing the non-secret profile identity plus any currently valid trust/auth
decision inputs that are safe to reuse for a fresh SFTP subsystem connection.

That reuse is strictly a launch convenience. The GUI tab then owns its own SFTP
connection attempt, directory snapshots, transfer queue, and disconnect state.
It does not become a child lifecycle of the shell tab, and the shell tab does
not become a hidden owner of the GUI transfer queue.

### `festerm-ssh` keeps the transport boundary and grows new GUI-oriented APIs

The existing `SftpSession` remains the transport-facing owner of one live SFTP
subsystem connection. Its current methods should be refactored and reused as
the low-level primitives beneath the GUI, not bypassed from `app/festerm`.

The implementing change should extend `festerm-ssh` with GUI-facing types that
are independent from the transcript command parser:

#### 1. Directory snapshot API

Add stable listing types along these lines:

```text
SftpDirectorySnapshot
  side: Local | Remote
  path: PathBuf/String
  loaded_at: SystemTime
  entries: Vec<SftpDirectoryItem>

SftpDirectoryItem
  name: String
  absolute_path: PathBuf/String
  item_type: Directory | File | Symlink | Other
  size_bytes: Option<u64>
  modified_at: Option<SystemTime>
  permissions: Option<u32>
```

These types exist so the GUI can sort/filter without reparsing transcript text.
The current `SftpDirectoryEntry` can either be extended or left as the
text-mode-facing adapter while the GUI consumes the richer snapshot type.

Snapshots must preserve enough metadata to render the Name/Size/Modified/Type
columns and to support stale read-only rendering after disconnect.

#### 2. Transfer queue API

Add an event-driven transfer manager, for example:

```text
SftpTransferManager
SftpTransferRequest
SftpTransferId
SftpTransferBatchId
SftpTransferDirection = Upload | Download
SftpTransferEvent
SftpTransferState
```

Required behavior:

- queue multiple independent items;
- process items without blocking unrelated queued work;
- emit aggregate and per-item progress, including bytes transferred where
  knowable;
- allow cancellation of pending items and the current running item;
- keep completed/failed/cancelled terminal states queryable for the drawer;
- refresh the affected destination listing after each committed item; and
- keep event payloads content-free, never embedding file contents.

Progress delivery should be an event stream or callback sink invoked at chunk
boundaries, not a polling UI that re-reads raw file sizes opportunistically.

#### 3. Collision-decision API

Add explicit collision modeling, for example:

```text
SftpCollision
SftpCollisionDecision = Replace | Skip | KeepBoth | MergeFolders
SftpCollisionScope = ThisItem | RemainingConflictsInBatch
```

Required semantics:

- never silently overwrite an existing destination file;
- pause only the conflicting item while unrelated queue items continue;
- revalidate immediately before commit if the destination changed after the
  prompt;
- `KeepBoth` delegates destination-name generation to backend code so naming is
  deterministic and shared by button-initiated and drag-initiated transfers;
- `MergeFolders` applies only to directory-to-directory name conflicts and
  means "copy descendants into the destination tree without deleting
  destination-only content"; and
- descendant file conflicts inside a merge continue through the same collision
  pipeline rather than inheriting a hidden overwrite rule.

The "apply to all" memory is scoped to one `SftpTransferBatchId` only. It is
never persisted to settings or profiles and must be cleared once that batch has
no remaining unresolved conflicts.

#### 4. Partial-file handling

The transfer manager should reuse the current best-effort cleanup discipline
from `get`/`put`, but formalize it for GUI transfers:

- copy into a temporary sibling name when the relevant local/remote backend can
  support that safely;
- rename into the final destination only after successful completion; and
- report when cleanup of a canceled/failed temporary could not be completed.

Uploads and downloads should use the same policy shape so the drawer can report
truthful, symmetric states.

### The GUI file-manager tab owns an explicit UI state machine

`SftpFileManagerTab` will own non-terminal surface state for:

- current connection state (`Connecting`, `Ready`, `DisconnectedStale`,
  `AuthRequired`, `Failed`);
- local and remote navigation histories;
- one snapshot plus filter/sort/selection state per pane;
- pane order preference application;
- transfer drawer visibility and queue projections; and
- collision dialog state for the currently paused conflict, if any.

The GUI surface should route actions through typed app commands just like the
rest of the application. Typical commands include:

- open SFTP file manager from a live SSH tab or saved profile;
- navigate local/remote pane;
- refresh pane;
- change sort/filter;
- queue upload/download;
- cancel transfer;
- answer collision dialog; and
- reconnect the remote SFTP surface.

This keeps command palette discovery and accessibility aligned with existing
application-command routing rather than hiding logic in widget-local closures.

### Add one new persisted preference and keep the rest of pane state ephemeral

`InterfaceSettings` gains a new additive, defaulted enum preference for pane
order, for example:

```text
SftpPaneOrderPreference = LocalLeft | RemoteLeft
```

This is persisted globally because the design explicitly calls it out as a
global Settings choice.

The following remain ephemeral tab/pane state in v1 unless a later ADR says
otherwise:

- current local and remote working directories;
- per-pane sort order;
- per-pane filters;
- per-pane selection;
- per-pane show-hidden toggles; and
- active collision-dialog/apply-to-batch state.

That keeps workspace restore aligned with ADR 0014's metadata-only posture and
avoids silently reviving stale remote directory state as if it were current.

### Deletion is explicitly out of scope for v1

The GUI SFTP surface will not expose delete buttons, delete keybindings,
drag-to-trash, or implicit delete side effects in v1.

Backend APIs added for this ADR therefore do **not** include local/remote
delete queue operations. A future delete policy needs its own review because it
changes safety, auditability, and recovery semantics in ways that file copying
does not.

## Alternatives considered

### Reuse `SessionTab` and render the two-pane manager inside the terminal tab

Rejected. ADR 0014 explicitly keeps session tabs terminal-shaped and limits
them to one terminal viewport. The GUI surface needs different ownership,
selection, drawer, modal, and stale-listing state than a transcript tab.

### Replace ADR 0028's text-mode SFTP surface with the GUI file manager

Rejected. The text-mode workflow already exists, has a different ergonomic
target, and remains useful for command-driven tasks. The design calls for a
distinct GUI application surface, not a retroactive redefinition of the
existing transcript feature.

### Let `app/festerm` talk to `russh-sftp` directly for the GUI only

Rejected. That would duplicate the repository's host-key verification,
authentication prompting, subsystem startup, error sanitization, and worker
thread boundaries that already exist in `festerm-ssh`.

### Add delete in the same slice

Rejected. The approved design explicitly defers delete in v1, and bundling it
here would widen risk, review scope, and validation burden without helping the
core transfer workflow.

## Consequences

- fesTerm will have two first-class SFTP surfaces: the existing transcript tab
  and a new graphical file-manager tab.
- `festerm-ssh` becomes the owner of richer non-transcript SFTP APIs: listing
  snapshots, queued transfers, collision decisions, and reconnect-aware GUI
  state propagation.
- `app/festerm` gains a new application-surface tab type rather than forcing
  non-terminal behavior through `SessionTab`.
- Settings gain one new additive global preference for pane order.
- The GUI implementation remains intentionally copy-only in v1 and explicitly
  does not promise delete, remote-file desktop drag-out, or resumed queues on
  restore.

## Validation impact

- **Invariants introduced or changed:** GUI SFTP is a first-class application
  surface distinct from text-mode SFTP; GUI and text-mode SFTP share SSH
  profile/trust/auth context but not tab lifecycle; queued GUI transfers never
  silently overwrite; collision approvals are batch-scoped and revalidated
  before commit; delete is absent in v1.
- **GUI/action edges affected:** Planned new edges include `LAUNCH-10` (open a
  GUI SFTP tab from a saved SSH profile or live SSH tab), `SFTP-GUI-01`
  (browse both panes and change sort/filter/path state), `SFTP-GUI-02` (queue
  an upload/download and observe progress/cancel states), `SFTP-GUI-03`
  (resolve a collision with Replace/Skip/Keep Both), and `SFTP-GUI-04`
  (show stale remote listing and reconnect affordance after disconnect).
- **Automated tests required:** Planned coverage includes
  `sftp_directory_snapshot_contains_sortable_metadata`,
  `sftp_transfer_manager_emits_progress_and_completion`,
  `sftp_transfer_manager_cancels_pending_and_running_items`,
  `sftp_collision_apply_to_all_is_limited_to_one_batch`,
  `sftp_keep_both_generates_deterministic_sibling_names`,
  `sftp_merge_folders_preserves_destination_only_descendants`,
  `gui_sftp_workspace_restore_requires_fresh_authentication`, and
  `interface_settings_parse_sftp_pane_order_additively`.
- **Native/manual evidence required:** Manual evidence is required for
  cross-pane drag/drop, external OS-file drop to the remote pane, stale remote
  listing presentation, keyboard navigation, collision safety defaults, and
  focused-pane narrow-width behavior. Stable scenario IDs should be added in
  the implementing change.
- **Coverage superseded:** None yet. `validation/traceability.json` should be
  updated in the implementing change that wires the new GUI SFTP edges and test
  relationships into real coverage.
