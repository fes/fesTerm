<!-- agent-navigation: {"domain": "config-secrets", "generator": "2", "model": "mai-code-1.1-flash", "sources": "8941541894007e9e22a170e680d1b4984fd5b0a352aa3f539ab4ec31bb5b3e39"} -->
# Configuration, workspace, and secrets

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Validated configuration and workspace metadata, opaque secret references, and per-user file and process security boundaries.

Canonical docs: [docs/configuration.md](../../docs/configuration.md), [docs/adr/0008-versioned-toml-configuration.md](../../docs/adr/0008-versioned-toml-configuration.md), [docs/adr/0016-native-secret-store-boundary.md](../../docs/adr/0016-native-secret-store-boundary.md)

## Config startup

Use for: Which config source wins before startup and how saves are gated?

- [`select_configuration_source`](../../app/festerm/src/configuration_startup.rs) — Chooses an explicit override or native config path and keeps it private.
- [`ConfigurationReloader`](../../app/festerm/src/configuration_startup.rs) — Private loader and save gate for automatic startup writes.

Evidence:
- [`unicode_override_takes_precedence_over_native_location`](../../app/festerm/src/configuration_startup.rs) — Override wins before the native config location.
- [`explicit_workspace_save_creates_only_the_missing_native_directory_and_loads`](../../app/festerm/src/configuration_startup.rs) — Private native directory creation and round-trip save behavior.

Caveats:
- Diagnostics avoid exposing the selected path or source TOML.
- Unknown or busy writes latch the save gate until restart.
- The selected path is intentionally inaccessible outside this module.

## Config schema

Use for: What counts as valid config and workspace metadata?

- [`Configuration`](../../crates/festerm-config/src/lib.rs) — Versioned TOML document for profiles, workspace, settings, and accounts.
- [`WorkspaceConfiguration`](../../crates/festerm-config/src/workspace.rs) — Metadata-only restore state with tab and window validation.

Evidence:
- [`workspace_replacement_preserves_profiles_and_validates_references`](../../crates/festerm-config/src/lib.rs) — Workspace validation preserves profiles while rejecting bad references.
- [`dev_tunnel_files_profile_roundtrip_keeps_only_metadata_and_exact_workspace_kind`](../../crates/festerm-config/src/lib.rs) — Files profile round-trips with metadata only and exact tab kind.

Caveats:
- Workspace metadata is valid only when workspace_enabled is true and the document is non-empty.
- Profile IDs and workspace tab profile_id references must stay unique and valid.
- Config files store metadata, never session content or secret bytes.

## Secret boundary

Use for: Where are secret references stored and how are they kept opaque?

- [`CredentialReference`](../../crates/festerm-config/src/profiles.rs) — Opaque native secret reference kept out of config contents.
- [`SecretReference`](../../crates/festerm-secret-store/src/lib.rs) — Random UUIDv4 handle owned by the platform secret store.

Evidence:
- [`atomically_saves_and_loads_an_opaque_credential_reference`](../../crates/festerm-config/src/lib.rs) — Config persists only the opaque reference and keeps the value hidden.
- [`references_are_random_canonical_uuid_v4_values`](../../crates/festerm-secret-store/src/lib.rs) — Generated references are canonical UUIDv4 values and not secret data.

Caveats:
- Config files store only IDs and references, never secret bytes or plain values.
- Unsupported, missing, locked, or unavailable native storage is a first-class error.
- Secret references remain opaque UUIDv4 strings.

## File security

Use for: How are private config files and directories locked down on each OS?

- [`make_private_directory`](../../crates/festerm-unix-security/src/lib.rs) — Clears inherited ACLs and enforces 0700 before staging.
- [`create_current_user_only_file`](../../crates/festerm-windows-security/src/lib.rs) — Creates a current-user-only DACL and verifies the exact handle.

Evidence:
- [`private_directory_clears_inherited_access`](../../crates/festerm-unix-security/src/lib.rs) — Directory ACLs are stripped before private mode is committed.
- [`private_creation_and_conditional_publication_keep_the_current_user_only_dacl`](../../crates/festerm-windows-security/src/lib.rs) — The created file keeps the current-user-only DACL through publication.

Caveats:
- New private objects are secured and verified before publication.
- Unsafe existing roots refuse access rather than silently repairing them.
- Metadata files never hold credential or terminal bytes.
