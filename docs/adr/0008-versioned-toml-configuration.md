# ADR 0008: Versioned TOML Configuration with Safe Hot Reload

- **Status:** Accepted
- **Date:** 2026-08-05

## Context

Users should be able to understand, edit, back up, and version their fesTerm configuration without proprietary tooling. The configuration model will evolve as profiles, workspaces, keybindings, themes, and SSH capabilities grow.

Some settings can be applied while the application is running, while others may require rebuilding sessions or restarting the application.

## Decision

fesTerm will use human-readable TOML for primary user configuration and profile metadata.

Configuration documents will include an explicit schema version. The configuration layer will parse and validate a complete candidate document before applying it. Invalid reloads will leave the last valid configuration active and produce actionable diagnostics.

Settings will hot reload where doing so is safe and understandable. Settings that cannot be applied live will be marked as requiring session recreation or application restart.

Secret values will not be stored in ordinary TOML. Configuration may contain opaque references to operating-system secure-storage entries.

## Consequences

- Configuration remains portable and reviewable.
- Schema evolution and migrations must be designed deliberately.
- The parser must distinguish warnings, unsupported fields, and fatal errors.
- Hot reload needs transactional validation and an application-level change model.
- Sensitive values remain outside normal files and synchronization.
