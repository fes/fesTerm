# fesTerm Agent Guide

## Start Here

Read `README.md`, then use the document that matches the task:

| Task | Primary reference |
| --- | --- |
| Product scope and priorities | `DESIGN.md`, `ROADMAP.md` |
| Dependency and ownership boundaries | `ARCHITECTURE.md` |
| Application command semantics and UI action routing | `docs/application-command-model.md` |
| Scope control and architecture-stability policy | `docs/development-governance.md` |
| Required behavior | `REQUIREMENTS.md`, `COMPATIBILITY.md` |
| Standards and security decisions | `docs/standards-and-implementation-notes.md`, `docs/adr/` |
| Golden fixtures | `tests/fixtures/README.md` |
| GUI workflow, chrome, session chips, and visual hierarchy | `docs/gui-design.md` |
| First-party icons, semantic names, accessibility, and asset pipeline | `docs/icon-system.md` |
| UI, rendering, and platform test strategy | `docs/ui-test-plan.md` |
| GUI exploration state/action graph and recovery paths | `docs/gui-action-graph.md` |
| Requirement/ADR/scenario/test traceability | `validation/traceability.json`, `validation/README.md` |
| Manual, native-platform, and usability validation inventory | `docs/manual-validation.md` |
| Bootstrap and manual checks | `docs/development-handoff.md` |

## Current Structure

- `crates/festerm-core`: GUI-, PTY-, and SSH-independent terminal state,
  parser, input encoding, and bounded queues.
- `crates/festerm-test-support`: repository-owned golden-fixture parser and
  assertions.
- `crates/festerm-ui-egui`: terminal presentation, input routing, selection,
  clipboard routing, and view diagnostics.
- `crates/festerm-session`: runtime-independent session lifecycle and bounded
  transport contract.
- `crates/festerm-pty`: local shell backend using `portable-pty`.
- `crates/festerm-pty-test-child`: deterministic repository-owned child used
  by controlled PTY and smoke tests.
- `crates/festerm-ssh`: in-process `russh` transport, trust decisions,
  transient authentication, OpenSSH metadata import, and bounded reconnect.
- `crates/festerm-config`: strict versioned TOML profiles and metadata-only
  workspace persistence with explicit transactional reload.
- `crates/festerm-macos-window`: cfg-gated macOS custom-window integration.
- `crates/festerm-windows-job`: cfg-gated safe wrapper around the Windows Job
  Object required for whole-ConPTY-tree shutdown.
- `crates/festerm-windows-runtime`: cfg-gated trusted selection and loading of
  an optional install-owned ConPTY sidecar.
- `app/festerm`: composition root; the only owner that mutates a terminal from
  session output.

## Architectural Invariants

1. Terminal protocol semantics belong in `festerm-core`, never GUI widgets.
2. Session backends exchange bytes and lifecycle events; they never mutate a
   `Terminal`.
3. The app has one logical writer for each terminal.
4. Session and core queues remain bounded, ordered, and observable. PTY event
   availability wakes the UI through the session notifier; do not add polling.
5. Secrets never enter ordinary configuration, workspaces, fixtures, logs, or
   synchronized metadata.
6. A compatibility fix requires a deterministic regression fixture where
   practical.
7. Product-level actions from launcher, shortcuts, chrome, menus, command
   palette, and session inspector converge on the application command model;
   do not implement independent widget-specific copies of the same operation.

## Validation

Run from the repository root:

```sh
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo check --workspace
git diff --check
```

For the pinned Windows ConPTY retention smoke, use
`pwsh -NoProfile -File scripts\stage-conpty.ps1 -RunSmoke`. The script owns
package download, hash verification, workspace build, and runtime staging; do
not duplicate that logic in a workflow or copy package files by hand.

Use the smallest relevant test while iterating, then run the workspace
commands before release. The CI workflow runs the quality checks on Windows,
macOS, and Linux.

Prefer repository-owned automation over repeated manual validation
instructions. Optional checks must remain explicitly opt-in and aggregate
under `scripts/run-optional-validation.sh` or
`scripts/run-optional-validation.ps1`, so a user can run the complete
optional suite in one command. Manual evidence remains required only where
automation cannot prove the behavior.

