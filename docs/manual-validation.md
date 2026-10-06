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

The first two rows below are M6 blocking evidence under
[`m6-validation-gate.md`](m6-validation-gate.md): deterministic platform
evidence, one qualifying native desktop path per supported OS, and
representative application semantics. Hardware/architecture breadth,
multi-monitor and mixed-DPI matrices, performance and peripheral coverage,
broad accessibility comprehension, and subjective visual/usability judgment
remain active rolling qualification but do not independently keep M6 open.

| Area | Required environments | Manual or usability evidence | Status / tracking |
| --- | --- | --- | --- |
| M6 native-window foundation | Windows, macOS, Linux desktop environments named by the M6 gate | Real focus, renderer/window startup, resize continuity, PTY input/output, compositor behavior | Manual pending; umbrella implementation [#8](https://github.com/fes/fesTerm/issues/8), Linux focus [#21](https://github.com/fes/fesTerm/issues/21), environment findings #32–#36 |
| Reference terminal applications | Representative supported desktops; tool-specific platforms where applicable | Shell editing, `less`, Vim/Neovim, tmux, htop, GitHub Copilot CLI, `vttest`, selection and input semantics | M6 blocking evidence pending; [#26](https://github.com/fes/fesTerm/issues/26), checklist in `m6-compatibility-checklist.md`; Emacs remains useful rolling compatibility evidence and `tack` remains M10 work under [#27](https://github.com/fes/fesTerm/issues/27) |
| Custom title bar and window chrome | Windows, macOS, Linux/X11 and Linux/Wayland; multiple scale factors | Drag/double-click, minimize/maximize/restore/close, snap/system menu behavior, multi-monitor DPI, narrow layout, chip drag interaction, accessibility | Manual pending; [#29](https://github.com/fes/fesTerm/issues/29) |
| Native macOS application menu | Logged-in macOS desktop | Menu installation and conventions; shortcuts; dynamic Close and Inspector state; focus-aware Copy/Paste without PTY leakage; native Services/Hide/Quit/window actions | Manual pending; [#44](https://github.com/fes/fesTerm/issues/44) |
| Launcher and integrated chrome | Windows, macOS, Linux; narrow and scaled viewports | Visual comparison to approved mockups; keyboard-only launch flow; focused-chip-first compaction before overflow; fixed New Session placement; first/middle/last focus reveal; chip overflow/reorder/rename; stable vertical geometry and compactness | Usability pending; deterministic allocation/geometry regressions are automated, while native visual and interaction judgment remains in the umbrella |
| Session Inspector | Windows, macOS, Linux; narrow and scaled viewports; local and SSH sessions | Overlay geometry without terminal resize; focus restoration; first-click dismissal; selectable facts; failure/diagnostic comprehension; active-session switching | Automated structurally; native visual/usability pass pending in umbrella |
| Keyboard bindings and input recorder (#154) | Native macOS/Linux/Windows, non-US layouts, AltGr/Option/IME, terminal and widget focus | TI-12/TI-13 below; real selected fake-auth-URL Copy without submission, effective native menu mappings, reset/recovery and bounded redacted keyboard/mouse observations | Deterministic production dispatch/editor/config/mouse/privacy tests automated on all three guest platforms. Linux native controlled keyboard sample passed; full layout/menu/selection and usability matrix remains pending; macOS native driver blocked on Accessibility consent |
| Terminal and session-chip context menus | Windows, macOS, Linux; ordinary and TUI mouse-reporting modes | Native secondary-click/Shift-override conventions, popup placement at edges and DPI scales, bounded naturally spaced path/URL previews and complete display-safe tooltips/accessibility labels, Copy path/Copy URL clipboard delivery and Go browser launch, clipboard focus, terminal-history snapshot commands when no selection is active, menu keyboard traversal, inactive-chip targeting, and destructive-action clarity | Structural/layout, display escaping, exact action values and wide-character wrap/history detection automated; native visual/accessibility/usability pass pending in [#43](https://github.com/fes/fesTerm/issues/43) |
| Scrollback, reflow, and read-only history | Windows, macOS, Linux; `Disabled`, 16 MiB, 64 MiB, and 256 MiB limits; wheel and trackpad; narrow/wide resize; live, alternate-screen, and disconnected sessions | New sessions use the selected limit while already-open sessions retain theirs; smooth navigation near the 64 MiB default; follow suspension/resume and `Jump to latest`; stable viewed content and selection through output, eviction, chip switches, and resize; scrollbar discoverability; selection/Copy across wraps without synthetic soft-wrap newlines; no alternate-screen leakage; disconnected history remains scrollable/copyable without input; terminal-history snapshots export the retained text model honestly | Core bounds, limit configuration and future-session policy, selection and viewport remapping through primary reflow, initial wheel/keyboard follow-anchor routing, conditional Jump control, scrollbar geometry, track paging, terminal-history snapshot semantics, and TUI input isolation are automated; native wheel/trackpad and drag feel, high-contrast/accessibility sizing, near-limit performance, and the pending eviction/disconnected-history usability slices remain in [#43](https://github.com/fes/fesTerm/issues/43) |
| Icons | Windows, macOS, Linux at supported scale factors | 16/20 px legibility, alignment, state distinction, fallback behavior, and accessibility | Runtime integration is implemented; SVG/runtime geometry convergence and native review remain in [#30](https://github.com/fes/fesTerm/issues/30) |
| Application typography | Windows, macOS, Linux; supported DPI and representative fallback scripts | Four terminal families with ligatures off/on, Inter legibility and metrics in compact chrome, non-Latin fallback, hierarchy, truncation, and stable layout | Native four-family review pending; deterministic grapheme-width and color-emoji fallback are implemented under ADR 0026 with a reviewed Windows baseline, native macOS/Linux appearance review remains, see NP-05 |
| Accessibility and discoverability | Windows UIA, macOS Accessibility, Linux AT-SPI where supported | Keyboard-only traversal, focus order/restoration, screen-reader names/states, icon-only control comprehension, tooltip coverage | Manual pending; tooltip wording [#31](https://github.com/fes/fesTerm/issues/31); platform findings get focused issues |
| Paste confirmation and destructive actions | All supported platforms; bracketed and ordinary terminal modes | Threshold comprehension, exact preview at narrow widths, safe Enter behavior, cancellation after session/generation/state changes, active and inactive-session close confirmation, and consistent immediate close from every route when the preference is off | Implemented with portable policy coverage; native functional and usability evidence pending under scenarios AS-06–AS-07 and PS-01–PS-10 below and [#43](https://github.com/fes/fesTerm/issues/43) |
| SSH interaction workflows | All supported platforms with repository-owned fixture plus controlled native UI | Host-key comprehension, authentication focus and secrets, explicit saved-password/store-unavailable feedback, no automatic workspace connection, disconnect/history behavior, conditional reconnect, port-forward manager state, and error recovery | Stored-password transport and headless UI paths automated; native secure-store/platform usability pass pending in umbrella [#42](https://github.com/fes/fesTerm/issues/42) |
| Serial interaction workflows | Windows, macOS, Linux with representative adapters and permission states | Native discovery/open/close behavior, unavailable/busy devices, configuration clarity, exclusive ownership, disconnect/history/reopen, and permissions. Repository-owned automation now covers config validation, app-layer startup/failure paths, and the Linux `socat` loopback. | Manual pending; Windows/macOS real-adapter execution and permission-denied evidence remain open under CP-04 |
| Native local session persistence daemon | Windows, macOS, Linux using signed/packaged builds | Executable installation, detach/reattach replay, newest-client takeover, process independence and cleanup, owner-only local IPC, Windows current-user pipe isolation and Job Object breakaway, and update continuity across compatible helper releases, plus Windows verified-ConPTY parity between an in-process tab and a durable sessiond session and an in-place upgrade while a daemon generation is live | Implementation provisional under ADR-0025; native evidence pending under CP-11 |
| Running Sessions discovery and churn | Native Windows sessiond; macOS/Linux sessiond, tmux and GNU screen when installed | New Session refresh and provider counts, same-process continuity after GUI detach, stale-click diagnostics, attached annotations, large-inventory scrolling, unaffected unrelated sessions | Deterministic parser/worker/headless tests and isolated real-provider churn automated for #155 (follow-up to closed #70); refreshed native GUI/usability evidence remains CP-12 / [#43](https://github.com/fes/fesTerm/issues/43). WSL is Linux evidence, not native Windows |
| Fixed native window title | Multiple simultaneous fesTerm windows; OS task switcher/overview | Whether fixed `fesTerm` identity remains understandable without dynamic session content | Usability pending in umbrella; create a focused issue only if evidence shows a concrete problem |

Accepted window/application teardown now has deterministic document-registry
evidence for CP-13/CP-15: releasing all remaining owned views, preserving a
sibling's unsaved text and undo/redo, avoiding double release after ordinary tab
close, and keeping a moved editor registered when its empty source window ends.
This automates ownership bookkeeping, not native close-dialog delivery,
cross-window pointer behavior, caret/focus, or usability acceptance.

## iOS Phase 1 feasibility (ADR 0042)

The isolated spike is implemented for review. The following evidence is
**manual pending**, not passed by portable tests or a linked Simulator app.
Record OS/SDK, device, orientation, scale and commit with each result; fixture
content is repository-owned. Physical-device runs require a provisioned build.
The persistent-keyboard, arrow and pinch input probes plus offline Files and
Markdown workflow previews are included in this host. Live mobile product work
remains blocked on this gate and a hosting go/no-go decision.

| ID | Environment / scenario | Acceptance / evidence |
| --- | --- | --- |
| MOB-01 | iPhone and iPad Simulator plus physical iOS device; cold launch, portrait/landscape, safe areas, touch selection/scrolling and software-keyboard feasibility | ANSI/Unicode/emoji fixture paints and resizes; no clipped/inaccessible controls or stale grid; record touch/keyboard gaps explicitly and assess whether public hosting APIs suffice. Visual/usability evidence required. |
| MOB-02 | iPhone/iPad portrait, landscape, Split View; native keyboard, accessory row, IME and hardware keyboard | With simulated hardware keyboard disconnected for software-keyboard evidence, keyboard remains visible without repeated hide/show animation during idle frames, toolbar taps, scrolling and terminal keys; final platform output must not repeatedly interrupt IME composition; terminal stays above keyboard using measured geometry; Ctrl/Alt apply once; long-press/drag shows a temporary direction helper with three repeat speeds, neutral pauses, no permanent arrow row; release, second touch, focus loss, rotation, keyboard resize and backgrounding stop repeats immediately; early drags retain ordinary behavior and arrow gestures never send duplicate mouse reports; verify helper visibility and gesture thresholds on real fingers; hardware keyboard reclaims space while retaining the row; counters match encoded key events without duplicate routing; terminal fixture does not echo input; reset clears counters; capture only fixture and content-free diagnostics. |
| MOB-03 | Background/foreground repeated ten times; Simulator memory warning; real-device suspension; terminate and relaunch | No background draw/crash/spin, foreground surface repaints, counters advance once per transition, view-cache reset retains the grid/history and chosen text size, process death starts the fixture rather than claiming restoration. Profile memory/CPU and record unsupported callbacks. |
| MOB-04 | iPhone/iPad portrait, landscape and Split View; pinch from rest and from a held arrow gesture; pinch at minimum/maximum text size | Only terminal text scales within 8–32 points; native keyboard/accessory controls remain stable; grid resizes coherently without losing the viewed content. Check precision/jitter with real fingers and immediate reversal at limits. Arrow helper/repeats stop when the second finger takes over; no mouse reports or text are emitted. Either finger lifting, a third contact, blur, rotation/keyboard geometry change and backgrounding cancel; remaining fingers cannot restart input until all lift. An already-started ordinary drag retains ownership; toolbar-origin touches do not pinch. Memory warning preserves text size; Reset fixture restores default. Portable arbitration/layout/bounds tests pass; native feel, visual stability and history anchoring remain manual pending. |
| MOB-05 | iPhone/iPad portrait, landscape and narrow Split View; switch Terminal/Files/README repeatedly; exercise both transfer directions, README collision choices, progress, Preview/Source/Contents and each heading | Keyboard dismisses outside Terminal and returns without a restart loop; terminal gestures cannot affect Files/README. No category loses in-memory state. Wide Files panes are horizontal; Compact panes are both visible with Remote on top and Local below, so upload moves up and download moves down; Minimal has a reachable pane toggle and transfer queue. Collision actions and progress are bounded and never claim real I/O. Markdown controls never collide, prose wraps, contents is reachable and heading activation changes the selected/visible section. Every screen says synthetic/offline. Record clipping, Dynamic Type and VoiceOver gaps; this does not accept live SSH/SFTP, credentials, file access, full tab lifecycle or restoration. |

Automated policy/input/headless-grid tests are in `festerm-mobile`; iOS CI
checks device compilation and Simulator linking, while the build script checks
normal/build graph exclusions. `scripts/smoke-ios-simulator.py --run` adds
isolated iPhone/iPad install, launch-survival, first UI callback, terminate/relaunch and screenshot
capture for the launch portions of `MOB-01`/`MOB-03`. Its Python policy tests
cover ownership, failure reporting and cleanup. Affected PRs retain mobile
unit/dependency/build checks but skip CoreSimulator preparation/runtime smoke;
that experimental evidence runs nightly at 08:17 UTC on the default branch
or via manual dispatch, not as a desktop merge gate. Failures still fail those
runs and retain their artifacts (#303); no retry or guard/deadline change.
Review an explicit runtime merge gate when mobile product support is accepted.
The manifest and PNGs are review evidence, not automatic rendering,
keyboard or lifecycle acceptance. Physical-device and most interaction
evidence above remains pending. Nightly/manual CI pins Xcode 16.4 and runs
read-only `--prepare-only` before compilation to initialize cold CoreSimulator caches
(runner-images #12862; local tracker #303). Preparation has a separate 180-second
bound, produces `prepared` rather than app `pass` evidence, and never retries or
mutates existing devices. The subsequent inventory, application and capture
deadlines and all ownership/UI/cleanup checks remain unchanged. Failed
preparation and command durations are retained in the same evidence artifact.
A local iPhone 17/iOS 26.5/Xcode 27 run
visually covered Terminal, Compact stacked Files, Markdown wrapping and
Contents/heading activation; it does not qualify iPad, rotation, Dynamic Type,
VoiceOver or a physical device. The `c7ecb14` run diagnosed a renderer initialization
limit mismatch on both Simulators (16 requested inter-stage variables, 15
supported). The mobile downlevel-limit fix requires native rerun and image
review; issue #261 tracks this MOB-01 startup blocker. SSH recovery and Keychain are **deferred** behind
the Phase 1 decision, not missing evidence for an implemented connection.

## Deferred desktop Store qualification

The [desktop Store distribution plan](app-store-distribution-plan.md) defines
the proposed Windows package matrix and Mac sandbox experiments. These are
**deferred**, not passed by existing direct-package or CP-09/CP-11 evidence:

| Track | Prerequisite | Evidence and owner |
| --- | --- | --- |
| Microsoft Store | MSIX build, test package identity, native x64/ARM64 environments and Store flight | Automated manifest/updater isolation; native install/update/uninstall, ConPTY and daemon lifecycle/security; external certification. [#142](https://github.com/fes/fesTerm/issues/142) owns execution. |
| Mac App Store | Signed sandbox feasibility prototype; any implementation then needs approved product scope and a Store test build | Native local PTY/daemon, file grants, serial and Keychain; usability of consent/denial states; separate App Review evidence. [#143](https://github.com/fes/fesTerm/issues/143) owns a go/no-go recommendation, not a shipping mandate. |

Keep current scenario IDs and results unchanged. Add implementation-specific
workflow/trace mappings when behavior exists; the planning change does not
add an M6/M10 gate or change ADR 0025 acceptance.

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
| AS-03 | At one fixed scale, repeat in both full-height 34 px and compact-height 28 px modes while resizing a multi-session window through the three approved chip-row states: natural widths, inactive-only compaction, and minimum-width overflow. Verify the focused chip remains at normal width for the selected density; inactive chips shrink toward the 72 px floor before any scroll affordance appears; Search then Inspector collapse before the focused chip is compromised; scrolling starts only after inactive minimums, preserves compact widths, and keeps bottom outlines visible; first/middle/last activation expands and fully reveals the new focused chip while the old chip becomes compactable; closing sessions lets inactive chips grow back. Then reorder by drag and menu, rename by double-click, and confirm stable active identity. | Functional + visual + usability | Automate exact width budgets, focused-width invariance, overflow threshold, focus-switch reveal, and grow-back; retain native trackpad/drag feel and visual comparison to both density rows in all three approved mockups for human review |
| AS-04 | Exercise every global shortcut from terminal, Vim, Emacs, palette, and application controls; application chords act once and ordinary terminal chords reach the TUI. | Native functional | Yes: controlled PTY byte oracle |
| AS-05 | Close Launcher, Settings, exited, failed, and disconnected surfaces immediately; closing the final surface returns Launcher. | Functional | Yes |
| AS-06 | With **Confirm before closing live sessions** on, close a live local/SSH session from chip, context menu, shortcut, palette, native menu, and overlay; each opens the same confirmation bound to the intended session. Turn it off and repeat; every route closes immediately through the same bounded policy. Restart and verify the chosen value persisted, then restore the default on value. | Native functional + usability | SSH control cadence and loopback shutdown during busy output with a full frontend queue are automated; native invocation routes, consequence wording and immediate-close expectation remain human review |
| AS-07 | With live-close confirmation open, initial Enter does not confirm, Escape cancels without PTY bytes, outside click does not dismiss, and deliberate focus + activation closes exactly the bound session. | Native functional | Yes: UI automation plus controlled PTY/lifecycle oracle |
| AS-08 | Request application/window quit with multiple live sessions (close button, native Quit menu, and Cmd+Q — fesTerm's single window means all three arrive as the same close request); aggregate consequence is accurate, Cancel returns exact window state without acting, and deliberate confirmation exits the process exactly once. Repeat Cancel and confirm through accessibility invocation, then with zero live sessions and confirm no dialog appears. | Native functional | Headless safe-default, Cancel/confirm and accessibility activation are automated, including terminal-blackout isolation. Native multi-transport counts, platform routes and whole-window/descendant teardown remain qualification evidence |
| AS-09 | With multiple local/SSH sessions and long/changing titles, toggle **Show session details in chips** at ordinary and narrow widths, with the status bar on and off. Verify one coherent resize per transition; `34→28` px chip and `42→36` px chrome geometry; stable chip identity/type/state/Close targets; only the active detail relocates to the footer; title-first/factual-fallback precedence; ellipsis priority; empty Launcher/Settings footer; and palette/hover/accessibility/Inspector access when both displays are off. Repeat with wrapped rows and on macOS traffic-light chrome. | Native functional + visual + usability + accessibility | Automate preference/state, grid-resize count, geometry, active-value, and narrow screenshots; retain native hit-target, title-churn readability, macOS optical alignment, and screen-reader review |
| AS-10 | Rename/reset one of two same-profile terminal views, move it between windows, restart an opted-in disposable workspace and authenticate restored SSH/SFTP. Include local/SSH tmux and Screen: a saved tab retains its alias after backend recreation, while a separate new attachment starts at default. Native views of the same generation may have different aliases; rename/reset of one must not rename another. With workspace restoration off, fresh exact native Running Sessions reattachment may use its saved seed, but a recycled native generation cannot. Ordinary/mux edits with restoration off remain live-only and do not enable it. Force an owned save failure and retry; a nonempty rejected edit must show a content-free refusal without changing anything, while an empty/sanitized-empty edit cancels quietly. Profiles, provider targets and OSC titles remain independent. | Deterministic view/config/auth/reconnect/window/native-seed/opt-out/failure/rejection/fixture-cleanup tests and Windows local CI-equivalent qualification passed on 2026-10-02. Required remote CI and native functional + accessibility + usability evidence remain pending on Windows/macOS/Linux. Physical mux identity naming is outside the chosen scope, not a pending prerequisite for saved-tab aliases. | Use owned fake transports/files for deterministic tests; keep native focus/click-away/menu accessibility, notice readability, owned provider workspace/recreation flows and packaged native daemon lifecycle in the human/native matrix. No new snapshot baselines or user sessions are required. |

The dialog-style regressions cover full-root `752 × 516`, `360 × 516`, and
`360 × 240` layouts with the production Dark/default visuals: About with license
and update disclosures; long-target close, bounded paste, update-consent, and
reset decisions; Open File/Save As with project-owned long paths and missing
folders in both chrome densities; and palette/overflow row bounds. They assert
minimum action targets, Cancel/default focus, non-destructive cancellation,
Escape/generation invalidation, and terminal focus restoration. These extend
AS-07, CP-05/06/09, and TI-15 automated geometry/semantic coverage, not native
platform or usability acceptance. Native DPI/edge placement, short-sheet
scrolling feel, screen-reader traversal, and visual comparison remain in NP-02
and the corresponding existing scenarios.

The retained matching 23-state run `style-pair-20261001-1700` is not a completed
style acceptance: actual candidate `daf9796` drawing shows short Save As table
content outside its sheet and narrow deep Open File breadcrumbs occupying the
initial ready/error viewport. Rectangle containment and safe wheel-reachable
Cancel tests pass but do not prove these painting/presentation properties.
Keep those deterministic visual blockers separate from the native follow-up
above; no capability/status is promoted by successful capture completion.

Follow-up deterministic tests reproduce the nested-panel paint escape, enforce
inherited paint/pointer clipping and initially visible modal-owned Save As
filename/Save/Cancel at all three roots with both chrome densities, and verify
deep-path/filter placement plus exact ancestor navigation after horizontal
scrolling. The stricter Open File short-root initial-control test now passes
after reserving its footer and budgeting its error/list viewport; initial
path/filter pointer focus and error paint are checked at all three sizes/both
densities. Cancel restores terminal input without an extra click. Save As
hidden row/action pointer tests also pass, with an explicit Save response ID
on the filename's modal layer. Fixes still require a fresh uniformly matched
drawing pair; the completed old run remains failed visual evidence.

The authorized fresh `style-pair-20261002-r2` pair is now retained separately:
production-f0 BEFORE executable `510af3f…`, corrected `426c458` AFTER executable
`5dc2f785…`, exactly 23 scenes, 242 real file SHA/mtime/kind matches, Microsoft
Basic Render Driver CPU/DX12 and PPP 1.0. All 23 AFTER images were inspected;
initial principal fields/actions and clipped paint converge, while compact
body navigation/rows/disclosures scroll. Successful candidate completion
performed only inventoried input cleanup, so neither completed pair may be
reused. Original 48 gallery assets and failed first-pair evidence are unchanged.
About-short's update action is outside its initial body viewport, overflow
popup areas remain unobserved, and collapsed-palette/update-announcement states
remain unqualified. Duplicate global Save labels are not ownership evidence;
the direct response/default-focus tests supply that proof. Native platform,
hardware and usability scenarios above remain pending.

### Terminal interaction, history, and overlays

| ID | Workflow and oracle | Evidence class | VM automation candidate |
| --- | --- | --- | --- |
| TI-01 | Type/edit at a controlled prompt; selection, Copy, Paste, focus transitions, cursor, and resize remain coherent. | Native functional | Yes: existing native smoke expansion |
| TI-02 | Run Vim/Neovim, Emacs, less, tmux, htop, Copilot CLI, and `vttest`; verify keyboard/mouse ownership and no application shortcut collisions. | Compatibility + usability | Partly: scripted launch/input/screenshots; interpretation remains human |
| TI-03 | Find the Shift+right-click hint in Settings, right-click with mouse reporting off/on and use Shift override; exactly one owner receives the complete press/release gesture and popup keyboard focus restores correctly. Repeat local/SSH and native secondary-click variants. | Native functional | Settings hint and mouse-mode/encoding matrix automated; native gesture/focus/usability evidence remains manual |
| TI-04 | Generate bounded history, scroll by wheel/trackpad/keyboard/scrollbar, switch sessions, receive background output, and use Jump to Latest without losing the reading anchor. | Native functional + usability | Mostly; trackpad/scroll feel remains human |
| TI-05 | Resize repeatedly while anchored in wrapped history, copy across wraps, approach eviction, and sustain one unbroken output line near the configured bound; repeat resize with history disabled, then exit/disconnect. Visible content and cursor survive resize, retained history remains bounded/scrollable/copyable, and exited or disconnected history rejects input. | Functional + performance + usability | Partly after reflow/disconnected slices land |
| TI-06 | Open Inspector over local and SSH sessions; terminal grid does not resize, facts/actions change with active session, outside first click is consumed, Escape restores focus, and diagnostics disclose safely. | Native functional + usability | Yes: semantic tree, grid probe, screenshot |
| TI-07 | Trigger failure, host-key verification, authentication-required, disconnect, and reconnect surfaces; focus, wording, trust facts, secrets, and allowed actions remain state-accurate. | Native functional + usability | Partly: repository SSH fixture, including input/resize/shutdown progress under busy output and a full frontend queue; native surfaces and secure-store prompts remain platform/manual |
| TI-08 | From Quick Connect, Advanced Connect, and a saved SSH profile, leave persistence off and then enable tmux/screen with valid and invalid names. Plain mode opens a fresh shell; durable mode attaches or creates the exact named session; when persisted host trust plus a non-interactive credential allow a background probe, a newly enabled untouched draft defaults to tmux if detected or GNU Screen otherwise without overwriting an explicit provider choice; automatic recovery is separately opt-in; Inspector language says Resume only for durable state. | Native functional + usability | Partly: headless form/command coverage; real-provider create/detach/reattach and capability-failure evidence remain under #49 |
| TI-09 | On macOS, launch the built-in Local Shell from an app started inside Apple Terminal, then launch saved Local profiles with persistence off and with named `festerm-sessiond`/tmux/screen persistence on. Built-in/plain/native-persistent children identify `TERM_PROGRAM=fesTerm`, clear an inherited `TERM_SESSION_ID`, and show no inherited “Restored session” transcript; multiplexer-owned children retain their provider identity, and only explicitly persistent saved profiles attach or create durable state. | Native functional + usability | Mostly: plain and native-persistent child environments plus profile command selection are automated; native Apple zsh startup plus real-provider behavior remains manual |
| TI-10 | Emit an explicit HTTP/HTTPS OSC 8 link whose visible text differs from its target. Verify ordinary click retains terminal behavior; Ctrl/Cmd-click and context **Open link** launch the normalized target through the native browser; **Copy link** copies that target; malformed, non-web, and spoofable targets expose no activation. Repeat with long hosts, paths, query strings and fragments at narrow widths and multiple DPI scales: the preview is one normally spaced line with explicit elision, the full target remains available on hover/accessibility, and copied/launched values are not abbreviated. | Native functional + security + visual/accessibility | Mostly: parser, UI intent, exact action values, bounded un-justified layout and application policy are automated; native OS-handler/clipboard delivery, edge placement, screen-reader delivery and human readability remain manual |
| TI-11 | In a live SSH session with saved and ephemeral local/remote forwards, open Port Forward Manager from both its shortcut and palette entry; verify the list distinguishes saved-profile vs ephemeral state, add/remove loopback forwards, surface per-mapping failures without losing the shell, disconnect to clear the live list, and reconnect to confirm nothing is silently restored. Exercise the shared 128-entry profile/pending/active/failed ceiling: excess additions visibly refuse without evicting a tunnel or losing the draft; remove a failed row and retry. Check message readability/accessibility in a narrow overlay and retained row identity after earlier removals. | Native functional + usability | Mostly: deterministic exact-limit, rollback, generation, same-binding remove/re-add incarnation, production local-listener token lifetime, capacity, snapshot-backpressure and headless stored/direct profile refusal/identity tests plus the owned loopback SSH fixture automate backend boundaries and active-byte preservation. Native end-to-end SSH usability, accessibility and OpenSSH-specific behavior remain manual/opt-in; no acceptance promotion |
| TI-12 | In the scoped, searchable keyboard editor, assign typed and captured chords, unbind/reset application shortcuts, restart an isolated configuration, verify native menu/palette/chrome hints and recovery Ctrl+Shift+F12; select a fake authentication URL and Copy repeatedly, then enter a fake token. Test actual Ctrl+C, Ctrl+A/B prefixes through Screen/tmux, AltGr/Option and IME with terminal/search/forms/menus focused. | Native functional + usability | Synthesized production paths automated by `python3 scripts/check_keyboard_routing.py`; `FESTERM_ISOLATED_TEST_DESKTOP=1 ... --native` adds the existing OS-input drivers and controlled byte/capture sample on a clipboard-isolated test desktop, not the entire matrix |
| TI-13 | In Session Inspector Diagnostics start/stop/clear input recording and explicitly copy its report. Exercise local selection, terminal-owned/unreported mouse motion, reported mouse events, local context/history gestures and subsequent Copy. Inspect a report for absence of fake token/URL/clipboard contents. | Native functional + privacy/usability | Core/queue/selection classification, bounded storage and redaction automated; physical-event attribution and remote-program interpretation remain unknown, not invented |
| TI-14 | In local, SSH, and text-mode SFTP sessions, right-click visible absolute paths, `~/` paths, relative paths, wrapped/Unicode names, and an OSC 8 link near path-like text. Verify **Open in viewer** appears only for the frozen clicked target, opens local files locally, opens remote files only through the same live verified SSH/SFTP transport, keeps OSC 8 actions intact, and explains unknown cwd / disconnected transport / missing subsystem / host-trust limits honestly instead of guessing or invoking a shell. Include long Windows/UNC/remote paths, real repeated filename spaces, combining/emoji names, display controls, and a wide character wrapping with one spare column, both live and in retained history. Preview spacing stays natural and single-line, elision keeps useful filename/root context, the complete escaped target is available on hover/accessibility, and Copy/Open preserve real filename bytes without synthetic wrap padding. | Native functional + security + usability/accessibility | Partly: parser, bounded display/grapheme layout, exact menu-action values, core-owned wrap extents, live/history detection, remote-text-tab routing, live-transport/fingerprint pinning, and unavailable-state coverage are automated. A real loopback SSH/SFTP server covers typed-password/session-only trust, exact file bytes, size limits, missing/non-file targets, stalled/refused subsystems, shell responsiveness, and shutdown. Native GUI end-to-end remote open, edge/DPI placement, screen-reader delivery and refusal-overlay/readability usability remain manual |
| TI-15 | From a live session, an alternate-screen TUI, and an exited/disconnected history, use the command palette and the terminal context menu (with and without a selection) to choose **Open Terminal History in Editor** and **Save Terminal History As…**. Confirm the new editor shows `UNTITLED`, starts dirty, contains logical plain text without ANSI/control sequences, omits synthetic soft-wrap newlines, uses retained primary history plus the applicable visible screen, and stays unchanged while later terminal output arrives or while the saved copy is edited. Confirm plain **Save** / `:w` / `:wq` on the untitled snapshot go through Save As until a destination exists, Save As cancellation/failure is non-destructive, oversize retained history is refused without partial export, and no PTY input is sent. | Native functional + usability | Mostly: core extraction, palette/context routing, disconnected access, immutable snapshot behavior, honest bounds refusal, untitled-save routing, and save-error handling are automated; native menu placement, edge positioning, alternate-screen usability, and cross-platform Save As feel remain human review |
| TI-16 | In primary/alternate screens and disconnected history, choose **Redraw Terminal** from the palette. Confirm content, cursor, modes, selection, reading anchor and zoom remain unchanged, terminal focus returns, and Ctrl+L/Ctrl+R still belong to the TUI. On eligible Windows Direct2D, unchanged regions are repainted once and ordinary reuse resumes. Do not substitute **Reset Terminal**, which changes terminal state. | Native functional + usability | Row-cache, native full-surface invalidation, palette/typed-command routing, core-state preservation, no input/resize and TUI-key coverage are automated; native desktop appearance and cross-platform focus/usability remain manual pending |

Windows ConPTY prompt timing qualification is automated by
`python scripts/check_windows_conpty_prompt.py --cycles 20` and the fixed
`windows-conpty-prompt-stress` VM adapter mode. It repeats the exact
prompt-return oracle without increasing its deadline or retrying a failed
attempt.

The #154 review follow-up adds synthesized regressions for chip-rename raw and
semantic clipboard paths, same-batch switch/capture ordering and recorder
targets, Inspector Paste exclusion, eventual pending-write settlement after
stop/clear/eviction/reconnect, effective Markdown toolbar hints, and IME
cancellation by tab click without Commit. These regressions are included in
the final three-platform synthesized runs below; the native sample does not
replace the complete focus/layout matrix. In TI-12, include rename
Copy/Cut/Paste and Settings preedit → terminal chip click → shortcut/recovery.
In TI-13, distinguish initial backpressure from eventual acceptance/rejection
and verify stopped recording can settle retained entries without reviving
cleared ones.

### Keyboard baseline and candidate evidence

The following-input ordering fix adds ready-callback + Enter/text, unresolved
request + same-batch input, bounded overflow, transport failure, recovery,
generation and confirmation regressions. Verify that a deliberately delayed
read never lets Enter overtake its paste; cancellation must report discarded
input without sending it to the original or a different session. Protocol
replies and mouse/focus reports must remain serviceable. The native smoke
oracle now observes UI bytes accepted by the session, not merely their earlier
encoding order. The existing native reader sample is not a forced delayed-read
or failure-injection test; these new cases remain separately qualified by
deterministic fake-reader/transport tests pending native evidence.
The final Linux native run below includes the revised accepted-byte oracle.

The delayed-paste follow-up adds identified native reads and fake-callback
regressions for tab/ownership/generation changes, replacements, duplicates,
same-batch supplied payloads, context/middle/native-menu/palette surfaces and
confirmation. The revised keyboard native mode sets the isolated
test desktop clipboard to `controlled-clipboard` (does not read/restore its
previous value), invokes palette Paste, and requires that exact prefix in its
controlled input oracle. The final Linux native run exercises this reader.
Native cross-session/reconnect cancellation remains an additional TI-12 check;
the deterministic fake-reader coverage is not OS-delivered proof.

The existing Linux Xorg OS-input baseline passed at `7e93ece` (run
`20260916T002402Z-linux-festerm-dd35f3ad-7af5-4f55-8291-e687b0213761`).
This is not the new keyboard-specific candidate qualification.
The macOS baseline run
`20260916T002533Z-macos-festerm-c846dd64-86e0-4025-b4a6-b03c1b0ecb2e`
built successfully but stopped at the evidence driver's Accessibility consent
dialog: “Accessibility permission is required for macOS OS-input smoke.”
No native input result was produced. Its original generic failure artifact
is retained; triage is **blocked prerequisite**, not a routing product failure.
Do not bypass TCC or count the baseline as passing selected-URL Copy.

Final implementation candidate `eb08168` used the reviewed adapter at
`3ce45cf`. These reruns include the ownership, identified-read, and input-order
corrections:

| Guest | Mode | Result | Run ID |
| --- | --- | --- | --- |
| Linux | `keyboard-routing-check` | Pass | `20260916T042148Z-linux-festerm-a25f51be-1600-482f-a3ae-113f18f3483f` |
| Windows ARM64 | `keyboard-routing-check` | Pass | `20260916T042450Z-windows-festerm-83b378fa-5497-4b32-8548-87717b838395` |
| macOS ARM64 | `keyboard-routing-check` | Pass | `20260916T042835Z-macos-festerm-4b8a1a73-6454-45fc-9e1f-424793b5584f` |
| Linux Xorg | `keyboard-routing-native` | Pass | `20260916T041844Z-linux-festerm-d217cb4b-7ab7-48c3-9055-47bdf7985071` |

The check mode runs synthesized production app/controller/configuration tests
on the actual guest platform, without claiming OS event delivery. The native
Linux run independently injected no-selection Copy, palette open/close, and
palette Paste. It verified session acceptance of `controlled-clipboard`,
then `02 02 09 1b 5b 41`, then `os-input-ok` and carriage return, in that order.
The dedicated Linux guest's shared-clipboard mode was confirmed off before
running the fixture; no host clipboard content was inspected or changed.
This is not real selected-URL clipboard,
Screen gesture, AltGr, IME, or exhaustive native-menu qualification.
The Windows guest's known graphics limitation and macOS Accessibility
prerequisite remain distinct from successful synthesized routing checks.
All dedicated guests were stopped after their runs.

The Windows OS-input driver now enumerates the PID's visible, unowned,
activating application window and excludes tool windows. It does not use
`Process.MainWindowHandle`, which can identify winit's internal event target.
It verifies foreground activation before injecting input; this does not
replace the isolated-desktop requirement for clipboard-specific checks.
Window/activation errors now terminate the driver, and stdout/stderr logs
are retained beside its result. Basic OS-input success requires the controlled
PTY child to acknowledge a complete line ending in `os-input-ok`; startup,
resize, focus reports and ordinary input echo cannot pass the oracle.
ConPTY can send legitimate focus bytes before the keystrokes, so the basic
check verifies child acknowledgment rather than requiring a focus-free raw
byte stream. The optional keyboard-routing check additionally retains its
exact byte and palette assertions. The corrected basic Windows native run
passed; clipboard-mutating mode was not run on the shared desktop.

The candidate's Settings captures use the production `show_settings` renderer
at normal/narrow widths; captures are review artifacts, not new cross-platform
golden baselines. No native user application or ordinary user configuration is
used by the keyboard-routing entrypoint. See the
[canonical inventory and limitations](keyboard-shortcuts.md).

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

### File-drop path insertion

| ID | Workflow and oracle | Evidence class | VM automation candidate |
| --- | --- | --- | --- |
| FD-01 | Drop one file, and separately multiple files in a deliberate order, from Finder/Explorer/file manager onto the active local live session. Preview shows the exact unquoted, space-joined, drop-ordered absolute path text; confirming inserts exactly that text as typed input with no trailing Enter; Cancel inserts nothing. | Native functional + usability | Automate the ordering/text/Cancel oracle; native drag-from-Finder/Explorer gesture remains manual |
| FD-02 | Drop a file onto an SSH session, a serial session, a disconnected/exited session, and Launcher/Settings with no active session; every case is rejected with a factual transient notice and no dialog, and no client-local path is ever inserted into a remote transport. | Native functional + privacy | Yes: fixture matrix over transport/session state |
| FD-03 | Drop a path containing control characters; the bounded preview escapes them for display (matching the large-paste preview's escaping) rather than silently rewriting or truncating the path that would actually be inserted. | Functional + usability | Yes: fixture path with embedded control bytes |
| FD-04 | With the confirmation open, initial Enter activates Cancel and inserts nothing; Escape and outside-click behave the same as the paste/close confirmations. | Native functional | Yes |
| FD-05 | In the GUI SFTP file manager (issue #137): drag selected item(s) between the local and remote panes and confirm the same transfer requests the toolbar/rail buttons would queue are enqueued; drag files from Finder/Explorer onto the remote pane and confirm they upload into its current directory; drag files from Finder/Explorer onto the local pane and confirm the drop is rejected with a factual notice; attempt to drag a remote item out to Finder/Explorer/Desktop and confirm the current, documented limitation (no native OS drag-out; use the transfer buttons to download first). | Native functional + usability | Payload plumbing, enqueue rules, and connection/writability gating are automated; the native Finder/Explorer drag gesture itself (both directions) remains manual |
| FD-06 | In the GUI SFTP file manager's local pane only, right-click a single item and a multi-selection, choose "Reveal in Finder"/"Show in File Explorer"/"Open Containing Folder", and confirm the OS file manager opens focused on the expected item (single selection) or the first selected item (multi-selection); confirm the action is absent from the remote pane's context menu. | Native functional | Per-platform command construction is automated; actually observing the OS file manager focus/selection remains manual |

### SFTP owner cancellation

| ID | Workflow and oracle | Evidence class | VM automation candidate |
| --- | --- | --- | --- |
| SFTP-01 | Using owned local/remote files, close GUI and text SFTP during connection/trust, browsing/metadata/Markdown reads, copying, and approved replacement. Keep a sibling shell usable. Verify interrupted operation handles retire, cleanup has a two-second grace, uncertain local/remote partials are preserved and reported for manual inspection, and uncertain replacement output remains recoverable. Introduce a destination during copying: its preserved-temporary notice must not prevent Replace, Skip or Keep Both. Move a GUI SFTP tab between windows before closing it: an incomplete-cleanup dialog must reach its current window without replacing another active error. Exercise replaced leaves/ancestors, stalled blocking filesystem work, full notice queue, whole-window close and final packaged process quit; distinguish requested cleanup from confirmed completion and check content-free diagnostics when no window remains. | Automated ownership/model + real loopback; native functional + accessibility/usability pending | Connect wait/observation drop, trust rejection, stalled text download/transport close with a reported partial, queue-independent cancellation, replaced local leaves and Unix ancestor symlinks, retained local/remote partials, late-collision choices without losing recovery output, interrupted replacement, unrelated-work continuation, two-second timeout, non-waiting text runtime retirement with a real blocked task, moved-window notice delivery and bounded notification fallback are automated. Windows reparse-point fault injection, packaged close/quit ordering, surviving shell/window behavior, diagnostic visibility and error-dialog keyboard/screen-reader usability remain manual on Windows/macOS/Linux. Process exit and already-sent server operations cannot be treated as rollback guarantees. |

### Text-mode SFTP input

| ID | Workflow and oracle | Evidence class | VM automation candidate |
| --- | --- | --- | --- |
| SFTP-02 | In an owned text SFTP session, paste a command above 256 KiB, including one whose trimmed prefix would be valid. Finish with CRLF or Ctrl+C, then run a small valid command. No prefix or fragment may execute; refusal must be visible without disconnecting. Exercise a chunk-split CRLF and UTF-8 scalar, Backspace/Delete, cancel after a large paste, and briefly blocked frontend delivery. Confirm SSH-shell input is unchanged. | Automated byte/state, metrics/notifier and owned real-loopback; native keyboard/refusal visibility and usability pending | Exact 262144-byte acceptance, overflow/refusal across 64 KiB chunks, allocation release, UTF-8 byte accounting, bounded scalar lookback, backspace reclamation, retained/coalesced notice identity under a full event queue and retry during awaited I/O are automated. The real loopback records remote mkdir requests and proves only the next valid command executes. Native paste/keyboard gestures, transcript clarity and accessible refusal feedback remain manual on Windows/macOS/Linux. |

### SFTP GUI backlog and transfer history

| ID | Workflow and oracle | Evidence class | VM automation candidate |
| --- | --- | --- | --- |
| SFTP-03 | With owned files, finish more than 128 GUI transfers, including failures, while another item remains running or collision-paused. Retain the latest 128 finished records by finish order, preserve ongoing decisions, and show the retired-record count. Verify pre-start collision/Skip/failure rows remain visible. Scroll to recent failures and use Retry/Cancel/Clear in wide, narrow and short windows. Saturate the command bridge: navigation must preserve path/history/scroll/loading, a rejected reconnect must preserve connection/spinner state, a rejected drop must not claim success, a rejected Markdown read must preserve its previous request, and a refused collision decision must remain available. With more than 64 pending or paused rows, header Cancel must refuse the whole action when full and retry using one free slot. Close with the event bridge full; owner cancellation and the documented cleanup policy still apply. | Automated storage/admission/order/lifetime; native functional + accessibility/usability pending | Exact 64-command/128-event bounds, 64-event polling with repaint continuation, 256/257 batch admission, failed-history retirement, late completion, active-row preservation, duplicate finish/index consistency, exceptional capacity release, progress identity/barriers, refused navigation/reconnect/drop/Markdown state, queue-recovery navigation, full-event-queue owner cancellation and local-producer receiver retirement are automated. Compositional admission/history and real engine collision-before-start/Skip regressions cover missing-row paths; backend refusal creates no phantom history. A production header click covers whole-action refusal/one-slot retry for 96 rows, real engine cancellation covers 96 paused collisions, and manager-state admission covers 1,024 items across batches plus later-work ordering. Native long-history drawer reachability, collision-decision interaction under saturation, OS drop gestures, short-window/high-contrast notice readability, keyboard and screen-reader delivery remain manual on Windows/macOS/Linux. Rows are bounded and scrollable, not virtualized; queue counts do not prove total payload-byte bounds. |

### Interactive-surface performance

The [interactive-surface performance probe](../validation/terminal-performance/README.md#editor-markdown-and-sftp-ui-construction)
automates synthetic release UI-construction and tessellation measurements for
the editor, Markdown Preview/Source and both SFTP panes. Deterministic
regressions cover virtualized directory scrolling through the last row,
whole-list selection/filtering, Open File activation, Save As selection and
overwrite wording, editor syntax/Find formats and clipped wrapped line numbers,
and Markdown source-cache reload/UTF-8 behavior. Markdown Find additionally
checks complete ordered Unicode/multiline positions and indexed
Preview/Source highlighting against a full-scan format oracle. The release
probe includes both Find-heavy views and a separate 20,000-match single-line
query measurement; no Markdown matches are discarded to achieve the result.
Four further scenes isolate Preview headings, prose, code and tables. A
complete constrained-layout oracle checks table-galley reuse across fractional
wrap widths, DPI, Unicode, empty cells, styles and Find; ordinary table width
and wrapping regressions remain in place. Separate production-widget gallery
comparisons cover visible presentation, not native latency.
The render-local code-header Copy caption has an ordinary/prepared oracle over
the actual outline-disabled 400-section fixture: complete clipped shapes,
accessibility nodes and live response IDs/rectangles, cold/warm and
width/font/theme/scale/content changes, selection, tail navigation and exact
distinct-fence Copy payloads. This extends CPU evidence for MD-04 and EDIT-09.
The matched synthetic comparison and adverse controls are recorded in
[`milestone-progress.md`](milestone-progress.md#preparing-a-repeated-code-header-caption-without-retaining-document-layout);
actual input identity and final shape/vertex parity are not pixel or native
input/clipboard/screen-reader qualification.
This extends automated evidence for SFTPG-01, EDIT-04/05/06 and MD-04/06;
it does not qualify native scroll/input latency, GPU presentation, transfer
throughput, screen-reader navigation or the FD-05/FD-06 native drag/reveal
gestures.

The same probe now retains preparation and all eight warmup-frame timings,
plus a separate warm Rust syntax-constructor diagnostic. This exposed repeated
query compilation during fenced-code loading, rather than steady rendering.
Per-language query reuse, independently compiled span equivalence, document
revision/bound isolation and fenced-block isolation extend automated evidence
for EDIT-16 and MD-04. The recorded Markdown improvement follows Rust editor
initialization; first-ever language compilation and native open latency remain
unqualified. A control-first reverse-order attempt stopped on the editor's
`ParseFailed` guard and is retained, not counted as a successful unhighlighted
sample. See the probe record for both completed and aborted sequences.

Full in-memory fenced-document loading is measured separately at 200, 2,000
and 4,000 JSON entries, with every entry required to retain string and number
highlighting. Forward line projection is checked against the former full scan
for Unicode, multi-line spans, newline boundaries and real grammar output.
The larger 8,000-entry stress series remains separate: it stopped on a
highlighting guard, and a syntax-only diagnostic reproduced `ParseFailed`
without invoking the Markdown projection. The existing parse budget remains;
these results neither qualify that stress case nor native document-open
latency.

### Terminal-content search

| ID | Workflow and oracle | Evidence class | VM automation candidate |
| --- | --- | --- | --- |
| SRCH-01 | Open find via `Ctrl+Shift+F`/`Cmd+F` and via the command palette ("Find in Terminal…") on an active session with scrollback and live-screen text; query focus is granted immediately and typed input reaches the query field rather than the PTY. | Native functional | Yes |
| SRCH-02 | Search a term present in retained scrollback and a term present only on the live screen; both are found without spanning a row boundary; case is ignored; navigate forward/back with Enter/Down and Shift+Enter/Up, including wraparound past the last/first match. | Functional | Yes |
| SRCH-03 | Search a term with no matches; the bar shows `No matches` rather than a fabricated `0 of 0`, and Copy while a match is current still requires an explicit terminal selection (no implicit copy of the match). | Functional + privacy | Yes |
| SRCH-04 | While in alternate-screen mode (e.g. a full-screen pager), confirm search only covers the visible alternate-screen content and does not surface primary-buffer text. | Functional | Yes: fixture with alt-screen program active |
| SRCH-05 | With a match selected, produce new output that grows scrollback; confirm the current match selection is preserved by content/row rather than jumping arbitrarily, and that navigating scrolls the terminal to bring the match into view without sending mouse/keyboard reporting to the PTY. | Functional | Yes |
| SRCH-06 | Press Escape while the find bar has focus; query and result state clear, terminal keyboard focus is restored, and no Escape byte reaches the PTY. | Native functional | Yes |
| SRCH-07 | Confirm there is no in-grid highlight of matches (disclosed scope reduction); the only indicators of match location are the `N of M` counter and auto-scroll-to-match. | Visual + usability | Human judgment of documented gap |

### Native platform, appearance, and accessibility

| ID | Workflow and oracle | Evidence class | VM automation candidate |
| --- | --- | --- | --- |
| NP-01 | Drag, double-click, minimize, maximize, restore, snap/tile, system-menu, and close custom chrome on each supported compositor. | Native functional | Partly; OS window-state APIs provide oracle |
| NP-02 | Move through scale factors and monitors; chrome, menus, modals, icons, text, and terminal remain aligned without clipping or unintended resize. | Native visual + usability | Screenshot sequence plus window/grid metadata |
| NP-03 | Verify macOS native menu dynamic labels/states, responder-chain Copy/Paste, Services/Hide/Quit, and absence of duplicated in-window native controls. | Native functional | Yes with Accessibility API; Services remains manual |
| NP-04 | Traverse every surface keyboard-only and with UIA/Accessibility/AT-SPI; names, roles, states, focus order, restoration, and icon tooltips are accurate. | Accessibility | Partly automated semantic assertions; screen-reader comprehension human |
| NP-05 | Review compact blue-graphite palette, active/inactive contrast, status semantics, icon legibility, all four bundled terminal families with ligatures off/on at supported scales, owned monochrome/color emoji fallback boundaries, and Inter application typography. Confirm Agency-style emoji remain aligned next to ASCII at each scale and that the color/monochrome control is understandable. | Visual + usability | Unicode 15.1 corpus tests, presentation-policy and cache-work tests, cold/warm Criterion workloads, reviewed Windows/Linux snapshots, and scheduled three-platform native emoji smoke automate geometry and color-texture submission coverage. Smoke startup/persistence regressions protect the controlled fixture from workspace restoration; CI isolates configuration and imposes an external deadline. Native pixel-level color appearance, scale judgment, and control comprehension remain human |
| NP-06 | Use IME composition and representative non-Latin/fallback scripts; composition commits once to its owner and cancels on session/focus changes without leaking pre-edit text. | Native functional | Platform-specific driver possible; human confirmation retained |

### Configuration, persistence, and future transports

| ID | Workflow and oracle | Evidence class | VM automation candidate |
| --- | --- | --- | --- |
| CP-01 | Load valid, missing, and invalid configuration; reload reports truthfully while existing sessions remain alive. | Functional | Yes: isolated fixture files |
| CP-02 | Save/restore workspace metadata; tab order/focus/profile identity restore, SSH requires authentication, and no runtime state, terminal content, or secret is persisted. | Functional + privacy | Yes: artifact inspection |
| CP-03 | Exercise native secret store available/locked/unavailable/failure states; saved-password and saved-private-key flows store only opaque references and expose actionable non-secret feedback. | Native functional + usability | Disposable put/get/update/delete lifecycles are scheduled for Keychain, Credential Manager, and Secret Service; locked/unavailable presentation and saved-profile usability remain manual |
| CP-04 | Configure/discover/open/close/reopen Serial devices including missing, busy, and permission-denied adapters; history, inspector line settings, and exclusive ownership follow session rules. | Native functional + hardware/permission validation (config parsing, app-layer failure paths, and Linux `socat` loopback are automated) | Linux virtual tests cover ordered bidirectional traffic, bounded shutdown/reopen, disconnect, busy/non-TTY/missing failures; Windows/macOS hardware and native permission states remain manual |
| CP-05 | In signed packaged builds, review Check for Updates, current/available/failure states, explicit download and install/restart transitions, restart consent with live sessions, signature rejection, and package-managed guidance. Publish a newer release after discovery and again after download: each action must refresh, display and directly select the latest eligible version, verifying new bytes before install. Same-version refresh reuses verified bytes. Offline/withdrawn/older/invalid releases or verification failures must leave sessions running, with a retryable error and no cached-version install. Cancelling restart consent must leave the verified update installable; confirming must install, exit once without a second quit prompt, replace, and relaunch. Developer/incompletely configured builds must expose no network action. Track native release/update evidence in [#62](https://github.com/fes/fesTerm/issues/62). | Native functional + visual + security | State-machine freshness, same-version reuse, fail-closed refresh/verification, live-session consent/cancellation, one-shot authorized close, and eligibility rules automated; signed cross-platform replacement/relaunch and failure evidence pending |
| CP-06 | Validate the native Markdown viewer against `docs/markdown-viewer-design.md`: More-actions local open/cancel; paste an absolute, home-relative, and folder-relative path into Open File (Ctrl+L/Command+L focuses it); correct an invalid path without retyping; Preview/Source toggle, Find (including offscreen fenced code), outline, local reload/stale states, inert raw HTML/resource policy, explicit bounded local image loads, accessibility/focus, and return-to-prior-surface behavior. From GUI SFTP, open a bounded remote Markdown snapshot tied to the verified host identity and confirm that reload after reconnect is truthfully unsupported. | Native functional + visual + accessibility + security | Repository-owned local/remote fixtures, bounds, identity, picker path resolution/paste/Enter/stale-result handling, and error-state probes are automated. CPU regressions cover 400-fence offscreen code-byte/Find navigation in viewer/shared Preview, wrapped-code geometry/offset, selection, raw Copy payload and keyboard activation; retain human review for native clipboard delivery, readability, focus, picker usability, and screen-reader behavior. Table-cell byte navigation remains a separate unimplemented gap |
| CP-07 | In two live sessions, zoom each to a different size through shortcuts and palette, switch repeatedly, reset one, move across DPI scales, and confirm only the active terminal grid changes while chrome, profile configuration, bottom/history anchoring, and the other session remain stable. | Native functional + visual + usability | Automate point-size/session-state assertions and resize-probe sequence; retain multi-DPI visual review |
| CP-08 | Enter and exit Focus Mode from the palette with a live session; confirm chrome/footer hide and restore without changing the active session or zoom, the hint is readable, Escape reaches the terminal without exiting, exceptional overlays remain available, and switching to a non-session surface exits safely. | Native functional + visual + usability | Automate state/focus/grid assertions and screenshots; retain native window-control/accessibility review |
| CP-09 | Open About from Launcher, Settings, and a live session at ordinary/narrow sizes; verify exact version/build/OS/architecture copy text, source link, license disclosure, no session/path/host/settings leakage, installation-appropriate update controls, keyboard reachability, and Close/Escape focus restoration. On each platform, quit normally and relaunch to verify the reason. With two isolated test-owned packaged instances, finish one, forcibly terminate only the other, and relaunch: the abandoned run remains unclean/unknown despite the unrelated clean exit; still-live instances are not classified as failed. | Native functional + visual + accessibility | Automated semantic content, redaction, update eligibility, journal transitions, same-process and concurrent owned-child lifetime locks, killed/abrupt child recovery, active retention/log protection, panic preservation, nonblocking hooks, atomic-write failure and unavailable-diagnostics initialization, narrow geometry, and screenshot. Cross-platform child regressions run in CI; they do not launch packaged GUIs. Retain native packaged teardown/relaunch and About presentation, power-loss/native-fault boundaries, link handoff, and screen-reader review |
| CP-10 | Review the Profiles editor (create/update/delete/reorder for local and SSH profiles, including stored-password/private-key and persistence-provider fields) and Settings' bundled terminal font/ligature selection for native keyboard/focus/error-presentation behavior and visual correctness at supported scales. In Local Shell and Local Profile forms, check autocomplete popup placement in narrow/scrolled viewports and single-click Save while search feedback is visible. | Native functional + visual + accessibility | Field validation, command dispatch, and stable Save geometry across pending/results/focus loss are automated; retain native keyboard-traversal, popup placement, and visual review |
| CP-11 | From a signed native package, confirm the platform's `festerm-sessiond` helper is installed beside `festerm`. Start a uniquely named session, detach while output continues, reattach and verify the bounded authoritative snapshot matches a fresh frontend state (including scrollback, modes, and local clear/reset or scrollback-limit effects), then attach a second client and verify the first receives the takeover notice and closes while only the second receives subsequent output. After transport loss, use Reconnect beside Open Diagnostics (or Inspector Resume) and verify the same tab resumes the existing shell without replaying pending input or terminal replies, and that the first post-reattach resize reflects the live window size without losing the detached terminal state. If the daemon is gone, recovery must report failure instead of starting a replacement shell. Verify natural shell exit and `kill` remove only the matching registry record, the daemon survives its launching terminal/application, and stale records are pruned. On Unix verify runtime directories are `0700` and registry/socket/lock files are owner-only. On Windows verify the named pipe rejects another local user's token, the daemon breaks away from the launcher's Job Object, and the trusted ConPTY runtime is selected or safely falls back to inbox ConPTY. With a live 0.2.0 legacy daemon, install the first signed release-versioned package without terminating it; verify install/relaunch succeeds and the old session reattaches to the same shell. Repeat with a daemon from the private release-versioned helper copy; verify a new session uses the new helper identity and superseded copies are removed on a later fesTerm/helper launch after their final daemon exits, including the first launch after reboot. Explicitly incompatible registry protocols and recovery snapshot schemas must remain intact and fail attachment with actionable version guidance. | Native functional + security | The native daemon IPC/input/snapshot-recovery/takeover/kill flow and Unix modes/launcher independence are automated in `native_daemon_survives_launcher_and_supports_input_replay_and_takeover` plus the large-output recovery smoke; helper resolution/staging/pruning, live legacy side-by-side installation, explicit protocol rejection, snapshot-schema rejection, and malformed-snapshot refusal have deterministic/native coverage. Windows package smoke installs the signed v0.2.0 package, starts its legacy daemon, installs the unsigned candidate side-by-side, verifies the old process and registry survive, terminates it through the new helper, and verifies the next helper launch prunes the unlocked legacy image. Signed-candidate reconnect/focus acceptance, Windows launcher independence, cross-user DACL rejection, explicit Job Object breakaway, and natural-exit/stale-prune evidence remain pending |
| CP-12 | Enable Running Sessions. Create unique test-owned native and available local tmux/screen sessions, detach the GUI, then verify provider/metadata/count/order and reattach a fresh challenge to the same shell PID/state. Native attached entries disappear; attached tmux/screen entries remain annotated. Delete/exit/recreate while refreshing; a stale click stays on Launcher with an actionable error and never creates a shell. Confirm empty groups disappear, unrelated sessions remain usable, and a large inventory scrolls/refreshes without stalling the window. | Native functional + usability | `python3 scripts/check_running_sessions.py --batch 8 --cycles 3` automates isolated native and Unix-provider process/state continuity, create/attach/detach/exit/delete/recreate, identity/count/order and stale-click refusal; both optional runners include it. `--batch 32 --cycles 10` increases stress. Missing binaries/native Windows tmux/screen are explicitly skipped, not passed; WSL remains separate Linux evidence. Native GUI interaction/focus and large-inventory feel still require recorded platform evidence; this does not close CP-11 package/security obligations or SSH recovery #49 |
| CP-13 | Open a second window from File ▸ New Window, the command palette, and the keyboard binding. A distinctly titled native OS window must appear on the Launcher, render its own content, and leave the first window's tabs, active tab, focus, and scroll position untouched. With both windows open, commit an interface setting, a profile edit, and a keyboard rebinding in one window and confirm each reaches the other without a restart while its focus, scroll offset, and in-progress text entry survive. Closing a later window confirms only its own live sessions and leaves the application running; closing the first window quits with the usual aggregate confirmation. | Native functional + visual | Automated coverage is headless and stops at the Application/Window model (ADR 0032); native multi-viewport window lifecycle, macOS menu-bar ownership, and window chrome still need recorded platform evidence |
| CP-14 | With two windows open, drag a live session's chip onto the other window's chip row and body, confirming the running shell, its scrollback, and its state survive the move and land where dropped. Drag a chip clear of every window and confirm a new window opens under the pointer owning that tab, sized like the one it left, and that detaching a later window's only tab does nothing. Move a later window's last tab away and confirm that window closes with no confirmation; do the same to the first window and confirm it falls back to the Launcher and keeps the menu bar and quit path. With workspace restore enabled, quit and restart and confirm both windows reopen with their own tabs and positions. Confirm the Launcher, Settings, and Profiles chips cannot be dragged into another window or detached into one. On macOS confirm that dragging a chip in a *later* window drags the chip rather than moving the window itself. On Wayland confirm a cross-window drag degrades to an in-window reorder rather than dropping a tab somewhere unexpected. | Native functional + visual | Automated coverage drives real press/move/release gestures against published window footprints and the Application-level move/detach/collapse and multi-window workspace round trip (ADR 0033); real cross-window pointer capture, native placement of a detached window, and Wayland's geometry refusal still need recorded platform evidence |
| CP-15 | Validate the native text editor against `docs/text-editor-design.md` and ADR 0034. Create two documents through New File and confirm each is a separate blank, dirty `UNTITLED` editor, Auto-save is unavailable, and the first Save opens Save As. Open one file into two views in two windows; type in one and confirm the other shows the text, the unsaved marker, and a single shared undo history. Change the file underneath a clean view and confirm it adopts; repeat dirty and confirm Conflict offers Compare, Reload, Keep my version, and Save As without losing either version. Replace All, then undo with a toolbar button still focused. Narrow the window until the Find verbs collapse and confirm every action is still reachable. Set a fixed column count and confirm by reading the file on disk that no newline was written. Save As onto a new name and onto an already-open file, confirming the saving view follows the new file, other views keep the original, and one file remains one buffer. Exercise every state with a screen reader and a high-contrast theme, confirming shape and words alone carry it. | Native functional + visual + accessibility | New-file identity and first-save routing, document sharing, freshness/conflict transitions, bounds, Save As rebinding, Find behaviour, bounded body-widget history, view-scoped text-widget teardown, and sibling widget-state survival under repeated open/close are automated; retain human review for readability, caret and focus behaviour, screen-reader wording, conflict comprehension under time pressure, and the honesty of disabled-control explanations |

CP-15 undo retention: exercise an owned large multiline file with widget
paste, vi edit and toolbar/colon substitution. A whole change whose undo
record exceeds 8 MiB must leave text, revision, dirty/saved state, undo and
redo intact and visibly state the required bytes, limit and smaller-change
recovery. Find calls this **Change refused**, not **Invalid pattern**. Small
typing remains usable when optional coalescing reaches its budget; no-ops
preserve redo and match counts. New File and terminal snapshots start dirty
without fake undo entries; undoing their real edits does not announce a
never-saved document as saved. Deterministic regressions cover actual
descriptor/string/slot capacity, exact-byte boundaries, churn/clone/clear,
saved/base identity, atomic admission and production refusal routes. Native
keyboard/paste/IME, caret/focus, narrow-window readability, high contrast and
screen-reader delivery remain manual/native/usability evidence. Retained
history bounds do not certify transient peaks, allocator fragmentation or RSS.

CP-15 vi repeat budget: enable vi keys on an owned fixture and exceed 8,192
recorded keys in one Insert/Replace change using typing/Backspace churn. All
accepted edits remain; the command area visibly says **Repeat unavailable**,
states the limit and says editing continues. Finish the change and press `.`;
it must refuse without replaying a prefix or an older change. Complete a
smaller edit and verify **Repeat available again**, working repeat and shared
undo. Long Visual navigation followed by Escape or yank must retain the
previous repeat without warning; a subsequent oversized Visual edit must warn.
Exact limits, bounded key storage, capacity retirement, production-view
warning/refusal/recovery and undo are automated. Real keyboard/paste/IME,
narrow-pane readability, high-contrast presentation and screen-reader delivery
remain native/usability/accessibility evidence, not passed by headless tests.

CP-09 diagnostic boundaries: the About semantic test verifies the warning that
raw local reports/logs can contain sensitive paths or secrets and are not
uploaded. Only the lifecycle support summary is metadata-only; these tests do
not establish comprehensive artifact redaction. Owned-child tests also verify
native-smoke diagnostic routing beside the configured result and refusal to
fall back to the user journal when that result path is missing. Native packaged
teardown and visual/accessibility review remain separate.

The pinned VM adapter exposes `running-session-stress` with an empty payload
and fixed 8-by-3 bounds. It calls the same CP-12 harness without a GUI, so
native Windows ConPTY evidence can run independently of a guest's GPU support.
The guest log records unavailable providers explicitly. Backend success does
not establish native-window, keyboard/focus, or usability qualification.

CP-12 automated evidence, 2026-09-15: macOS 26.6.2 arm64 developer builds
passed the full required CI commands, all four native-daemon smoke tests,
and the final default 8×3 churn run for sessiond, tmux 3.7c and GNU screen
4.00.03. A 16×3 run also passed during implementation. These are automated
native-process/headless results, not signed-package, native-window visual,
Windows, Linux, WSL, or VM acceptance evidence.

The subsequent 32-by-10 host run initially failed on screen's bounded `ps`
query. Instrumentation reproduced a live, zero-output `ps` at its deadline;
sampling identified accumulated detached-client PTY control workers spinning in
`wait4`, not a completed child awaiting an async notification. Shutdown now drains
and discards final client output, and disconnected controllers do not busy-poll.
The churn harness also requires each detached/exited client's reader and control
workers to complete. A controlled drop regression records the former timeout,
and a separate deterministic discovery test makes a real subprocess timeout
visible before coalesced explicit Refresh restores a known inventory. Unexpected
provider errors in foreground churn remain failures, not hidden retries.

After the correction, the same macOS host passed all three providers at
32-by-10 with no command-error diagnostics. Across 4,609 screen identity queries,
`ps` had a 25 ms median and 120 ms maximum; 113 full 33-PID queries had a 40 ms
median and 59 ms maximum. The owned test process sampled after the first tmux
cycle fell from 917% CPU before the fix to 4.2% afterward. The full required CI
commands and four existing isolated native-daemon smoke/churn tests also passed.
This follow-up is host backend/headless evidence only; parent-owned candidate
VM and native GUI qualification are separate.

The macOS provider-PATH regressions distinguish a bare executable found on
inherited PATH from an absolute executable using normal login-environment
correction. A controlled login shell with a deliberately different PATH
reproduced the former required-CI assertion failure; the corrected tests also
check that discovery actually executes with the PATH retained for attachment.
The login-environment probe is interactive as well as login-scoped so paths
configured by the user's interactive startup files, including `~/.local/bin`,
are available to packaged Finder-launched sessions. Its three-second deadline
still bounds startup files that prompt or hang.

The macOS VM backend failure at `59ba3b7` exposed a separate checkout-depth
problem: its generated native socket pathname was 106 bytes, beyond macOS's
103-byte pathname limit. A host reproduction exposed `SUN_LEN` from the daemon;
the old `start` helper had reported only its exit status. Startup now surfaces
the invalid address, byte count and shorter-state-directory guidance.
The harness creates a private `0700` temporary namespace under `/tmp` on Unix
(normal temporary storage on Windows), independent of checkout and macOS's
potentially long `TMPDIR`. Native registry/socket, tmux socket and screen socket
isolation all remain within that owned namespace. Direct native smoke tests
also default to short private roots; explicit overlong test roots fail with
an actionable diagnostic. Success and ordinary runner failure clean owned
resources; incomplete cleanup retains and reports only the owned namespace.
All three providers passed 8-by-3 through a controlled 188-byte checkout path
on the macOS host, and all four native smoke/churn tests passed with their new
default roots. These are host backend results, not a rerun of the failed VM.

Set `FESTERM_DISCOVERY_TIMING=1` when running `check_running_sessions.py` to
record command argv, owned child PIDs, spawn/total elapsed milliseconds and
exit statuses. This opt-in log includes local session names; it does not dump
command output or environment values. Deadline diagnostics include stdout/stderr
byte counts, EOF state and whether exit-wait started. The two-second provider
I/O/exit deadline is unchanged; runtime/spawn overhead is measured separately.

Reviewer-correction evidence on the same macOS host: all six ignored native
daemon tests and default 8-by-3 churn passed. The native additions pin reconnect
to a nondefault registry and generation, reject a replaced generation while its
new client stays usable, and assert socket/lease removal after kill, natural
exit and forced-death pruning before deleting fixture roots. A nonignored
startup-failure regression also checks artifacts directly. Screen 4.00.03
coverage now includes an already-attached session and a delayed/failing new
client through the actual Launcher command, useful failure diagnostics, explicit
Refresh/retry, and fresh same-shell PID/state proof from the recovered client.
That intermediate candidate inspected the server's exact new terminal using
bounded macOS `lsof` or Linux `/proc` descriptors. The Fedora follow-up below
replaced the modern Screen inspection requirement with a public query.
Earlier guest results on `59ba3b7` did not establish coverage for these corrections.
The corrected candidate also passed sustained 32-by-10 host churn for all three
providers (30 provider cycles), including the failing-client/recovery regression,
with no command-error diagnostics and complete owned-namespace cleanup.
Its 4,663 `ps` queries peaked at 70 ms and 337 `lsof` queries at 37 ms; the
multiplexer test completed in 643.26 seconds. The owned test process sampled
during Screen churn used 3.7% CPU rather than accumulating spinning workers.
All required repository checks passed on the host. This remains backend/headless
evidence, not updated guest or native-window qualification.

The subsequent Linux VM at `7321011`, using actual Screen 4.9.1, failed after
successful discovery, client confirmation, Launcher recovery and fresh state
challenges: shutting down the recovered client left no Screen session for the
original client. This was not an initial-list parser or startup-readiness
failure. A retained-terminal PTY regression reproduced the dependency writer's
destructor injecting newline/EOF after client exit. Unix writer teardown now
uses a safe owned descriptor that sends no bytes. The deterministic regression
failed with the old writer and passed with the correction; all 12 PTY tests and
macOS 8-by-3 churn passed, including a new fresh challenge from the surviving
Screen client after the recovered client shuts down. Actual Linux Screen 4.9.1
must be rerun on this corrected candidate; earlier host passes are not that
evidence. Discovery parsing, provider deadlines and Windows writer behavior are
unchanged.

Fedora portability follow-up: normal setgid Screen can be nondumpable, making
`/proc/PID/fd` unsuitable as a required attachment check. Modern Screen now uses
`-Q @echo` from the exact owned terminal, requiring initial terminal context and
frontend PID to agree. It does not fall back to descriptor inspection after query
failure. The old Apple `-Q`-unsupported case retains explicit `lsof` compatibility.
Quiet-query tests reject another display's PID and a missing initial context,
even when fallback returns the expected PID. A timeout regression checks removal
of private query reply sockets while preserving the selected server socket.

Actual source-built GNU Screen 4.9.1 and 5.0.1 on the macOS host passed 8-by-3 churn with
`--deny-screen-process-inspection`, including delayed/failing attachment,
Launcher recovery, no query messages on another display, fresh same-shell state
and last-client detach. This switch injects denied inspection only into the Rust
test binary, not production. Screen's graceful hangup handshake also resolved a
last-client failure reproduced independently with the old inspection path;
ignored hangups receive bounded escalation within the owned process group.
These are host/provider and modeled-permission results, not an actual Fedora
setgid-package or new VM qualification. Run
`python3 scripts/check_running_sessions.py --batch 8 --cycles 3 --deny-screen-process-inspection`
where the installed Screen supports public queries. Default runners retain old
Screen compatibility; the fixed VM adapter requires no registration changes.
Screen 5.0.1 places cursor-control sequences immediately after challenge text,
which exposed a harness assumption that the response occupied an entire raw
line. Explicit end markers now delimit the exact state/PID/challenge response;
a deterministic regression rejects partial or mismatched replies while allowing
the trailing terminal controls. This does not change production discovery or
terminal parsing. Public-query authentication/permission failures stay on Launcher
with diagnostics rather than publishing an unconfirmed client.

CP-12 candidate VM evidence, 2026-09-15: exact source
`9ba4563f5dc0013a3ebbd4074598ea6e46918e02` passed the fixed
`running-session-stress` mode on all three dedicated guests, using reviewed
shared controller `f93d6b0` and adapter `59ba3b7`. Each native run includes the
generation/root reconnect and artifact-cleanup regressions, plus 8-by-3 churn.
Provider-level logs were recovered in addition to the aggregate manifests.

| Guest | Native sessiond | Multiplexer coverage |
| --- | --- | --- |
| Ubuntu Noble ARM64 | Three native tests passed; 8-by-3 churn passed | tmux and GNU Screen 4.9.1 each passed 8-by-3, including failed-client recovery and surviving-shell challenges |
| Windows ARM64 | Three native ConPTY tests passed; 8-by-3 churn passed | tmux and Screen explicitly skipped as non-native providers; this is not WSL or GPU/window qualification |
| macOS ARM64 | Three native tests passed; 8-by-3 churn passed | Apple Screen 4.00.03 passed 8-by-3; tmux was absent and explicitly skipped in this guest, with separate host coverage above |

Private manifest and guest-log run identifiers:

- Linux: `20260915T231031Z-linux-festerm-94cbebde-b855-4918-8356-7a9ac35a3c01`
- Windows: `20260915T231340Z-windows-festerm-a63c8b58-3842-449a-8578-a047aada87d7`
- macOS: `20260915T231546Z-macos-festerm-e2bcb324-24d1-45f6-8e9e-3871719f8360`

Earlier failures remain recorded, rather than being replaced by successful
retries. Missing guest Python and Screen prerequisites were supplied in
user-owned locations with separate task baselines; original snapshots and
privilege policies were preserved. The macOS guest's original Parallels Tools
update reboot interrupted a run after its native cases passed; the successful
run used an explicit post-update baseline. All three guests were stopped after
evidence collection.

These results establish native-process/backend coverage, not signed-package,
Launcher focus/usability, native keyboard delivery, or actual Fedora
setgid-package qualification. The latter remains distinct from the real-provider
modeled-denial tests above. Those remaining checks stay in CP-11/CP-12 and #43.

The saved-profile conversion also opts into graceful Screen shutdown, covering
configured launch, relaunch and workspace restoration rather than only Running
Sessions. The same isolated provider harness now exercises
`StartConfiguredLocalProfile` twice, checks last-client detach and verifies the
original shell PID/state from a subsequent inventory attachment. A configuration
regression asserts that only Screen receives this policy and that working
directories and other providers retain their existing behavior.

CP-11 recovery ordering now also has deterministic saturated
output/control/resize/exit, failed-candidate rollback, maximum-frame boundary,
frontend resize deferral, and CLI acknowledgement/query-suppression coverage.
The owned native daemon suite exercises large snapshots and takeover on the
host platform. Signed-package reconnect/focus, native Windows acceptance, and
visual recovery during rapid resizing remain native evidence, not implied by
these tests. A replacement handshake can pause daemon processing for up to
15 seconds; CLI attach is a text projection rather than full styled rendering.

Windows attachment-backlog checks are now automated with the production
broker and owned local pipes: 16 waiting connections, EOF/refusal on the 17th,
unchanged active bidirectional bytes, admission after consuming a slot, and
queued-handle closure with normal broker exit. Portable regressions cover
1,024 refusal/consume/retry cycles, immediate ownership release, one adoption
per turn under refill, independent bounded listener-failure delivery, and
early-recovery disconnect guidance without adopting a snapshot. A production
GUI regression now proves the guidance is visible in the viewport overlay and
Inspector state message with Diagnostics closed, both Reconnect and Resume
dispatch the same-tab recovery command, and accepting recovery removes stale
guidance while preserving history. Only the known content-free recovery
message is promoted; arbitrary backend details remain in Diagnostics.
CP-11 still requires signed-package reconnect/focus, cross-user isolation and
native usability evidence, including narrow-window readability of the retry
guidance. These checks neither remove the existing 15-second adoption wait
nor prove a global daemon RSS or recovery-snapshot peak budget.

### Rendering performance

Issue #327 and proposed ADR 0044 track the macOS ordinary-row-cache repair.
Its isolated synthetic profiles use no personal shells, remote accounts,
clipboard or secrets. Native qualification requires matched binaries, actual
grid/window identity and geometry, output delivery, foreground/background/idle
and high-mutation CPU samples. Preliminary results are mixed and are not accepted
performance evidence on their own; the completed cleanup disposition follows
below. Matched high-resolution-bundle comparisons while the
owner's macOS display is locked are recorded separately, never as foreground
qualification; this repair does not unlock the screen or restart the live app.
An authorized unlocked 14-case ABBA comparison at source `6f3667a` found
consistent localized/Unicode savings but about 8% higher shaped foreground
full-mutation CPU. Extending the full-mutation bypass to shaping required fresh
native qualification; the adverse receipts remain preserved, not superseded
by a passing source regression.

The exact integrated source `58c5cd684885a9d30d44a9b384d26ff6bd8814dd`
(release-binary SHA256
`0ed4e813b0c2aab915a1f63211307de68ee47c6cff85ed0a021d229299309521`)
has now completed all 56 unlocked runs. Each receipt passed copied-binary,
on-console/foreground/unlocked, stable 1280x860 logical bounds, 2x scale,
150x42 foreground grid and complete 10 Hz producer-delivery checks. Runs used
15 seconds of warmup and fifteen 2-second CPU intervals. Localized/Unicode
cases improved 14-30%; shaped full foreground was 7.863% baseline versus
7.698% candidate (-2.1%), improving in both pairs. Visible-cursor quiet
controls remained about 0.1-0.2% CPU; their relative percentages are not useful
at the process-clock resolution.

Unshaped full foreground was 5.875% versus 6.320% (+7.6%), with paired changes
of +19.1% and -2.1%. These contradictory samples are retained, not averaged
into a claimed gain or accepted as a tradeoff. Eight longer ABBA/BAAB controls
on that exact binary used 30 seconds of warmup, thirty 2-second CPU intervals
and a bounded 1,200-frame producer. All eight receipts passed the same native
and geometry guards, with at least 975 producer frames and p99 intervals below
110 ms. Baseline CPU averaged 5.763% versus 6.466% candidate (+12.2%);
paired changes were +57.2%, +3.1%, +0.8% and +3.2%. The first baseline's
4.124% CPU was lower than later baselines (6.239-6.352%), despite consistent
producer delivery; its CPU intervals oscillated between about 2.5-3% and
5-7.5%. Neither the low first baseline nor the remaining smaller differences
has an established cause. Those controls did not clear native acceptance,
and none of their intervals or runs is discarded.

Local workspace/portable/vendor gates, independent security/reliability/scope
reviews and all 11 integrated-head GitHub checks passed. CI success does not
resolve the remaining native CPU uncertainty.
Issue #334's owner-authorized cleanup removes per-rebuilt-row revision
allocations and unconditional capture bookmarks without changing retention
eligibility or atlas invalidation. Batch/independent-cache/clone identity and
row-position mesh regressions pass; restoring both old policies fails four
focused regressions.

Cleanup source `86e4274bb5ebbbe5cb8423cbb24d0db34688c62c` (release-binary
SHA256 `2e4b5f8bc88d0945cab2c3f685d254c7405e4bc46ee37d0725a0231e068f6a6b`)
completed 52 isolated unlocked runs. Original baseline remains `d67aee3`
with binary SHA256
`60c9adf5b10519409a9f1994ca138094fdcb2bf00832b3c5851fd9797cdfca35`.
The predeclared 36-run plan comprised eight longer unshaped full-foreground
controls against the previous candidate, eight against the original baseline,
and five focused four-run cases. Eight current-source quiet controls and,
after the mixed short shaped-full result, eight longer shaped-full controls
were each predeclared before launch. Candidate-first BAAB/ABBA order reversed
the earlier first-baseline ordering; no result was removed or replaced.
Longer runs used 30-second warmup, thirty 2-second CPU intervals and bounded
1,200-frame producers; focused/quiet runs used 15-second warmup, fifteen
2-second intervals and bounded 600-frame producers.

All 52 receipts passed exact copied-binary, foreground/unlocked/on-console,
input and stable 1280x860 logical bounds/2x scale/150x42 foreground-grid guards.
Both 10 Hz producers spanned measurements, with at least 979 frames in longer
controls and 516 in standard controls; worst per-run p99 interval was 109.93 ms.
No app error/panic/device-loss matches were found. Background output means an
inactive synthetic tab in the foreground owned application, not an inactive app.

| Comparison / workload | Baseline CPU | Cleanup CPU | Relative change / paired changes |
| --- | ---: | ---: | --- |
| Previous candidate, unshaped full foreground (eight longer) | 6.475% | 5.807% | -10.31%; -33.14%, -2.81%, +0.24%, -6.15%; first-run confounded, not a causal speedup |
| Original baseline, unshaped full foreground (eight longer) | 6.433% | 6.404% | -0.46%; +4.26%, -0.02%, -1.31%, -4.46%; approximately neutral, not parity on every run |
| Original baseline, unshaped localized foreground | 5.524% | 4.811% | -12.90%; -20.52%, -3.94% |
| Original baseline, unshaped localized background tab | 5.506% | 4.645% | -15.65%; -16.15%, -15.14% |
| Original baseline, shaped localized foreground | 5.390% | 4.164% | -22.74%; -23.33%, -22.16% |
| Original baseline, shaped full foreground (four standard) | 7.899% | 8.114% | +2.72%; -2.08%, +7.60%; retained mixed result |
| Original baseline, unshaped Unicode foreground | 6.369% | 5.110% | -19.76%; -23.66%, -15.94% |
| Original baseline, unshaped quiet visible cursor | 0.133% | 0.133% | unchanged low absolute CPU; relative percentages not meaningful |
| Original baseline, shaped quiet visible cursor | 0.133% | 0.133% | unchanged low absolute CPU; relative percentages not meaningful |
| Original baseline, shaped full foreground (eight longer) | 7.850% | 8.051% | +2.56%; +2.82%, +1.75%, +2.04%, +3.65%; all pairs adverse |

The direct previous-candidate comparison includes a low first cleanup run
(4.216%); that reproduces low-first-run behavior with the candidate first,
but does not establish its cause. Its aggregate is not a causal 10% cleanup
speedup. Current original-baseline controls show 13-23% savings in the tested
localized/Unicode cases, approximately neutral unshaped heavy redraw, and a
remaining 2.56% shaped-heavy cost (about 0.20 CPU percentage points).

On 2026-10-05 the owner explicitly accepted this documented shaped-heavy
tradeoff for PR #328 and retained #334 for residual cost and first-run variance.
This clears that owner's native-performance decision, not a no-regression
claim, external PR approval, deployment or general platform/latency acceptance.
All 11 checks on measured source `86e4274` completed/SUCCESS after one bounded
retry of jobs that never acquired hosted runners; original infrastructure
failures remain preserved. Subsequent evidence-only heads require their own CI
conclusions and do not change the measured source/executable identity.
The shaped/unshaped static/localized/full-mutation CPU-stage diagnostic is in
both optional runners under `FESTERM_RUN_ROW_CACHE_PROFILE=1`.
Existing CP-16/17/18 and mixed-DPI/native-input/physical
latency gates remain unchanged.

| ID | Workflow and oracle | Evidence class | VM automation candidate |
| --- | --- | --- | --- |
| CP-16 | On a Windows software-rendered desktop, leave a maximized Launcher with unchanged Running Sessions idle, then leave a second maximized window on Launcher with an idle background PowerShell tab that has unread startup output. Confirm low CPU, preserved session discovery, and a visible static ring-and-dot unread cue. On accelerated displays, including macOS, confirm the same static cue regardless of focus and immediate clearing on tab activation. Confirm Settings has no unread-pulse control. Compare chrome, text, and window geometry before/after at ordinary and high DPI. | Native performance + visual + usability | `scripts/check-windows-idle-rendering.ps1` (also in the opt-in Windows suite) measures both cases with isolated configurations and a default 5% total-machine CPU ceiling; `-RequireSoftwareRenderer` rejects hardware-only evidence. Deterministic tests assert no animation repaint scheduling, fixed color/geometry across time and focus, activation clearing, and retired-setting migration. Native readability, mixed-DPI appearance, and accelerated-device behavior still require visual review. Sustained-output rendering cost is covered separately by CP-17, not this idle budget. |
| CP-17 | On a Windows software-rendered desktop, run the controlled 10 Hz foreground-output fixture in a maximized window without interacting with it. Confirm reduced CPU while output keeps updating and the terminal background, text, cursor, clipping, overlays and window transparency remain correct. Repeat moving between DPI scales and opening another window. | Native performance + visual | `check-windows-idle-rendering.ps1 -IncludeSustainedOutput -RequireSoftwareRenderer` measures CPU and GUI frame construction (defaults: at most 30% total CPU and at least 5 GUI frames/s). Headless GPU readback compares native and ordinary painting pixel-for-pixel, with a visible colored-text oracle, at 100%, 125% and 200% scale, clipped/unclipped and enabled/disabled, including a translucent overlay. sRGB/other formats retain the ordinary path. Native multi-window/mixed-DPI interaction and dense-output workloads remain additional evidence; the frame counter is not an OS presentation-latency measurement. |

The #242 hot-stack follow-up identified WARP pixel-rasterization work continuing
after GUI construction stopped. The two large Launcher panels now have a
textureless fill path on Windows DX12 CPU adapters, using the same rounded
geometry, clipping and dithering. Full pixel comparisons and backend/fallback
policy tests cover this path; the opt-in `replay_large_warp_panels` test
reproduces its draw cost without desktop input. Native measurements and their
limits are in `validation/windows-warp/README.md`. This does not replace CP-16's
accelerated-device, mixed-DPI or usability review.

The same path now covers bordered Settings/Profiles panels, connection forms,
and running-session groups. `replay_warp_ui_surfaces` renders the real
application UI at 3548 x 2150 pixels, separately times UI construction and
completed drawing/readback, and can compare every pixel against a preserved
baseline. `textureless_bordered_panels_match_pixels_across_dpi` covers border
geometry in normal CI. Native Settings scrolling, profile interaction, window
drag/resize responsiveness, and Windows Terminal comparison remain additional
evidence: an offscreen replay does not establish their latency or parity.

The bounded Inspector/SFTP pane-and-table coverage adds complete widget
pixel comparisons and an opt-in completed-render replay. Windows correctness
and all 20 paired framebuffer comparisons passed; matched draw/readback
improved while UI construction increased slightly in absolute cost. Raw
repeats, adverse control samples and limits are in
`validation/windows-warp/README.md`. This changes no native/manual
classification: TI-06 still owns native Inspector focus/selection behavior,
FD-05/FD-06 still own OS drag/reveal, and CP-16's mixed-DPI, accelerated-device
and usability evidence remains open. Transfer/collision painting and styling
are not part of this change. The offscreen same-executable renderer
differential is not shipping before/after or native presentation evidence.
The bounded non-terminal expansion adds production-widget menu/About/safety/
picker fixtures to those same opt-in probes and gallery, not to the native CPU
oracle. Its [reconciled matrix](../validation/windows-warp/surface-matrix.json)
retains 33 families, all audited state groups and named native prerequisites.
The initial scaffolding awaits exclusive validation; no new headless evidence
has been accepted. Even completed offscreen reports cannot qualify About/menu
idle CPU, input-to-display, native clipboard/link handoff, packaged updates,
mixed-DPI or platform menus. CP-16/17 budgets and all native/manual checks remain
unchanged; no new latency budgets are implied.

`TYPE-01`/`TERM-01` glyph-layout retirement has automated survivor, hot-set
churn, bounded slot, forced-collision, owner-drop and reset checks, alongside
existing both-platform rendering baselines. The 4,096-entry limit is unchanged;
tracking adds bounded metadata and avoids wholesale invalidation, not a
lower-retained-RAM guarantee. No new GUI/native workflow is introduced. NP-05
appearance/usability and CP-18 native resource/latency obligations remain open.

### Experimental Direct2D qualification

Native quad preparation has a device-free deterministic allocation control in
`crates/festerm-windows-direct2d/native/quad_tests.cpp`. It compares the actual
production predicate and complete prepared operations with the frozen former
predicate over cold/warm/mutation/refusal, exhaustive topology and dense glyph
cases. Two heap allocations per valid quad become zero, with at most two
borrowed stack pointers and no retained allocation. Windows CI's existing
Direct2D self-test includes this control; `-QuadSelfTestOnly` runs it without GPU
work, timing or Python image dependencies. Exact native framebuffer tests remain
the separate automated pixel/lifetime oracle. This allocation evidence does not
advance CPU, native-window, sustained-resource or latency acceptance and does
not attribute #297; a baseline/candidate coordinator slot must include actual
capture/preparation as described in `validation/direct2d/README.md`.

Issue #298's font-atlas copy mechanism has deterministic coverage for trusted
revision invalidation, same-frame glyph pixels, DPI/font changes, unchanged
snapshot identity, preserved ordinary font deltas, 64 MiB retention and painter
teardown. Native upload/retained-frame tests cover equal-Arc adoption and changed
alpha pixels without changing older surfaces. The opt-in
`profile_native_font_atlas_capture` compares cache-disabled and cached capture
at identical 20 Hz cadence with small/grown atlases, actual native frames,
capture bytes/time, uploads and process CPU. It is offscreen mechanism evidence,
not a native-window comparison, sustained resource acceptance, physical latency
or attribution of #297. CP-18 and those remaining native/manual checks stay open;
see ADR 0043's accepted snapshot ownership/vendoring contract and
`validation/direct2d/README.md`; that disposition accepts no native
performance/resource/presentation gate.
The 2026-10-03 source-bound eight-case offscreen control passed copy/upload,
pixel and unchanged-cadence checks, but failed the external quiet-host guard.
Only its deterministic mechanism evidence is recorded; process-CPU improvement
remains unqualified pending an exclusively quiet host.

**Window-identity correction (#242):** The Windows CPU and OS-input probes
share `scripts/windows-application-window.ps1`. A native Win32 regression
(`test_windows_application_window.py`, run by Windows CI) covers visible helper,
hidden, owned, wrong-PID, stale and ambiguous windows without desktop input.
CP-16/17/18 CPU samples require the actual application HWND to remain
foreground, responsive and maximized at unchanged physical size/DPI.
Desktop input during the fixed warmup or measurement is recorded as
`invalid-input`, not a pass or a confirmed CPU regression. JSON includes
window identity and approximately one-second CPU/GUI-frame intervals.
`-DebuggerPath <path-to-cdb.exe>` optionally captures thread times and stacks
after a budget failure, before cleanup; a post-sample capture may already be
cold and must not be described as a hot stack. No warmup or CPU/FPS budget
has been relaxed. Forced cleanup after a pending close is reported explicitly.

Earlier native results did not record this window identity and are not
complete maximized-window qualification. Fresh observations are recorded in
`validation/direct2d/README.md`. A verified-window 10.638% background-idle
failure was unresolved at the probe-correction stage; correcting the probe
alone was not a rendering fix. The later hot-stack investigation and Launcher
rasterization mitigation are documented separately in
`validation/windows-warp/README.md`, without retroactively classifying that
older sample.

**Active TUI versus quiet terminal:** `replay_terminal_tui_workloads` adds real
Copilot/Vim/htop/tmux captures and four shared synthetic workloads with actual
completed rendering and the existing two-level per-channel native pixel
tolerance. Repainting the quiet fixture in this offscreen test is deliberately
forced; it is not an idle-CPU observation. See
`validation/terminal-performance/README.md` for opt-in execution.
The separate native comparison uses two isolated applications, temporarily
registers the bundled font with explicit consent, verifies Windows Terminal's
reported face and both 120x40 PTYs, and requires quiet warmup/sample intervals.
Unlike the maximized-window budget probe above, these windows are sized to a
matched grid; their different physical client/chrome dimensions are recorded.
Producer write completion and GUI frame counts do not prove displayed updates
or presentation latency. Native image/interaction review, mixed DPI, device
recovery, and issue #263's dragging investigation remain separate evidence.

Retained native rendering adds automated multi-frame pixel comparisons across
100%, 125% and 200% scaling, fractional clipping, erase/recolor/underline,
cursor, emoji, disabled-painter transitions and overlays. Native texture tests
also preserve older frame pixels after changed/erased regions and reject invalid
geometry before reuse. The optional TUI replay requires late localized updates
to redraw less than one quarter of the terminal surface. These checks are not
native CPU or presentation-latency measurements. Use the native driver's
`-FesTermOnly` mode for matched baseline/candidate binaries without starting
Windows Terminal or registering fonts; JSON records CPU capacity, memory
snapshots and whether geometry setup required a forced redraw.

Narrow-damage coverage additionally compares native partial/full pixels exactly
at 100%, 125%, 150% and 200%, including resampled glyphs, feathered/translucent
overlap, erasure, clipping, texture replacement, resize and immutable older
frames. Original-frame geometry limits and aggregate padded-raster fallback
remain enforced. A preparation-count regression covers 4, 8 and 31 separated
changes in a full-frame-clipped mesh at 1024- and 4096-pixel heights: each
retained update performs exactly one native geometry preparation, replaces a
small area, matches a complete native render exactly and preserves older
published pixels. This bounds preparation work independently of scratch area;
it does not eliminate per-patch drawing or final composition.
The [narrow-damage campaign](../validation/terminal-performance/README.md#narrow-damage-and-native-draw-culling)
initially completed a guarded 16-sample native ABBA sequence: localized mean CPU
fell 32.1%, with unchanged producer bytes/cadence; streaming/full-redraw remained
variable and have no claimed improvement. A fresh Windows Terminal comparison
completed all four workloads and the requested full-repaint control, but active
parity remains far off. All fesTerm workload windows still required forced
cleanup. Damage counters describe pixels replaced in the result, not temporary
raster padding or presentation. This evidence does not close CP-18, qualify
hardware/monitor transitions, establish physical latency or fix #263 dragging.

The optional `profile_terminal_residual_cpu` measures process-wide CPU with a
persistent offscreen target and completed submissions, excluding screenshot
readback from timing. It can include the real app chrome around a deterministic
fake session, but does not measure ConPTY or native presentation. Its solid-fill
omission and nearest-sampler cases are explicitly diagnostic. The probe also
isolates image copying, native solid patches, UI capture and unchanged
retained validation. `FESTERM_TUI_PROFILE_CASES` selects named cases with strict
validation. `FESTERM_TUI_PROFILE_COPY=1` adds a BGRA direct-copy compositor
experiment that requires exact initial/final pixels and owns the target outside
the production callback. The same-format shader is its control; cadence and
CPU-ms/frame must be reviewed, not CPU percentage alone. These additions do not
qualify native presentation or change the production renderer.
`FESTERM_TUI_PROFILE_SCENE=application-palette` opens and asserts the real
command palette over that synthetic terminal, retaining the original
application/terminal controls and reporting ordinary mesh bounds and solid-fill
triangle counts. This is optional offscreen attribution; removal of overlay
fills is still a pixel-changing diagnostic, not an accepted rendering change.
Native palette appearance, focus, input, presentation and CP-18 remain manual
or separately qualified evidence.
The palette's graphics-only textureless frame/shadow and opaque untextured
search/selection-rectangle paths have automated exact
RGBA comparisons for dark/light/short/query states at 100%, 125%, 150% and
200%, plus root opacity, nonzero origin, palette transform, secondary viewport
and translucent-frame fallback. Candidate geometry, invisible/root-transform
guards, current-renderer reinstallation and absent-renderer fallback are also
deterministic. Existing palette focus/navigation/dispatch tests remain separate
from graphics. `FESTERM_TUI_PROFILE_PALETTE_FRAME=0|1` is an opt-in test-only
same-source comparison; unset uses the production path. Automatic copy and
retention must still decline the overlay. Offscreen savings do not replace
native visual/input, sustained-resource, graphics-recovery, mixed-monitor or
physical presentation-latency evidence.
The rectangle extension additionally rejects textured/translucent candidates
and requires the same eligible opaque window frame. A same-source release BAAB
control preserves all four initial captures and each case's final pixels across
1,200 completed measured frames while reducing CPU work by roughly 49-52%. A separate
guarded native shipping/candidate/candidate/shipping series observed about 42%
less app CPU, with all output, overlay-ineligibility and ordinary-cleanup guards
passed. One candidate had lower GUI frame rate than its preceding baseline;
all four scores remain explicit. See the
[complete records](../validation/terminal-performance/README.md#opaque-palette-rectangle-follow-up).
These are shared-host efficiency observations, not physical display FPS,
latency, a resource budget, hardware qualification or complete CP-18 acceptance.

The separately guarded 2026-10-04 release BAAB comparison adds actual native
palette-open efficiency observations: approximately 44% less app CPU and 2.49x
constructed GUI frame rate on this Windows CPU adapter, with all four strict
desktop/input/foreground/geometry/output/fallback and ordinary-cleanup guards
passed. Source/binary identities, ordered output, host load and external final
captures are preserved in the
[complete record](../validation/terminal-performance/README.md#2026-10-04-guarded-native-palette-comparison).
This is one shared-host series, not physical presentation FPS, interaction
latency, sustained-resource acceptance or complete CP-18 qualification.
The fresh post-chrome native pair still measures 8.602% versus 0.446% localized
CPU; its campaign stopped at a later foreground-activation failure. Streaming,
full-redraw, force-full-repaint control and repeated parity evidence remain
incomplete. The measured renderer-host boundary and rejected experiments are
recorded in the terminal-performance README and #267; CP-18 remains open.
The fesTerm samples required forced cleanup after the graceful-close timeout,
so sample validity does not qualify shutdown behavior.
The production chrome/status-bar fill optimization has exact framebuffer
comparisons at 100%, 125% and 200%, including fractional clipping and translucent fallback. Native
before/after qualification still uses the isolated `-FesTermOnly` driver and a
fresh quiet-desktop interval; no installed application is changed by these tests.

**Owner-approved automatic WARP rollout (2026-10-03):** the supported Windows
x64 DX12 CPU/BGRA gamma, compatible opaque-root pipeline uses host-copy and
retention without on/off settings. All three former environment switches are
ignored. Policy matrices and a real Windows installation regression cover
automatic eligibility, hardware/platform/backend/format exclusions and
copy/retention activation. Target/lifecycle/error fallbacks remain required.
The owner accepts repeatable material process-CPU savings despite recorded
shared-host noise; exact percentages and broad native/resource/latency claims
remain unqualified follow-ups in #282/CP-18. The historical receipts below
are preserved, not relabeled as new-binary or default-smoke results. Native
A/B/C runs require their pinned historical driver/binary; current native probes
measure the automatic route and reject removed controls before desktop access.

**Historical default-off host-copy prototype (ADR-0040):** with owner authorization,
`FESTERM_EXPERIMENTAL_HOST_COPY=1` adds a final-target copy on the eligible
Windows x64 WARP/BGRA path only. Unset/`0` and all unsupported app routes retain
existing behavior. Deterministic exact-pixel coverage exercises DPI changes,
resize, fractional clipping, overlays, opacity fallback, screenshot-target
usage transitions, and older immutable callbacks after subsequent frames;
MSAA/depth and incompatible targets are rejected.
The real-app off/on plus reversed localized campaign passed desktop guards
and verified actual copy counters: localized mean CPU 8.448% to 5.158%,
streaming 5.246% to 3.520%, full redraw 11.805% to 9.152%, quiet counter-rounded
to zero. Producer completion and frame cadence were retained, but individual
displayed frames and latency were not measured. An initial input-contaminated
attempt was rejected; every accepted CPU sample required forced cleanup.
See the terminal-performance README for hashes, memory snapshots and all
result ranges. This does not close CP-18, approve the vendored architecture,
or qualify mixed-monitor DPI, native screenshot/overlay presentation,
multiwindow/transparent surfaces, device recovery, hardware-negative routing,
latency or graceful shutdown. That historical qualification planned both
host-copy settings and verified fallback rather than assuming a request executed.

The final prototype's isolated native-window self-smoke also passed with
host copying off and on: observed focus, four resize generations, real PTY
output continuity and CSI 6n recognition. Enabled logs contain actual copies
at changed sizes; disabled logs contain none. Both self-smoke processes exited
normally, unlike the separate CPU workload windows. This adds native
focus/resize evidence, not native screenshot, independent OS-input or general
shutdown acceptance.

**Historical default-off retained-prefix prototype (ADR-0041):** the owner separately
authorized retaining unchanged preceding window/chrome pixels. Its old controls enabled both
`FESTERM_EXPERIMENTAL_HOST_COPY=1` and
`FESTERM_EXPERIMENTAL_RETAINED_COMPOSITION=1`; isolated prefix experiments compare
against host-copy alone, while #282's native default-on qualification must also
compare the shipping shader path. The cache owns one immutable image up to
64 MiB and up to 1 MiB of exact signatures. Pixel regressions cover panel input
changes, DPI, terminal movement, clear color, clipping, overlays, disabled
painting, capture targets, full/partial texture changes, removal, renderer
replacement, actual managed-texture exports, external bindings and unkeyed
callback side effects. Additional regressions submit an older queued copy only
after rebuilding and destroying the cache, require ordinary pixels and recovery
after an oversized signature, and count both callback preparation phases on
cache hits. Separate pure tests cover exact bounds and identity keys.
The cache-owned limits exclude temporary capture/rebuild allocations and
images retained by recorded or in-flight GPU work.

The optional completed-work probe rejects missing reuse and mismatched initial
or final pixels. The native comparison driver records reused and rebuilt frame
rates and rejects active samples with no actual reuse. These safeguards do not
replace remaining native visual/lifecycle checks or qualify mixed-monitor,
transparent/secondary-window, recovery, memory growth, physical latency or
hardware-negative-routing behavior. Default enablement and architectural
approval remain separate from this prototype's implementation and measurements.
Separate offscreen ABBA/BAAB series on `8721b0b` and `d868509` completed with
exact pixels, bounded current cache resources, actual reuse and approximately
10 Hz cadence; source hashes, variable controls and results remain separately
documented in `validation/terminal-performance/README.md`. The later series
predates #280's syntax/fenced-loading merge. Native qualification is still incomplete:
an initial foreground failure produced no sample, then a separately authorized
attempt completed four off-mode workloads before desktop input invalidated the
first on-mode quiet sample. No matched native active-workload comparison exists,
no native improvement is claimed, and neither failure was retried automatically.

On publication commit `55db377`, both retained-prefix off/on native-window
self-smokes subsequently passed focus, four resize generations, PTY continuity
and CSI 6n, and exited normally. Enabled logs prove 93 prefix reuses and 12
rebuilds; disabled logs contain none. A newly authorized native CPU series
again completed only four off-mode controls before input invalidated on/quiet,
so no matched active comparison exists. Its five CPU windows needed forced
cleanup; this does not replace the normal-exit self-smoke evidence or qualify
general shutdown. All attempts remain separately recorded. The reusable
offscreen ABBA/BAAB runner and artifact validator are checked in, with synthetic
CI coverage rather than flaky timing assertions. Explicit default-on gates for
both copy experiments are tracked in
[#282](https://github.com/fes/fesTerm/issues/282).

The current-source Windows qualification driver adds a predeclared balanced
shipping/host-copy/combined series, a changing-title control and an opt-in
palette fallback control, with portable saved-evidence rejection tests.
Per-second process resources and external owned-client captures complement
the existing application counters; neither proves total in-flight memory or
displayed-frame cadence. The release-capable OS-input driver can independently
exercise maximize/minimize/exact restore without rebuilding during a run.
Restoring a minimized maximized window first recovers its maximized geometry,
then a separate restore recovers the original normal geometry. Activation and
every native CPU interval require a still-active, unlocked desktop; a
disconnected RDP session remains unavailable rather than a background pass.
CPU-window cleanup explicitly answers the ordinary primary Quit confirmation
and verifies the captured child tree, rather than counting a forced kill as a
successful shutdown. These are automation seams, not a declaration that CP-18
or #282 has passed. Mixed-DPI hardware, other architectures/GPUs, genuine device
loss, complete all-view interactions, independent latency, resource budgets and
architectural/default-on approvals retain their native/manual boundaries.

The 2026-09-30 final-source `5f2d601` qualification completed separate offscreen
ABBA/BAAB, saved-evidence, release routing/lifetime, ConPTY resize/shutdown and
controlled TUI/Direct2D replay checks. External initial/maximized WARP captures
are partial evidence: restore/focus failed, and the subsequently authorized
native A/B/C series stopped before launch on WTSDisconnected. No native CPU
comparison or independent latency result exists. CPU-fixture Quit confirmation
still needs native verification; the inhibitor was released. The
[sanitized report and raw evidence](https://github.com/fswiderski/fesTerm/releases/tag/qualification-x64-20260930-5f2d601)
preserve these failures and leave every compound #282 gate open.

On 2026-10-02, connected Windows DevBox functional input runs passed the
maximize/minimize/exact-restore sequence in shipping A (`0`,`0`) and requested
combined C (`1`,`1`), with real PTY acknowledgment, four resize generations,
eight reviewed owned-client captures and normal descendant-tree exit.
The OS-input fixture now uses its ordinary mouse-input path at a bounded,
physical-DPI, process/root-verified visible terminal point after restoration;
fully occluded points are refused. This addresses the background driver's
foreground assumption without changing the application's focus behavior or the
shared no-input CPU helper. Evidence is limited to that connected host at 200%
scale: mixed-DPI transitions, cache eligibility/native CPU comparison, the
CPU-fixture Quit path and the broader CP-18/#282 matrix remain unqualified.

The subsequent first native A quiet interval passed its strict interval guards
but failed normal Quit cleanup: the unique owned UIA invocation focused the
button without activating it. The failed series is retained, not a native
performance result. An untimed observer and two failing headless accessibility
tests established that terminal modal blackout removed accessibility requests
before foreground confirmation widgets rendered. The corrected view defers
those requests until after its own controls, preserving terminal blackout and
the ordinary confirmation policy. A clean committed `537bbcc` release then
passed a shipping-A (`0`,`0`) untimed native proof on the same connected 200%
DevBox: one owned UIA invocation was followed by exit code 0 in under one second,
without a second close request or forced termination. Both actual before-close
and confirmation captures were reviewed, and the app and two captured descendant
PIDs were independently absent. This qualifies that local-process Quit route,
not the full AS-08 platform/transport matrix or a balanced native CPU comparison.

The later clean `483ae06` / unchanged `AF411D78...` release completed all 60
balanced native A/B/C cases on that connected 192-DPI Windows x64 WARP host.
Both saved-checker invocations passed; 60 captures were covered by 14 reviewed
full-resolution unique images with exact SHA256 duplicate links. All 61 apps,
including the first failed palette control, exited normally and all 183 old
app/child identities were independently absent. The palette control stopped
before sampling because its case-sensitive UIA query used **Command palette**
instead of the actual **Command Palette** title; an untimed owned-window tree
and actual capture proved the mismatch. The corrected query has a portable
regression. Its fresh replacement completed A but stopped in B on new input
with foreground/geometry unchanged; the incomplete 2/12 control was rejected
and preserved, with both process trees normally exited and independently
absent. A later separately authorized fresh `fd9dd14` / unchanged application
control completed all 12 palette cases with strict guards and both saved
validations passing. All 12 owned trees exited normally and 36 old identities
were independently absent. Twelve captures were reviewed through six
full-resolution unique images and exact duplicate hashes. B/C host-copy frames
and C retained reuse/rebuild frames stayed zero while the palette was open,
qualifying that single-host ineligibility fallback. The high ordinary-
composition CPU cost is retained; producer completion/captures do not establish
settled presentation or displayed-frame cadence. See the
[single-host record](../validation/terminal-performance/README.md#2026-10-02-connected-single-host-balanced-evidence)
for raw-series boundaries and adverse results. CP-18/#282 remain open for
broader equipment, recovery, resources, latency and maintainer decisions.

**Unsupported-frame recovery (2026-10-02):** deterministic native integration
covers palette-budget refusal with unchanged ordinary-frame pixels, followed
by executed native painting when supported terminal content returns. Typed
SDK/Rust classification distinguishes a refusal from a real native failure
even when both use the same HRESULT; device/allocation/submission failures
still retire the painter. Retained native pixels are invalidated on refusal.
Strict CPU probes reject any refusal instead of accepting mixed paths.
Repeated native-window transitions and sustained unsupported-content resource
and CPU behavior remain CP-18 work, as do real device-loss recovery and
issue #297's degraded-process attribution. No broader acceptance is advanced.

| ID | Workflow and oracle | Evidence class | VM automation candidate |
| --- | --- | --- | --- |
| CP-18 | On supported Windows x64 WARP, verify automatic root-terminal/native copy/retention selection without settings. Exercise sparse/dense/scrolling/colored output, resize, selection/copy, cursor, emoji, overlays, mixed-DPI monitor transitions, secondary/transparent windows, initialization/render failure and device recovery. Confirm unsupported paths keep ordinary pixels, no blank/stale frame appears, all retired variable values are ignored, intrinsic fallback works, and hardware rendering is unchanged. Historical off/on comparisons require pinned historical binaries/drivers; only offscreen test executables expose feature controls. | Automated framebuffer + native performance + manual interaction; broader qualification remains open | Integrated GPU comparisons cover 100%, 125%, 200%, clipping, opacity and palette-budget fallback. `validation/direct2d/run.ps1` provides isolated replay through `FESTERM_RUN_DIRECT2D_PROBE=1`. On a known eligible WARP host, use `check-windows-idle-rendering.ps1 -IncludeSustainedOutput -DenseOutput -RequireSoftwareRenderer -RequireDirect2D`; actual native frames are required and CPU/FPS budgets remain unchanged. The aggregate optional runner includes sustained output without a retired-variable gate; dense/native-required checks are explicit CLI requests. The current native comparison driver builds its clean checkout and measures only automatic mode; A/B/C fails before desktop access. Owner-approved ADRs 0039/0040/0041 accept bounded rollout, not all CP-18 qualification. #244/#282 retain mixed-DPI, multi-window, device-loss, physical latency, sustained resources and representative-hardware follow-ups; #242 retains idle failures. |

The separately opt-in six-session aging probe under CP-18 automates a bounded,
owned fresh/churned/rebuilt GUI comparison, exact normalized pixels, native
damage, atlas/prefix reuse, completed CPU and sampled private bytes/working
set/handles/threads. Only idle demand is event-driven; active/frozen/background
frames are forced and paced. Public wgpu ID/vacant-slot snapshots and separately
held renderer/context teardown windows add bounded retirement discriminators,
not complete native/GPU allocation accounting or an approved resource budget.
This replaces no degraded-user-process capture:
multi-day behavior, native presentation, real persistent-shell reconnect,
WARP thread-stack attribution and complete GPU resource accounting remain
manual/native evidence under #297/#282. It never operates installed sessions.
See `validation/terminal-performance/README.md#bounded-six-session-aging`.
The 2026-10-04 source-pinned 120/400-cycle runs passed all 24 phases and exact
normalized pixels, without reproducing the multi-day plateau. Moderate process
memory growth remains an observation requiring separate attribution; no
native/multi-day acceptance row is closed.
The subsequent optimized 400/1,200-cycle matrices added all 23 public registry
reports and four independently clipped held-teardown windows each. Counts stayed
flat through churn; full fixture/context destruction cleared public IDs and most
private memory, while renderer-only destruction did not. Early release spikes
and remaining workers are recorded. This is partial ownership/lifetime evidence,
not complete GPU retirement, a leak finding or a resource-budget acceptance.

### Desktop Markdown file associations

| ID | Scenario | Evidence class | Current evidence / remaining acceptance |
| --- | --- | --- | --- |
| CP-19 | Install the direct-distribution macOS app, current-user Windows NSIS package, or Linux Debian/integrated AppImage. Choose fesTerm in Open With for `.md` and `.markdown`, cold and warm, including multiple files, spaces/Unicode, a blocking dialog, an existing dirty document in another window, and a missing file. Confirm document tabs, no terminal input/upload, unchanged default handler, and clean upgrade/uninstall association behavior. | Automated routing/packaging + native lifecycle + usability | Deterministic application, ingress and packaging coverage accompanies the implementation. Native installed-package menu discovery, OS foreground/focus restrictions, upgrade/uninstall and AppImage desktop integration remain qualification work; no Store/mobile document-access claim. |

On 2026-09-28, an isolated macOS bundle with private configuration/runtime paths
received a cold LaunchServices `.md` open, a warm `.markdown` open, and a real
secondary-process CLI open in the same responsive frontend. The secondary
exited without creating its configuration file. The temporary process and
LaunchServices registration were removed afterward. The committed main-thread
AppleEvent harness separately covers the AppKit launch/handler-replacement
interval, cold buffering, warm backpressure ordering and non-file rejection.
Native package smoke asserts generated macOS/Linux metadata and Windows
installed/upgrade/uninstall registration while preserving a sentinel default.
This is not a claim about Finder menu presentation or Windows/Linux foreground
behavior, and does not close CP-19.

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
- `m6-evidence-collection.md` and `scripts/collect-m6-evidence.{sh,ps1}` run
  every scriptable M6 suite on a real laptop and bundle the results.
- `m6-manual-evidence-instructions.md` is the step-by-step protocol for the
  M6 reference-application, `vttest`, and usability evidence that has no
  automated oracle.
- `vm-evidence-framework.md` defines controlled cross-platform execution and
  evidence handling.
- `gui-design.md` defines intended behavior and identifies usability
  hypotheses; this registry says where their validation is tracked.
