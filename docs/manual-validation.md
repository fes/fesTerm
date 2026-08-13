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
| Scrollback, reflow, and read-only history | Windows, macOS, Linux; wheel and trackpad; narrow/wide resize; live, alternate-screen, and disconnected sessions | Smooth navigation near the 64 MiB default; follow suspension/resume and `Jump to latest`; stable viewed content through output, eviction, chip switches, and resize; scrollbar discoverability; selection/Copy across wraps; no alternate-screen leakage; disconnected history remains scrollable/copyable without input | Core bounds, initial wheel/keyboard follow-anchor routing, conditional Jump control, scrollbar geometry, track paging, and TUI input isolation are automated; native wheel/trackpad and drag feel, high-contrast/accessibility sizing, near-limit performance, and the pending eviction, selection, reflow, and disconnected-history slices remain in [#43](https://github.com/fes/fesTerm/issues/43) |
| Icons | Windows, macOS, Linux at supported scale factors | 16/20 px legibility, alignment, state distinction, fallback behavior, and accessibility after runtime integration | Deferred until runtime integration in [#30](https://github.com/fes/fesTerm/issues/30) |
| Application typography | Windows, macOS, Linux; supported DPI and representative fallback scripts | Inter legibility and metrics in compact chrome, non-Latin fallback, hierarchy, truncation, and stable layout | Deferred until bundled font integration; shaping/fallback contract remains [#22](https://github.com/fes/fesTerm/issues/22) |
| Accessibility and discoverability | Windows UIA, macOS Accessibility, Linux AT-SPI where supported | Keyboard-only traversal, focus order/restoration, screen-reader names/states, icon-only control comprehension, tooltip coverage | Manual pending; tooltip wording [#31](https://github.com/fes/fesTerm/issues/31); platform findings get focused issues |
| Paste confirmation and destructive actions | All supported platforms; bracketed and ordinary terminal modes | Threshold comprehension, exact preview at narrow widths, safe Enter behavior, cancellation after session/generation/state changes, active and inactive-session close confirmation | Implemented with portable policy coverage; native functional and usability evidence pending under scenarios PS-01–PS-10 below and [#43](https://github.com/fes/fesTerm/issues/43) |
| SSH interaction workflows | All supported platforms with repository-owned fixture plus controlled native UI | Host-key comprehension, authentication focus and secrets, explicit saved-password/store-unavailable feedback, no automatic workspace connection, disconnect/history behavior, conditional reconnect, error recovery | Stored-password transport and headless UI paths automated; native secure-store/platform usability pass pending in umbrella [#42](https://github.com/fes/fesTerm/issues/42) |
| Serial interaction workflows | Windows, macOS, Linux with representative adapters and permission states | Discovery, unavailable/busy devices, configuration clarity, exclusive ownership, disconnect/history/reconnect, permissions | Deferred until serial implementation |
| Fixed native window title | Multiple simultaneous fesTerm windows; OS task switcher/overview | Whether fixed `fesTerm` identity remains understandable without dynamic session content | Usability pending in umbrella; create a focused issue only if evidence shows a concrete problem |

## Executable workflow inventory

Use these stable identifiers in issue comments, evidence manifests, and defect
reports. A scenario is not complete until its required platform set has one
result for the candidate commit. “Human” means the pass criterion is a
judgment; the deterministic mechanics should still be automated where
possible.

### Application surfaces and session management

| ID | Workflow and oracle | Evidence class | VM automation candidate |
| --- | --- | --- | --- |
| AS-01 | Launch with no workspace; Launcher is the sole root surface, initial row focus is visible, keyboard navigation and Enter start the selected transport. | Native functional + visual | Yes: accessibility driver plus screenshot |
| AS-02 | Open Launcher and Settings repeatedly; each remains a singleton, replaces its own stale instance, and never changes a live session's terminal geometry. | Native functional | Yes: semantic tree and grid-dimension probe |
| AS-03 | Create enough sessions to overflow chrome; scroll chips, activate first/middle/last, reorder by drag and menu, rename by double-click, and confirm stable active identity. | Functional + usability | Partly: semantic actions and screenshots; drag feel remains human |
| AS-04 | Exercise every global shortcut from terminal, Vim, Emacs, palette, and application controls; application chords act once and ordinary terminal chords reach the TUI. | Native functional | Yes: controlled PTY byte oracle |
| AS-05 | Close Launcher, Settings, exited, failed, and disconnected surfaces immediately; closing the final surface returns Launcher. | Functional | Yes |
| AS-06 | Close a live local/SSH session from chip, context menu, shortcut, palette, native menu, and overlay; each opens the same confirmation bound to the intended session. | Native functional + usability | Yes; consequence wording remains human review |
| AS-07 | With live-close confirmation open, initial Enter does not confirm, Escape cancels without PTY bytes, outside click does not dismiss, and deliberate focus + activation closes exactly the bound session. | Native functional | Yes: UI automation plus controlled PTY/lifecycle oracle |
| AS-08 | Request application/window quit with multiple live sessions; aggregate consequence is accurate and cancellation leaves every session intact. | Deferred until aggregate quit confirmation | Later |

### Terminal interaction, history, and overlays

| ID | Workflow and oracle | Evidence class | VM automation candidate |
| --- | --- | --- | --- |
| TI-01 | Type/edit at a controlled prompt; selection, Copy, Paste, focus transitions, cursor, and resize remain coherent. | Native functional | Yes: existing native smoke expansion |
| TI-02 | Run Vim/Neovim, Emacs, less, tmux, htop, Copilot CLI, and `vttest`; verify keyboard/mouse ownership and no application shortcut collisions. | Compatibility + usability | Partly: scripted launch/input/screenshots; interpretation remains human |
| TI-03 | Right-click with mouse reporting off/on and use Shift override; exactly one owner receives the complete press/release gesture and popup keyboard focus restores correctly. | Native functional | Yes: input byte oracle + accessibility driver |
| TI-04 | Generate bounded history, scroll by wheel/trackpad/keyboard/scrollbar, switch sessions, receive background output, and use Jump to Latest without losing the reading anchor. | Native functional + usability | Mostly; trackpad/scroll feel remains human |
| TI-05 | Resize repeatedly while anchored in wrapped history, copy across wraps, approach eviction, then exit/disconnect; history remains scrollable/copyable and rejects input. | Functional + performance + usability | Partly after reflow/disconnected slices land |
| TI-06 | Open Inspector over local and SSH sessions; terminal grid does not resize, facts/actions change with active session, outside first click is consumed, Escape restores focus, and diagnostics disclose safely. | Native functional + usability | Yes: semantic tree, grid probe, screenshot |
| TI-07 | Trigger failure, host-key verification, authentication-required, disconnect, and reconnect surfaces; focus, wording, trust facts, secrets, and allowed actions remain state-accurate. | Native functional + usability | Partly: repository SSH fixture; secure-store prompts remain platform/manual |

### Paste safety

| ID | Workflow and oracle | Evidence class | VM automation candidate |
| --- | --- | --- | --- |
| PS-01 | In ordinary mode paste one line below the large threshold; it is sent once without a dialog. | Functional | Yes: controlled PTY bytes |
| PS-02 | In ordinary mode paste multiline text; dialog title has exact line count and stable identity, warning explains execution, and preview preserves whitespace. | Functional + usability | Yes, with human wording review |
| PS-03 | In bracketed mode paste ordinary multiline text; it is sent once with protocol markers and no dialog. | Functional | Yes: controlled PTY bytes |
| PS-04 | In bracketed mode exceed either large-paste threshold; confirmation appears and accepted text is one ordered bracketed write. | Functional + usability | Yes |
| PS-05 | Exercise tabs, spaces, CRLF/CR normalization, trailing newline, Unicode, and other control characters; exact counts and escaped preview remain truthful while submitted text is not trimmed or shell-rewritten. | Functional | Yes: fixture matrix |
| PS-06 | At narrow and scaled sizes inspect the bounded preview and exact omitted-line/character counts; all actions remain visible and keyboard reachable. | Native visual + usability | Screenshot automation plus human review |
| PS-07 | Press Enter immediately; Cancel owns initial focus and no paste is submitted. Move focus deliberately to Paste and activate it once. | Native functional | Yes |
| PS-08 | Escape or Cancel dismisses without terminal bytes; clicking outside does not dismiss or interact with terminal. | Native functional | Yes |
| PS-09 | While dialog is open switch/close/disconnect/reconnect or transition the bound session; pending paste cancels and never follows another chip or transport generation. | Functional | Yes |
| PS-10 | Exercise OS clipboard delivery from menu, shortcut, context menu, and accessibility action; only captured repository-owned text enters the target and no clipboard content is retained in artifacts. | Native functional + privacy | Yes with sanitized byte hash/count oracle |

### Native platform, appearance, and accessibility

| ID | Workflow and oracle | Evidence class | VM automation candidate |
| --- | --- | --- | --- |
| NP-01 | Drag, double-click, minimize, maximize, restore, snap/tile, system-menu, and close custom chrome on each supported compositor. | Native functional | Partly; OS window-state APIs provide oracle |
| NP-02 | Move through scale factors and monitors; chrome, menus, modals, icons, text, and terminal remain aligned without clipping or unintended resize. | Native visual + usability | Screenshot sequence plus window/grid metadata |
| NP-03 | Verify macOS native menu dynamic labels/states, responder-chain Copy/Paste, Services/Hide/Quit, and absence of duplicated in-window native controls. | Native functional | Yes with Accessibility API; Services remains manual |
| NP-04 | Traverse every surface keyboard-only and with UIA/Accessibility/AT-SPI; names, roles, states, focus order, restoration, and icon tooltips are accurate. | Accessibility | Partly automated semantic assertions; screen-reader comprehension human |
| NP-05 | Review compact blue-graphite palette, active/inactive contrast, status semantics, icon legibility, JetBrains Mono terminal faces/fallbacks, and Inter application typography at supported scales and high contrast. | Visual + usability | Screenshot/contact sheet automation; judgment human |
| NP-06 | Use IME composition and representative non-Latin/fallback scripts; composition commits once to its owner and cancels on session/focus changes without leaking pre-edit text. | Native functional | Platform-specific driver possible; human confirmation retained |

### Configuration, persistence, and future transports

| ID | Workflow and oracle | Evidence class | VM automation candidate |
| --- | --- | --- | --- |
| CP-01 | Load valid, missing, and invalid configuration; reload reports truthfully while existing sessions remain alive. | Functional | Yes: isolated fixture files |
| CP-02 | Save/restore workspace metadata; tab order/focus/profile identity restore, SSH requires authentication, and no runtime state, terminal content, or secret is persisted. | Functional + privacy | Yes: artifact inspection |
| CP-03 | Exercise native secret store available/locked/unavailable/failure states; saved-password flows store only references and expose actionable non-secret feedback. | Native functional + usability | Partly: platform test stores in disposable accounts |
| CP-04 | Configure/discover/open/close/reconnect Serial devices including missing, busy, and permission-denied adapters; history and exclusive ownership follow session rules. | Deferred until Serial implementation | Later with virtual loopback plus representative hardware |
| CP-05 | Review profile editing, bundled font choice, appearance controls, update behavior, file-drop path insertion, and terminal search when each capability lands. | Deferred by capability | Add a focused scenario before implementation closes |
| CP-06 | After Local, SSH, and Serial plus their UI are complete, decide whether native Markdown viewing belongs in fesTerm; if accepted, validate the chosen readability/fidelity target, safe local/remote ownership, accessibility, resource isolation, bounds, and return-to-prior-surface behavior. | Deferred product/design review | No implementation or VM workflow until the prerequisite gate and scope decision are complete |
| CP-07 | In two live sessions, zoom each to a different size through shortcuts and palette, switch repeatedly, reset one, move across DPI scales, and confirm only the active terminal grid changes while chrome, profile configuration, bottom/history anchoring, and the other session remain stable. | Native functional + visual + usability | Automate point-size/session-state assertions and resize-probe sequence; retain multi-DPI visual review |
| CP-08 | Enter and exit Focus Mode from the palette with a live session; confirm chrome/footer hide and restore without changing the active session or zoom, the hint is readable, Escape reaches the terminal without exiting, exceptional overlays remain available, and switching to a non-session surface exits safely. | Native functional + visual + usability | Automate state/focus/grid assertions and screenshots; retain native window-control/accessibility review |
| CP-09 | Open About from Launcher, Settings, and a live session at ordinary/narrow sizes; verify exact version/build/OS/architecture copy text, source link, license disclosure, no session/path/host/settings leakage, no update control, keyboard reachability, and Close/Escape focus restoration. | Native functional + visual + accessibility | Automate semantic content, redaction, narrow geometry, and screenshot; retain link handoff and screen-reader review |

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

- `gui-action-graph.md` defines the traversable actions, guards, oracles, and
  cancel/undo/return paths that produce these evidence results.
- `ui-test-plan.md` defines the automated and platform test architecture.
- `m6-validation-gate.md` defines the current milestone acceptance gate.
- `m6-compatibility-checklist.md` defines reference-application scenarios.
- `vm-evidence-framework.md` defines controlled cross-platform execution and
  evidence handling.
- `gui-design.md` defines intended behavior and identifies usability
  hypotheses; this registry says where their validation is tracked.
