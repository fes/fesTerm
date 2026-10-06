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
- bound admitted batches, queued items, recursive-plan size, command delivery,
  and event delivery with explicit observable backpressure;
- process items without blocking unrelated queued work;
- emit aggregate and per-item progress, including bytes transferred where
  knowable;
- allow cancellation of pending items, recursive planning, and the current
  running item;
- keep the most recent 128 completed/failed/cancelled/skipped terminal records
  queryable for the drawer, retiring older finished records with visible
  accounting without evicting active or collision-paused work;
- refresh the affected destination listing after each committed item; and
- keep event payloads content-free, never embedding file contents.

Progress delivery is a bounded event stream invoked at chunk boundaries.
Intermediate progress may be coalesced to the newest value while collision,
terminal-state, and destination-refresh events remain ordered and lossless.
Recursive planning shares 65,536 owned items and a 64-MiB conservative
metadata allowance per live worker, rather than granting those limits to each
plan. Enumeration, retained/collision-paused units and overlapping managed
container storage share actual-owner reservations. A row transfers its
reservation into its unit; progress/cancellation releases metadata without
evicting another admitted plan. New planning that cannot fit fails explicitly
before destination materialization. Per-item path/collision projection is
conservative, not a process-memory measurement.

Local enumeration checks admission before retaining rows; remote enumeration
uses the existing subsystem's paged protocol client and never first collects
the whole directory. The owner approved a narrowly vendored `russh-sftp` 2.3.0
getter because its convenience API eagerly collects and repeatedly recopies
all prior pages. Its license/provenance and exact patch are recorded in
`vendor/russh-sftp/PATCHES.md`; no second SSH/SFTP connection is introduced.
The decoded current reply is charged before conversion, but protocol-private
pre-decode allocations remain outside the bound. Ordinary browsing snapshots,
queued request/event/public-snapshot payloads and allocator/private-I/O
behavior also remain separate scopes. Sparse queue retirement moves at most
128 live entries, releases the old allocation before rebuilding, and retains
a requeue slot.

Changed-row snapshot publication preserves stable ordering and indexed
updates without per-unit whole-inventory reconstruction. Membership
compaction/reindexing and explicit public snapshot requests remain bounded
full-vector operations; unchanged request allocations are retained.

The tab-owned command/event bridges are also bounded (64 commands, 128 events).
Frontend polling and worker transfer-event batching each consume at most 64
events per invocation; only adjacent same-batch/same-transfer progress is
coalesced. Async worker sends await capacity without blocking a runtime thread.
The dedicated local loader may block on delivery and unblocks when the receiver
retires. Owner cancellation bypasses queue admission and interrupts those waits.
Observed cleanup notices are reported separately before awaiting GUI event
capacity; their buffer markers remain progress barriers, so owner cancellation
cannot discard a report already extracted from the backend.
GUI admission checks the existing 256-item transfer batch ceiling before
retaining the batch. Refused actions use the existing nonfatal error surface:
navigation does not change pane/history/scroll/loading state, Markdown fetch
does not replace an admitted pending request, external drop reports failure,
reconnect does not claim a new connection attempt, and a collision decision is
not dismissed before admission.

After successful backend batch admission, the worker publishes the receipt's
ordered transfer IDs paired with request metadata before forwarding engine
events. This creates Queued rows for pre-start collisions, Skip and pre-copy
failures; a refused batch creates no rows. The extra metadata clone is bounded
by the existing 256-item batch ceiling, not a payload-byte budget.

The drawer's bulk Cancel crosses each command bridge as one command. Full or
closed admission refuses the entire action visibly. The manager flags all
existing items and rebuilds the ready queue once, sorting by stable transfer ID;
it does not perform an individual `ready.retain` traversal per canceled item.
Queued and collision-paused work preceding the command is canceled; later
admitted work is unaffected. Single-item and batch-specific cancellation retain
their existing scope.
Collision presentation rechecks the indexed row's active state, retiring a
stale prompt when a cancellation/terminal event overtakes its presentation.

