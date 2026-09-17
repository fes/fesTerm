# ADR 0032: Single-Process Multi-Window via egui Viewports

- **Status:** Accepted
- **Date:** 2026-09-16
- **Supersedes:** None

## Context

fesTerm is strictly single-window today: `app/festerm/src/main.rs` builds one
`ViewportBuilder` and makes a single `eframe::run_native` call, and
`FesTermApp` owns `Configuration` by value alongside `configuration_status`
and a `ConfigurationReloader`. Because there is exactly one of these, "the
active configuration" and "this window's configuration" are the same object
by construction.

Issue #119 asks for multiple OS windows *and* for interface settings,
profiles, and keyboard bindings changed in one window to apply to every other
window immediately. Settings and profile edits autosave on every change (ADR
0015); there is no explicit Save step whose completion could serve as an
external propagation trigger, and the workspace deliberately contains no file
watcher.

ADR 0014 already fixes the in-memory model: `Application -> Window ->
Workspace view -> Tabs -> Session`, where Application owns cross-window
policy and each Window owns "its open tab order, focused tab, window
presentation state, and window-scoped UI preferences". That model does not
need redesigning. The gap is that `FesTermApp` collapses Application and
Window into a single type.

The remaining decision is the process model, because everything else follows
from it.

## Decision

### 1. One process, one `eframe` application, N egui viewports

fesTerm renders additional windows as additional egui viewports inside the
single existing process, not as additional processes.

Concretely, a new `FesTermApplication` type implements `eframe::App` and owns
an ordered list of `FesTermApp` windows. Window 0 is the **primary** window
and renders into `ViewportId::ROOT`; every additional window is rendered from
the primary window's pass via
`egui::Context::show_viewport_immediate`.

Immediate viewports are chosen over `show_viewport_deferred` for a specific
reason: the deferred callback must be `Fn + Send + Sync + 'static`, so it
cannot borrow window state mutably. A fesTerm window owns live PTYs, SSH
transports, SFTP clients, and `egui` texture handles, none of which are
`Send + Sync`, and all of which must be mutated during the window's own pass.
An immediate viewport takes `FnOnce(&Context, ViewportClass)`, which can
borrow `&mut FesTermApp` directly. The cost is that all windows repaint in
one serialized pass on the main thread, which matches how fesTerm already
pumps every session's transport for every frame.

### 2. Propagation is an in-process broadcast, not a configuration reload

**This does not reverse ADR 0015, and the two must not be conflated.**

ADR 0015 decided that fesTerm does not watch configuration files and that
"once loaded, the active configuration only changes through fesTerm's own
writes." A sibling window's Settings edit *is* one of fesTerm's own writes.
Telling the rest of the process what this process just deliberately wrote is
a broadcast of an already-committed in-memory value; it is not a re-read of
the file, and it cannot pick up a third party's edit. No file watcher is
introduced, and `ConfigurationReloader` keeps its existing
explicit-transaction contract.

Every successful configuration write already funnels through one choke point,
`FesTermApp::apply_configuration_save`, which replaces the window's own
configuration only after the atomic file write succeeds. That function
additionally records the committed document as a pending broadcast. After
each window's pass, `FesTermApplication` drains any pending broadcast and
hands the same document to every *other* window via
`AppState::replace_configuration`.

Three properties follow, and are the reason this shape was chosen over
sharing one `Rc<RefCell<Configuration>>`:

- **Commit-only-on-success is preserved.** A failed save broadcasts nothing,
  so a sibling can never adopt a document that is not on disk.
- **Window-scoped state is untouched.** `replace_configuration` swaps the
  immutable document consulted for future Launcher choices and preference
  reads. It does not walk tabs, focus, scroll offsets, selections, or
  in-progress text entry, so a sibling's settings change cannot disturb them.
- **There is no last-writer-wins clobbering.** All windows are in one
  process, edits are serialized on the main thread, and each write is built
  from the document the writing window currently holds — which the broadcast
  keeps current.

Conflicting concurrent edits to the same profile therefore resolve as
last-edit-wins *within a single serialized process*, which is the same
behaviour two tabs in one window already have.

### 3. Application-scoped versus window-scoped ownership

Answering issue #119's open questions for this increment:

| Concern | Scope | Rationale |
| --- | --- | --- |
| `Configuration`, `ConfigurationReloader`, configuration status | Logically Application; physically replicated per window and kept coherent by broadcast | Avoids rewriting ~9,000 lines of `&self.state.configuration()` reads through an `Rc<RefCell<_>>` for no behavioural gain |
| Secret store handle | Application | Already an `Arc<dyn SecretStore>`; sharing avoids repeated keychain prompts per window |
| sessiond connection | Application | Per ADR 0014, a transport must not become a global mutable singleton, but the *daemon connection* is a host service; one connection avoids contention on the registry lock |
| Native macOS menu, wake monitor, traffic-light chrome | Primary window only | The menu bar is per-application on macOS; the wake signal is per-machine and the primary window's liveness pass already covers every session in the process |
| Workspace persistence | Primary window only, for now | The persisted schema is a single tab list; generalising it to N windows is deferred (see Consequences) |
| Tab order, focused tab, focus mode, palette, overlays, scroll, in-progress text | Window | ADR 0014's "independent focus and presentation state" |
| Interface settings, profiles, keyboard bindings | Application, via broadcast | Issue #119's explicit requirement |

