# ADR 0027: SSH Port Forwarding for Profiles and Live Sessions

- **Status:** Proposed
- **Date:** 2026-09-03
- **Supersedes:** None

## Context

Milestone 7 deliberately left SSH port forwarding outside the first native-SSH
delivery. `ROADMAP.md` still classifies port forwarding as a separate future
capability, and that remains correct: port forwarding is not just "one more
SSH request." It changes what a live SSH session can expose to the local or
remote network, needs profile and live-session UX, and must preserve the
existing repository rules around safe defaults, bounded diagnostics, and
truthful reconnect semantics.

The current SSH architecture already has the right ownership boundary for the
transport work itself. `festerm-ssh` starts one dedicated worker thread per
session, feeds it bounded `WorkerCommand`s over a synchronous channel, and
returns bounded `SessionEvent`s through `WorkerShared::try_emit`. After
authentication, `run_authenticated_channel` owns the authenticated
`russh::client::Handle`, the live channel, and the command-poll loop that
already handles input, resize, explicit reconnect, shutdown, and ADR 0018's
liveness probes. That means forwarding should be modeled as additional worker
commands and additional sanitized worker events, not as ad hoc background
tasks or UI-thread socket ownership.

The configuration layer is likewise already opinionated in the right way.
`festerm-config` keeps `SCHEMA_VERSION` at `1`, uses `#[serde(default)]` for
additive compatibility, validates replacements through `Configuration::with_*`
and `Profile` helpers, and keeps SSH profiles secret-free except for opaque
native-store credential references. `SshProfileConfiguration` today carries
host, port, username, terminal metadata, optional credential references, and
optional durable-session persistence. Adding saved port forwards must fit that
same validated, additive, non-secret model rather than inventing a second SSH
profile document or forcing a schema bump for older config files that simply
lack the new field.

The application tab model also already constrains restore behavior.
`WorkspaceTab::SshSession` stores only metadata, and
`TabState::from_workspace` restores it as
`TabContent::SshAuthenticationRequired(SshAuthenticationRequiredTab)` rather
than a live connection. ADR 0018 further establishes that plain SSH reconnect
is a fresh transport decision, not silent recovery of all previous side
effects. Port forwarding must not weaken that stance by implying that a
disconnected session's listeners, binds, or exposure policy automatically
reappear just because a new SSH transport was created later.

Finally, the app already has two reusable UI seams for this capability:

- `OverlayState` is the centralized owner for application-owned blocking
  overlays and transient notices; and
- `ApplicationShortcut`, `palette_items`, and
  `dispatch_palette_selection` are the stable shortcut/palette routing path for
  discoverable live-session actions.

Those seams are sufficient for a live port-forward manager without inventing a
new top-level screen.

## Decision

### Scope: support local and remote forwarding only

fesTerm will support exactly two SSH forwarding directions in this slice:

- **Local forwarding**: bind a local listening socket and forward accepted
  connections to a destination reachable from the remote SSH server.
- **Remote forwarding**: request that the SSH server bind a listening socket
  and forward accepted connections back toward a destination reachable from the
  client side.

Dynamic/SOCKS forwarding is explicitly out of scope for this ADR. Nothing in
the current application or transport stack implements a local SOCKS parser,
policy surface, or per-request destination inspection boundary, and adding one
would require materially more product, security, and validation review than is
justified for the first forwarding pass.

### Saved SSH profiles may carry zero or more validated forward mappings

`SshProfileConfiguration` gains an additive
`#[serde(default, skip_serializing_if = "Vec::is_empty")]` collection of saved
forward definitions. Older configuration documents therefore continue to parse
unchanged under `SCHEMA_VERSION = 1`; they simply deserialize the missing field
as an empty list.

Each saved mapping records only non-secret connection metadata:

```text
SshPortForwardConfiguration
  direction: Local | Remote
  bind_host: String
  bind_port: u16
  destination_host: String
  destination_port: u16
```

