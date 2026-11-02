# ADR 0057: Portable Dev Tunnels Client for iOS

- **Status:** Proposed
- **Date:** 2026-11-02
- **Supersedes:** None

## Context

ADR 0053 added a native Dev Tunnels client and a separate host bridge for
remote persistent sessions. Both live in desktop-oriented crates:
`festerm-tunnel-bridge` links `festerm-sessiond` (PTY, daemon spawn, Unix
sockets, Windows security) unconditionally, so the iOS dependency policy
(`scripts/build-ios-spike.py --check-dependencies`) cannot admit it. The iOS
shell needs to sign in with GitHub, pick a tunnel, resume the session already
running on the host and host it in the terminal exactly like an SSH session,
without sharing code paths that spawn processes.

The 2026-10-31 decision in ADR 0053 stands: publishing a bridge is an accepted,
revocable risk. Nothing here changes what the host exposes; the client still
trusts only the host key bound to the profile and the numeric GitHub account.

## Decision

1. **Split `festerm-tunnel-bridge` by feature, not by fork.** `client` is the
   portable part (relay attach, wire framing, file-call types, the mobile
   session client). `relay` (default) adds the host: `festerm-sessiond`,
   identity files, `transport`, `relay_host`, setup storage and the file
   service. The desktop binary and tests are unchanged; iOS builds the bridge
   with `--no-default-features --features client`.
2. **`bridge::mobile`** is a `festerm_session::Session` implementation over any
   async stream (`describe`, `establish`, `TunnelSession`). Input is refused
   until the recovered terminal is adopted; terminal replies belong to the
   backend; shutdown sends `DETACH`. It never reconnects.
3. **Protocol additions, backward compatible on the wire for new peers:**
   `Hello.takeover` (default true, omitted when true), `Description.attached_by`
   and `takeover_allowed`, and a `DISPLACED` frame carrying who displaced the
   client (`Local`, `Remote`, `Unknown`). The host honors takeover only when its
   own `--takeover` flag is set *and* the client asked for it. Older desktop
   clients use `deny_unknown_fields` and reject a `Description` carrying the new
   fields; they appear only when a session is held or takeover is enabled, so a
   desktop client must be updated together with a host that has takeover on.
4. **`festerm-mobile-tunnels`** is the UI-agnostic, async-free API. Calls
   block on a bounded private runtime, honor a `CancelToken` and a per-call
   deadline: GitHub device-flow `SignIn` (user code, verification URL, upper
   bound expiry, wait/cancel), account verification bound to the numeric id,
   token storage and in-place refresh through `festerm-secret-store` (Keychain
   on iOS), `list_tunnels`, `describe_tunnel` (host fingerprint and trust:
   new/trusted/changed), `list_sessions` (unattached/attached, attached by),
   `attach` honoring the profile's `take_over_if_attached`, and `detach`.
   Seams (`TunnelDirectory`, `BridgeConnector`, identity transport) let tests
   run offline.
5. **Typed errors** (`TunnelError`) distinguish sign-in required, expired,
   denied, rate limited, account changed, host not trusted/changed, session in
   use, `Displaced(Local|Remote|Unknown)`, connection lost and session exited.
6. **No silent reattach after displacement.** After displacement the session
   ends and reports a typed reason; only an explicit `attach` by the user
   reconnects (and takes the session back only if the profile allows
   takeover). A *lost* session may reconnect automatically under the
   amendment below, but never by taking over another client.
7. **Dependency policy** now also checks `festerm-tunnel-client`,
   `festerm-tunnel-bridge` (client feature) and `festerm-mobile-tunnels` for both
   iOS targets, with the same forbidden desktop crates and secret backends.

## Alternatives considered

- Fork the bridge client into a new crate: duplicates the protocol and drifts.
- Reconnect transparently after displacement: hides a user-visible takeover and
  could fight the other client for the session.
- Expose an async API: the shell already runs blocking work on worker threads.

## Consequences

- iOS can compile the tunnel client path; desktop behavior is unchanged.
- File reads over the tunnel are not available on mobile yet (host-only
  `FileService` stays behind `relay`).
- The bridge serves one daemon session per tunnel.
- The shell must apply `RecoverySync` events with `apply_recovery_sync` and not
  answer terminal queries itself.
- Real Microsoft service, Keychain and device behavior are unvalidated here.

## Validation impact

- **Invariants introduced or changed:** the client feature never links daemon
  or PTY code; takeover requires both client request and host consent; a
  displaced mobile session never reconnects by itself; an automatic reconnect
  only reclaims the same tab's own attachment; accounts stay bound to their
  numeric GitHub id.
- **GUI/action edges affected:** None; no user-visible UI changes in this PR.
- **Automated tests required:** `bridge::mobile` unit tests,
  `festerm-mobile-tunnels/tests/api.rs`, the real-daemon loopback
  `mobile_api_attaches_takes_over_and_is_displaced_on_a_real_daemon`, and
  `scripts/tests/test_ios_ssh_dependencies.py`.
- **Native/manual evidence required:** none for this backend; the shell PR and
  device validation carry `MOB-*` evidence.
- **Coverage superseded:** None.

### Amendment: automatic reconnect of lost Dev Tunnel sessions

*Amended after device testing showed people reconnecting by hand after every
phone lock or laptop sleep.*

- **Per-profile setting.** Dev Tunnel profiles on iOS and desktop gain
  `auto_reconnect` (serde default `true`, shown as "Reconnect automatically").
  Unlike plain SSH (ADR 0018, off by default), a Dev Tunnel always reattaches
  the same durable `festerm-sessiond` session, so reconnecting never silently
  replaces a shell.
- **When.** A tab whose session was *lost* (network loss, app suspension or
  sleep) reconnects once per activation event: the app returns to the
  foreground, the window regains focus, or the tab is activated, or at once if
  the loss is seen while the tab is already active. A failed attempt leaves the
  manual Reconnect in place; there is no retry loop. Displacement, a remote
  shell exit and an explicit Disconnect never trigger it, and an automatic
  attempt never prompts for sign-in or host trust.
- **Never a takeover.** Each tab carries a random `ClientId` (128-bit, hex)
  that it sends in `Hello.client`, but only to hosts whose `Description`
  advertises `reclaim` (hosts that allow takeover), so older hosts never see the
  field. The bridge records the id of the client that holds the session and
  clears it when that attachment ends for any reason, including displacement.
  A `Hello` without takeover is granted as a *reclaim* only when its id
  matches the recorded holder, which replaces the tab's own stale attachment
  (for example a relay connection the host has not yet noticed is dead). Any
  other held session is refused, so an automatic reconnect surfaces "in use"
  and the person decides whether to take over. A device that was taken over
  from can therefore never take the session back automatically.
- **Ids are per tab, not per device,** so two tabs in one app never reclaim
  from each other. The holder check and the daemon admission are not atomic;
  a takeover racing a reclaim resolves like two takeovers (last one wins).
- **Validation impact:** new invariant above; automated tests
  `a_suspended_tunnel_tab_reconnects_once_as_the_same_client` and
  `being_taken_over_ends_the_session_without_reattaching` (iOS shell), the
  real-daemon loopback `mobile_api_attaches_takes_over_and_is_displaced_on_a_real_daemon`
  (own tab reclaims, other tab refused, displaced tab cannot reclaim) and the
  desktop tunnel reconnect tests. Device evidence: lock and unlock the phone
  and wake the laptop with a tunnel tab open.
