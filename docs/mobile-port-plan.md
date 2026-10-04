# fesTerm Mobile Port Plan (iOS/Android)

**Status:** iOS Phase 1 started at the owner's request on 2026-09-26;
implementation for review under ADR 0042, native feasibility pending.
The owner also requested the persistent-keyboard/input slice for both iPhone
and iPad as part of this initial host. Other Phase 2 work, Android and
Phases 3–5 remain deferred. ADR 0031 supplies the planning
boundaries; ADR 0042 proposes the concrete additive host/dependency decision.

This document is the companion delivery plan for ADR 0031. It sequences the
work required to bring fesTerm to iOS and Android while reusing the existing
Rust workspace rather than forking the terminal engine.

## Why not a rewrite

`crates/festerm-core` already isolates terminal-protocol state and
keyboard/mouse/paste/focus encoding from any GUI toolkit. `crates/festerm-ui-egui`
depends only on `egui` — not `eframe` or `winit` — so its rendering and
selection logic is already windowing-agnostic. The desktop-only surface is
narrow and concentrated in `app/festerm` (the composition root): `eframe`
(window/event loop), `rfd` (native file dialogs), `directories` (desktop path
conventions), and `festerm-pty` (local shell via `portable-pty`, which cannot
port to mobile — sandboxing forbids arbitrary process execution on iOS and
largely on Android). A mobile client is a new host around the existing core
and renderer crates, not a second implementation of terminal emulation.

## Termius UX research summary