The finished-history window is completion-ordered, includes failures and uses
indexed transfer-ID lookup and stable transfer-scoped widget identity.
Exceptional row/index capacity retires with
hysteresis. A visible cumulative retirement count explains missing old rows;
the ordinary Clear action still preserves retained failures. A 240-logical-pixel
scroll region bounds drawer geometry without claiming row virtualization or
a total payload-byte budget. Aggregate snapshots/plans remain a distinct
admission concern.

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

Exclusive creation proves ownership only at creation. A later pathname can
resolve to a replaced leaf or ancestor, and checking metadata before unlink
does not remove that race. The current local and remote APIs therefore preserve
failed/canceled partials and report their paths instead of deleting them.
Interrupted destructive replacement also preserves recovery output and reports
both uncertain paths; reporting releases the ledger for unrelated work.

Closing the owner cancels connection, trust, browsing/read and transfer waits.
Cleanup has a two-second grace, followed by non-waiting runtime retirement;
already-started blocking I/O cannot be forcibly canceled. Incomplete cleanup
uses a bounded notice queue in the current owning window or content-free
diagnostics if delivery is unavailable. Final process exit can interrupt
asynchronous cleanup; it is not a rollback guarantee.

Preserved-output notices do not change collision or cancellation control flow.
A destination appearing during copying still offers Replace, Skip and Keep
Both, without silently discarding the completed temporary.

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
  before commit; delete is absent in v1; owner cancellation preserves and reports
  partials when race-free current ownership cannot be established; GUI bridges,
  per-poll work and finished history are bounded with explicit admission
  refusal and retirement accounting. Recursive enumeration and retained plans
  share a per-worker allowance; metadata credit follows actual data ownership
  through copy, collision, cancellation and exceptional-capacity retirement.
- **GUI/action edges affected:** `LAUNCH-10` opens a GUI SFTP tab from a
  saved SSH profile or live SSH tab. `SFTPG-01/02/03` cover browsing,
  transfer/cancel/history and collision decisions; `SFTPG-04` retains deferred
  stale-listing/reconnect evidence. Owner teardown refines `SFTPG-08`.
  Bounded GUI work refines `SFTPG-01/02/03/04/05/06/08` (including reconnect,
  Markdown fetch and drag/drop admission) without changing the
  application/transport ownership boundary or accepting native disconnect
  recovery.
