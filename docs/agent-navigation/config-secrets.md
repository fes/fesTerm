<!-- agent-navigation: {"domain": "config-secrets", "generator": "2", "model": "codex-source-verified", "sources": "28fbab10ca9fe3b7534c034797aac07a84568089e81a4e04fa3ee8af00267ac3"} -->
# Configuration, workspace, and secrets

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Owns validated configuration and workspace metadata, opaque native credential references, and current-user file/process security boundaries.

Canonical docs: [docs/configuration.md](../../docs/configuration.md), [docs/adr/0008-versioned-toml-configuration.md](../../docs/adr/0008-versioned-toml-configuration.md), [docs/adr/0016-native-secret-store-boundary.md](../../docs/adr/0016-native-secret-store-boundary.md)

## Config startup

Use for: Config selection and startup persistence.

- [`select_configuration_source`](../../app/festerm/src/configuration_startup.rs) — Chooses explicit override or native config path and keeps it private.
- [`ConfigurationReloader`](../../app/festerm/src/configuration_startup.rs) — Private startup loader and save gate for automatic config writes.

Evidence:
- [`unicode_override_takes_precedence_over_native_location`](../../app/festerm/src/configuration_startup.rs) — Proves the override wins before the native config location.
- [`explicit_workspace_save_creates_only_the_missing_native_directory_and_loads`](../../app/festerm/src/configuration_startup.rs) — Shows private native-directory creation and round-trip save behavior.

Caveats:
- Diagnostics avoid exposing the selected path or source TOML.
- Unknown or busy writes latch the save gate until restart.
- The selected path is intentionally inaccessible outside this module.

## Config schema

Use for: Validated config and workspace metadata.

- [`Configuration`](../../crates/festerm-config/src/lib.rs) — Versioned TOML document for profiles, workspace, settings, and accounts.
- [`WorkspaceConfiguration`](../../crates/festerm-config/src/workspace.rs) — Metadata-only restore state with tab and window validation.
- [`DevTunnelFilesProfileConfiguration`](../../crates/festerm-config/src/profiles.rs) — Files destination/account metadata; restore grants no connection or credential authority.

Evidence:
- [`dev_tunnel_files_profile_roundtrip_keeps_only_metadata_and_exact_workspace_kind`](../../crates/festerm-config/src/lib.rs) — Checks Files schema and exact restored tab kind.
- [`workspace_replacement_preserves_profiles_and_validates_references`](../../crates/festerm-config/src/lib.rs) — Confirms workspace validation and profile-reference checks.

Caveats:
- Workspace metadata is valid only when workspace_enabled is true and the document is non-empty.
- Profile IDs and workspace tab profile_id references must stay unique and valid.
- Config files store metadata, never session content or secret bytes.

## Secret boundary

Use for: Native credential and opaque-reference custody.

- [`CredentialReference`](../../crates/festerm-config/src/profiles.rs) — Wraps an opaque native-store reference rather than password or token data.
- [`SecretStore`](../../crates/festerm-secret-store/src/lib.rs) — Native store contract for create, get, update, and delete of secret bytes.

Evidence:
- [`atomically_saves_and_loads_an_opaque_credential_reference`](../../crates/festerm-config/src/lib.rs) — Shows config persists only the opaque reference and keeps the value opaque.
- [`references_are_random_canonical_uuid_v4_values`](../../crates/festerm-secret-store/src/lib.rs) — Proves persisted references are canonical, random, and opaque.

Caveats:
- Config files store only IDs; never secret bytes, plain values, or key material.
- Unsupported, missing, locked, or unavailable native storage is a first-class error.
- Secret references are opaque and must remain canonical UUIDv4 strings.

## File security

Use for: How are config directories and save payloads kept private before publication on Unix or Windows?

- [`make_private_directory`](../../crates/festerm-unix-security/src/lib.rs) — Ensures a directory is owned and restricted to the current user.
- [`create_current_user_only_file`](../../crates/festerm-windows-security/src/lib.rs) — Creates a file with a current-user-only DACL and verified publication flow.

Evidence:
- [`private_directory_clears_inherited_access`](../../crates/festerm-unix-security/src/lib.rs) — Validates inherited ACL cleanup before a private directory is committed.
- [`private_creation_and_conditional_publication_keep_the_current_user_only_dacl`](../../crates/festerm-windows-security/src/lib.rs) — Shows private creation and publication preserve the current-user-only DACL.

Caveats:
- New private objects are secured and verified; unsafe existing roots refuse access.
- Configuration and workspace files contain metadata, never credential or terminal content.
- Successful file creation alone does not prove private ownership or safe publication.

## Windows starter context

Use for: Hosted successor routing.

- [`same_start_context`](../../crates/festerm-windows-security/src/lib.rs) — Matches user, logon, session, integrity and elevation.
- [`verify_successor`](../../crates/festerm-windows-security/src/independent_start/windows.rs) — Bounds the same-image successor lifetime.

Evidence:
- [`unicode_environment_safearray_preserves_drive_entries_and_wide_values`](../../crates/festerm-windows-security/src/independent_start/windows.rs) — Preserves drive entries and UTF-16.

Caveats:
- Context equality and WMI PIDs confer no creation consent, session or termination authority.
