# fesTerm Product Strategy and Execution Guardrails

**Status:** Active project guidance

This document records product-level conclusions that cut across individual milestones. It exists to preserve direction while allowing implementation details and milestone sequencing to evolve.

## Product Center: Session Management

fesTerm should be designed as a **session-management platform with an excellent terminal engine**, rather than only as a terminal emulator that happens to support SSH.

The terminal engine remains foundational: correctness, compatibility, latency, Unicode behavior, rendering, and PTY integration are non-negotiable. The product differentiation, however, comes from making long-lived local and remote sessions easy to create, identify, organize, reconnect, restore, diagnose, and eventually synchronize across environments.

A useful product flow is:

```text
Launcher
  -> Workspace
    -> Session Manager
      -> Terminal
```

This framing should influence product decisions without causing premature implementation of every future session feature.

## Stable Identity Is a Product Primitive

The application owns stable identity; terminal contents and transports are dynamic.

A session identity should survive:

- terminal title changes;
- foreground application changes;
- alternate-screen transitions;
- SSH transport reconnects; and
- workspace restoration when the session can be recreated.

Profiles describe how sessions are created. Sessions are concrete running or reconnecting instances. Terminals contain protocol state. Workspaces organize restorable application state. Windows present workspaces and application surfaces.

These concepts should not be collapsed simply because the first implementation can represent them with one object.

See `docs/gui-design.md` for tab-label precedence and user-facing identity rules.

## Current Execution Priorities

### 1. Finish the M6 validation gate

Validation remains the highest priority. The project has accumulated capability faster than native rendered-window evidence. The work in `docs/m6-validation-gate.md` should complete before the project treats the current terminal/render/session foundation as fully accepted.

The objective is confidence that core state, renderer, session integration, and native-window behavior agree under real workloads and resize/focus/input conditions.

### 2. Stabilize the GUI/session-management vertical slice

The narrow GUI slice is implemented: launcher and local-session tabs, chips,
rename/reorder, command palette, settings, inspector, connection overlays,
custom title bar, and configurable status bar. The next work is usability and
platform stabilization, not another chrome feature phase: settle shortcuts,
chip layout, session-switcher behavior, semantic theming, and custom-title-bar
validation through actual use and focused evidence.

### 3. Advance profiles, persistence, and interaction consistency

Native SSH is implemented through the same stable session identity, application
command, diagnostics, and workspace concepts as local sessions. The next
product track is to validate M6 and advance M9 scrollback/reflow while bringing
implemented GUI interactions into conformance with the approved detailed
contracts. M8's narrow profile/workspace and native SSH-password persistence
scope is implemented; profile editing/import UI, persistent trust, and
additional credential types remain future capabilities. Serial's product and
UI contract is defined now, but its backend remains a later focused track.

## Preemptive Seams Worth Establishing

The project should avoid speculative framework construction, but several cross-cutting seams are cheap to define now and expensive to retrofit after GUI expansion.

### Unified application commands

Menus, shortcuts, launcher actions, command palette entries, tab controls, and future automation should express user intent through the same typed application-command model.

This is not a plugin API. It is an internal mechanism to ensure that `Reconnect`, `Open Settings`, or `New SSH Session` has one implementation regardless of entry point.

Implemented through the application command model in
[`application-command-model.md`](application-command-model.md).

### Window, workspace, session, and terminal ownership

Lifetime and ownership rules are defined in
[ADR 0014](adr/0014-window-workspace-tab-session-ownership.md). It fixes the
Application -> Window -> Workspace view -> Tab -> Session -> transport-attempt
model, preserves tab/session identity across reconnect, and keeps restoration
metadata separate from live process, channel, and terminal state. Issue #20
tracks the later persistence and restoration tests required to implement that
model.

### Semantic GUI theming

Application chrome should use semantic roles rather than scattered literal colors. Terminal ANSI/xterm palettes remain a separate concern.

The first implementation does not need a full theme editor, but launcher, tabs, sidebar, settings, focus, and connection states should be expressed through reusable semantic roles.

Tracked by GitHub issue #18.

### Performance measurement before optimization

Interactive performance is a product requirement. Establish repeatable measurements for parser throughput, scrolling, resize, queue pressure, dirty regions, layout/cache behavior, frame work, and input-to-paint-submission latency before optimizing heavily.

Measurements should report trends before arbitrary blocking budgets are adopted.

Tracked by GitHub issue #19.

## Scope-Control Rule

Good ideas discovered during design do not automatically become current-milestone requirements.

Classify each new idea as one of:

1. **Required now** — necessary to satisfy the current milestone's documented behavior or validation criteria.
2. **Preserve the seam** — not implemented now, but current design must avoid precluding it or creating obviously expensive migration work.
3. **Future enhancement** — record it and defer implementation.

Examples such as scripting, plugins, built-in multiplexing, detachable sessions, advanced synchronization, rich graphics, and highly customizable chrome should normally remain in categories 2 or 3 until a concrete use case changes their priority.

This rule is intended to prevent useful product discovery from silently expanding M6 or any later milestone indefinitely.

## Validation Is Part of Feature Completion

Use the milestone vocabulary defined by the M6 validation gate:

- **Implemented:** capability exists and focused automated tests pass.
- **Validation pending:** required integration, rendered-frame, native-platform, or reference-application evidence remains.
- **Accepted:** required completion evidence has passed, or an explicit deferral with replacement evidence is recorded.

Implementation velocity should not be confused with milestone acceptance.

## Architecture Freeze After the First Validated GUI Slice

After the M6 validation gate and the first GUI/session-management vertical slice are accepted, establish a **0.1 architecture freeze**.

The freeze does not prohibit refactoring or better designs. It means changes to foundational contracts require explicit reasoning and review, including an ADR where appropriate.

The protected areas should include at least:

- crate responsibility boundaries;
- terminal-state ownership;
- session/backend ownership;
- application command dispatch;
- window/workspace/session identity and lifetime rules;
- terminal-to-renderer contract; and
- secret/configuration boundaries.

Feature development may continue rapidly above those seams.

## What Not to Build Preemptively

Do not turn the recommendations above into a general-purpose framework.

In particular, do not build yet:

- a public event-bus framework when typed application commands and explicit state transitions suffice;
- a plugin ABI;
- a scripting runtime;
- a full theme editor;
- detachable session infrastructure;
- a tmux replacement;
- a distributed workspace service; or
- speculative renderer abstractions unsupported by profiling.

Prefer small explicit Rust types and ownership boundaries. Generalize only after multiple real consumers demonstrate the need.

## Project Health Signals

The project should continue to optimize for these signals:

- protocol behavior remains independent of GUI widgets;
- session management becomes more capable without weakening terminal correctness;
- new UI entry points reuse existing application commands;
- stable identities survive transient transport and terminal state;
- validation evidence grows with implementation capability;
- performance regressions can be measured rather than guessed;
- GUI visual language remains semantic and testable; and
- roadmap additions are intentional rather than cumulative scope drift.

If those conditions continue to hold, fesTerm can expand substantially without losing the architectural clarity established during the foundation milestones.
