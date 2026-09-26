# ADR 0038: Authoritative Terminal Snapshots for `festerm-sessiond` Reattach Recovery

- **Status:** Accepted
- **Date:** 2026-09-25
- **Approval:** Project owner, 2026-09-25
- **Updates:** ADR 0025

The project owner approved daemon-owned authoritative recovery state with a
frontend display copy after reviewing the ownership boundary and tradeoffs.
Implementation was merged in [PR #237](https://github.com/fes/fesTerm/pull/237).
Architectural acceptance does not close the separate native GUI and
signed-package validation obligations in CP-11.

## Context

ADR 0025 introduced `festerm-sessiond` as a per-session local daemon that
keeps a PTY/ConPTY shell alive across frontend exits. The first shipped
implementation bounded reattach recovery by replaying only a raw output tail.
That was operationally simple but architecturally incomplete: byte replay
cannot recover the terminal parser state, partial UTF-8/CSI/OSC/DCS
fragments, alternate-screen state, scrollback, cursor/margins, input modes,
or resize-derived layout. After an unexpected frontend exit, reattaching to a
surviving daemon could therefore present visibly wrong rendering or scrolling
until the application was manually refreshed.

Increasing the replay buffer is not a real fix. The failure is not "not
enough bytes"; it is that bytes alone are insufficient once terminal state has
meaningfully diverged from a clean power-on parser. Likewise, synthesizing a
clear-screen or forcing application-level redraws would paper over symptoms
while still dropping authoritative state and still risk replaying
side-effectful replies.

Issue #236 therefore requires a bounded, race-free recovery mechanism that:

- restores the same terminal state a continuously attached frontend would
  have had,
- does not replay device-query answers, clipboard side effects, or queued user
  input,
- preserves the existing single-live-client / steal-on-reconnect model,
- works on Unix and Windows/ConPTY,
- remains compatible with older helpers already recorded in the on-disk
  registry.

## Decision

`festerm-sessiond` will maintain an **authoritative recovery mirror** of the
session terminal as a private `festerm-core::Terminal` owned by the daemon.

The daemon will:

1. Ingest PTY/ConPTY output into that mirror in order.
2. Apply accepted resize commands to the mirror in the same order they are
   applied to the PTY.
3. Clear reply/input queues and overflow flags when cloning the mirror for
   recovery, so reattach never replays side-effectful terminal replies or
   stale GUI input.
4. Send a serialized recovery snapshot to each newly attached protocol-v2
   client **before** any post-attach live output.

The transport contract becomes:

- **Protocol v1:** legacy raw replay behavior remains readable by newer
  frontends when attaching to older daemons already on disk.
- **Protocol v2:** attach begins with a length-prefixed recovery snapshot
  prelude. The daemon withholds every post-snapshot output/control frame until
  the frontend explicitly acknowledges adoption, after which daemon-to-frontend
  traffic is framed as output, recovery controls, resize acknowledgements, exit,
  or takeover notices.

Operational details of protocol v2:

- Recovery snapshots are capped at **768 MiB**: 3x the largest supported
  256 MiB scrollback preference, leaving bounded room for visible-screen and
  cell-serialization overhead while still rejecting arbitrary advertised
  lengths before allocation.
- Snapshot decoding is accepted only over the existing owner-scoped local IPC
  channel to a compatible helper generation. Invalid magic bytes, oversized
  lengths, truncated payloads, or failed deserialization abort recovery rather
  than being repaired heuristically.
- Protocol-v2 records now carry an explicit **recovery snapshot schema
  version**. This release writes schema **2**; legacy v2 records without that
  field deserialize as schema **0** and are rejected before attach/replacement
  rather than silently taking over a live session with an unreadable snapshot.
- The serialized payload remains the helper's own `bincode` encoding of
  `festerm-core::Terminal`, but it is now decoded under the snapshot byte cap
  and then validated structurally before adoption. Screen dimensions, ring
  indices, scrollback row origins/charges, parser bounds, queued-input/reply
  emptiness, and related invariants must all hold or recovery aborts without
  replacing local state. This is intentionally a **same-protocol-epoch**
  contract, not a long-term archival format: future incompatible layout
  changes require a new snapshot schema and the existing registry/version
  negotiation.
- Schema 2 retains allocation-capacity accounting for cell text and history
  vectors. Text length is not equivalent to owned capacity, and ordinary
  serialization/clone can otherwise change future history eviction. Charges
  are validated against explicit capacity metadata before bounded allocation
  restoration; retained history allocation cannot exceed the snapshot protocol
  limit. Cumulative trimmed offsets describe discarded cells, not an index
  bounded by the currently retained suffix.
- For protocol v2, the daemon owns terminal query replies while parsing the
  mirror and forwards them to the PTY even when no frontend is attached. The
  frontend suppresses duplicate replies generated by its adopted/live terminal
  clone. Reattach must therefore answer shell/TUI DSR/DA-style queries exactly
  once, never replay clipboard side effects, and never inject a stale keystroke
  after the frontend returns.
- Frontend-owned terminal state that is not PTY-derived but affects recovery
  fidelity — current scrollback limit, embedder color scheme, GUI clear, and
  GUI reset — is synchronized back to the daemon mirror through protocol-v2
  control frames. Recovery is therefore not merely "display replay"; it
  preserves the same authoritative state a continuously attached frontend would
  have owned.

The authoritative mirror is **daemon-owned state**. The frontend remains the
only live interactive writer of user input, but on reattach it must replace
its local terminal instance with the daemon-provided recovery snapshot before
resuming ordinary event pumping. To keep that snapshot authoritative, protocol
v2 clients now temporarily hold outgoing input, resize commands, and recovery
controls until the recovered terminal has actually been adopted; only then is
the live frontend geometry forwarded. Resize acknowledgements are emitted only
after the daemon has successfully applied the resize to the PTY and mirror.
Frontend geometry, clear/reset, and configuration controls are applied from
the same ordered event stream as output. Confirmed resize uses the existing
viewport/selection reflow mapping. Saturated delivery queues retain typed
control/resize/exit frames; they cannot silently drop a state transition.

Takeover is transactional with respect to adoption: snapshot serialization
and transfer must succeed and the candidate must acknowledge adoption before
the old client is retired. Failed candidates leave the old client usable.
The daemon quiesces normal PTY/input processing during this handshake, for
at most 15 seconds; bounded PTY queues can backpressure the child meanwhile.
Already accepted old-client recovery controls remain ordered after adoption,
while stale old-client keystrokes are discarded.

Standalone protocol-v2 CLI attach uses the same snapshot acknowledgement and
framing, then renders a text-only screen projection. Raw query escapes are not
forwarded to the hosting terminal, which would generate duplicate replies.
History is emitted once on attach, not appended again on each live frame.
Full styled/native-GUI rendering is not claimed for this CLI projection.

## Rationale

This is chosen over larger replay buffers or synthetic redraws because only a
terminal snapshot can correctly preserve:

- parser/decoder state at arbitrary byte boundaries,
- screen + scrollback contents,
- alternate-screen selection,
- cursor position, margins, wrap state, and dirty-region state,
- negotiated modes such as mouse reporting and bracketed paste,
- resize history as reflected in the terminal's current modeled state.

Using `festerm-core::Terminal` as the snapshot payload keeps state ownership
aligned with the component that already models terminal correctness, instead
of creating a second, weaker "recovery state" format inside the daemon.

## Consequences

### Positive

- Reattached frontends recover the same bounded terminal state a continuously
  attached frontend would have had.
- Recovery no longer depends on arbitrary replay size.
- Query replies and clipboard/device side effects are not replayed.
- Fresh and retained frontends can be compared deterministically by comparing
  terminal snapshots.

### Costs

- `festerm-sessiond` now depends on `festerm-core` serialization-compatible
  terminal state.
- Recovery protocol negotiation is more complex because both v1 and v2 must be
  supported during rollout.
- Detached persistent sessions now pay a second in-memory copy of terminal
  state in the daemon mirror, plus snapshot serialization CPU/memory during
  attach. That duplication is bounded and accepted because exact recovery is
  otherwise impossible.
- Snapshot generation makes attach semantics explicitly stateful; tests must
  cover snapshot/live-output ordering, resize retention, and large-output
  recovery.

## Validation impact

Changes implementing this ADR must provide evidence for:

- protocol-v2 snapshot decode/reattach handling in the library client,
- daemon takeover tests proving retained state survives reconnects, resize
  changes, and frontend-owned mirror-control operations,
- native daemon integration proving large (>1 MiB) output and mode-bearing
  terminal state recover equivalently to a fresh frontend model,
- continued helper/protocol compatibility for older v1 sessions.
- saturated control/resize/exit ordering, a maximum-size frame followed by
  another frame in the same read, failed-candidate rollback, and CLI framing;
- frontend backend-owned resize deferral and output/resize/output ordering,
  with duplicate query replies suppressed and invalid controls reported.

## Rejected alternatives

### Larger replay tail

Rejected because it still cannot recover parser state, modes, or arbitrary
fragment boundaries.

### Synthetic `Ctrl-L` / forced redraw on attach

Rejected because it is application-specific, not authoritative, and still
replays none of the missing hidden state.

### Replaying terminal replies/device side effects

Rejected because it violates the single-writer / single-side-effect rule and
can repeat clipboard/query behavior incorrectly.