Settings remains a per-window tab, so two windows may show Settings at once.
Both read and write the same broadcast document, so the second edit wins and
the first window's Settings screen shows the new value on its next pass.

### 4. Window lifecycle

- "New Window" is a window-scoped request (`AppCommand::OpenWindow`) that the
  owning window records and `FesTermApplication` fulfils after that window's
  pass, so a new window is never spawned mid-borrow.
- A new window starts on the Launcher with the current configuration. It does
  not clone the originating window's tabs; sessions are not duplicable.
- Closing a secondary window runs the same live-session confirmation as
  quitting, then removes just that window. Closing the primary window is
  unchanged: it is the application quit path.

## Consequences

**Accepted costs**

- **Crash blast radius is the whole application.** A panic or a wgpu device
  loss in any window takes down every window and every live session in the
  process. This is a genuine regression against the multi-process
  alternative and is accepted deliberately: the alternative costs live
  propagation (ADR 0015 forbids the file watching it would need),
  last-writer-wins clobbering of profile edits between processes, contention
  on the sessiond registry lock, and repeated keychain prompts. Session
  durability against process death is already the job of ADR 0025's
  persistence daemon, not of process isolation.
- **All windows share one main-thread frame budget.** A window doing
  expensive work delays its siblings' repaints.
- **Configuration is physically replicated per window.** Coherence depends on
  every write going through `apply_configuration_save`. That is already an
  invariant, and it is now load-bearing, so it is asserted by test.

**Deferred, explicitly out of scope for this increment**

- Dragging a tab between windows, and pop-out/re-attach of a tab into its own
  window.
- Per-window workspace restore. The persisted workspace schema stays a single
  tab list owned by the primary window; generalising it to a per-window list
  needs its own schema decision and migration.
- Window-scoped *preferences*. ADR 0014 reserves the category; nothing
  populates it yet, and this increment does not add one.
- Per-window geometry persistence.

**Alternatives rejected**

- **Multiple processes.** Blocked on live propagation by ADR 0015, and
  independently harmful: each process holding a full `Configuration` and
  autosaving every change produces last-writer-wins clobbering, where one
  window's profile edit silently destroys another's.
- **Deferred viewports.** Cannot borrow non-`Send` window state mutably.
- **One shared `Rc<RefCell<Configuration>>`.** Would require touching every
  configuration read in `app.rs` and `tabs.rs`, and would make a failed save
  visible to siblings before it committed, breaking ADR 0015's
  commit-only-on-success rule.

## Validation impact

- **Invariants introduced or changed:** every configuration write reaches
  sibling windows through `apply_configuration_save` and nowhere else; only a
  *committed* document is broadcast, so a failed save reaches no sibling;
  adopting a sibling's document never re-queues it, so two windows cannot
  ping-pong one write; adoption touches only the configuration and its
  derived preferences, never a window's tabs, focus, scroll, selection, or
  in-progress text entry; window creation and destruction belong to the
  composition root, so a window may only *request* them; only the primary
  window persists the workspace, owns the native menu bar, the wake monitor,
  and native window chrome.
- **GUI/action edges affected:** `WINDOW-01` ("New Window" from the command
  palette or its keyboard binding opens an additional window) and `WINDOW-02`
  (a configuration change committed in one window applies in every other
  window on its next pass).
- **Automated tests required:** `opening_a_window_adds_one_window_to_the_application`,
  `a_new_window_starts_on_the_launcher_rather_than_cloning_the_originating_tabs`,
  `the_command_palette_offers_new_window_and_only_requests_it`,
  `a_closed_secondary_window_is_dropped_and_the_primary_window_is_not`,
  `an_interface_setting_committed_in_one_window_reaches_every_other_window`,
  `a_profile_saved_in_one_window_is_visible_in_every_other_windows_launcher`,
  `a_keyboard_binding_changed_in_one_window_applies_in_every_other_window`,
  `adopting_a_siblings_configuration_leaves_this_windows_tabs_untouched`,
  `adopting_a_broadcast_does_not_re_broadcast_it`, and
  `a_secondary_window_does_not_persist_its_workspace`.
- **Manual/interop evidence:** none. Every behaviour above is reachable
  headlessly; opening a real second OS window is covered by the existing
  native-window smoke only for the primary viewport, and multi-viewport
  rendering is exercised interactively.