- **Automated tests required:** Planned coverage includes
  `sftp_directory_snapshot_contains_sortable_metadata`,
  `sftp_transfer_manager_emits_progress_and_completion`,
  `sftp_transfer_manager_cancels_pending_and_running_items`,
  `sftp_collision_apply_to_all_is_limited_to_one_batch`,
  `sftp_keep_both_generates_deterministic_sibling_names`,
  `sftp_merge_folders_preserves_destination_only_descendants`,
  `gui_sftp_workspace_restore_requires_fresh_authentication`, and
  `interface_settings_parse_sftp_pane_order_additively`. Owner-teardown
  regressions include
  `owner_shutdown_interrupts_copy_and_reports_uncertain_partial_output`,
  `cancellation_cleanup_preserves_a_confirmed_remote_partial_file`,
  `owner_shutdown_during_replace_preserves_the_completed_temporary_for_recovery`,
  `gui_sftp_cleanup_report_outlives_the_tab_and_follows_its_current_window`,
  `late_collision_preserves_and_reports_output_without_losing_any_decision`,
  `cancelled_copy_reports_preserved_output_and_keeps_cancelled_state`,
  `live_gui_copy_cancellation_reports_partial_output_without_losing_cancelled_state`,
  and `gui_sftp_preserved_output_notice_reports_a_recovery_path`.
  Backlog/history regressions include
  `gui_sftp_admitted_prestart_outcomes_enter_finished_history`,
  `gui_sftp_refused_backend_admission_creates_no_phantom_history`,
  `gui_sftp_terminal_rows_are_ineligible_for_a_collision_modal`,
  `gui_sftp_bulk_cancel_header_refuses_whole_action_then_uses_one_slot`,
  `collision_before_start_and_skip_emit_no_item_started`,
  `bulk_cancel_refuses_full_admission_then_cancels_all_items_with_one_slot`,
  `bulk_cancel_finishes_more_than_one_command_queue_of_collision_paused_items`,
  `bulk_cancel_does_not_cancel_work_enqueued_after_the_command`,
  `gui_sftp_finished_history_retires_old_failures_at_the_approved_limit`,
  `gui_sftp_history_keeps_active_rows_and_retires_by_finish_order`,
  `gui_sftp_history_duplicate_finishes_and_clear_preserve_index_integrity`,
  `gui_sftp_history_reclaims_a_large_active_peak_and_reports_retirement`,
  `gui_sftp_history_retirement_preserves_the_active_rows_widget_identity`,
  `gui_sftp_command_bridge_refuses_full_and_closed_queues_without_false_success`,
  `gui_sftp_refused_reconnect_preserves_connection_and_spinner_until_admitted`,
  `gui_sftp_external_drop_and_oversized_batches_refuse_before_bridge_admission`,
  `gui_sftp_refused_navigation_preserves_back_stack_breadcrumbs_and_loading_state`,
  `gui_sftp_admitted_back_and_ancestor_navigation_restore_scroll_after_queue_recovery`,
  `gui_sftp_refused_markdown_read_keeps_the_previous_request_generation`,
  `gui_sftp_event_bridge_and_poll_budget_are_exact_and_preserve_order`,
  `gui_sftp_owner_close_preempts_a_full_async_event_bridge`,
  `gui_sftp_local_event_producer_unblocks_when_the_frontend_closes`, and
  `gui_sftp_progress_coalescing_keeps_the_latest_value_and_critical_barriers`.
  `gui_sftp_observed_cleanup_notice_survives_a_blocked_bridge_and_owner_close`
  covers the separate cleanup reporter before blocked event forwarding.
  Aggregate-planning coverage includes
  `paused_recursive_plan_refuses_aggregate_growth_and_cancel_restores_admission`,
  `multiple_paused_plans_preserve_decisions_and_resume_releases_shared_admission`,
  `shared_planning_admission_is_atomic_and_restores_exact_credits`,
  `failed_and_overflowing_growth_never_changes_shared_accounting`,
  `planning_data_drops_before_its_credit_and_outlives_a_removed_queue`,
  `planning_queue_refuses_storage_before_allocation_and_preserves_admitted_rows`,
  `sparse_planning_queue_retires_backing_and_keeps_a_collision_requeue_slot`,
  `local_planning_refuses_oversized_enumeration_and_returns_sorted_rows`,
  `remote_planning_refuses_before_another_page_and_closes_the_directory`,
  `remote_planning_returns_sorted_budgeted_rows_without_recollecting_pages`,
  `remote_planning_reports_close_failure_without_losing_the_planning_error`,
  `canceled_remote_planning_closes_its_open_handle_without_closing_the_session`,
  `transfer_snapshot_updates_only_affected_rows_and_preserves_request_allocations`,
  and `transfer_snapshot_keeps_batch_order_and_updates_resolved_collision`.
- **Native/manual evidence required:** Manual evidence is required for
  cross-pane drag/drop, external OS-file drop to the remote pane, stale remote
  listing presentation, keyboard navigation, collision safety defaults, and
  focused-pane narrow-width behavior. Stable scenario IDs should be added in
  the implementing change. `SFTP-01` retains packaged owner close/quit,
  diagnostic visibility and cleanup-notice accessibility qualification.
  `SFTP-03` retains native long-history scrolling, full-queue refusal and
  retirement-notice readability/accessibility qualification, plus shared-plan
  exhaustion, progress/cancel followed by Retry, and preserved paused-decision
  usability on native Windows/macOS/Linux.
- **Coverage superseded:** None yet. `validation/traceability.json` should be
  updated in the implementing change that wires the new GUI SFTP edges and test
  relationships into real coverage.
