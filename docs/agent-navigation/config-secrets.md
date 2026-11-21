<!-- agent-navigation: {"domain": "config-secrets", "generator": "2", "model": "codex-source-verified", "sources": "d36187b0ba432feba7d33943b204747a9b0408cc29dccbae24134ca9ae8278fb"} -->
# Configuration, workspace, and secrets

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Owns config, workspace metadata and opaque native references.

Canonical docs: [docs/configuration.md](../../docs/configuration.md), [docs/adr/0008-versioned-toml-configuration.md](../../docs/adr/0008-versioned-toml-configuration.md), [docs/adr/0016-native-secret-store-boundary.md](../../docs/adr/0016-native-secret-store-boundary.md)

## Config startup

Use for: Config selection and autosave.

- [`select_configuration_source`](../../app/festerm/src/configuration_startup.rs) — Validates override versus native config path.
- [`ConfigurationReloader`](../../app/festerm/src/configuration_startup.rs) — Gates startup autosave.

Evidence:
- [`unicode_override_takes_precedence_over_native_location`](../../app/festerm/src/configuration_startup.rs) — Checks override precedence.
- [`explicit_workspace_save_creates_only_the_missing_native_directory_and_loads`](../../app/festerm/src/configuration_startup.rs) — Checks private-directory creation and save.

Caveats:
- Diagnostics avoid exposing the selected path or source TOML.
- Unknown or busy writes latch the save gate until restart.
- The selected path is intentionally inaccessible outside this module.

## Config schema

Use for: Validated workspace metadata.

- [`Configuration`](../../crates/festerm-config/src/lib.rs) — Versioned TOML metadata.
- [`WorkspaceConfiguration`](../../crates/festerm-config/src/workspace.rs) — Validates metadata-only restoration.
- [`CredentialReference`](../../crates/festerm-config/src/profiles.rs) — Opaque credential metadata.

Evidence:
- [`workspace_replacement_preserves_profiles_and_validates_references`](../../crates/festerm-config/src/lib.rs) — Checks workspace references.
- [`dev_tunnel_files_profile_roundtrip_keeps_only_metadata_and_exact_workspace_kind`](../../crates/festerm-config/src/lib.rs) — Checks exact Files tab kind.

Caveats:
- Workspace metadata is valid only when workspace_enabled is true and the document is non-empty.
- Profile IDs and workspace tab profile_id references must stay unique and valid.
- Config files store metadata, never session content or secret bytes.

## Secret boundary

Use for: Native credential custody.

- [`CredentialReference`](../../crates/festerm-config/src/profiles.rs) — Opaque reference, never a secret value.
- [`SecretStore`](../../crates/festerm-secret-store/src/lib.rs) — Owns native secret operations.

Evidence:
- [`atomically_saves_and_loads_an_opaque_credential_reference`](../../crates/festerm-config/src/lib.rs) — Checks reference-only persistence.
- [`references_are_random_canonical_uuid_v4_values`](../../crates/festerm-secret-store/src/lib.rs) — Checks random canonical references.

Caveats:
- Config files store only IDs; never secret bytes, plain values, or key material.
- Unsupported, missing, locked, or unavailable native storage is a first-class error.
- Secret references are opaque and must remain canonical UUIDv4 strings.
- Dedicated macOS owner-readiness disables process Keychain UI before store construction; shared foreground getters do not.

## File security

Use for: Private file publication.

- [`make_private_directory`](../../crates/festerm-unix-security/src/lib.rs) — Checks current-user directory access.
- [`create_current_user_only_file`](../../crates/festerm-windows-security/src/lib.rs) — Publishes current-user-only files.

Evidence:
- [`private_directory_clears_inherited_access`](../../crates/festerm-unix-security/src/lib.rs) — Checks inherited ACL cleanup.
- [`private_creation_and_conditional_publication_keep_the_current_user_only_dacl`](../../crates/festerm-windows-security/src/lib.rs) — Checks DACL preservation.

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

## Selected Mac HOST items

Use for: Fresh Desktop purpose creation and retained update.

- [`MacHostCreationContext`](../../crates/festerm-secret-store/src/host_items.rs) — Consumes one purpose/reference.
- [`HostPurposeStore`](../../crates/festerm-host-setup/src/host_items.rs) — Keeps HOST updates separate from ordinary accounts.

Evidence:
- [`mac_host_item_creation_only_follows_proven_absence`](../../crates/festerm-unix-security/src/macos_keychain.rs) — Refuses error-to-create fallback.
- [`host_cloud_fresh_creation_is_consumed_before_native_effect_and_never_rearmed`](../../crates/festerm-host-setup/src/cloud.rs) — Checks one-call creation.

Caveats:
- Existing pending receipts never reauthorize create; no native acceptance is claimed.
- Ad-hoc self and production signed-DR qualification stay distinct.
- No ACL migration, upsert or hostile same-user write-isolation claim.
