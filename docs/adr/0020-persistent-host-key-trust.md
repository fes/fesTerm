# ADR 0020: Persistent host-key trust

- **Status:** Accepted
- **Date:** 2026-08-22
- **Supersedes:** None (extends ADR-0013's host-trust decisions and
  ADR-0019's fingerprint-first prompt UI; both remain in force for the
  per-connection SSH-transport decision)

## Context

`HostTrustDecision` (ADR-0013) has always had three variants — `Reject`,
`AcceptOnce`, `AcceptAndPersist` — but `check_server_key` treats
`AcceptOnce`/`AcceptAndPersist` identically: both accept only the current
connection. `AcceptAndPersist` has never actually persisted anything, and the
application-facing `HostKeyTrustDecision` enum did not even expose it,
deliberately, pending "M8 storage" owning the persistence boundary. ADR-0016
explicitly listed a persistent known-host trust service as out of scope
until an explicit host-key replacement/revocation policy existed.

Every SSH connection therefore prompts fresh, even to a host the user has
already accepted, which does not match `ssh`'s own `known_hosts` behavior and
adds needless friction to reconnecting to familiar hosts. `festerm-config`
(ADR-0015) already has an established immutable-replacement pattern for
non-secret configuration state (profiles, workspace, interface settings) that
this fits naturally: host public keys and their fingerprints are not secret,
so this is ordinary configuration, not a `festerm-secret-store` (ADR-0016)
credential.

## Decision

`SshSessionOptions` gains an optional `known_host_fingerprint`. When set, the
worker's `check_server_key` accepts a server key that exactly matches it
silently — no prompt at all, mirroring `ssh`'s own already-in-`known_hosts`
behavior. Any other presented key still prompts, but the emitted
`HostKeyPrompt` is flagged via a new `previously_trusted_fingerprint` field
(`is_key_change()`) so the application can distinguish an ordinary first-seen
host from a changed one.

`festerm-config`'s `Configuration` gains a `known_hosts: Vec<KnownHostEntry>`
document field (plain `host`/`port`/`sha256_fingerprint`, following
`SshProfileConfiguration`'s existing convention of storing raw fields rather
than a `HostIdentity`, which has no `Serialize`/`Deserialize`) and three
methods following the crate's immutable-replacement pattern:
`known_host_fingerprint` (lookup), `with_known_host_trust` (upsert — an
existing record for the same host:port is replaced outright, never merged),
and `without_known_host` (explicit revocation). `AppState::execute_ssh_session`
is the single choke point that looks up a persisted fingerprint before every
SSH launch (Quick Connect, the Advanced form, and both plain and
stored-password configured profiles), so a saved trust record benefits every
launch path uniformly, including ad-hoc Quick Connect destinations that have
no saved profile.

`HostKeyTrustDecision` gains `AcceptAndPersist`. Persisting the trust record
itself needs the configuration reloader (a composition-root resource
`AppState` never holds), so `FesTermApp::screen_command` intercepts this one
decision specifically — reading the tab's pending `HostKeyPrompt` before
dispatch clears it, writing the record, and updating `configuration_status`
 — then still forwards the command through `AppState::dispatch` unchanged, so
the SSH-level accept proceeds exactly like `AcceptOnce`. This mirrors the
existing `StoreSshPassword` interception precedent, except the underlying
session-level dispatch still happens afterward (unlike `StoreSshPassword`,
which fully replaces it).

The first-seen host-key prompt keeps its existing `y`/`n`/Escape keys and
gains `a` ("accept and remember"). A changed-key prompt is a materially
different, harder-to-dismiss screen (`docs/gui-action-graph.md`'s `TRUST-04`,
previously target-only): it never offers a plain, single-key accept. The only
way to proceed is to type the literal word `yes` and press Enter — reusing
the raw, unechoed keystroke-capture technique from the live SSH password
prompt (ADR-0019), except what is typed is echoed here (it is not secret) so
the deliberate act is visible. Escape, or Enter without exactly `yes`,
rejects the connection.

## Alternatives considered

- **Silently overwrite a changed key on `AcceptOnce`, same as a first-seen
  host.** Rejected: a changed key is the one case host-key verification
  exists to catch (possible interception, not just a rotated key); offering
  the same low-friction path as a first-seen host would defeat the point of
  distinguishing them at all, and `docs/gui-action-graph.md`'s `TRUST-04`
  already specified this must not happen.
