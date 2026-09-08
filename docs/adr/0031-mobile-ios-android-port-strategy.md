# ADR 0031: Mobile (iOS/Android) Port Strategy

- **Status:** Proposed
- **Date:** 2026-09-08
- **Supersedes:** None

## Context

ROADMAP.md's Future Capability Tracks intentionally exclude mobile: fesTerm's
critical path through Milestone 10 is a desktop `egui`/`eframe` application
(ADR 0007) distributed via `cargo-packager` and GitHub Releases (ADR 0021).
No mobile target exists in the workspace today.

Product interest in an iOS/Android client requires deciding, before any code
is written, how such a port would fit fesTerm's existing crate boundaries,
secrets model, and distribution trust model — because a naive port would
otherwise force several changes gated by the 0.1 Architecture-Stability
Period in `docs/development-governance.md`: crate dependency direction
(`app/festerm` currently owns all windowing/host-integration deps),
session-ownership contracts (ADR 0014's window/workspace/tab model assumes a
desktop multi-window host), and the secrets boundary (ADR 0016/0024's native
keyring backends are desktop-OS-specific).

Research into Termius (a widely used mobile SSH client) surfaced a mobile UX
pattern worth emulating functionally — extra-keys toolbar, sticky modifiers,
gesture-based arrow-key emulation, standard OS text-selection for copy/paste,
pinch-to-zoom, snippets — but also a product anchor (mandatory cross-device
cloud account sync) that directly conflicts with ADR 0003's local-first,
no-mandatory-cloud-identity model and `DESIGN.md`/`PRODUCT_POSITIONING.md`'s
explicit rejection of mandatory account/cloud sync. A fesTerm mobile client
must reuse the already-planned encrypted profile export/import path (tracked
under ROADMAP.md's Future Capability Tracks as "password-encrypted profile
export/import") instead of adopting an account-based sync service.

Distribution is also a materially different trust model from ADR 0021: both
Apple's App Store and Google Play prohibit in-app binary self-updates (they
own the update channel), so the `cargo-packager-updater` mechanism cannot
extend to mobile builds, and store review/signing introduces credential types
(Apple Distribution certificate, Play upload key) that do not exist in
today's desktop signing setup (`docs/signing-and-release.md`).

This ADR is the Phase 0 architectural review the governance process requires
before any mobile-targeting crate or dependency-direction change is proposed.
It does not implement mobile support; it fixes the boundaries and constraints
a future implementation must honor, and defers concrete crate/dependency
changes to their own follow-up ADRs at implementation time.

## Decision

fesTerm accepts mobile (iOS/Android) as a **future capability track**, scoped
as follows, and detailed in three companion design documents:

- `docs/mobile-port-plan.md` — phased delivery plan (rendering spike, input
  surface, session lifecycle, platform-integration crates, packaging).
- `docs/mobile-layout-design.md` — responsive layout tiers (Wide/Compact/
  Minimal) for the terminal and SFTP views on phone and tablet, extending
  `docs/sftp-ui-design.md`'s existing narrow-width precedent rather than
  branching on device type.
- `docs/mobile-signing-and-release.md` — App Store/Play Store signing,
  credentialing, and CI/CD strategy, explicitly scoped separately from
  ADR 0021 (which remains desktop-only).

Architectural boundaries fixed by this decision:

- **Reuse, don't fork, the terminal engine.** `crates/festerm-core` (protocol
  and input encoding) and `crates/festerm-ui-egui` (rendering/selection,
  already free of `eframe`/`winit` dependencies) are the mobile client's
  terminal engine and renderer. A mobile port is a new host
  (`app/festerm-mobile` or platform-native shells embedding the same crates),
  not a rewrite in a different UI toolkit or language.
- **No local shell/PTY on mobile.** `festerm-pty` (`portable-pty`-based local
  shell execution) is explicitly out of scope for iOS and Android; mobile
  sandboxing forbids arbitrary process execution. Mobile builds expose SSH
  and SFTP session types only.
- **No mandatory cloud sync or account identity.** Cross-device continuity
  uses the encrypted export/import mechanism already planned per ADR 0003,
  never an account-based sync service, on mobile or desktop.
- **Secrets stay behind `festerm-secret-store`.** An iOS Keychain backend and
  an Android Keystore backend extend the existing cfg-gated per-OS backend
  pattern established by ADR 0016/0024. They are additive backends behind the
  existing trait boundary, not a new secrets pathway.
