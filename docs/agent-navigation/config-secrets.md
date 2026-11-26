<!-- agent-navigation: {"domain": "config-secrets", "generator": "2", "model": "codex-source-verified", "sources": "3a4577137577578a0d69b43b070b9f35fdb544ce0ca4d96cd4ba57fdd70beda3"} -->
# Configuration, workspace, and secrets

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Owns config/workspace metadata, opaque references and private publication.

Canonical docs: [docs/configuration.md](../../docs/configuration.md), [docs/adr/0008-versioned-toml-configuration.md](../../docs/adr/0008-versioned-toml-configuration.md), [docs/adr/0016-native-secret-store-boundary.md](../../docs/adr/0016-native-secret-store-boundary.md)

## Config startup

Use for: Config selection and autosave.

- [`select_configuration_source`](../../app/festerm/src/configuration_startup.rs) — Selects override versus native path.
- [`ConfigurationReloader`](../../app/festerm/src/configuration_startup.rs) — Gates startup autosave.

Evidence:
- [`unicode_override_takes_precedence_over_native_location`](../../app/festerm/src/configuration_startup.rs) — Checks override precedence.
- [`explicit_workspace_save_creates_only_the_missing_native_directory_and_loads`](../../app/festerm/src/configuration_startup.rs) — Checks private creation and reload.

Caveats:
- Diagnostics hide the selected file path and TOML.
- Unknown or busy writes latch the save gate until restart.

## Config schema

Use for: Validate persisted metadata.

- [`Configuration`](../../crates/festerm-config/src/lib.rs) — Versioned TOML schema.
- [`WorkspaceConfiguration`](../../crates/festerm-config/src/workspace.rs) — Validates metadata-only restore.
- [`InterfaceSettings`](../../crates/festerm-config/src/settings.rs) — Persists UI defaults.

Evidence:
- [`workspace_replacement_preserves_profiles_and_validates_references`](../../crates/festerm-config/src/lib.rs) — Checks workspace/profile references.
- [`dev_tunnel_files_profile_roundtrip_keeps_only_metadata_and_exact_workspace_kind`](../../crates/festerm-config/src/lib.rs) — Checks exact metadata round-trip.

Caveats:
- Workspace metadata is valid only when workspace_enabled is true and the document is non-empty.
- Profile IDs and workspace tab profile_id references must stay unique and valid.

## Secret boundary

Use for: Native credential references.

- [`CredentialReference`](../../crates/festerm-config/src/profiles.rs) — Wraps an opaque reference.
- [`SecretReference`](../../crates/festerm-secret-store/src/lib.rs) — Canonical UUIDv4 metadata.
- [`SecretStore`](../../crates/festerm-secret-store/src/lib.rs) — Native store contract.

Evidence:
- [`atomically_saves_and_loads_an_opaque_credential_reference`](../../crates/festerm-config/src/lib.rs) — Checks opaque-reference round-trip.
- [`references_are_random_canonical_uuid_v4_values`](../../crates/festerm-secret-store/src/lib.rs) — Checks random canonical UUIDv4.

Caveats:
- Config files store only IDs; never secret bytes, plain values, or key material.
- Background updates never unlock or fall back to interactive methods.
- Opaque canonical refs only; native no-UI qualification remains separate.

## File security

Use for: Private publication and link metadata.

- [`make_private_directory`](../../crates/festerm-unix-security/src/lib.rs) — Checks private ownership.
- [`create_current_user_only_file`](../../crates/festerm-windows-security/src/lib.rs) — Creates a private DACL file.
- [`pin_data_link`](../../crates/festerm-unix-security/src/contained_data/link.rs) — Metadata only.
- [`open_ios_selected_directory`](../../crates/festerm-secret-store/src/native_root.rs) — Checks the selected root.

Evidence:
- [`private_directory_clears_inherited_access`](../../crates/festerm-unix-security/src/lib.rs) — Checks inherited ACL cleanup.
- [`private_creation_and_conditional_publication_keep_the_current_user_only_dacl`](../../crates/festerm-windows-security/src/lib.rs) — Checks DACL retention.

Caveats:
- Private creation retains owner, ACL, no-follow, descriptor and receipt-child checks.
- File-security helpers are about publication safety, not connection authority.

## Program storage settings

Use for: Reviewed digest-bound save and migration metadata

- [`JavaScriptProgramStorageSettings`](../../crates/festerm-config/src/settings.rs) — Persists explicit per-program storage review.
- [`TransactionalSaveStore`](../../app/festerm/src/javascript_program_storage.rs) — Owns exact reviewed save publication.

Evidence:
- [`javascript_program_storage_reopen_retains_confirmed_save_for_explicit_resume`](../../app/festerm/src/javascript_program_storage.rs) — Resume retains the selected confirmed save.

Caveats:
- Storage enablement, budgets and migration require explicit review; no path-derived authority.

## Selected Mac HOST items

Use for: Fresh Desktop purpose creation and retained update.

- [`MacHostCreationContext`](../../crates/festerm-secret-store/src/host_items.rs)
- [`HostPurposeStore`](../../crates/festerm-host-setup/src/host_items.rs)
- [`update_existing_noninteractive`](../../crates/festerm-secret-store/src/host_items.rs)

Evidence:
- [`mac_host_item_creation_only_follows_proven_absence`](../../crates/festerm-unix-security/src/macos_keychain.rs) — Refuses error-to-create fallback.
- [`host_cloud_fresh_creation_is_consumed_before_native_effect_and_never_rearmed`](../../crates/festerm-host-setup/src/cloud.rs) — Checks one-call creation.

Caveats:
- Old references never rearm; new Desktop approval reserves a distinct reference after private retention.
- Ad-hoc self and production signed-DR qualification stay distinct.
- No ACL migration, upsert or hostile same-user write-isolation claim.