For every new GUI or platform slice, classify its remaining evidence as
automated, manual/native, usability, or deferred behind a named prerequisite.
Update `docs/manual-validation.md` in the same change when a slice adds,
removes, or automates one of those checks. Open a focused issue only when the
check has its own environment, setup, owner, acceptance boundary, or discovered
defect; otherwise keep it in the umbrella validation tracker.

Before changing behavior, resolve the affected stable IDs in
`docs/gui-action-graph.md` and `validation/traceability.json`. Update design,
graph edges, automated tests, manual scenarios, and ADR Validation impact in
the same change whenever their relationship changes. Run
`python scripts/check_validation_traceability.py` before handoff. Commits use a
`Validation-Impact:` trailer naming affected `GUI:<edge>`/`ADR-<number>` IDs,
or `none - <reason>` for a behavior-preserving change.

## Scope Classification

Classify proposed work before expanding an implementation:

- **Current milestone requirement:** required for an active completion
  criterion, regression fix, or validation blocker. Implement now.
- **Architectural enabler:** the smallest seam required to prevent a near-term
  accepted capability from hitting a known dead end. Implement narrowly; do
  not pull the future capability forward with it.
- **Deferred product capability:** useful future behavior that is not needed
  for current acceptance. Document or issue it and return to assigned scope.

A good adjacent idea is not automatically current scope. See
`docs/development-governance.md` for the full decision rules.

## Architecture Stability

After M6 validation and acceptance of the first GUI vertical slice, the project
enters the documented 0.1 architecture-stability period. Material changes to
crate responsibilities, dependency direction, terminal/session ownership,
renderer/core boundaries, configuration/secrets boundaries, bounded critical
queues, or the application-command ownership model require explicit
architectural review and an ADR before merge.

Routine internal refactoring and additive changes that preserve those
contracts do not require an ADR. See `docs/development-governance.md` for the
complete policy.

## Working Conventions

- Keep the active milestone status accurate in `ROADMAP.md` and the concise
  status in `README.md`.
- Review open GitHub issues when starting and releasing a milestone. Fix
  current-criterion regressions now; assign compatible reports to their owning
  milestone; do not pull future product requests into the current scope.
- Treat `docs/gui-design.md` and its canonical wireframe as the source of truth
  for application chrome and session-tab work. Session tabs are independent,
  neutral lozenge/chip objects embedded directly in the upper window chrome,
  alongside New Tab and compact global controls. Do not place them on a
  detached shelf below a separate title bar. Preserve stable identity and use
  compact status indicators instead of full-chip state colors.
- Use the screen-specific PNGs in `docs/images/gui-mockups/` for native visual
  comparison. Each image is governed by its adjacent `docs/gui-design.md`
  section; prose and negotiated deviations take precedence over details a
  static mockup cannot represent.
- Treat `assets/icons/source` as the canonical first-party icon source and
  `docs/icon-system.md` as its naming/accessibility contract. Use semantic Icon
  names in Rust, keep color in UI state, and never add branded OS glyphs as a
  required remote-session identity.
- Treat each `assets/fonts/*/manifest.json` as authoritative for its bundled
  terminal font. Run `python scripts/manage_bundled_font.py` after font-related
  changes. Use `--update-latest` only for an explicit reviewed JetBrains Mono
  upgrade; rebuild or replace the other families according to their provenance
  notes. Never copy an installed system font or auto-merge upstream metrics.
- Route application-level GUI actions through
  `docs/application-command-model.md`; invocation surfaces translate intent,
  while application policy remains centralized and testable.
- Do not duplicate long design documents in new summaries; link to the source
  of truth instead.
- Treat terminal output and protocol input as untrusted. Preserve parser,
  allocation, and queue bounds.
- Do not add scripting, plugins, persistent history, cloud synchronization, or
  other deferred capabilities ahead of their documented milestones. Tabs and
  native SSH are implemented. Serial's product/UI contract is approved, but
  its backend remains a focused future capability track.