- **No in-app self-update on mobile.** The `cargo-packager-updater` mechanism
  and its "check/download/install" state machine (ADR 0021) are
  feature-gated out of mobile builds entirely. Store-hosted distribution is
  the only update channel.
- **Layout decisions key off measured width/height breakpoints, not device
  type or OS**, per `docs/mobile-layout-design.md`, consistent with the
  existing narrow-width precedent in `docs/sftp-ui-design.md`.
- **Concrete implementation changes get their own ADRs.** This ADR authorizes
  planning and design work only. The first crate that introduces a real
  dependency-direction change (e.g., a new `festerm-ios-keychain` crate, a
  mobile rendering host crate) requires its own ADR referencing this one,
  per the governance stability-period rule, before merge.

## Alternatives considered

- **Adopt Termius-style mandatory cloud account sync as the mobile
  continuity mechanism.** Rejected: conflicts with ADR 0003 and
  `PRODUCT_POSITIONING.md`'s explicit local-first, no-mandatory-account
  stance. The planned encrypted export/import mechanism satisfies the same
  cross-device need without introducing an account/service dependency.
- **Rewrite the client in a cross-platform mobile framework (Flutter, React
  Native, Kotlin Multiplatform).** Rejected: this would duplicate
  `festerm-core`'s terminal protocol/state engine in a second language and
  runtime, directly contradicting ADR 0004's componentized/testable-core
  goal and doubling the maintenance surface for terminal-compatibility work
  (ADR 0001, ADR 0012, ADR 0026). Reusing the existing Rust core is strictly
  cheaper and keeps one source of terminal-emulation truth.
- **Ship an embedded webview/Tauri-style shell for mobile only, keeping
  desktop on `egui`.** Rejected: ADR 0007 deliberately chose `egui` for a
  pragmatic, single-renderer boundary; introducing a second UI stack for one
  platform family reintroduces the exact fragmentation ADR 0007 avoided, for
  no established benefit over hosting `egui` natively on mobile.
- **iOS distribution via Enterprise or ad hoc sideloading instead of the App
  Store.** Rejected as the primary channel: Enterprise distribution is
  contractually restricted to internal-only use, and ad hoc/free-account
  sideloading does not reach a general audience. The App Store is the only
  viable general-distribution channel for iOS.
- **Reuse the existing `cargo-packager-updater` trust chain for mobile
  updates.** Rejected: both app stores prohibit in-app binary self-update as
  a matter of review policy; the mechanism cannot be extended, only removed
  from mobile builds via feature gating.

## Consequences

- No code changes result directly from this ADR. It gates future work: any
  proposal to add a mobile-targeting crate, host mobile rendering, or extend
  `festerm-secret-store` with a new backend must reference this ADR and, if
  it changes dependency direction or the secrets boundary, add its own ADR.
- `docs/mobile-port-plan.md`, `docs/mobile-layout-design.md`, and
  `docs/mobile-signing-and-release.md` become durable references for that
  future work; they should be revised in place as implementation reveals
  new constraints, rather than treated as fixed specifications.
- `docs/sftp-ui-design.md` gains a pointer to the responsive-tier design and
  a conceptual reframing of `SftpPaneOrderPreference` as orientation-neutral
  (Local-primary/Remote-primary), so a future implementation does not need
  to invent a mobile-only preference.
- ROADMAP.md's Future Capability Tracks gains an entry pointing to this ADR
  and its companion documents, consistent with how the Markdown viewer
  future track is recorded.
- No milestone timeline is created. Mobile work does not begin until a
  follow-up decision moves it into an active milestone, and the rendering
  spike in `docs/mobile-port-plan.md` Phase 1 is treated as a feasibility
  gate before further phases are pursued.

## Validation impact

- **Invariants introduced or changed:** None yet in code. This ADR records
  invariants a future mobile implementation must honor: no `festerm-pty` on
  mobile targets; no in-app self-update on mobile targets; mobile secrets
  storage only through `festerm-secret-store`'s backend trait; no mandatory
  cloud account/sync mechanism.
- **GUI/action edges affected:** None — this ADR is design/planning only and
  changes no implemented, user-observable behavior in the existing desktop
  application.
- **Automated tests required:** None yet. Implementation-phase follow-up
  ADRs must name concrete tests when they introduce mobile-targeting code.
- **Native/manual evidence required:** None yet — deferred to the Phase 1
  rendering-spike feasibility gate described in `docs/mobile-port-plan.md`.
- **Coverage superseded:** None.
