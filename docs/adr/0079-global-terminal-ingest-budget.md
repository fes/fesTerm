# ADR 0079: Global Terminal Ingest Budget

- **Status:** Accepted; owner-directed execution of the reviewed performance plan
- **Date:** 2026-11-07
- **Supersedes:** None
- **Preserves:** ADRs 0007, 0014 and 0017

## Context

The desktop application pumps session events before painting, so ordinary
output is visible in the current frame. The previous time budget was scoped to
each pump, while every session also had an independent event/byte cap. With
multiple busy sessions, total synchronous terminal parsing could therefore
grow with the number of sessions. The active session was also pumped again
after painting, outside the first pump's budget.

The design must preserve event and input order, make progress across every
session, and yield often enough for GUI input and painting. A display-frame
rate limit is not a substitute for bounded main-thread work.

## Decision

Give all desktop sessions one shared **4 ms terminal-ingest budget per
application update**, checked between bounded session slices. Each slice
processes at most 64 events or 256 KiB of ingested bytes. A single event may
cross the slice threshold by at most that event's bounded payload. A 16 MiB
aggregate byte threshold stops further slices; the final slice may cross it
by at most one bounded slice.

Rotate the first session pumped after each slice and update, and continue
round-robin only for sessions whose slice reports remaining work. When time or
the aggregate threshold is reached, preserve queued events and request another
frame. Output consumed before painting remains visible in that frame; remove
the second post-paint active-session pump.

Keep parsing and terminal state mutation on the existing UI thread. Preserve
each session's ordered bounded write queue. Replies produced by the pre-paint
output prefix follow writes already accepted by the transport and use the
existing pending-queue insertion rule: append after ordinary pending writes,
or insert at the clipboard barrier before its held suffix. They are forwarded
before keyboard or paste input newly admitted by that frame. Keyboard and
bracketed-paste encoding uses the terminal modes after that frame's ingested
prefix, not mode changes still waiting in deferred output. This decision adds
no priority queue or parallel parser.

For the local PTY backend, a reader blocked by the bounded output event queue
or byte budget waits for queue space instead of retrying on a timer. Consumption
and cancellation signal the same condition under the existing metrics lock, so
the reader cannot miss a wakeup between checking the bound and waiting. A
backpressure notice still precedes resumed output. Shutdown and failure wake
the reader even when the command queue is full; after cancellation it continues
draining the PTY without publishing. The ordered command queue and its
automation reservations are unchanged.

## Alternatives considered

- Keep a per-session time budget: aggregate work grows with session count and
  does not prevent one session from repeatedly consuming a full budget.
- Pump only the active session: background sessions stop making progress.
- Move parsing to a worker: requires a separate design for parser ownership,
  ordering, resize, replies and terminal snapshots.
- Remove all ingest time limits: maximizes drain throughput but leaves GUI
  responsiveness dependent on backlog size.
- Keep retrying a full PTY output queue every 5 ms: that wakes without space
  becoming available and can delay resumption.
- Use a blocking channel send for output: a full queue could prevent
  cancellation and bounded shutdown.

## Consequences

Busy sessions share bounded UI-thread work and receive round-robin service;
backlogs may take additional frames to drain. The budget can reduce single
session throughput at low repaint rates, so no end-to-end speedup or input
latency improvement is claimed without qualified measurements. The byte limit
does not split ordinary transport events; one bounded event can cross the
per-slice accounting threshold. Recovery mirror commands are capped by the
session protocol at 64 KiB, below the 256 KiB slice limit.

This decision does not change terminal rendering, session ownership, transport
queue capacity, background-output indicators or input encoding. It does not
cap frame rate or establish physical input-to-display latency.

The PTY reader no longer repeatedly increments the backpressure counter while
space is unchanged, so that diagnostic count is not comparable to the prior
timer-retry count. The control worker's existing idle polling remains. If the
child exits while its output cannot be consumed, the existing reader-stop
timeout still initiates cancellation and wakes the reader.

## Validation impact

- **Invariants introduced or changed:** Shared per-update ingest budget,
  round-robin progress, bounded session slices and continuation repaint;
  the local PTY reader waits for space or cancellation without changing output
  bounds or ordering; per-session ordered input/reply semantics remain unchanged.
- **GUI/action edges affected:** `TERM-01` and background session progress
  represented by `CHIP-01`, plus existing `CLOSE-*` and `STOP-01` shutdown;
  no new user action or indicator is introduced.
- **Automated tests required:** `global_ingest_budget_rotates_busy_sessions_between_updates`,
  `ingest_byte_budget_bounds_a_single_session_slice`,
  `ingest_byte_budget_stops_at_the_session_slice_ceiling`,
  `pending_commands_survive_backpressure_and_preserve_reply_input_order`,
  `keyboard_multiline_paste_preserves_source_order_in_both_terminal_modes`,
  the six-session aging/pump tests, `bounded_output_queue_reports_pressure_before_resuming_output`,
  `queued_output_bytes_apply_backpressure_before_the_event_queue_fills`,
  `output_never_resumes_ahead_of_the_pressure_that_stalled_it`,
  `blocked_output_wakes_on_shutdown_with_a_full_command_queue`,
  `reader_drains_without_publishing_after_blocked_output_is_cancelled`, and
  `blocked_reader_delivers_queued_output_then_observes_eof`.
- **Native/manual evidence required:** CP-17 remains the controlled native
  output-workload gate; this change adds no separate manual workflow.
- **Coverage superseded:** None.
