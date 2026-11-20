<!-- agent-navigation: {"domain": "config-secrets", "generator": "2", "model": "mai-code-1.1-flash", "sources": "9a8dea8652c692c743bbd1ddefc6e3f772918d0e2c915829da94c049ec813bef"} -->
# Configuration, workspace, and secrets

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Owns validated configuration metadata, opaque native credentials, and current-user private storage boundaries for startup and persistence.

Canonical docs: [docs/configuration.md](../../docs/configuration.md), [docs/adr/0008-versioned-toml-configuration.md](../../docs/adr/0008-versioned-toml-configuration.md), [docs/adr/0016-native-secret-store-boundary.md](../../docs/adr/0016-native-secret-store-boundary.md)

## Config startup

Use for: Choosing the active config path and persisting startup state without exposing it.

- [`select_configuration_source`](../../app/festerm/src/configuration_startup.rs) — Selects override or native config path and keeps it private.
- [`ConfigurationReloader`](../../app/festerm/src/configuration_startup.rs) — Private startup loader and automatic-save gate around config writes.

Evidence:
- [`unicode_override_takes_precedence_over_native_location`](../../app/festerm/src/configuration_startup.rs) — Proves override wins before native config location.
- [`explicit_workspace_save_creates_only_the_missing_native_directory_and_loads`](../../app/festerm/src/configuration_startup.rs) — Shows private native-directory creation and round-trip save behavior.

Caveats:
- Selected path and source TOML stay intentionally opaque to diagnostics.
- Unknown or busy writes latch the save gate until restart.
- The selected file is private to the startup/reload boundary.

## Config schema

Use for: Validating workspace metadata, profile references, and persisted settings.

- [`Configuration`](../../crates/festerm-config/src/lib.rs) — Versioned TOML document for profiles, workspace, settings, and accounts.
- [`WorkspaceConfiguration`](../../crates/festerm-config/src/workspace.rs) — Metadata-only workspace state with tab and window validation.

Evidence:
- [`workspace_replacement_preserves_profiles_and_validates_references`](../../crates/festerm-config/src/lib.rs) — Confirms workspace validation and profile-reference checks.
- [`dev_tunnel_files_profile_roundtrip_keeps_only_metadata_and_exact_workspace_kind`](../../crates/festerm-config/src/lib.rs) — Checks Files schema and exact restored tab kind.

Caveats:
- Workspace metadata is valid only when workspace_enabled is true and the document is non-empty.
- Profile IDs and tab profile_id references must remain unique and valid.
- Config stores metadata, never session content or secret bytes.

## Secret boundary

Use for: Keeping opaque native credential references out of ordinary config files.

- [`CredentialReference`](../../crates/festerm-config/src/profiles.rs) — Wraps an opaque native-store reference rather than password or token data.
- [`SecretStore`](../../crates/festerm-secret-store/src/lib.rs) — Native store contract for create, get, update, and delete of secret bytes.

Evidence:
- [`atomically_saves_and_loads_an_opaque_credential_reference`](../../crates/festerm-config/src/lib.rs) — Shows config persists only the opaque reference and keeps the value opaque.
- [`references_are_random_canonical_uuid_v4_values`](../../crates/festerm-secret-store/src/lib.rs) — Proves persisted references are canonical, random, and opaque.

Caveats:
- Config files store only IDs and metadata; never secret bytes or key material.
- Unsupported, missing, locked, or unavailable native storage is a first-class error.
- Credential references must remain canonical UUIDv4 strings.

## File security

Use for: Securing config directories and save payloads before publication on Unix or Windows.

- [`make_private_directory`](../../crates/festerm-unix-security/src/lib.rs) — Ensures a directory is owned and restricted to the current user.
- [`create_current_user_only_file`](../../crates/festerm-windows-security/src/lib.rs) — Creates a file with a current-user-only DACL and verified publication flow.

Evidence:
- [`private_directory_clears_inherited_access`](../../crates/festerm-unix-security/src/lib.rs) — Validates inherited ACL cleanup before a private directory is committed.
- [`private_creation_and_conditional_publication_keep_the_current_user_only_dacl`](../../crates/festerm-windows-security/src/lib.rs) — Shows private creation and publication preserve the current-user-only DACL.

Caveats:
- New private objects are secured and verified; unsafe existing roots refuse access.
- Configuration and workspace files contain metadata, never credential or terminal content.
- Successful creation alone does not prove safe publication or ownership.
