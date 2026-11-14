<!-- agent-navigation: {"domain": "config-secrets", "generator": "2", "model": "mai-code-1.1-flash", "sources": "d1ae82c94bd90fb5ef2157f4c1850cb5a3610f12d2446ec06478139bfd53ea3f"} -->
# Configuration, workspace, and secrets

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Owns versioned TOML config, workspace metadata, opaque secret references, and private file protection for config and workspace storage.

Canonical docs: [docs/configuration.md](../../docs/configuration.md), [docs/adr/0008-versioned-toml-configuration.md](../../docs/adr/0008-versioned-toml-configuration.md), [docs/adr/0016-native-secret-store-boundary.md](../../docs/adr/0016-native-secret-store-boundary.md)

## Config startup

Use for: Startup chooses a config source, loads it on boot, and persists workspace or profile changes.

- [`select_configuration_source`](../../app/festerm/src/configuration_startup.rs) — Chooses the override or native config path while keeping the selected source private.
- [`ConfigurationReloader`](../../app/festerm/src/configuration_startup.rs) — Private loader and save gate for startup writes, auto-save reconciliation, and load status.

Evidence:
- [`unicode_override_takes_precedence_over_native_location`](../../app/festerm/src/configuration_startup.rs) — Validates override precedence before the native config location.
- [`explicit_workspace_save_creates_only_the_missing_native_directory_and_loads`](../../app/festerm/src/configuration_startup.rs) — Shows native directory creation, private save behavior, and config round-trip.

Caveats:
- Diagnostics intentionally omit the selected path and source TOML.
- Unknown or busy writes latch the save gate until restart.

## Config schema

Use for: Parsing, validating, or serializing a configuration document or workspace snapshot.

- [`Configuration`](../../crates/festerm-config/src/lib.rs) — Versioned TOML document holding profiles, workspace, settings, and accounts.
- [`WorkspaceConfiguration`](../../crates/festerm-config/src/workspace.rs) — Metadata-only tab and window restore state with geometry and profile validation.

Evidence:
- [`saves_and_loads_a_complete_configuration`](../../crates/festerm-config/src/lib.rs) — Round-trips a valid document through file save and load.
- [`without_profile_deletes_an_unreferenced_profile_but_rejects_a_workspace_reference`](../../crates/festerm-config/src/lib.rs) — Ensures deleted profiles cannot leave invalid workspace references.

Caveats:
- Workspace metadata is valid only when workspace_enabled is true and the document is non-empty.
- Profile IDs and workspace tab profile_id references must stay unique and valid.

## Secret boundary

Use for: A profile password, refresh token, or credential is created, rotated, or loaded from native storage.

- [`CredentialReference`](../../crates/festerm-config/src/profiles.rs) — TOML wrapper that stores a UUID string without exposing secret material in debug output.
- [`SecretStore`](../../crates/festerm-secret-store/src/lib.rs) — Native store contract for create, get, update, and delete of secret bytes.

Evidence:
- [`persists_an_opaque_ssh_credential_reference_without_exposing_it_in_debug_output`](../../crates/festerm-config/src/lib.rs) — Shows config persists only the opaque UUID reference and redacts debug output.
- [`references_are_random_canonical_uuid_v4_values`](../../crates/festerm-secret-store/src/lib.rs) — Proves the persisted reference value is canonical, random, and opaque.

Caveats:
- Config files store only IDs; no secret bytes, plain values, or key material.
- Unsupported, missing, locked, or unavailable native storage is a first-class error.

## File security

Use for: Creating or preserving a private config directory or staging file on Unix or Windows.

- [`make_private_directory`](../../crates/festerm-unix-security/src/lib.rs) — Ensures a directory is owned and restricted to the current user.
- [`create_current_user_only_file`](../../crates/festerm-windows-security/src/lib.rs) — Creates a file with a current-user-only DACL and safe publication flow.

Evidence:
- [`private_directory_clears_inherited_access`](../../crates/festerm-unix-security/src/lib.rs) — Validates private directory ACL cleanup before committing it.
- [`private_creation_and_conditional_publication_keep_the_current_user_only_dacl`](../../crates/festerm-windows-security/src/lib.rs) — Shows private creation and publication preserve the current-user-only DACL.

Caveats:
- Private file creation rejects inherited or foreign ACLs before publication.
- Directory ancestry and ownership must remain trusted; otherwise the secure path is not accepted.