Validation follows the existing `SshProfileConfiguration`/
`LocalProfileConfiguration` style:

- `bind_host` and `destination_host` must be non-empty and control-character
  free;
- `bind_host` and `destination_host` must not contain obviously secret-bearing
  values;
- `bind_port` and `destination_port` must be non-zero;
- duplicate bindings within one profile are rejected, where "duplicate" means
  the same `(direction, bind_host, bind_port)` tuple appearing more than once;
  and
- validation remains a replace-outright operation through the existing
  `Configuration::with_profile` / `Profile` editing flow rather than an
  in-place mutable exception.

The UI default for a newly added mapping is loopback binding. Persisted data
still stores the explicit chosen host; the important policy is that a blank or
implied wildcard bind is never the silent default.

### Bind-host policy is safe by default and explicit when widened

The default bind host for both saved and live-added mappings is loopback
(`127.0.0.1` by default; equivalent loopback values may be allowed when
entered explicitly). A non-loopback bind is an advanced, deliberate opt-in:
the user must explicitly edit the bind host to widen exposure, and the UI
should surface that this makes the listener reachable beyond the local machine
or remote host itself.

This preserves the repository's safe-by-default posture. Port forwarding is
useful for databases, web UIs, and debug agents even when confined to
loopback; widening exposure should therefore be a conscious exception, not the
baseline.

### Saved mappings apply only to a freshly started profile session

When a saved SSH profile is launched into a new live session, the worker
applies that profile's validated forward mappings for the lifetime of that SSH
session. The application remains responsible only for supplying the immutable
profile metadata; the worker owns the actual forward requests, accept loops,
and teardown because it already owns the authenticated `russh` handle.

This does **not** mean saved mappings become an always-on reconnect policy.
Launching a profile is the explicit decision path that authorizes applying its
saved forwards. A later reconnect is a separate transport event governed by ADR
0018.

### Live forwarding is managed through a separate ephemeral overlay

fesTerm will expose a dedicated live **Port Forward Manager** overlay reachable
from:

- a dedicated application shortcut routed through `ApplicationShortcut`; and
- a command-palette item routed through `palette_items` and
  `dispatch_palette_selection`.

This overlay is session-scoped and live-session-only. It serves three jobs:

1. show the current active forward mappings for the selected SSH session;
2. add a new mapping to the current live session; and
3. remove an active or failed mapping from the current live session.

The overlay must show both **profile-sourced** mappings and **ephemeral**
overlay-added mappings, clearly distinguishing their source. Ephemeral
mappings are never written back to `SshProfileConfiguration`, never change the
saved profile, and disappear when that live session ends.

A compact status-bar affordance such as an icon or count may be added later if
it remains factual and non-noisy, but it is not required by this ADR. The
authoritative live-management surface is the overlay itself.

### Live inventory is bounded without retiring working or failed mappings

The owner-approved live-session ceiling is 128 combined profile and ephemeral
mappings. Pending requests reserve slots before command admission; the
reservation remains owned by the resulting active or failed record. Full or
closed command delivery, canceled pending work, and explicit removal release
the reservation. Removal releases its slot after the forwarding owner stops.
No mapping is automatically retired: a full inventory visibly refuses the
addition and preserves the overlay draft until the user removes a row.

Profile collection refuses a 129th mapping before launch, without truncating
or rejecting the whole saved configuration. Stored-credential and direct
credential-free saved-profile launch both display the same factual refusal;
the interactive launch returns a typed error rather than a discarded boolean. Binding validation and source
metadata remain unchanged. The separate 32-in-flight connection bound still
protects bridges rather than mapping inventory. This is a count bound, not an
aggregate host-string, payload-byte, allocator-fragmentation, or RSS guarantee.

Admission and queued removals are transport-generation scoped; stale work
cannot act on a replacement connection or release its reservations. Indexed
Each mapping also has a unique incarnation retained by accepted local and
remote connection events before they enter the bounded queues. Removing and
re-adding the same bind never routes an old queued connection to the new
destination. Old queue tokens retire with their rejected connections. Indexed
binding lookup removes repeated scans and bind-host clones. Removal preserves
row order, and exceptional outer-vector/index capacity is reclaimed.

