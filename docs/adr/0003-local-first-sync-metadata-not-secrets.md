# ADR 0003: Local-First Operation and Metadata-Only Sync

- **Status:** Accepted
- **Date:** 2026-08-05

## Context

fesTerm may eventually allow users to sign in through an identity provider and synchronize profiles or workspace information across computers. Terminal and SSH configuration can also contain sensitive material, including passwords, private keys, tokens, host details, command history, and terminal output.

Cloud synchronization must not become a prerequisite for using a local terminal or SSH client, and ordinary profile synchronization must not become an accidental secret-distribution system.

## Decision

fesTerm will be local-first. Core terminal, local-shell, SSH, profile, and workspace functionality must work without an account or network-based synchronization service.

Optional synchronization may later include non-secret profile metadata, preferences, and workspace definitions.

Passwords, private keys, tokens, and other credentials must not be stored in ordinary synchronized profile data. Secrets will use platform-appropriate secure storage or another explicitly selected secret-management mechanism.

Terminal scrollback will not be persisted to disk by default. Any future persistent history will require explicit, visible retention and clearing controls.

## Consequences

### Positive

- The application remains useful offline and independent of a service provider.
- A compromised sync dataset does not automatically expose private keys or passwords.
- Cloud identity and synchronization can be added behind a replaceable boundary.
- Privacy expectations for terminal output remain conservative by default.

### Negative

- Users may need to provision credentials independently on each device.
- Cross-device profile synchronization may produce incomplete profiles until local secrets are associated.
- Platform key-store implementations require operating-system-specific integration.

## Follow-up

- Define stable identifiers that associate synchronized profiles with local secret records.
- Specify how missing local credentials are presented after profile synchronization.
- Select serialization, encryption, account-provider, and conflict-resolution strategies only when sync work is scheduled.
- Document all retained data and provide user-accessible clearing controls.
