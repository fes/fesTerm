# Architecture Decision Records

Architecture decision records preserve decisions that affect the project across subsystems or over time. They record context and consequences so future changes can revisit the reasoning rather than only the result.

## Accepted Decisions

- [ADR 0001: Terminal Compatibility Baseline](0001-terminal-compatibility-baseline.md)
- [ADR 0002: Separate Profiles and Workspaces](0002-separate-profiles-and-workspaces.md)
- [ADR 0003: Local-First Operation and Metadata-Only Sync](0003-local-first-sync-metadata-not-secrets.md) — local-first/no-secret-sync remains accepted; see the 2026-09-01 status update on cloud metadata sync scope
- [ADR 0004: Componentized, Testable Terminal Core](0004-componentized-testable-terminal-core.md)
- [ADR 0005: Foundation-First Delivery with Capability Milestones](0005-foundation-first-capability-milestones.md)
- [ADR 0006: Native SSH Backend with OpenSSH Interoperability](0006-native-ssh-with-openssh-interoperability.md)
- [ADR 0007: `egui` Front End with a Pragmatic Renderer Boundary](0007-egui-front-end-pragmatic-renderer-boundary.md)
- [ADR 0009: Mutable Core State with Early Observability](0009-mutable-core-and-early-observability.md)
- [ADR 0010: Preserve Future Extension Seams without Early Scope Expansion](0010-preserve-future-extension-seams.md)
- [ADR 0011: Trusted Windows ConPTY Runtime Selection](0011-trusted-windows-conpty-runtime-selection.md)
- [ADR 0012: Cell Geometry Owns Ligature and Fallback Mapping](0012-cell-geometry-owns-ligature-and-fallback-mapping.md)
- [ADR 0013: `russh` for Native SSH Transport](0013-russh-native-ssh-transport.md) — reconnect-default policy refined by ADR 0018
- [ADR 0014: Window, Workspace, Tab, and Session Ownership](0014-window-workspace-tab-session-ownership.md)
- [ADR 0015: Startup Load and Transactional Autosave Without External Reload](0015-explicit-transactional-configuration-reload.md)
- [ADR 0016: Native Secret Store Boundary](0016-native-secret-store-boundary.md) — extended to stored private keys by ADR 0024
- [ADR 0017: Bounded Logical Scrollback and Anchored Viewports](0017-bounded-logical-scrollback-and-anchored-viewports.md)
- [ADR 0018: SSH Liveness, Reconnect, and Persistent Session Recovery](0018-ssh-liveness-reconnect-and-persistent-session-recovery.md)
- [ADR 0019: Fingerprint-First SSH Authentication Ordering](0019-fingerprint-first-ssh-authentication-ordering.md)
- [ADR 0020: Persistent Host-Key Trust](0020-persistent-host-key-trust.md)
- [ADR 0021: `cargo-packager` and GitHub Releases for Distribution and Updates](0021-cargo-packager-github-releases-distribution.md)
- [ADR 0022: Focused-Chip-First Single-Row Chrome Allocation](0022-focused-chip-first-single-row-chrome.md)
- [ADR 0023: `serialport` Crate and Worker-Thread Model for the Serial Session Backend](0023-serialport-worker-thread-serial-backend.md)
- [ADR 0024: Native Secret Store Extended to Stored Private Keys](0024-native-secret-store-stored-private-keys.md)
- [ADR 0026: Grapheme Width and Color Emoji Fallback](0026-grapheme-width-and-color-emoji-fallback.md)
- [ADR 0032: Single-Process Multi-Window via egui Viewports](0032-single-process-multi-window.md) — see issue #119
- [ADR 0033: Moving Tabs Between Windows](0033-moving-tabs-between-windows.md) — builds on ADR 0032
- [ADR 0035: Syntax Highlighting as a Cached View of Parsed Text](0035-syntax-highlighting.md) — extends ADR 0034's document/view split to tree-sitter highlighting
- [ADR 0036: The Core Reports Colors From an Embedder-Supplied Scheme](0036-embedder-color-scheme-in-core.md) — keeps `OSC 4/10/11/12` answers in `festerm-core` while `festerm-ui-egui` owns the values; see issue #222
- [ADR 0037: Reverse Wraparound Climbs Only the Line It Is On](0037-reverse-wraparound-bounds.md) — implements `DECSET 45` with xterm's post-383 bounds and retires stale soft-wrap marks on explicit line breaks
- [ADR 0038: Authoritative Terminal Snapshots for `festerm-sessiond` Reattach Recovery](0038-sessiond-terminal-recovery-snapshots.md) — updates ADR 0025's recovery model to use protocol-v2 terminal snapshots rather than raw replay tails

## Superseded Decisions

- [ADR 0008: Versioned TOML Configuration with Safe Hot Reload](0008-versioned-toml-configuration.md) — superseded by ADR 0015

## Proposed Decisions

- [ADR 0039: Direct2D Terminal Composition on Supported Windows x64 WARP](0039-opt-in-direct2d-terminal-composition.md) — Proposed; owner-approved default selection is bounded to supported Windows x64 WARP, while CP-18 and issue #244 qualification remain open
- [ADR 0025: fesTerm-Owned Local Session Persistence via a Standalone `festerm-sessiond` Executable](0025-native-local-session-persistence-daemon.md)
- [ADR 0027: SSH Port Forwarding for Profiles and Live Sessions](0027-ssh-port-forwarding-for-profiles-and-live-sessions.md) — see issue #38
- [ADR 0028: Text-Mode SFTP Session Tabs via `russh-sftp`](0028-text-mode-sftp-session-tabs-via-russh-sftp.md)
- [ADR 0029: GUI SFTP File Manager Surface](0029-gui-sftp-file-manager.md) — see `docs/sftp-ui-design.md`
- [ADR 0030: Native Markdown Viewer as a First-Class Bounded Application Surface](0030-native-markdown-viewer.md) — amended by ADR 0034
- [ADR 0031: Mobile (iOS/Android) Port Strategy](0031-mobile-ios-android-port-strategy.md) — exploratory design only; see `docs/mobile-port-plan.md`, `docs/mobile-layout-design.md`, and `docs/mobile-signing-and-release.md`
- [ADR 0034: Shared Mutable Documents for Native Text Editing](0034-shared-mutable-text-documents.md) — amends ADR 0030's read-only snapshot boundary; see issue #166

## Status Values

- **Proposed:** under discussion and not yet binding.
- **Accepted:** the current project decision.
- **Superseded:** replaced by a later ADR.
- **Rejected:** considered and deliberately not adopted.

New decisions should use the next sequential number and should link to any ADR they supersede.
Start from [`TEMPLATE.md`](TEMPLATE.md). Every new or materially changed ADR
must include `## Validation impact` and update the machine-readable trace
registry when its requirement, scenario, or evidence relationships change.