Snapshots are prepared only after mutations, with initial profile changes
batched. A blocked publication retains one latest snapshot and retries through
the existing command cadence without rebuilding unchanged metadata or adding
a polling loop. Unpublished snapshots retire with their transport-owned
inventory. The overlay borrows the controller snapshot and gives each binding
explicit stable widget identity; it does not clone the whole list every frame.
Existing network-operation and teardown timeouts remain unchanged; shutdown
is checked between profile mappings and commands.

### Transport integration stays inside the existing worker/channel architecture

`festerm-ssh` remains the only owner of live SSH forwarding mechanics. The
implementation extends the current worker protocol instead of bypassing it.

Concretely:

- `WorkerCommand` gains forwarding commands for live add/remove operations;
- `run_authenticated_channel`, which already owns the authenticated handle and
  polls commands while the session is running, becomes the integration point
  for applying profile-defined forwards at startup and processing live
  add/remove requests afterward; and
- `WorkerShared::try_emit_retaining` publishes sanitized forward-state updates to the app
  via new `SessionEvent` data, suitable for `SessionController` and the live
  overlay to render.

Those events must be content-free and credential-free. They may include the
direction, bind host/port, destination host/port, stable runtime state, and a
concise failure reason; they must not include terminal payloads, forwarded data
bytes, copied credentials, or any transcript-derived guesswork.

### Reconnect and teardown are intentionally conservative

All active forwards — both profile-sourced and ephemeral — must be torn down
when the SSH session disconnects, shuts down, or fails. The worker's runtime
forward table is session-generation state, not durable profile state.

They are **not automatically restored on reconnect**. This mirrors ADR 0018's
plain-SSH rule that a new transport does not gain new policy simply because the
old one died. In practice:

- disconnecting a live session removes all active listeners/binds;
- explicit reconnect creates only a fresh SSH transport unless a future,
  separately reviewed UX asks the user to reapply forwards; and
- a brand-new launch from a saved profile may apply that profile's saved
  mappings again, because that launch is the fresh explicit decision path.

Ephemeral mappings never survive any disconnect boundary.

## Alternatives considered

### Dynamic / SOCKS forwarding in the same change

Rejected for now. Dynamic forwarding requires a local SOCKS server and parser,
per-request destination handling, and more review of exposure, diagnostics, and
UI than the existing local/remote mapping model. The repository has no current
SOCKS implementation seam to reuse, so adding it now would be a materially
larger security and product decision than "support the two fixed-destination
directions SSH already models directly."

### Persist overlay-added ephemeral forwards

Rejected by design. The whole point of the live overlay is to separate
temporary experimentation from profile policy. Auto-saving overlay changes back
into `SshProfileConfiguration` would blur that line, create surprising future
bind exposure, and make "live only" no longer truthful.

### Default wildcard binding (`0.0.0.0`, `::`, or server-default bind)

Rejected. It is convenient for some collaboration and device-lab scenarios,
but it is the wrong default for a terminal workstation whose product posture is
safe by default. Loopback serves the common case while still allowing explicit
advanced widening when the user truly intends broader reachability.

## Consequences

- SSH profile editing gains a new validated collection of saved forward
  mappings without changing the configuration schema version.
- The SSH worker protocol grows beyond terminal I/O/reconnect/shutdown and
  becomes the owner of a second authenticated-session capability: forwarding.
- The application needs a new session-scoped overlay state plus palette and
  shortcut routing, but it does not need a new top-level screen or a second SSH
  transport owner.
- Disconnect and reconnect messaging must remain honest: no copy, status text,
  or diagnostics may imply that old listeners survived or that a reconnect
  silently restored them.
- Remote-forward failures remain possible and expected; the UI must surface
  them as concise per-mapping state, not as opaque terminal noise or as proof
  that the whole SSH shell session failed.

