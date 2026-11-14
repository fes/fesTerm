<!-- agent-navigation: {"domain": "config-secrets", "generator": "2", "model": "mai-code-1.1-flash", "sources": "e36b3354df2a79deab6cab1afd10fe54bc4ecf8b5a9a24d95d920bd31567bc1c"} -->
# Configuration, workspace, and secrets

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Owns versioned TOML configuration, workspace metadata, opaque secret references, and private file protection for config and workspace storage.

Canonical docs: [docs/configuration.md](../../docs/configuration.md), [docs/adr/0008-versioned-toml-configuration.md](../../docs/adr/0008-versioned-toml-configuration.md), [docs/adr/0016-native-secret-store-boundary.md](../../docs/adr/0016-native-secret-store-boundary.md)

## Config startup

Use for: Startup picks the config path and saves workspace or profile updates.

- [`select_configuration_source`](../../app/festerm/src/configuration_startup.rs) — Chooses the override or native config path while keeping it private.
- [`ConfigurationReloader`](../../app/festerm/src/configuration_startup.rs) — Private loader and save gate for startup writes and auto-reconciliation.

Evidence:
- [`unicode_override_takes_precedence_over_native_location`](../../app/festerm/src/configuration_startup.rs) — Validates the override wins before the native config location.
- [`explicit_workspace_save_creates_only_the_missing_native_directory_and_loads`](../../app/festerm/src/configuration_startup.rs) — Shows native directory creation, private save behavior, and round-trip.

Caveats:
- Diagnostics deliberately omit the selected path and source TOML.
- Unknown or busy writes latch the save gate until restart.

## Config schema

Use for: Parsing, validating, or serializing a configuration document or workspace snapshot.

- [`Configuration`](../../crates/festerm-config/src/lib.rs) — Versioned TOML document holding profiles, workspace, settings, and accounts.
- [`WorkspaceConfiguration`](../../crates/festerm-config/src/workspace.rs) — Metadata-only tab and window restore state with profile validation.

Evidence:
- [`saves_and_loads_a_complete_configuration`](../../crates/festerm-config/src/lib.rs) — Round-trips a valid document through file save and load.
- [`without_profile_deletes_an_unreferenced_profile_but_rejects_a_workspace_reference`](../../crates/festerm-config/src/lib.rs) — Rejects invalid workspace references after profile deletion.

Caveats:
- Workspace metadata is valid only when workspace_enabled is true and the document is non-empty.
- Profile IDs and workspace tab profile_id references must stay unique and valid.

## Secret boundary

Use for: A profile password, refresh token, or credential is created, rotated, or loaded from native storage.

- [`CredentialReference`](../../crates/festerm-config/src/profiles.rs) — Stores a UUID string without exposing secret material in debug output.
- [`SecretStore`](../../crates/festerm-secret-store/src/lib.rs) — Native store contract for create, get, update, and delete of secret bytes.

Evidence:
- [`persists_an_opaque_ssh_credential_reference_without_exposing_it_in_debug_output`](../../crates/festerm-config/src/lib.rs) — Shows config persists only the opaque UUID reference and redacts debug output.
- [`references_are_random_canonical_uuid_v4_values`](../../crates/festerm-secret-store/src/lib.rs) — Proves persisted references are canonical, random, and opaque.

Caveats:
- Config files store only IDs; never secret bytes, plain values, or key material.
- Unsupported, missing, locked, or unavailable native storage is a first-class error.

## File security

Use for: Creating or preserving a private config directory or staging file on Unix or Windows.

- [`make_private_directory`](../../crates/festerm-unix-security/src/lib.rs) — Ensures a directory is owned and restricted to the current user.
- [`create_current_user_only_file`](../../crates/festerm-windows-security/src/lib.rs) — Creates a file with a current-user-only DACL and safe publication flow.

Evidence:
- [`private_directory_clears_inherited_access`](../../crates/festerm-unix-security/src/lib.rs) — Validates inherited ACL cleanup before committing a private directory.
- [`private_creation_and_conditional_publication_keep_the_current_user_only_dacl`](../../crates/festerm-windows-security/src/lib.rs) — Shows private creation and publication preserve the current-user-only DACL.

Caveats:
- Private file creation rejects inherited or foreign ACLs before publication.
- Directory ancestry and ownership must remain trusted; otherwise the secure path is rejected.
