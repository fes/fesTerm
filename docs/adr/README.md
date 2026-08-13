# Architecture Decision Records

Architecture decision records preserve decisions that affect the project across subsystems or over time. They record context and consequences so future changes can revisit the reasoning rather than only the result.

## Accepted Decisions

- [ADR 0001: Terminal Compatibility Baseline](0001-terminal-compatibility-baseline.md)
- [ADR 0002: Separate Profiles and Workspaces](0002-separate-profiles-and-workspaces.md)
- [ADR 0003: Local-First Operation and Metadata-Only Sync](0003-local-first-sync-metadata-not-secrets.md)
- [ADR 0004: Componentized, Testable Terminal Core](0004-componentized-testable-terminal-core.md)
- [ADR 0005: Foundation-First Delivery with Capability Milestones](0005-foundation-first-capability-milestones.md)
- [ADR 0006: Native SSH Backend with OpenSSH Interoperability](0006-native-ssh-with-openssh-interoperability.md)
- [ADR 0007: `egui` Front End with a Pragmatic Renderer Boundary](0007-egui-front-end-pragmatic-renderer-boundary.md)
- [ADR 0008: Versioned TOML Configuration with Safe Hot Reload](0008-versioned-toml-configuration.md) — superseded by ADR 0015
- [ADR 0009: Mutable Core State with Early Observability](0009-mutable-core-and-early-observability.md)
- [ADR 0010: Preserve Future Extension Seams without Early Scope Expansion](0010-preserve-future-extension-seams.md)
- [ADR 0011: Trusted Windows ConPTY Runtime Selection](0011-trusted-windows-conpty-runtime-selection.md)
- [ADR 0012: Cell Geometry Owns Ligature and Fallback Mapping](0012-cell-geometry-owns-ligature-and-fallback-mapping.md)
- [ADR 0013: `russh` for Native SSH Transport](0013-russh-native-ssh-transport.md)
- [ADR 0014: Window, Workspace, Tab, and Session Ownership](0014-window-workspace-tab-session-ownership.md)
- [ADR 0015: Explicit Transactional Configuration Reload](0015-explicit-transactional-configuration-reload.md)
- [ADR 0016: Native Secret Store Boundary](0016-native-secret-store-boundary.md)
- [ADR 0017: Bounded Logical Scrollback and Anchored Viewports](0017-bounded-logical-scrollback-and-anchored-viewports.md)

## Status Values

- **Proposed:** under discussion and not yet binding.
- **Accepted:** the current project decision.
- **Superseded:** replaced by a later ADR.
- **Rejected:** considered and deliberately not adopted.

New decisions should use the next sequential number and should link to any ADR they supersede.
Start from [`TEMPLATE.md`](TEMPLATE.md). Every new or materially changed ADR
must include `## Validation impact` and update the machine-readable trace
registry when its requirement, scenario, or evidence relationships change.
