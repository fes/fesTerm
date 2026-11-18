<!-- agent-navigation: {"domain": "config-secrets", "generator": "2", "model": "codex-agent", "sources": "ffbdbfcd7656c5ae4ba86cc6825e3b169b04b368bc407f52da8fa9df38a36f8e"} -->
# Configuration, workspace, and secrets

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Owns validated configuration and workspace metadata, native credential references, and current-user file and process-context security boundaries.

Canonical docs: [docs/configuration.md](../../docs/configuration.md), [docs/adr/0008-versioned-toml-configuration.md](../../docs/adr/0008-versioned-toml-configuration.md), [docs/adr/0016-native-secret-store-boundary.md](../../docs/adr/0016-native-secret-store-boundary.md)

## Config startup

Use for: Which config file is selected and when startup persists workspace or profile updates?

- [`select_configuration_source`](../../app/festerm/src/configuration_startup.rs) — Chooses the explicit override or native config path and keeps it private.
- [`ConfigurationReloader`](../../app/festerm/src/configuration_startup.rs) — Private startup loader and save gate for config writes and reconciliation.

Evidence:
- [`unicode_override_takes_precedence_over_native_location`](../../app/festerm/src/configuration_startup.rs) — Proves the override wins before the native config location.
- [`explicit_workspace_save_creates_only_the_missing_native_directory_and_loads`](../../app/festerm/src/configuration_startup.rs) — Shows private native-directory creation and round-trip save behavior.

Caveats:
- Diagnostics avoid exposing the selected path or source TOML.
- Unknown or busy writes latch the save gate until restart.
- The selected path is intentionally inaccessible outside this module.

## Config schema

Use for: How are documents parsed, validated, and saved, including workspace restore metadata?

- [`Configuration`](../../crates/festerm-config/src/lib.rs) — Versioned TOML document holding profiles, workspace, settings, and accounts.
- [`WorkspaceConfiguration`](../../crates/festerm-config/src/workspace.rs) — Metadata-only restore state with tab and window validation.

Evidence:
- [`saves_and_loads_a_complete_configuration`](../../crates/festerm-config/src/lib.rs) — Round-trips a valid doc through file save and load.
- [`workspace_replacement_preserves_profiles_and_validates_references`](../../crates/festerm-config/src/lib.rs) — Confirms workspace validation and profile reference checks.

Caveats:
- Workspace metadata is valid only when workspace_enabled is true and the document is non-empty.
- Profile IDs and workspace tab profile_id references must stay unique and valid.
- Config files store metadata, never session content or secret bytes.

## Secret boundary

Use for: When is a profile password, refresh token, or credential created, rotated, or loaded from native storage?

- [`CredentialReference`](../../crates/festerm-config/src/profiles.rs) — Wraps an opaque native-store reference rather than a password or token value.
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

Use for: A hosted Windows start moves between an application-owned Job and a locally created WMI successor.

- [`same_start_context`](../../crates/festerm-windows-security/src/lib.rs) — Compares user, logon identity, session, integrity and elevation between processes.
- [`verify_successor`](../../crates/festerm-windows-security/src/independent_start/windows.rs) — Validates a live same-image parent and bounds the short-lived successor lifetime.

Evidence:
- [`unicode_environment_safearray_preserves_drive_entries_and_wide_values`](../../crates/festerm-windows-security/src/independent_start/windows.rs) — Checks native BSTR-array preservation of drive entries and UTF-16 values.

Caveats:
- Context equality is routing evidence, not creation consent or session authority.
- A WMI-returned PID is observed without granting process-termination authority.
- Generation, launch request, host lease and native binding remain separate checks.
