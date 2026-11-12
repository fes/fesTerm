<!-- agent-navigation: {"domain": "config-secrets", "generator": "2", "model": "mai-code-1.1-flash", "sources": "bd6a1d14cb6986f1fcbe9fa3338ab318c07d74b75464b2af7fd27df4035f53ad"} -->
# Configuration, workspace, and secrets

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Owns versioned, secret-free TOML configuration, metadata-only workspace restoration, and opaque native secret references with private startup save gating.

Canonical docs: [docs/configuration.md](../../docs/configuration.md), [docs/adr/0008-versioned-toml-configuration.md](../../docs/adr/0008-versioned-toml-configuration.md), [docs/adr/0016-native-secret-store-boundary.md](../../docs/adr/0016-native-secret-store-boundary.md)

## Config startup

Use for: At process start or when auto-saving settings, workspace, host trust, or profile changes

- [`ConfigurationReloader`](../../app/festerm/src/configuration_startup.rs) — Private selected path plus shared save gate and auto-save lifecycle
- [`select_configuration_source`](../../app/festerm/src/configuration_startup.rs) — Chooses FESTERM_CONFIG_PATH or native config directory and keeps path private

Evidence:
- [`docs/configuration.md`](../../docs/configuration.md) — Startup discovery, missing/invalid config behavior, and save-gate reconciliation policy
- [`unicode_override_takes_precedence_over_native_location`](../../app/festerm/src/configuration_startup.rs) — Proves override precedence and content-free diagnostics

Caveats:
- Diagnostics never expose the selected path or source TOML.
- An unknown or busy save latches the gate and blocks later writes until restart.

## Config schema

Use for: Parsing, validating, or serializing a TOML document

- [`Configuration`](../../crates/festerm-config/src/lib.rs) — Versioned configuration object with profiles, workspace opt-in, and settings
- [`WorkspaceConfiguration`](../../crates/festerm-config/src/workspace.rs) — Metadata-only tab/window order, focus, pane groups, and geometry validation

Evidence:
- [`docs/configuration.md`](../../docs/configuration.md) — Schema version 1, workspace opt-in semantics, and tab identity rules
- [`persists_an_opaque_ssh_credential_reference_without_exposing_it_in_debug_output`](../../crates/festerm-config/src/lib.rs) — Shows credential refs are opaque and persisted without secret leakage

Caveats:
- Profile and workspace identifiers must be valid and unique within their collections.
- Workspace metadata is only valid when workspace_enabled is true and the document is non-empty.

## Secret boundary

Use for: When a profile password or account refresh token is created, rotated, or read

- [`SecretReference`](../../crates/festerm-secret-store/src/lib.rs) — Opaque UUIDv4 handle; actual values stay in native keychain-backed storage
- [`CredentialReference`](../../crates/festerm-config/src/profiles.rs) — Metadata-only TOML shell that serializes as a UUID string but redacts debug output

Evidence:
- [`docs/adr/0016-native-secret-store-boundary.md`](../../docs/adr/0016-native-secret-store-boundary.md) — Decision that secret store owns storage; TOML contains only opaque references
- [`atomically_saves_and_loads_an_opaque_credential_reference`](../../crates/festerm-config/src/lib.rs) — Round-trips only the reference value and keeps it secret-free

Caveats:
- The native store owns secrets; config stores no secret bytes, plain values, or key material.
- Unsupported, missing, locked, or unavailable platform storage remains a first-class error.

## Profile lifecycle

Use for: Editing, deleting, reordering, or validating profile metadata and references

- [`with_profile`](../../crates/festerm-config/src/lib.rs) — Upserts a validated profile while preserving other config and workspace state
- [`without_profile`](../../crates/festerm-config/src/lib.rs) — Rejects deletion if workspace tabs still reference the profile

Evidence:
- [`without_profile_deletes_an_unreferenced_profile_but_rejects_a_workspace_reference`](../../crates/festerm-config/src/lib.rs) — Ensures deleted profiles cannot leave dangling workspace references
- [`saves_and_loads_a_complete_configuration`](../../crates/festerm-config/src/lib.rs) — Proves full document persistence and reload round-trip remain stable

Caveats:
- Profile IDs are unique, and workspace tab profile_id references must stay valid.
- A profile deletion can fail when it would otherwise leave a saved workspace in an invalid state.
