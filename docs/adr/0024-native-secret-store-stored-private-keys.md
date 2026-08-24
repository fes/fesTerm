# ADR 0024: Native Secret Store Extended to Stored Private Keys

- **Status:** Accepted
- **Date:** 2026-08-21
- **Supersedes:** None (extends ADR-0016's boundary; ADR-0016 remains in
  force for the store's shape, error semantics, and no-fallback policy)

## Context

ADR-0016 deliberately scoped its first slice to SSH password references
only: `festerm-config`'s `credential_id` named "only a canonical lowercase
UUID-v4 opaque reference to a native stored SSH password," and explicitly
excluded private keys, passphrases, agents, key files, and host trust from
persistence. That scoping was correct for the narrow M8 acceptance criteria,
but it left private-key authentication unable to reuse a saved profile the
same way password authentication could: every SSH session using a key had to
re-paste the key text (and any passphrase) through the transient M7 form.

Host-key trust has since gained its own persistence path (ADR-0020) using
plain `festerm-config` TOML, because host keys are not secret. Private keys
and passphrases are secret, so they belong behind the `festerm-secret-store`
boundary ADR-0016 established, not in TOML.

## Decision

`festerm-config::CredentialKind` gains a `PrivateKey` variant alongside the
existing `Password` default. `SshProfileConfiguration` retains exactly one
`credential_id` field regardless of kind; `credential_kind` disambiguates
what the opaque reference names. As with the password slice, the parser
rejects malformed, noncanonical, or non-v4 references, and no other
credential field or metadata is allowed in profiles or workspaces.

`festerm-ssh` adds `encode_stored_private_key`/`decode_stored_private_key` to
pack an OpenSSH private key and its optional passphrase into one
`SecretBytes` blob behind a single `SecretReference` (a 4-byte
little-endian passphrase-length prefix, the passphrase bytes, then the raw
key text — a length prefix rather than a delimiter so the key text's own
bytes are never constrained). `SshAuthentication::stored_private_key`
mirrors `stored_password`: the store and reference are moved into a
`StoredPrivateKeyAuthentication` with no accessors or `Debug` impl, and
`resolve_stored_private_key` fetches, decodes, and parses the key only on
the SSH worker, immediately before authentication — never on the UI thread
and never returned to application code.

The profile editor (`app/festerm/src/screens.rs`) gains a private-key
authentication method symmetric with the existing password one: a multiline
secret text field for the key plus an optional passphrase field, a "Save
private key" action that dispatches `AppCommand::StoreProfilePrivateKey`, and
the same "already stored, enter a new one to replace it" messaging password
storage uses. `AppState::start_stored_password_ssh_profile` dispatches to
`SshAuthentication::stored_password` or `stored_private_key` based on the
profile's `credential_kind`, so every launch path (Quick Connect equivalent,
the profile list, and workspace-restored authentication-required surfaces)
resolves either kind uniformly.

## Alternatives considered

- **A separate `credential_kind`-specific reference field
  (`password_credential_id`/`private_key_credential_id`).** Rejected: a
  profile has exactly one credential at a time; two optional fields would
  let a document represent an invalid combination (both set, or a kind with
  no matching reference) that the parser would then need to reject anyway.
  A single reference plus a kind discriminant makes that combination
  unrepresentable.
- **Store the passphrase as a second, separate secret reference.** Rejected:
  the key and its passphrase are only ever meaningful together, are only
  ever resolved together, and are never surfaced independently; splitting
  them would double the native-store round trips and error paths for no
  benefit, since the store already treats each reference as opaque bytes of
  the caller's choosing.
- **Reuse the OS keychain's own "generic password" account field for the
  passphrase.** Rejected: `festerm-secret-store`'s boundary is opaque
  reference in, opaque bytes out; giving the private-key slice a bespoke
  second entry per profile would leak structure through the native store's
  own account-naming scheme (ADR-0016) that the reference model exists to
  avoid.

## Consequences

- `festerm-config` profile documents remain secret-free: only an opaque
  `credential_id` plus a `credential_kind` discriminant are persisted,
  matching ADR-0016's boundary; ADR-0016's "does not add persistence for
  private keys" sentence is now historical rather than current — this ADR
  is the record of that follow-on decision.
- A saved private-key profile behaves exactly like a saved password
  profile from the profile list and workspace-restoration surfaces: the
  authentication-required surface still requires one explicit user action
  before any SSH connection starts (ADR-0016/ADR-0018's restoration
  boundary is unchanged), but that action no longer requires re-entering
  key material.
- Losing or rotating a stored private key follows the same actionable,
  content-free `SecretStoreError`/`StoredPrivateKeyResolutionError` path as
  a stored password: missing, locked, or unavailable native storage is
  surfaced rather than silently downgraded.
- Cross-platform SSH-agent authentication (issue #40) and literal key-file
  path references remain separate, deferred capabilities; this ADR does not
  advance either.

## Validation impact

- **Invariants introduced or changed:** A `credential_id` may now name a
  native-stored private key (optionally passphrase-protected) in addition to
  a password; `credential_kind` is the sole discriminant; profile/workspace
  documents remain fully secret-free regardless of which kind is stored.
- **GUI/action edges affected:** `AUTH-04` (stored-credential native-store
  states now cover private-key references, not password only).
- **Automated tests required:**
  `festerm_config::tests::stored_private_key_credential_kind_saves_loads_and_defaults_to_password`,
  `festerm_ssh::tests::stored_private_key_authentication_resolves_only_through_the_worker_source`,
  `festerm_ssh::tests::stored_private_key_round_trips_with_a_passphrase`,
  `festerm_ssh::tests::stored_private_key_resolution_errors_are_actionable_and_redacted`,
  `festerm::screens::tests::ssh_profile_editor_offers_a_private_key_field_that_dispatches_store_profile_private_key`.
- **Native/manual evidence required:** Covered by the existing CP-03 native
  secret-store scenario (`docs/manual-validation.md`); no new manual scenario
  is required because it already exercises the shared native-store
  available/locked/unavailable/failure states rather than one credential
  kind specifically.
- **Coverage superseded:** None; this extends the `ssh-authentication`
  coverage entry's decisions and automated tests rather than replacing any.

Update `validation/traceability.json` in the same change whenever this section
adds, removes, or changes a trace relationship.
