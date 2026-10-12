# ADR 0047: Bounded Local Document Background I/O

- **Status:** Accepted (owner architectural approval on 2026-10-12; native qualification remains CP-15)
- **Date:** 2026-10-12
- **Supersedes:** None
- **Refines:** ADR 0034's local freshness and debounced-save scheduling

## Context

The application frame checked eligible local files and performed debounced
local saves synchronously, while a 750-ms repaint timer ran for any nonempty
document registry. Unchanged files already have a cheap metadata fast path;
the problem is unnecessary complete UI passes and filesystem latency on the
UI thread, not unconditional hashing. Slow metadata, a changed-file load, or
durable save can delay every presented surface.

Moving writes off-thread also creates a real admission/ordering contract:
queued old bytes must not run after a newer explicit save, a delayed reload
must not erase edits, and an ambiguous save must not lose recovery evidence
when its last view closes. That contract warrants a decision despite preserving
the application's sole ownership of live document mutation.

## Decision

The application-scoped registry owns one private local I/O worker. Requests
carry immutable document ID, operation epoch, revision, loaded generation,
source authority, bounded bytes when needed, and an opaque core history save
point. The worker never accesses the live registry or a view.

There are 16 slots across queued, running, and completed requests combined,
including cancelled work until it actually retires. Each request retains at
most one bounded save byte snapshot or one bounded load completion; the
ordinary document byte/line bounds remain in force. These are retained-work
bounds reserved before copying save payloads, so refused work does not clone
the document again.
They are not process RSS or transient-allocation guarantees. Registration,
thread-start/termination and queue errors are explicit document errors with
manual Refresh/Save guidance. Closing another document can release capacity;
one coalesced capacity-release wake admits waiting work, not a retry timer.
An unexpected write panic instead poisons admission and explicitly requires
preserving unsaved text/recovery evidence and restarting; it is not mislabeled
as a transient busy write or silently recovered.

The worker owns the 1.5-second metadata freshness deadline and 900-ms typing
debounce. Unchanged available/unavailable checks and repeated identical read
errors do not wake the UI. Untitled, remote, conflicted and recovery-pending
documents have no local registration. Focus regain forces existing eligible
registrations without duplicating them or bypassing the save debounce. Changed
results wake the root event loop, which drives the shared window pass. Only
expiration of an actual reload notice needs a UI timer.

A failed autosave is not attempted again for the same revision. New content
is new information, including when its registration predates an older failed
completion; that older failure cannot cancel or pause the newer attempt.
Capacity warnings likewise cannot disable their own later admission. A worker
with no resident request waits for commands without an idle timeout. A bounded
command batch precedes due I/O, so continuously due checks cannot starve
registrations or discovery that the application has dropped the worker.
Due requests are removed without reordering their peers; unchanged checks
return to the tail. Continuously overdue freshness checks therefore cannot
alternate at the head indefinitely while starving other checks or autosaves.

The app validates ID, operation epoch, origin and loaded generation before
applying a completion. Text mutations cancel pending requests; clean reloads
additionally require the captured revision. A successful save updates authority
and the exact history point actually written, never falsely marking newer text
clean. Undo to the former pre-save baseline is therefore dirty. Cancellation
is checked before preparation and again before any transactional displacement.
After that publication boundary, existing verification/rollback/recovery must
finish; cancellation cannot undo a committed write.

All local writes are serialized by one private write admission gate. Worker
writes hold it through completion enqueue, then release it before notifying
the app. A notified completed save therefore no longer occupies admission.
Explicit Save/Save As remain
synchronous when admitted, retaining their existing success/rebinding/close
semantics. They cancel queued background work and use **nonblocking** gate
admission; ordinary freshness reads do not occupy the gate. Only a genuinely
running local write can refuse a concurrent explicit command, with the
existing actionable Retry/Save As banner: wait for completion, then retry.
There is no automatically repeated command or refusal storm. Explicit Refresh
and Reload likewise avoid racing an active publication. Their explicit reads,
and initial Open, are still synchronous; this decision optimizes automatic
frame work, not all user-requested file operations.

Pending completions are drained again after explicit gate admission and before
invalidating operation epochs, so a just-finished background generation cannot
be missed. Old queued autosaves cannot run after the admitted newer write.
Ambiguous publication always queues its exact recovery breadcrumb and blocks
any still-open or reopened same-source document, even if the originating view
closed or its request was cancelled. Remote SFTP saves remain on their existing
asynchronous transport.

Unresolved recovery protection lives independently of documents and notification
acknowledgement in the application registry. Up to 64 records retain the source
path/authority and exact recovery path/error, including loaded identity handles
for ordinary and background saves. Save As retains its confirmed generation's
identity as denial-only evidence without extending the confirmation handle's
publication-sensitive lifetime. Opens inherit matching protection, and
Save As checks protected destinations even when no destination view is open.
Closing a view, draining a completion before reopening, acknowledging a notice,
editing, or reloading cannot resolve it. Protection does not authorize any
filesystem operation. On protection overflow, existing records remain intact
and one explicit overflow error with the first untracked exact breadcrumb
refuses **all** local writes (including Save As and autosave) for the rest of
the application lifetime; the separately bounded notice queues/logs still
report subsequent breadcrumbs. There is no eviction or automatic resolution.

