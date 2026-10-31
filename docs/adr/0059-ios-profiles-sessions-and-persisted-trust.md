# ADR 0059: iOS Profiles, Multi-Session Tabs and Persisted Host Trust

- **Status:** Accepted
- **Date:** 2026-10-31
- **Related:** ADRs 0031, 0042, 0054, 0056
- **Scope:** `app/festerm-mobile` shell wiring; `scripts/build-ios-spike.py` simulator signing.

## Context

ADR 0056 delivered the shell over an in-memory profile store. ADR 0054 added
`festerm-mobile-profiles` (profile store, Keychain secrets, known hosts). The
shell must now use them for a real connect flow.

## Decision

- `profiles.rs` opens `ProfileStore` and `KnownHosts` in the app sandbox with
  the Keychain secret store. A probe write/delete checks the Keychain at start.
  If it fails, an in-memory secret store is used and the UI states that
  credentials last until fesTerm closes. Secrets are never written to disk.
- Profiles carry auth (password, imported or generated Ed25519 key with
  optional passphrase, ask each time) and a remote session strategy
  (Plain, tmux, Screen plus name). tmux/screen run an attach-or-create command.
- Host trust persists after explicit approval. A changed key is refused with
  no approve action; Settings can forget a host.
- `tabs.rs` keeps several sessions alive; chips and the All-sessions sheet
  switch without reconnecting, and background output keeps pumping.
- Reconnect starts a new shell and says so; with tmux/screen it reattaches.
- Bottom sheets measure their content and grow in place.
- Simulator builds link a `keychain-access-groups` entitlement into the Mach-O
  `__TEXT,__entitlements` section; `codesign --entitlements` makes the
  simulator refuse to launch the app. Device builds use real provisioning.
- `scripts/run-ios-shell-harness.sh` provides a loopback sshd for manual runs.

## Consequences

`festerm-ssh` applies a 10 s connect timeout while the host-key sheet is open,
so a slow review fails the attempt; this needs a backend change. SFTP, app
lock and Dev Tunnel screens remain separate slices.

## Validation impact

`mobile_profiles_crud_recents_and_remote_session_strategy`,
`mobile_profiles_keys_are_generated_stored_and_rejected_when_invalid`,
`mobile_profiles_trusted_hosts_persist_and_can_be_forgotten`,
`mobile_editor_saves_edits_and_deletes_profiles_with_credentials`,
`mobile_tabs_switch_without_reconnect_keep_background_output_and_close` and
`mobile_reconnect_banner_fits_and_failed_reconnect_keeps_history_without_replay`
cover the model; the loopback tests cover unchanged SSH trust policy.
Simulator runs against the harness are manual evidence, not device acceptance.