## Validation impact

- **Invariants introduced or changed:** Saved SSH profiles may declare zero or
  more validated local/remote forwards; loopback is the default bind policy;
  overlay-added forwards are always ephemeral; all active forwards tear down on
  disconnect; reconnect never silently restores prior forward state. Each
  live inventory admits at most 128 combined profile/pending/active/failed
  mappings, refusing new work until explicit removal rather than evicting a
  tunnel or discarding failed-row diagnostics.
- **GUI/action edges affected:** New planned edges `SSH-06` (open Port Forward
  Manager from a live SSH session), `SSH-07` (add a validated local or remote
  forward in that overlay, including visible full-inventory refusal), and
  `SSH-08` (remove an active or failed forward and confirm
  the live list updates without mutating the saved profile). A later optional
  status-bar count, if implemented, should receive its own stable `STATUS-*`
  edge rather than piggybacking on these.
- **Automated tests required:** Planned coverage includes
  `ssh_profile_saved_port_forwards_parse_with_additive_defaults`,
  `duplicate_ssh_port_forward_bindings_are_rejected`,
  `local_forward_bridges_bytes_bidirectionally`,
  `remote_forward_bridges_bytes_bidirectionally`,
  `port_forward_manager_lists_profile_and_ephemeral_mappings`,
  `removing_an_ephemeral_forward_does_not_mutate_the_saved_profile`, and
  `reconnect_does_not_reapply_forward_state_without_a_fresh_launch_decision`.
  Deterministic inventory coverage includes
  `profile_port_forward_inventory_accepts_128_and_rejects_129`,
  `pending_port_forward_inventory_refuses_a_129th_request`,
  `port_forward_command_refusal_and_cancellation_release_admission`,
  `initial_profile_and_live_forward_requests_share_admission_before_running`,
  `queued_forward_removal_records_the_transport_generation`,
  `failed_port_forward_inventory_churn_plateaus_and_reclaims_capacity`,
  `indexed_port_forward_removal_preserves_order_and_other_bindings`,
  `stale_forward_reservation_cannot_release_a_new_generation_binding`,
  `stale_forward_reservation_cannot_release_a_readded_same_generation_binding`,
  `queued_forward_connections_cannot_use_a_readded_binding`,
  `local_forward_queue_retains_the_accepting_mapping_incarnation`,
  `forward_reservation_is_released_only_after_local_owner_stops`,
  `disconnecting_clears_the_retained_port_forward_snapshot`,
  `forward_snapshot_retries_without_recloning_and_coalesces_latest_state`,
  `forward_snapshot_closed_receiver_does_not_rebuild_or_survive_owner_retirement`,
  `port_forward_manager_inventory_refusal_is_visible_and_preserves_the_draft`,
  `stored_password_profile_forward_inventory_limit_is_visible_and_preserves_configuration`,
  `credential_free_saved_profile_forward_inventory_refusal_is_visible`,
  `port_forward_row_widget_identity_survives_removing_an_earlier_mapping`, and
  `live_forward_inventory_limit_preserves_active_bytes_and_admits_retry_after_failed_removal`.
- **Native/manual evidence required:** Manual SSH-fixture evidence is required
  for loopback-default behavior, explicit non-loopback opt-in messaging, a
  server-accepted remote forward, a server-rejected remote forward, and clean
  teardown on disconnect. Existing scenario `TI-11` also retains native
  full-inventory message, draft-preservation, failed-row removal/retry, and
  narrow-overlay/accessibility checks; headless and owned-loopback evidence
  do not establish native usability acceptance. `LAUNCH-02`, `LAUNCH-04` and
  `LAUNCH-07` also cover the pre-connect count refusal and factual saved-profile
  feedback without truncating the stored configuration.
- **Coverage superseded:** None yet. `validation/traceability.json` must be
  updated in the implementing change that wires these edges and tests into real
  coverage.