## Alternatives considered

- A longer or eligibility-only UI repaint timer reduces frames but leaves
  slow filesystem work blocking the UI.
- Filesystem watchers are hints, not authority; they do not replace bounded
  freshness/load/save work or cover unavailable sources reliably.
- Unbounded per-document threads or queues violate retained-work bounds.
- Blocking explicit Save on a worker defeats the responsiveness objective.
- Converting every explicit command and dirty-close/Save As continuation to
  asynchronous operations is a broader product/command contract change.
  The narrow, visible in-flight-write refusal is preferable for this change.

## Consequences

Crate responsibilities, dependency direction, path identity, document/view
ownership and transactional safe-save security are unchanged. The pure core
adds an opaque history checkpoint API, not filesystem policy. No security
privilege, generation check, Unix metadata preservation or Windows handle/
publication rule is weakened. One slow filesystem operation can delay other
worker requests, but not the UI; cancellation cannot interrupt an OS call
already in progress. More than 16 eligible local documents explicitly lose
automatic coverage until capacity is available; manual operations remain.
Recovery protection is deliberately conservative for a reused source pathname.
Manual recovery must be completed before restarting to discard application-
lifetime protection; startup recovery discovery remains out of scope.

Native/network-volume latency, real multi-window focus/repaint delivery,
privileged Windows replacement, and shutdown during blocked OS I/O remain
native qualification. Releasing retained native identity handles while
adopting or discarding a completion can still occur on the application thread;
controlled tests isolate automatic freshness/load/save execution, not every
native handle-destruction or allocation latency. No new desktop acceptance or
performance percentage is claimed by CPU controlled-worker tests.

## Validation impact

- **Invariants introduced or changed:** bounded immutable local work, exact
  asynchronous history save point, completion-only idle wake, cancellation
  before publication, fair due-request rotation, nonblocking explicit local
  write admission and bounded view/notification-independent recovery protection
  with fail-closed overflow.
- **GUI/action edges affected:** `EDIT-04`, `EDIT-08`, `EDIT-10`, `EDIT-11`,
  `EDIT-12`, `EDIT-14`, `MD-06`.
- **Automated tests required:**
  `background_document_io_leaves_real_frame_idle_until_completion_wake`,
  `background_worker_accepts_commands_while_existing_freshness_is_continuously_due`,
  `background_worker_fairly_services_overdue_freshness_and_autosave`,
  `background_slow_freshness_and_reload_leave_frame_nonblocking`,
  `background_unchanged_and_ineligible_documents_do_not_wake_or_time_repaint`,
  `background_reload_cancelled_by_edit_rechecks_into_conflict_without_losing_text`,
  `background_late_reload_cannot_mutate_closed_reopened_or_explicitly_reloaded_document`,
  `background_capacity_covers_queued_running_and_completed_work_and_recovers_on_close`,
  `background_completed_slots_stay_bounded_until_app_consumption_even_after_close`,
  `background_autosave_is_nonblocking_debounced_and_preserves_newer_edit_save_point`,
  `background_autosave_error_is_truthful_and_not_retried_for_the_same_revision`,
  `background_late_save_error_does_not_pause_already_registered_newer_edits`,
  `background_poll_does_not_refuse_save_as_and_cancelled_queued_autosave_never_writes`,
  `background_autosave_debounce_coalesces_edits_and_disable_cancels_a_running_prepare`,
  `background_stable_unavailable_and_read_errors_do_not_repeat_repaints`,
  `background_real_missing_source_reports_once_and_unchanged_missing_does_not_wake`,
  `background_completed_save_wakes_after_releasing_write_admission`,
  `background_recovery_completion_survives_close_and_is_not_silently_discarded`,
  `background_recovery_drained_before_reopen_still_refuses_all_same_source_writes`,
  `unresolved_recovery_protection_is_bounded_and_overflow_refuses_all_local_writes`,
  `manual_recovery_notice_survives_polling_and_names_the_recovery_directory`,
  `save_as_recovery_is_promoted_while_the_source_view_remains_open`,
  `background_worker_failure_wakes_and_surfaces_an_error_instead_of_default_success`,
  `background_write_panic_refuses_explicit_save_with_truthful_coordination_error`,
  `background_cancelled_save_refuses_before_io_or_publication`,
  `background_save_point_marks_only_the_written_history_even_after_new_edits`,
  `background_save_point_cannot_mark_a_coalesced_newer_revision_clean`.
- **Native/manual evidence required:** `CP-15`, including slow owned local/
  network fixtures, multi-window wake/focus, exceptional write-in-flight retry
  UX, native secure-save privileges and blocked-I/O teardown.
- **Coverage superseded:** None; existing synchronous explicit-operation
  transition regressions remain, with new real-worker scheduling coverage.