- **Store known-host trust in the native secret store (`festerm-secret-store`,
  ADR-0016) instead of plain TOML.** Rejected: host public keys and
  fingerprints are not secret — they are meant to be publicly knowable — so
  routing them through the secret-store boundary (and its background-thread
  write path) would add asynchronous complexity for no confidentiality
  benefit. `Configuration`'s existing atomic-write path (ADR-0015) is
  synchronous and sufficient.
- **A single-keypress confirmation for the changed-key case (e.g. a second,
  differently labeled key).** Rejected per the explicit `TRUST-04` design
  intent: a security-sensitive override should cost more than one keystroke,
  and typing a full word is closer to `ssh-keygen -R`'s own deliberate,
  typed nature than any single key would be.

## Consequences

- `SshSessionOptions`, `SshClientHandler`, and `establish_connection` now
  carry an optional expected fingerprint end to end; every reconnect within
  one session's lifetime (manual or automatic, ADR-0018) reuses the same
  fingerprint captured once at session start, since it is a property of the
  destination, not of any one connection attempt.
- `festerm-ssh::is_sha256_fingerprint` is now `pub` so `festerm-config` can
  validate a stored fingerprint's format without duplicating the parsing
  logic.
- A configuration document with a stale or attacker-supplied `known_hosts`
  entry could suppress a legitimate host-key prompt; this is no worse than
  `ssh`'s own `~/.ssh/known_hosts` trust model, and revocation
  (`without_known_host`) is available, but no UI currently exposes revocation
  directly (only via hand-editing TOML or accepting a legitimate key change).
  A future Settings-surfaced known-hosts manager is a natural follow-up but
  out of scope here.
- A save failure when persisting a new trust record (e.g. an unwritable
  configuration file) does not block the current connection: the SSH-level
  accept always proceeds, and only the "remembered for next time" property is
  lost, with `configuration_status` surfacing the failure.

## Validation impact

- **Invariants introduced or changed:** A server key that exactly matches a
  persisted `known_hosts` fingerprint is accepted without prompting; any
  other key still prompts, and is flagged as a changed-key warning whenever a
  persisted record already exists for that host:port; a changed-key prompt
  never offers a single-key accept, only typed-`yes` override or reject;
  `known_hosts` entries are unique per host:port and validated the same way
  the SSH backend validates its own fingerprints.
- **GUI/action edges affected:** `TRUST-03` (first-seen host now also offers
  `a` to accept and remember), `TRUST-04` (implemented: changed-key warning
  requires typed `yes`, never an ordinary Accept Once), new edge `TRUST-05`
  (`SshUnknownHost` accept-and-remember → silent future reconnect with no
  prompt at all).
- **Automated tests required:**
  `festerm_ssh::tests::handler_silently_accepts_a_matching_known_host_fingerprint`,
  `festerm_ssh::tests::handler_flags_a_mismatched_known_host_fingerprint_as_a_changed_key_warning`,
  `festerm_config::tests::records_upserts_and_revokes_a_known_host_trust_entry`,
  `festerm_config::tests::known_host_trust_round_trips_through_toml_and_rejects_invalid_entries`,
  `festerm::app::tests::host_key_prompt_ui_accepts_and_remembers_a_first_seen_host_on_a_key`,
  `festerm::app::tests::changed_host_key_prompt_ui_requires_typing_yes_to_replace_trust`,
  `festerm::app::tests::changed_host_key_prompt_ui_accepts_and_persists_only_after_typing_the_literal_word_yes`,
  `festerm::tabs::tests::host_key_trust_decisions_map_onto_the_ssh_transport_decisions_including_persistence`.
- **Native/manual evidence required:** Manual confirmation that reconnecting
  to an already-trusted host in a real window skips the prompt entirely, and
  that the changed-key warning is visibly distinct from the first-seen
  prompt; not yet performed this session.
- **Coverage superseded:** The prior test
  `application_host_key_decisions_cannot_request_persistence`
  (`festerm::tabs::tests`) asserted the deliberate absence of a persistent
  decision; it was renamed and extended to
  `host_key_trust_decisions_map_onto_the_ssh_transport_decisions_including_persistence`
  covering the same mapping plus the new variant.
