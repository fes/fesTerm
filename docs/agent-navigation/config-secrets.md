<!-- agent-navigation: {"domain": "config-secrets", "generator": "2", "model": "mai-code-1.1-flash", "sources": "878b7e23c8983dfa9f1f4a4b26729754bad2ecf43e72e9ea0c3a6637067f23f4"} -->
# Configuration, workspace, and secrets

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

This domain owns validated configuration and workspace metadata, opaque native credential references, and current-user private file and process security boundaries.

Canonical docs: [docs/configuration.md](../../docs/configuration.md), [docs/adr/0008-versioned-toml-configuration.md](../../docs/adr/0008-versioned-toml-configuration.md), [docs/adr/0016-native-secret-store-boundary.md](../../docs/adr/0016-native-secret-store-boundary.md)

## Config startup

Use for: Choosing the active config source and persisting automatic startup saves safely.

- [`select_configuration_source`](../../app/festerm/src/configuration_startup.rs) — Selects the env override or native config path without exposing it.
- [`ConfigurationReloader`](../../app/festerm/src/configuration_startup.rs) — Private startup loader and save gate for automatic config writes.

Evidence:
- [`unicode_override_takes_precedence_over_native_location`](../../app/festerm/src/configuration_startup.rs) — The override wins before the native config directory.
- [`explicit_workspace_save_creates_only_the_missing_native_directory_and_loads`](../../app/festerm/src/configuration_startup.rs) — It creates only the missing native directory and reloads cleanly.

Caveats:
- Diagnostics intentionally hide the selected file path and TOML.
- Unknown or busy writes latch the save gate until restart.
- The selected path is intentionally inaccessible outside this module.

## Config schema

Use for: Validating configuration and workspace persistence without secret or runtime state.

- [`Configuration`](../../crates/festerm-config/src/lib.rs) — Versioned TOML schema covering profiles, workspace, settings, and accounts.
- [`WorkspaceConfiguration`](../../crates/festerm-config/src/workspace.rs) — Metadata-only restore state with tab and window validation.
- [`DevTunnelFilesProfileConfiguration`](../../crates/festerm-config/src/profiles.rs) — Files destination metadata; it grants no connection or credential authority.

Evidence:
- [`workspace_replacement_preserves_profiles_and_validates_references`](../../crates/festerm-config/src/lib.rs) — Workspace validation and profile reference checks are enforced.
- [`dev_tunnel_files_profile_roundtrip_keeps_only_metadata_and_exact_workspace_kind`](../../crates/festerm-config/src/lib.rs) — It round-trips only metadata and exact workspace tab kind.

Caveats:
- Workspace metadata is valid only when workspace_enabled is true and the document is non-empty.
- Profile IDs and workspace tab profile_id references must stay unique and valid.
- Config files store metadata, never session content or secret bytes.

## Secret boundary

Use for: Persisting credentials and opaque IDs outside config while keeping secret bytes native-only.

- [`CredentialReference`](../../crates/festerm-config/src/profiles.rs) — Wraps an opaque native-store reference rather than the secret itself.
- [`SecretReference`](../../crates/festerm-secret-store/src/lib.rs) — Canonical UUIDv4 secret identifier stored in config metadata.
- [`SecretStore`](../../crates/festerm-secret-store/src/lib.rs) — Native store contract for create, get, update, and delete of secret bytes.

Evidence:
- [`atomically_saves_and_loads_an_opaque_credential_reference`](../../crates/festerm-config/src/lib.rs) — The file stores only the opaque reference and reloads it unchanged.
- [`references_are_random_canonical_uuid_v4_values`](../../crates/festerm-secret-store/src/lib.rs) — Persisted references are random, canonical, and opaque.

Caveats:
- Config files store only IDs; never secret bytes, plain values, or key material.
- Unsupported, missing, locked, or unavailable native storage is a first-class error.
- Secret references are opaque and must remain canonical UUIDv4 strings.

## File security

Use for: Making config directories and save payloads private before publication on Unix or Windows.

- [`make_private_directory`](../../crates/festerm-unix-security/src/lib.rs) — Ensures a directory is owned and restricted to the current user.
- [`create_current_user_only_file`](../../crates/festerm-windows-security/src/lib.rs) — Creates a file with a current-user-only DACL and verified publication flow.

Evidence:
- [`private_directory_clears_inherited_access`](../../crates/festerm-unix-security/src/lib.rs) — It validates inherited ACL cleanup before a private directory is committed.
- [`private_creation_and_conditional_publication_keep_the_current_user_only_dacl`](../../crates/festerm-windows-security/src/lib.rs) — Private creation and publication preserve the current-user-only DACL.

Caveats:
- New private objects are secured and verified; unsafe existing roots refuse access.
- Configuration and workspace files contain metadata, never credential or terminal content.

## Starter context

Use for: Comparing process identity and startup provenance before a Windows successor is trusted.

- [`same_start_context`](../../crates/festerm-windows-security/src/lib.rs) — Matches user, logon, session, integrity, and elevation before reusing startup state.
- [`verify_successor`](../../crates/festerm-windows-security/src/independent_start/windows.rs) — Bounds the same-image successor lifetime and startup context.

Evidence:
- [`unicode_environment_safearray_preserves_drive_entries_and_wide_values`](../../crates/festerm-windows-security/src/independent_start/windows.rs) — It preserves drive entries and wide UTF-16 environment values.

Caveats:
- Context equality and WMI PIDs confer no creation consent, session, or termination authority.
