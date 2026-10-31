# ADR 0061: iOS Dev Tunnel Profiles, Sign-in and App Lock Wiring

- **Status:** Accepted
- **Date:** 2026-11-01
- **Related:** ADRs 0042, 0054, 0057, 0058, 0059, 0060
- **Scope:** `app/festerm-mobile` tunnel/lock UI, `festerm-ios-window::open_url`.

## Context

ADR 0057 added the blocking `festerm-mobile-tunnels` client and ADR 0058 the
`festerm-ios-lock` gate. The shell hid the Dev Tunnel kind and had no lock UI.

## Decision

- Dev Tunnel is a profile kind (`profiles.rs::TunnelSpec`; editor in `forms.rs`)
  with GitHub account, tunnel, optional host session and "take over" switch.
- `tunnel_link.rs` runs sign-in, listing and connect as cancellable worker
  `Job`s (drop cancels) so the UI thread never blocks. Sign-in shows the device
  code sheet; "Open GitHub" uses `open_url` (https only).
- Connect stages: Describing, Verify (host-key sheet with persisted trust and
  hard refusal of a changed key), Attaching. The attachment's recovered
  terminal is adopted into a session tab; `RecoverySync` is applied by
  `festerm_mobile_tunnels::apply_recovery_sync` and terminal replies are left to
  the backend.
- Displacement shows "Taken over by another device"; there is no automatic
  reattach. A refused attach because the session is held tells the user to
  enable take over.
- `app_lock.rs` hides all content behind the unlock screen until the platform
  authentication completes, re-locks on background/Lock now, and Settings hosts
  the App lock section. Enabling without a passcode reports an error.

## Consequences

Tunnel tabs have no SSH spec, so the Files button is hidden for them. Real
GitHub approval and a real bridge host were not exercised on the simulator;
the attach path is covered by scripted in-memory relay tests.

## Validation impact

`tunnel_tests::*` and `app_lock::tests::*` cover attach, trust, take-over,
displacement and recovery, and the lock gate.
