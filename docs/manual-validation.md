# Manual and Usability Validation Registry

**Status:** Active registry

**GitHub tracker:** [#43 — Manual and usability verification inventory](https://github.com/fes/fesTerm/issues/43)

This document is the canonical inventory of behavior that still requires a
person, a native desktop, or a usability judgment. GitHub issues track
execution, ownership, and discovered defects; they do not replace the stable
scenario and evidence contract recorded here.

Prefer deterministic automation whenever a stable oracle exists. Keep a check
manual only when it depends on native platform integration, assistive
technology, visual judgment, reference-application screen semantics, or a real
usability question. A reproducible failure should become the smallest suitable
fixture, interaction test, snapshot, native smoke, or focused defect issue.

## Status vocabulary

- **Automated:** a repository-owned test provides the acceptance evidence.
- **Manual pending:** the capability exists but qualifying human/native evidence
  has not been recorded for every required platform.
- **Usability pending:** behavior is implemented provisionally and needs an
  observed human-use judgment rather than a binary correctness check.
- **Blocked:** the required environment or prerequisite capability is not
  currently available; the blocker must be named.
- **Deferred:** the product capability does not exist yet. Deferred is not a
  failed or skipped test.
- **Pass / Fail / Not run:** result for one specific platform and commit. Not run
  always includes a reason and never counts as pass.

## Evidence record

Every execution records:

- commit SHA and fesTerm version;
- operating system/version and architecture;
- desktop environment, compositor/window manager, and display protocol where
  relevant;
- display scale/DPI, monitor arrangement, and input method where relevant;
- exact scenario identifier and pass/fail/not-run result;
- a concise content-free observation and linked defect for every failure; and
- sanitized screenshots or video only when the scenario needs visual evidence.

Never retain terminal content, clipboard values, credentials, usernames,
hostnames, filesystem paths, SSH destinations, serial identifiers, or other
session secrets in validation artifacts.

## Active registry

| Area | Required environments | Manual or usability evidence | Status / tracking |
| --- | --- | --- | --- |
| M6 native-window foundation | Windows, macOS, Linux desktop environments named by the M6 gate | Real focus, renderer/window startup, resize continuity, PTY input/output, compositor behavior | Manual pending; umbrella implementation [#8](https://github.com/fes/fesTerm/issues/8), Linux focus [#21](https://github.com/fes/fesTerm/issues/21), environment findings #32–#36 |
| Reference terminal applications | Representative supported desktops; tool-specific platforms where applicable | Shell editing, `less`, Vim/Neovim, Emacs, tmux, htop, GitHub Copilot CLI, `vttest`, selection and input semantics | Manual pending; [#26](https://github.com/fes/fesTerm/issues/26), checklist in `m6-compatibility-checklist.md`; `tack` remains [#27](https://github.com/fes/fesTerm/issues/27) |
| Custom title bar and window chrome | Windows, macOS, Linux/X11 and Linux/Wayland; multiple scale factors | Drag/double-click, minimize/maximize/restore/close, snap/system menu behavior, multi-monitor DPI, narrow layout, chip drag interaction, accessibility | Manual pending; [#29](https://github.com/fes/fesTerm/issues/29) |
| Native macOS application menu | Logged-in macOS desktop | Menu installation and conventions; shortcuts; dynamic Close and Inspector state; focus-aware Copy/Paste without PTY leakage; native Services/Hide/Quit/window actions | Manual pending; [#44](https://github.com/fes/fesTerm/issues/44) |
| Launcher and integrated chrome | Windows, macOS, Linux; narrow and scaled viewports | Visual comparison to approved mockups; keyboard-only launch flow; chip overflow/reorder/rename; stable geometry and compactness | Usability pending; retain in umbrella until a platform-specific defect or independent work package appears |
| Session Inspector | Windows, macOS, Linux; narrow and scaled viewports; local and SSH sessions | Overlay geometry without terminal resize; focus restoration; first-click dismissal; selectable facts; failure/diagnostic comprehension; active-session switching | Automated structurally; native visual/usability pass pending in umbrella |
| Terminal and session-chip context menus | Windows, macOS, Linux; ordinary and TUI mouse-reporting modes | Native secondary-click/Shift-override conventions, popup placement at edges and DPI scales, clipboard delivery/focus, menu keyboard traversal, inactive-chip targeting, and destructive-action clarity | Automated structurally; native visual/accessibility/usability pass pending in [#43](https://github.com/fes/fesTerm/issues/43) |
| Scrollback, reflow, and read-only history | Windows, macOS, Linux; wheel and trackpad; narrow/wide resize; live, alternate-screen, and disconnected sessions | Smooth navigation near the 64 MiB default; follow suspension/resume and `Jump to latest`; stable viewed content through output, eviction, chip switches, and resize; scrollbar discoverability; selection/Copy across wraps; no alternate-screen leakage; disconnected history remains scrollable/copyable without input | Core bounds and initial wheel/keyboard follow-anchor routing are automated; native feel/performance and the pending eviction, selection, scrollbar, reflow, and disconnected-history slices remain in [#43](https://github.com/fes/fesTerm/issues/43) |
| Icons | Windows, macOS, Linux at supported scale factors | 16/20 px legibility, alignment, state distinction, fallback behavior, and accessibility after runtime integration | Deferred until runtime integration in [#30](https://github.com/fes/fesTerm/issues/30) |
| Application typography | Windows, macOS, Linux; supported DPI and representative fallback scripts | Inter legibility and metrics in compact chrome, non-Latin fallback, hierarchy, truncation, and stable layout | Deferred until bundled font integration; shaping/fallback contract remains [#22](https://github.com/fes/fesTerm/issues/22) |
| Accessibility and discoverability | Windows UIA, macOS Accessibility, Linux AT-SPI where supported | Keyboard-only traversal, focus order/restoration, screen-reader names/states, icon-only control comprehension, tooltip coverage | Manual pending; tooltip wording [#31](https://github.com/fes/fesTerm/issues/31); platform findings get focused issues |
| Paste confirmation and destructive actions | All supported platforms; bracketed and ordinary terminal modes | Threshold comprehension, exact preview at narrow widths, safe Enter behavior, cancellation after clipboard/session-generation changes, active-session close confirmation | Deferred until implementation; keep as a usability hypothesis in `gui-design.md` |
| SSH interaction workflows | All supported platforms with repository-owned fixture plus controlled native UI | Host-key comprehension, authentication focus and secrets, explicit saved-password/store-unavailable feedback, no automatic workspace connection, disconnect/history behavior, conditional reconnect, error recovery | Stored-password transport and headless UI paths automated; native secure-store/platform usability pass pending in umbrella [#42](https://github.com/fes/fesTerm/issues/42) |
| Serial interaction workflows | Windows, macOS, Linux with representative adapters and permission states | Discovery, unavailable/busy devices, configuration clarity, exclusive ownership, disconnect/history/reconnect, permissions | Deferred until serial implementation |
| Fixed native window title | Multiple simultaneous fesTerm windows; OS task switcher/overview | Whether fixed `fesTerm` identity remains understandable without dynamic session content | Usability pending in umbrella; create a focused issue only if evidence shows a concrete problem |

## Intake rule for new work

Every implemented GUI or platform slice must state which of these applies:

1. automated acceptance added now;
2. manual/native verification added to an existing registry row;
3. a usability hypothesis added to an existing registry row; or
4. deferred verification tied to a named prerequisite capability.

Create a focused GitHub issue only when the area has its own environment,
setup, owner, acceptance boundary, or substantive defect. Otherwise keep it as
one row in this registry and one checkbox in the umbrella issue. When a manual
check becomes automated, update this registry and the owning test plan in the
same change.

## Relationship to other validation documents

- `ui-test-plan.md` defines the automated and platform test architecture.
- `m6-validation-gate.md` defines the current milestone acceptance gate.
- `m6-compatibility-checklist.md` defines reference-application scenarios.
- `vm-evidence-framework.md` defines controlled cross-platform execution and
  evidence handling.
- `gui-design.md` defines intended behavior and identifies usability
  hypotheses; this registry says where their validation is tracked.
