<!-- agent-navigation: {"domain": "config-secrets", "generator": "2", "model": "mai-code-1.1-flash", "sources": "8fc1a0b2a208b1d55190d3cd4315de595aebeceb53581fe3c6ddf4b6cb03b017"} -->
# Configuration, workspace, and secrets

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Owns versioned TOML configuration, opaque native secret references, and private filesystem protections for config and workspace data.

Canonical docs: [docs/configuration.md](../../docs/configuration.md), [docs/adr/0008-versioned-toml-configuration.md](../../docs/adr/0008-versioned-toml-configuration.md), [docs/adr/0016-native-secret-store-boundary.md](../../docs/adr/0016-native-secret-store-boundary.md)

## Config startup

Use for: When startup selects a config source or an auto-save writes workspace, profile, or settings changes.

- [`ConfigurationReloader`](../../app/festerm/src/configuration_startup.rs) — Private config source plus shared save gate and auto-save lifecycle
- [`select_configuration_source`](../../app/festerm/src/configuration_startup.rs) — Chooses override or native config path and keeps path private

Evidence:
- [`unicode_override_takes_precedence_over_native_location`](../../app/festerm/src/configuration_startup.rs) — Proves override wins before native configuration location
- [`explicit_workspace_save_creates_only_the_missing_native_directory_and_loads`](../../app/festerm/src/configuration_startup.rs) — Shows native config directory creation and round-trip save behavior

Caveats:
- Diagnostics intentionally omit the selected path and source TOML.
- Unknown or busy writes latch the save gate until restart.

## Config schema

Use for: When parsing, validating, or serializing a TOML document or workspace snapshot.

- [`Configuration`](../../crates/festerm-config/src/lib.rs) — Versioned document with profiles, workspace opt-in, and settings
- [`WorkspaceConfiguration`](../../crates/festerm-config/src/workspace.rs) — Metadata-only tab and window restore state with geometry validation

Evidence:
- [`saves_and_loads_a_complete_configuration`](../../crates/festerm-config/src/lib.rs) — Round-trips a full, valid configuration document
- [`without_profile_deletes_an_unreferenced_profile_but_rejects_a_workspace_reference`](../../crates/festerm-config/src/lib.rs) — Ensures deleted profiles cannot leave invalid workspace references

Caveats:
- Workspace metadata is valid only when workspace_enabled is true and the document is non-empty.
- Profile identifiers and workspace tab profile_id references must remain unique and valid.

## Secret boundary

Use for: When a profile password or account refresh token is created, rotated, or loaded.

- [`SecretReference`](../../crates/festerm-secret-store/src/lib.rs) — Opaque UUID handle; secrets remain in native keychain-backed storage
- [`CredentialReference`](../../crates/festerm-config/src/profiles.rs) — TOML-only wrapper that serializes as a UUID string and redacts debug output

Evidence:
- [`persists_an_opaque_ssh_credential_reference_without_exposing_it_in_debug_output`](../../crates/festerm-config/src/lib.rs) — Shows credential refs are persisted but never exposed in debug output
- [`atomically_saves_and_loads_an_opaque_credential_reference`](../../crates/festerm-config/src/lib.rs) — Proves the config round-trips only the opaque reference value

Caveats:
- Config files store opaque IDs only; no secret bytes, plain values, or key material.
- Unsupported, missing, locked, or unavailable native storage remains a first-class error.

## File security

Use for: When creating or preserving a private config directory or staging file on Unix or Windows.

- [`make_private_directory`](../../crates/festerm-unix-security/src/lib.rs) — Ensures a directory is owned and restricted to the current user
- [`create_current_user_only_file`](../../crates/festerm-windows-security/src/lib.rs) — Creates a file with a current-user-only DACL and safe publication flow

Evidence:
- [`private_directory_clears_inherited_access`](../../crates/festerm-unix-security/src/lib.rs) — Validates private-directory ACL cleanup before use
- [`private_creation_and_conditional_publication_keep_the_current_user_only_dacl`](../../crates/festerm-windows-security/src/lib.rs) — Shows creation and publication preserve the private current-user DACL

Caveats:
- Private file creation rejects inherited or foreign ACLs before publication.
- Directory ancestry and ownership must remain trusted; otherwise the secure path is not accepted.
