<!-- agent-navigation: {"domain": "config-secrets", "generator": "2", "model": "mai-code-1.1-flash", "sources": "39869b0a37e8eb8cd1e5bf94699cc4f0f2cccbdf72a69cd62e7d502daeead864"} -->
# Configuration, workspace, and secrets

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

This domain owns validated configuration and workspace metadata, opaque native credential references, and user-private file and process security boundaries for safe persistence.

Canonical docs: [docs/configuration.md](../../docs/configuration.md), [docs/adr/0008-versioned-toml-configuration.md](../../docs/adr/0008-versioned-toml-configuration.md), [docs/adr/0016-native-secret-store-boundary.md](../../docs/adr/0016-native-secret-store-boundary.md)

## Config startup

Use for: Choose the active config source and persist automatic workspace and settings saves safely.

- [`select_configuration_source`](../../app/festerm/src/configuration_startup.rs) — Chooses the env override or native path without exposing the value.
- [`ConfigurationReloader`](../../app/festerm/src/configuration_startup.rs) — Private startup loader and save gate for auto writes.

Evidence:
- [`unicode_override_takes_precedence_over_native_location`](../../app/festerm/src/configuration_startup.rs) — The override wins before the native config directory.
- [`explicit_workspace_save_creates_only_the_missing_native_directory_and_loads`](../../app/festerm/src/configuration_startup.rs) — Only the missing native directory is created and reloads stay clean.

Caveats:
- Diagnostics hide the selected file path and TOML.
- Unknown or busy writes latch the save gate until restart.
- The selected source is intentionally private to this module.

## Config schema

Use for: Validate saved configuration, workspace metadata, and settings without secret or runtime state.

- [`Configuration`](../../crates/festerm-config/src/lib.rs) — Versioned TOML schema for profiles, workspace, settings, and accounts.
- [`WorkspaceConfiguration`](../../crates/festerm-config/src/workspace.rs) — Metadata-only restore state with tab and window validation.
- [`InterfaceSettings`](../../crates/festerm-config/src/settings.rs) — Persisted UI behavior and terminal defaults, not live session state.

Evidence:
- [`workspace_replacement_preserves_profiles_and_validates_references`](../../crates/festerm-config/src/lib.rs) — Workspace validation and profile reference checks are enforced.
- [`dev_tunnel_files_profile_roundtrip_keeps_only_metadata_and_exact_workspace_kind`](../../crates/festerm-config/src/lib.rs) — It round-trips only metadata and exact workspace tab kind.

Caveats:
- Workspace metadata is valid only when workspace_enabled is true and the document is non-empty.
- Profile IDs and workspace tab profile_id references must stay unique and valid.
- Config files store metadata, never session content or secret bytes.

## Secret boundary

Use for: Persist credential references in config while keeping secret bytes only inside the native store.

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

Use for: Make config directories and save payloads private before publication on Unix or Windows.

- [`make_private_directory`](../../crates/festerm-unix-security/src/lib.rs) — Ensures a directory is owned and restricted to the current user.
- [`create_current_user_only_file`](../../crates/festerm-windows-security/src/lib.rs) — Creates a file with a current-user-only DACL and verified publication flow.

Evidence:
- [`private_directory_clears_inherited_access`](../../crates/festerm-unix-security/src/lib.rs) — It validates inherited ACL cleanup before a private directory is committed.
- [`private_creation_and_conditional_publication_keep_the_current_user_only_dacl`](../../crates/festerm-windows-security/src/lib.rs) — Private creation and publication preserve the current-user-only DACL.

Caveats:
- New private objects are secured and verified; unsafe existing roots refuse access.
- Configuration and workspace files contain metadata, never credential or terminal content.
- File-security helpers are about publication safety, not connection authority.
