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

The #496 amendment in [ADR 0057](0057-portable-dev-tunnels-client-for-ios.md#amendment-retained-mobile-attachments-and-foreground-host-checks-496)
separates native privacy/app-lock state from tunnel lifetime. Established active
and parked tunnels survive inactivity; an attachment-owned asynchronous host
probe gates input on foreground return, with bounded safe recovery on failure.
No native/device evidence is claimed; MOB-03/MOB-06 retain those checks.

## Amendment: multiple GitHub accounts

The iOS Dev Tunnel editor lists stored GitHub accounts and offers **Add GitHub
account** without signing out another identity. It uses the existing per-account
credential store; no schema migration is required. Device approval instructions
ask the user to select the intended GitHub identity in the browser. Reauthorizing
the same identity retains its account key.

Explicitly switching accounts clears the draft tunnel/session and old discovery
(including pending jobs and destructive-action state). Saved profiles retain
their own bindings. A missing bound account never silently selects another
stored identity. Sign-out removes only the selected account through the existing
backend; established sessions keep their existing lifecycle.

Signing out the selected account also clears its draft tunnel/session, and Save
refuses an account that is no longer stored.

Validation impact: `TUNNEL-11`, `MOB-06`; deterministic regressions
`choosing_another_tunnel_account_clears_discovery_and_preserves_saved_binding`,
`choosing_the_same_tunnel_account_keeps_the_draft_target`,
`an_unbound_saved_tunnel_does_not_choose_another_account`,
`adding_a_github_account_keeps_the_first_and_selects_the_new_identity`,
`the_editor_lists_every_stored_account_and_shows_the_bound_login` and
`signing_out_the_selected_account_clears_its_draft_and_it_cannot_be_saved`. Native GitHub approval
with two accounts and Keychain persistence across relaunch remain manual.