The owner selected persistent keyboard layout and terminal long-press/drag
with a temporary arrow helper on iPhone and iPad. The current source review,
implemented gesture behavior, and prioritized follow-ups are maintained in
[the mobile layout design](mobile-layout-design.md#touch-navigation-and-termius-conventions).
Phase 1 includes that narrow input slice; other conventions remain Phase 2
or their named later milestone.

## Phase 0 — Governance (this document + ADR 0031)

Establish the architectural boundaries and get them reviewed before any
mobile-targeting code exists. Complete once ADR 0031 and this plan are
accepted as the reference design. No implementation in this phase.

## Phase 1 — Rendering feasibility spike

The first iOS host is `app/festerm-mobile`; build/run instructions and current
limits are in its README. It uses the pinned eframe/winit public host API,
the shared renderer/core, and no session backend. Affected PRs keep mobile
unit/dependency checks, device compilation and Simulator app builds. The iOS
workflow runs isolated launch/relaunch and screenshot smoke nightly/manual
only while this is an experiment, not a desktop merge gate. Failed smoke
runs retain their status and artifacts (#303), with unchanged bounds and
ownership checks. Native acceptance remains `MOB-01`–`04` in
`docs/manual-validation.md`; accepting mobile product support requires an
explicit review of the runtime merge gate.

Goal: prove `egui` can host on iOS and Android with acceptable touch input
and lifecycle behavior before committing to further phases.

- Spike `egui`/`eframe`'s Android backend (via `winit`'s Android support) and
  iOS backend (via `winit`'s iOS support or a native `UIKit`/`UIView` host
  embedding `egui`'s wgpu/glow renderer directly).
- Validate touch-event delivery into `festerm-ui-egui`'s existing selection
  and scrolling logic without modification to that crate's public surface.
- Validate app lifecycle integration: backgrounding, foregrounding, memory
  pressure, and process termination/restoration on both platforms — this is
  the least mature part of `egui`/`winit`'s mobile support as of this
  writing and is the primary feasibility risk for this phase.
- **Gate:** if lifecycle or input integration requires forking `egui`/
  `winit` or produces an unacceptably unstable app on either platform,
  reassess before proceeding to Phase 2. This phase's output is a go/no-go
  recommendation, not committed product scope.

## Phase 2 — Input surface

Goal: build the mobile-specific input affordances on top of
`festerm-core`'s existing GUI-independent keyboard/mouse/paste encoding —
no changes to that crate's encoding logic should be required.

- Extra-keys toolbar (Esc/Tab/Ctrl/Alt; optional extended navigation/F-keys), customizable and
  scrollable, feeding the same key-encoding path desktop keyboard input uses.
- Sticky/latching modifier keys.
- Gesture-based arrow-key emulation (terminal long-press/drag with three
  repeat speeds and a temporary helper; spacebar alternative subject to UIKit feasibility), implemented as a mobile-only input adapter that
  translates gestures into the existing key-encoding calls.
- Selection/copy gestures: standard OS text-selection (long-press, drag
  handles) mapped onto `festerm-ui-egui`'s existing selection model; see the
  desktop selection/copy behavior already implemented (deselect-after-copy,
  middle/right-click-to-copy) as the baseline behavioral contract to match,
  not diverge from, on mobile.
- Pinch-to-zoom for font size, reusing the existing font-size configuration
  path rather than introducing a mobile-only setting.
- Snippets/command-history panel, if pursued, should reuse existing
  profile/command-model concepts rather than introducing a parallel store.

## Phase 3 — Mobile session lifecycle

Goal: define an honest mobile session story given OS-enforced backgrounding
and process suspension.

- Reuse `festerm-ssh`'s existing bounded-reconnect logic (ADR 0018) as the
  mechanism for resuming connectivity after the OS suspends and later
  resumes the app process.
- Do not promise an always-live SSH session across backgrounding; frame the
  mobile session model as **durable session resume** — attaching to a
  remote `tmux`/`screen` session (already supported per ADR 0018's
  persistent-session-recovery design) is the realistic and honest mobile
  session story, not a continuously connected socket.
- Define the on-resume UX: reconnect automatically when the app returns to
  the foreground, surface a clear state when reconnection fails, and never
  silently drop scrollback the user expects to still be there.

## Phase 4 — Platform integration crates

Goal: extend existing per-platform seams rather than inline `cfg` blocks in
the application, following the precedent set by `festerm-windows-job`,
`festerm-windows-power`, `festerm-macos-window`, and `festerm-linux-power`.

- `festerm-ios-keychain` / `festerm-android-keystore`: new backends behind
  `festerm-secret-store`'s existing cfg-gated backend trait (ADR 0016,
  extended by ADR 0024), replacing the desktop `directories`/`rfd`
  dependencies with platform-native equivalents.
  **Risk to validate early:** Android Keystore backend maturity in
  `keyring-core` (the crate the desktop backends are built on) is unverified
  for this use case as of this writing and should be spiked before committing
  to the dependency.
- `festerm-ios-lifecycle` / `festerm-android-lifecycle` (naming
  illustrative): own OS lifecycle callbacks (background/foreground, memory
  warnings, termination) and hand off to Phase 3's session-resume logic,
  keeping lifecycle plumbing out of shared crates.
- Each new crate requires its own ADR at implementation time per ADR 0031's
  final bullet, since each is a real dependency-direction change under the
  0.1 Architecture-Stability Period.

## Phase 5 — Packaging and distribution

Goal: get a signed build into TestFlight/Play Internal Testing, then the
public stores. Full detail in `docs/mobile-signing-and-release.md`; this
phase is a new CI pipeline, explicitly separate from `release.yml`/ADR 0021,
since the trust and distribution model differs (store-mediated, not
self-hosted signed updates).

- Provision Apple Distribution certificate + App Store Connect API key
  (reusing the existing API key infrastructure from desktop notarization,
  net-new certificate).
- Provision Android Play App Signing + a CI-held upload keystore, authenticated
  via Workload Identity Federation rather than a long-lived service-account
  secret.
- Feature-gate out `cargo-packager-updater` entirely for mobile targets;
  replace the in-app "check for updates" action with a store deep link.
- TestFlight external testing / Play Internal-then-Closed testing tracks as
  the staged-rollout equivalent of the desktop `workflow_dispatch` unsigned
  dry-run before any production release.

## Explicitly out of scope for mobile

- Local shell/PTY sessions (`festerm-pty`) — sandboxing prohibits arbitrary
  process execution.
- Mandatory cloud account/profile sync — conflicts with ADR 0003 and
  `PRODUCT_POSITIONING.md`; use encrypted export/import instead.
- In-app binary self-update — prohibited by both stores' review policies.
- Plugin/scripting systems — already out of scope for desktop per
  `DESIGN.md` and the 0.1 Architecture-Stability Period; mobile does not
  reopen this.
