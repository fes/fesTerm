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
3. remove an active mapping from the current live session.

The overlay must show both **profile-sourced** mappings and **ephemeral**
overlay-added mappings, clearly distinguishing their source. Ephemeral
mappings are never written back to `SshProfileConfiguration`, never change the
saved profile, and disappear when that live session ends.

A compact status-bar affordance such as an icon or count may be added later if
it remains factual and non-noisy, but it is not required by this ADR. The
authoritative live-management surface is the overlay itself.

### Transport integration stays inside the existing worker/channel architecture

`festerm-ssh` remains the only owner of live SSH forwarding mechanics. The
implementation extends the current worker protocol instead of bypassing it.

Concretely:

- `WorkerCommand` gains forwarding commands for live add/remove operations;
- `run_authenticated_channel`, which already owns the authenticated handle and
  polls commands while the session is running, becomes the integration point
  for applying profile-defined forwards at startup and processing live
  add/remove requests afterward; and
- `WorkerShared::try_emit` returns sanitized forward-state updates to the app
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
  disconnect; reconnect never silently restores prior forward state.
- **GUI/action edges affected:** New planned edges `SSH-06` (open Port Forward
  Manager from a live SSH session), `SSH-07` (add a validated local or remote
  forward in that overlay), and `SSH-08` (remove an active forward and confirm
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
- **Native/manual evidence required:** Manual SSH-fixture evidence is required
  for loopback-default behavior, explicit non-loopback opt-in messaging, a
  server-accepted remote forward, a server-rejected remote forward, and clean
  teardown on disconnect. Stable scenario IDs should be added to
  `docs/manual-validation.md` in the implementing change.
- **Coverage superseded:** None yet. `validation/traceability.json` must be
  updated in the implementing change that wires these edges and tests into real
  coverage.
