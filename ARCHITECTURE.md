# fesTerm Architecture

The Android host in `app/festerm-android` now shares the mobile application,
including its live SSH/SFTP/Dev Tunnels composition. The
`festerm-android-platform` JNI boundary supplies native services through the
existing window, lock and secret-store facades, selected only on Android.
[ADR 0073](docs/adr/0073-android-shared-mobile-host.md) records this additive
boundary, GLES/IME ownership and the explicit private-document-import policy.
No platform FFI enters the terminal core or renderer.

The additive experimental iOS composition root in `app/festerm-mobile` reuses
`festerm-core` and `festerm-ui-egui` through eframe/winit, without depending on
the desktop application. [ADR 0042](docs/adr/0042-ios-rendering-spike-host.md)
defines its initial Phase 1 ownership and validation boundary. Later mobile
ADRs add the shared session, credential, profile and document capabilities.

The phased iOS editor extension in [ADR 0069](docs/adr/0069-ios-authorized-shared-documents.md)
adds `festerm-mobile-docs -> festerm-document` for a UI-/I/O-free registry over
the existing text/undo engine. Native source authorities and scheduling stay
in the mobile composition/native boundary; display paths grant no authority.
The initial registry slice does not yet wire a native Files picker or editor.

**Status:** Draft

This document defines the proposed subsystem boundaries, dependency direction,
runtime data flow, and Rust workspace structure for fesTerm. The repository
contains `festerm-core`, `festerm-session`, `festerm-pty`, `festerm-sessiond`,
`festerm-test-support`, the `festerm-ui-egui` presentation crate, and the
application composition shell. Native SSH, strict `festerm-config` loading,
local/SSH/serial profile launch, profile CRUD, persistent host trust, opaque
native-secret-store SSH password/private-key references, metadata-only
workspace save/restore, and the opt-in `festerm-sessiond` local-persistence
path are integrated. OpenSSH-config import UI, SSH-agent adapters, key-file
references, and the remaining M6/M9/M10 validation work remain target
architecture rather than finished acceptance.

## Architectural Goals

The architecture should make it possible to:

- Build and test terminal emulation without a GUI, PTY, or network connection.
- Support local and SSH sessions through a common lifecycle and byte-stream model.
- Prioritize terminal correctness and interactive performance.
- Use `egui` initially without making the terminal engine dependent on it.
- Replace or specialize rendering later without rewriting protocol behavior.
- Keep configuration, persistence, portability, and secrets behind explicit boundaries.
- Add future scripting, plugins, or multiplexing without designing those systems prematurely.

## Dependency Direction

Dependencies should point inward toward stable domain logic:

```text
festerm-app
  |-- festerm-ui-egui
  |-- festerm-pty -----> festerm-session
  |-- festerm-sessiond -> festerm-core, festerm-session and festerm-pty
  |-- festerm-config
  |-- festerm-ssh -----> festerm-session

festerm-ui-egui ------> festerm-core
festerm-pty ----------> festerm-session
festerm-session ------> festerm-admission
festerm-automation ---> festerm-admission
festerm-sessiond -----> festerm-core and festerm-session
festerm-ssh ----------> festerm-session
festerm-test-support -> festerm-core and session implementations
festerm-document -----> (no workspace dependencies)
festerm-unix-security -> (platform APIs only)
festerm-windows-security -> (platform APIs only)
festerm-admission ----> (standard library only)
```

`festerm-core` must not depend on GUI, PTY, SSH, operating-system keychain, cloud identity, or persistence implementations.

`festerm-admission` owns the existing process-wide automation admission lock
and attachment epoch without depending on policy, session, GUI or backends.
Session re-exports its existing opaque guard and APIs, and the authority kernel
can name the same guard without depending on session implementations. This
changes dependency ownership, not terminal ownership or lock scope. See
[ADR 0065](docs/adr/0065-mcp-and-javascript-automation.md).

`festerm-automation` also owns the existing four-frame in-memory outbound
queue and its inert frame and target-guard representation; the application
still owns transport, size/deadline and admission checks, and shutdown. Queue
insertion requires the shared guard, which the app retains from those checks.
It is not a broker authorization or complete revocation fence and does not
qualify metadata routing or physical delivery.

The Windows Direct2D experiment adds an app-owned
`festerm-windows-direct2d` graphics boundary consuming egui primitives, not
terminal state. On supported Windows x64 DX12 CPU-adapter targets with
`Bgra8Unorm` or `Rgba8Unorm`, it is the automatic root-terminal path without
a setting. Hardware adapters, unsupported formats, and other platforms retain ordinary
egui-wgpu. The UI retains layout, input, fonts and paint order; the native
crate owns SDK interop and immutable shared surfaces. See accepted
[ADR 0039](docs/adr/0039-opt-in-direct2d-terminal-composition.md).
The native renderer retains one frame's pixels and bounded presentation-region
snapshots. Small changes redraw damaged regions and copy them into a new
immutable frame; unchanged frames share their existing texture. This does not
move terminal ownership or introduce a partial-present/window backend.
An optional complete-source snapshot retains at most 1 MiB of CPU payload.
After original-frame/texture admission, exact equality with a successfully
prepared frame skips native conversion and region materialization. Oversized
snapshots use ordinary preparation; changed sources keep complete native
validation before damage comparison. This is not a peak/process-memory bound.

Pinned egui-wgpu 0.36.1 supports automatic host-copy and retained-prefix
composition on the Windows x64 DX12 CPU/BGRA gamma path. The owner approved
rollout without production switches; the former Direct2D, host-copy and
retention environment overrides are removed. Hardware, ARM64, other platforms
and incompatible targets never gain this route. Resource limits, exact pixels,
callback ownership and same-frame fallback remain required. Shared-host CPU
noise limits benchmark precision, not the owner's bounded rollout decision;
broader resource/native/latency qualification remains tracked in #282/CP-18.
The eligible app may advertise an immutable terminal image as an exact substitute
for its final callback. The host validates the actual opaque root target,
ends the preceding UI pass, and copies that image before its normal submission
and presentation. Unsupported surfaces, overlays, clipping or geometry
mismatches retain ordinary callback painting in the same frame. Layout, input,
terminal ownership and hardware-GPU selection are unchanged; this is not
partial presentation or a replacement window backend. See
[ADR 0040](docs/adr/0040-opt-in-final-target-terminal-copy.md) and
[ADR 0041](docs/adr/0041-opt-in-retained-window-prefix.md), with exact bounded
immutable signatures for the preceding UI and ordinary same-frame fallback, and
[`vendor/egui-wgpu/FESTERM-PATCH.md`](vendor/egui-wgpu/FESTERM-PATCH.md).

Accepted [ADR 0046](docs/adr/0046-opt-in-retained-document-frame.md) extends
that same host-owned immutable image to eligible complete non-terminal frames.
The supported Windows x64 DX12 CPU/BGRA gamma route requests it after native
installation; `FESTERM_WARP_RETAIN_DOCUMENT_FRAMES=0` disables this extension
without changing terminal-prefix policy. Generic vendor defaults remain off.
Both modes share the 64-MiB image and 1-MiB exact-signature limits, not a
process/peak allocation guarantee. Input, callback preparation, document
freshness, UI construction, acquisition and presentation remain unchanged.
CP-18 binds fresh all-miss evidence to clean source and the actual release test
executable. Final integration review accepted the bounded shared-cache policy;
the broader native/resource/latency matrix is not accepted by this extension.

Accepted [ADR 0051](docs/adr/0051-adaptive-exact-document-prefix.md) lets
that eligible complete-frame route retain an exact leading paint-job prefix
while scrolling changes a suffix. The host copies the clear-initialized
prefix image and paints the current suffix in original order, while preserving
the existing image/signature limits, full-frame admission, callback preparation,
managed-texture epochs, and ordinary same-frame fallback. This generic renderer
mechanism applies to every eligible complete non-terminal frame, although its
source-matched qualification covers four Markdown scrolling cases and plain
Edit. The owner accepted this policy on 2026-10-28 after twenty balanced native
WARP observations; broader CP-18 hardware/resource/usability and physical-latency
gates remain open. This is not universal surface or freeze-repair acceptance.

The app-owned textureless panel pipeline also handles the full-width chrome
band and status-bar frame on eligible Windows DX12 CPU adapters. The UI exposes
a graphics-only optional fill callback; the app retains device/format,
opacity and transform policy, shader resources, and egui's exact feathered
geometry. There is no new terminal state or presentation backend in this hook.

### PowerShell and enterprise development boundaries

Owner-authorized development under
[ADR 0082](docs/adr/0082-native-powershell-enterprise-and-sessiond-boundaries.md)
adds separate backend foundations rather than changing the terminal byte
contract. `festerm-enterprise` owns transient public-client identity and bounded
Dev Center queries; `festerm-powershell` owns native structured PSRP operations.
The experimental desktop PowerShell tab is separate from terminal sessions:
typed application actions control a background worker, structured output has
bounded retention, and per-worker cleanup handles gate asynchronous teardown.
Profiles/workspaces contain endpoint metadata and opaque credential references,
never passwords, tokens, command history or live runspace identity.
Ephemeral enterprise and remote-session discovery tabs share the typed action,
bounded background-worker and retained cleanup model. The remote picker opens
the existing PersistentSession terminal path for an exact SSH-pinned daemon
generation, while enterprise discovery remains read-only account metadata.
Neither surface enters workspace restoration or implicitly authorizes a shell.
Both reuse the secret-byte boundary. Neither makes the terminal core depend on
cloud identity, nor feeds PSRP/CLIXML through the VT parser.

Enterprise accounts and remote-daemon selection are not yet GUI surfaces or
automatic workspace connections. Future GUI integration must use typed
application commands and keep network operations off the frame thread.
Entra discovery does not authenticate a Windows shell. Sessiond's versioned
JSON helper is read-only; its separate binary stdio bridge revalidates an
exact owned daemon generation and requires explicit takeover authorization.
`festerm-ssh` supplies bounded native binary exec without a PTY, while
`festerm-sessiond` retains snapshot decoding/adoption and terminal ownership.
There is no reverse SSH-to-sessiond dependency or new network listener.
Native interoperability and broker/compliance gates remain CP-24, CP-25 and CP-21.

## Current and Target Workspace Layout

The repository is a Cargo workspace. The initial core, test-support, and
application crates establish the dependency direction below; additional crates
will be introduced when their responsibilities are implemented. Exact names may
change, but the responsibilities should remain distinct.

```text
/
  Cargo.toml
  crates/
    festerm-core/
    festerm-session/       # implemented M5 common lifecycle boundary
    festerm-pty/           # implemented M5 local PTY/ConPTY backend
    festerm-pty-test-child/ # implemented M6 deterministic PTY test child
    festerm-sessiond/      # ADR-0025 local persistence daemon and Session client
    festerm-host-setup/    # GUI-free embedded setup and authenticated helper artifacts
    festerm-ssh/
    festerm-config/
    festerm-ui-egui/       # implemented M4 presentation layer
    festerm-document/      # ADR-0034 GUI-free shared text document core
    festerm-unix-security/ # ADR-0034 handle-bound save metadata/publication
    festerm-windows-job/   # Windows process-tree shutdown support
    festerm-windows-security/ # named-pipe and ADR-0034 save access policy
    festerm-windows-runtime/ # trusted optional ConPTY sidecar loading
    festerm-windows-direct2d/ # experimental SDK/wgpu graphics interop, no terminal ownership
    festerm-test-support/
  app/
    festerm/
  tests/
    fixtures/
    compatibility/
    ssh/
```

### `festerm-core`

Owns terminal protocol behavior and state:

- Byte decoding and ANSI/VT parsing.
- Typed terminal operations or actions.
- Primary and alternate screen buffers.
- Grid cells, attributes, cursor, tab stops, margins, and modes.
- Scrollback and resize semantics.
- Keyboard, paste, focus, and mouse encoding.
- Device replies emitted toward a session.
- Dirty-region or change information for rendering.

The core should use straightforward mutable state. Rust ownership should provide a single authoritative owner of terminal state; immutable/event-sourced architecture is not a goal.

Parsing and state mutation should still have a testable seam. A useful conceptual flow is:

```text
bytes -> decoder/parser -> TerminalOp -> TerminalState::apply
```

The implementation may fuse steps where profiling or simplicity justifies it, provided parser and state behavior remain independently testable.

The current implementation through M6 provides a bounded ESC/CSI parser,
primary and alternate grid buffers, scrolling regions, SGR state, tab stops,
DECSCUSR cursor styles, sanitized OSC titles, conservative device-attribute
replies, resize without reflow, typed mode-aware keyboard/paste/focus/mouse
input, and dirty-row inspection. Cells retain leading text with attached
common combining code points and a width role with explicit width-two
continuation cells, and cell text is stored as a `CompactString` rather than
`std::String` so printing does not heap-allocate for the common single-byte
or single-codepoint case. `Screen`'s row storage is a ring buffer (a rotating
logical-to-physical row offset) rather than a flat, physically ordered grid,
so a full-screen scroll is an O(rows-scrolled) rotation instead of an
O(rows x columns) copy of every cell on every line feed; this is entirely an
internal `screen.rs` implementation detail behind an unchanged row/cell API.
The application-level `SessionController` is the
production and controlled-test seam while remaining the single logical
terminal writer. The first M9 slice now retains bounded logical primary-screen
history with content-free accounting and explicit alternate-screen isolation,
following [ADR 0017](docs/adr/0017-bounded-logical-scrollback-and-anchored-viewports.md).
Resize anchors reuse history's existing physical-row index and carry a
transient, identity-checked line-index hint through capture and resolution.
Wrapped cell offsets use binary row-boundary lookup. There is no new
persistent cache or recovery field; stale hints retain ID-based fallback,
and width reflow still processes logical content.
The UI now projects retained physical rows through a borrowed snapshot and
stores follow/anchor state in each session-owned `TerminalView`, without
changing PTY grid dimensions or workspace persistence. Selection across
history, resize reflow, renderer shaping, and full grapheme segmentation remain
later work; native rendered-window validation remains an M6 gate.

### `festerm-session`

Defines session-facing abstractions and lifecycle types shared by local and remote backends:

- Start, stop, reconnect, and shutdown lifecycle.
- Reading output bytes and writing encoded input bytes.
- Terminal-size changes.
- Connection state and user-visible status.
- Backend capabilities and errors.

The terminal core should not own session I/O. The application coordinates bytes between a session and a terminal instance.

A session trait should describe behavior rather than expose a specific async runtime. The concrete concurrency model can be selected after experiments, but cancellation and clean shutdown must be explicit.

M5's `Session` trait is synchronous and runtime-independent: it provides
nonblocking input, resize, shutdown, and event polling plus a caller-bounded
shutdown wait. Its events carry bytes, lifecycle, resize, backpressure, and
content-free errors; it has no terminal-core dependency.

### `festerm-pty`

Implements local process sessions:

- Unix PTYs on Linux and macOS.
- ConPTY or the appropriate Windows pseudoconsole API.
- Platform shell discovery and configurable commands.
- Process exit and resize handling.

Platform-specific code should remain isolated behind the session abstraction.

M5 uses `portable-pty` 0.9's `native_pty_system`, which selects its Unix PTY
implementation on Unix and ConPTY on Windows. The backend owns process and
stream workers, not terminal state. It has bounded command and application
event queues, pauses reads under event pressure, reports metrics/events, and
uses an event notifier to wake the application after enqueuing output. The app
preserves session-backpressured input/replies in one ordered bounded buffer.
Shutdown owns whole trees: Unix uses the PTY session process group and Windows
uses a kill-on-close Job Object.

On Windows, `festerm-windows-runtime` runs before the first native PTY
allocation. It accepts only the installer-owned sidecar layout documented in
[`third_party/conpty/README.md`](third_party/conpty/README.md), hashes both
`conpty.dll` and its `OpenConsole.exe` host against the pinned manifest, limits
the process DLL search to System32, and loads a valid sidecar by absolute path.
That preloaded module is what `portable-pty` 0.9 observes when it probes its
relative `conpty.dll` name. Missing or invalid sidecars use inbox Kernel32
ConPTY; an unverified module loaded before selection is an error. This preserves
the `Session` byte/event contract while making the Windows runtime decision
explicit and safe.

### `festerm-host-setup`

Owns embedded legacy installer bytes and the GUI-free authenticated helper
release boundary (`artifacts`, ADR 0084 §9). Its strict signed manifest binds
package/version/platform/architecture/filename/length/hash to the existing
embedded updater key. Only its verifier constructs an immutable
`VerifiedArtifact` containing exact bounded allowlisted regular file bytes;
setup consumers must stage those bytes privately/atomically rather than
reopening an archive path. No desktop/updater/GUI dependency, downloaded trust
key, service mutation or unsigned fallback is introduced. The offline
`festerm-verify-artifact` command verifies without extraction/execution.
See [the wire/staging/bootstrap contract](docs/host-release-artifacts.md).
New headless CLI inclusion, service switching and migration remain separate
implementation gates; this is not full stage-7 acceptance.

### `festerm-sessiond`

Implements ADR-0025's opt-in persistent local-session backend. Its packaged
standalone executable owns one detached PTY/ConPTY plus an authoritative
`festerm-core::Terminal` recovery mirror. PTY output, accepted resizes, and
frontend-owned terminal controls that affect recovery (`scrollback_limit`,
color-scheme reporting, clear, and reset) are applied to that mirror in order.
New protocol-v2 clients attach by reading one bounded recovery snapshot,
adopting it explicitly, and only then receiving framed live output, ordered
daemon-applied controls, resize acknowledgements, exit/takeover notices, and
terminal-query ownership. Older protocol-v1 helpers remain attachable through
their legacy byte-stream path. The crate's library target implements
`festerm-session::Session` directly over owner-scoped Unix-domain sockets or
Windows named pipes, with bounded input/resize/control commands, explicit
snapshot-schema negotiation/rejection for incompatible helpers, validation of
decoded `Terminal` invariants before adoption, and a reconnect-time resize
gate so the frontend cannot overwrite detached geometry or encode input before
it has adopted the daemon's snapshot. The app still flushes the actual current
viewport after adoption, so recovered state and the live frontend converge
without a race while preserving single-client steal-on-reconnect semantics.
Snapshot encoding sizes the bounded payload before writing directly into one
fixed header-prefixed wire buffer; decoded payload storage retires before
terminal allocation-capacity restoration. This reduces managed wire overlap,
not the accepted mirror/display ownership or a total process-memory budget.
The old client is retired only after candidate adoption; a failed candidate
leaves it usable. The daemon quiesces PTY/input processing for this handshake,
bounded by a 15-second adoption deadline. Full output queues retain controls,
resizes, and exit notices in order rather than dropping them. The frontend
applies confirmed geometry with its normal history/selection reflow, not
optimistically. CLI v2 attach renders a text projection of the mirror instead
of forwarding raw query-producing escapes to the host terminal; it is not a
full-fidelity graphical or styled terminal renderer.
Closing the application session
detaches from the daemon rather than terminating its shell, and the daemon
answers terminal queries exactly once for protocol-v2 sessions instead of
dropping or replaying queued replies, clipboard actions, or stale user input
across reattach.

Platform-sensitive file authority remains isolated in the two ADR 0034
security crates. `festerm-unix-security` owns no-follow opens, private staging,
metadata snapshots and no-overwrite publication. `festerm-windows-security`
owns native handle-relative creation/publication and complete Windows access
policy handling in addition to named-pipe DACL support. Each named-pipe
instance is created while the daemon token temporarily has a current-user-only
default DACL, and the original token DACL is restored before the shell is
launched.

### `festerm-ssh`

Implements native SSH sessions within the application process. It must not require an external `ssh` executable to be installed.

Responsibilities include:

- Host-key verification.
- Authentication and agent/keychain integration.
- SSH channel and remote PTY lifecycle.
- Environment and terminal-size requests.
- Reconnect policy and connection state.
- Mapping supported OpenSSH configuration into fesTerm's internal profile model.

`festerm-ssh` uses the Tokio-native `russh` client under the trust,
authentication, reconnect, and OpenSSH-import policy in
[ADR 0013](docs/adr/0013-russh-native-ssh-transport.md). It must not enable
insecure legacy algorithms or invoke a system `ssh` binary.

### `festerm-config`

ADR 0084's public host/session recipe foundation is factored into the additive
`festerm-client-recipes -> festerm-host-protocol` metadata contract. Both
`festerm-config` and `festerm-mobile-profiles` depend on it; mobile does not
depend on desktop configuration/PTY. A separate recipe collection preserves
legacy profiles and launch paths. Shared typed metadata commands and explicit
browser request state have no network/vault/process executor or principal
authority. Desktop composes the command with its existing save gate; mobile
uses its atomic profile document. Live host inventory/ensure/attach and native
UI remain gated by the reviewed endpoint and identity contracts (#681).

Owns versioned, human-readable configuration and persistence models:

- TOML parsing and validation.
- Schema versioning and migrations.
- Profiles, preferences, keybindings, themes, and workspace definitions.
- Explicit transactional reload from the selected source.
- References to secrets without storing secret values in TOML.

Operating-system credential storage belongs behind a separate interface and should not leak into configuration documents.

The current M8 slice parses and serializes schema-versioned, strict TOML local
and SSH profile metadata. It validates a complete candidate before atomically
replacing active configuration, retaining the prior valid document and a
content-free diagnostic on failure. The application loads at startup and
reloads only on an explicit Settings action; it does not watch files. It can
launch configured local profiles and explicitly save/restore a deliberately
limited metadata-only workspace while preserving user-authored profiles. An
SSH profile may contain only an opaque native stored-password reference; the
transport resolves it on its worker immediately before password
authentication. Profile editing, all other credential forms, trust
persistence, and full workspace policy remain later M8 work. ADR 0015 defines
reload semantics.

### `festerm-ui-egui`

Provides the initial graphical front end:

- Windows, tabs, focus, and workspace presentation.
- Terminal rendering.
- Input event collection and routing.
- Selection, clipboard, menus, settings, and connection indicators.
- Mapping cell-space terminal state to fonts, shaping, pixels, and GPU-backed drawing.

M4 exposes a borrowed `TerminalSnapshot` contract containing the visible
screen, dimensions, cursor, and modes. `TerminalRenderCache` copies only rows
identified by `Terminal::take_dirty_rows` (with a complete refresh on initial
view or size change), so a GUI frame does not clone the entire core grid.
Presentation cell text uses the core's existing `CompactString` representation:
common graphemes stay inline, while replacing long text releases its old heap
payload. Dirty rows and same-dimension viewport refreshes reuse cell-vector
backing; dimension changes retire the old row storage rather than retaining an
exceptional high-water capacity. Value copying, changed-row revision identities
and the renderer's owned shaping-run strings remain unchanged.
Cell metrics and point-to-cell helpers remain UI-owned and convert only to
valid core dimensions. A selected pinned primary face alone determines cell
metrics. The glyph cache is generation-keyed so replacing egui's font atlas
cannot reuse stale layouts. Its unchanged 4,096-entry bound uses indexed LRU
retirement: a capacity-crossing miss drops one owned layout, not every warm
entry. Cached hits update constant-time links without scanning or copying text.
Slot IDs and recorded hashes are bounded metadata, not glyph identity; manual
and font/atlas resets release that tracking storage along with cached layouts.
This intentionally retains a full bounded warm cache after saturation, rather
than claiming reduced retained bytes. One-cell layout remains the default;
an explicit ligature policy may shape compatible ASCII runs inside their preallocated
cell spans.
Width-two leading cells and their continuations are submitted as one
two-column paint span.

The optional native painter may supply metadata-only atlas admission after
tessellation and before pixel capture. Backend policy owns eligibility and
refusal reporting; rejection retains ordinary shapes without consuming font
deltas. Hook revalidation and current-owner teardown protect replacement
state. Accepted ADR 0045 changes only ADR 0043's oversized capture ordering;
trusted revision and immutable owned-snapshot contracts remain intact.

The UI routes egui keyboard, text, paste, focus, pointer, wheel, selection,
and clipboard events through M3 `InputEvent` values. It drains the core's
encoded queue to an application-provided sink and reports an accepted core
resize to that same application boundary. M5's app sink forwards those bytes and dimensions to the local session; its
bounded pending buffer retains writes rejected by temporary session
backpressure. The UI itself remains PTY-free.

The boundary should be practical rather than doctrinaire. The core may expose cell widths, underline styles, cursor shape, dirty rows, and other rendering-relevant terminal semantics. It should not own fonts, pixel coordinates, glyph caches, or GUI widgets.

`egui` is the selected initial front end, not an irreversible product dependency. Its rendering stack does not preclude GPU acceleration. If profiling later shows a need for a specialized terminal renderer, it should be possible to replace the cell-rendering path while retaining the application shell and terminal core.

Ligatures are a committed later capability. Text shaping belongs in the rendering layer, which must preserve the mapping between shaped glyph runs and terminal cells so cursor placement, selection, and mouse coordinates remain correct.

### `festerm-macos-window`

Owns the narrow AppKit boundary that cannot be expressed truthfully through
portable egui widgets: native traffic-light placement and the native macOS
application menu. Menu items send semantic `NativeMenuCommand` values back to
`festerm-app`; they do not mutate tabs, sessions, or settings directly. Dynamic
labels and enabled state are supplied by the application command model, while
standard Copy, Paste, window, and application actions remain on AppKit's
responder chain.

### `festerm-document`

Owns the editable-text domain described by ADR-0034, with no GUI, filesystem,
or network dependencies:

- `DocumentId` handles and `DocumentOrigin` identity, including the aliasing key
  that lets one file be open in many views at once.
- Encoding, line-ending, and indentation detection, plus write-back that
  restores what the file originally used.
- Bounded undo history with typing coalescence and token-based dirty tracking,
  so undoing back to the saved content stops reporting unsaved changes.
- Size and line-length bounds with a refusal reason the UI can render verbatim.
- The line comparison used by Compare.
- `DocumentStatus`, the single source of truth for banner severity and wording,
  command enablement, and auto-save state.

Reading and writing bytes, watching for outside changes, and every widget stay
in `festerm-app`.

Ordered multi-edits build one result from borrowed unchanged/replacement spans;
inverse construction tracks original and applied coordinates without cloning
inverse payloads. Apply, replay and vi scratch share that helper. Document byte
admission precedes output allocation, followed by existing line bounds before
history commit. Single/equal-length undo/redo stays in place; length-changing
multi-edit replay temporarily owns one additional bounded result and drops the
replaced buffer immediately, without adding a retained owner or widening bounds.

Ordinary ready/Normal vi motions and count prefixes use borrowed UTF-8
boundaries/local line scans, without constructing full-document character or
byte-offset vectors. Word motions share one algorithm between borrowed byte
coordinates and the existing indexed operator fallback. Unsupported/pending,
Insert/Replace and Visual routes retain that keystroke-local fallback.
Dot-repeat's final diff streams common prefix/suffix characters and owns only
its changed payloads, not two extra full-document character arrays. No document
snapshot, revision cache or additional retained owner is added.

### `festerm-test-support`

Provides shared test infrastructure:

- Human-reviewable golden fixture parsing.
- Grid, cursor, mode, scrollback, and emitted-byte assertions.
- Recorded terminal byte streams.
- Fake and in-process session implementations.
- OpenSSH test environment helpers.
- Performance workload generators.

Test fixtures belong in the repository from the beginning and are versioned alongside behavior changes.

### `festerm-app`

The application composition root:

- Creates windows, sessions, terminal instances, and persistence services.
- Coordinates session bytes, terminal mutations, UI redraws, and shutdown.
- Restores workspaces and creates sessions from profiles.
- Owns application-level commands and diagnostics configuration.

Business and protocol behavior should be moved out of this crate when it becomes independently testable.

Application-level user intent must converge on a single command-handling path.
Launcher actions, keyboard shortcuts, application chrome, overflow menus,
command-palette entries, session-inspector controls, and future first-party
automation surfaces should dispatch the same semantic application commands
rather than implementing duplicate widget-specific policy. The application
layer resolves those commands into session, window, workspace, or settings
state changes and exposes the resulting state back to the UI. Low-level
terminal input remains outside this command model and continues through the
`festerm-core` typed input path. The detailed rules live in
`docs/application-command-model.md`.

Window, workspace, tab, session, and transport-attempt lifetime rules are
defined in [ADR 0014](docs/adr/0014-window-workspace-tab-session-ownership.md).

## Runtime Data Flow

### Session output

```text
PTY or SSH read
  -> bounded application channel
  -> application terminal owner ingests bytes
  -> parser and state mutation
  -> replies queued to session
  -> dirty rows/change summary published to UI
  -> renderer redraws affected content
```

### User input

```text
OS/egui input event
  -> UI command and key mapping
  -> terminal input encoder consults active modes and returns an explicit
     encoded/selection/overflow outcome
  -> encoded bytes written to active session
```

### Resize

```text
window/pane size changes
  -> renderer computes rows and columns
  -> terminal core resizes logical state
  -> application forwards accepted dimensions to session backend
  -> affected rows are redrawn
```

### Configuration reload

```text
TOML file change
  -> parse and validate complete candidate configuration
  -> reject invalid candidate without losing current settings
  -> apply safe live changes
  -> mark restart-required changes explicitly
```

## Concurrency Model

The terminal state for a session should have one logical owner. Session I/O may
use worker threads, but multiple threads must not mutate one terminal state
concurrently. M5 makes the app that owner: its frame pump drains backend
events, ingests output, forwards replies, and sends UI-produced input.
Session event enqueueing uses an application-provided notifier; the egui
composition root maps it to `Context::request_repaint` rather than polling.

The design should prefer message passing or bounded queues over broad shared locking. Bounded flow control is necessary so sustained output cannot create unbounded memory growth. Rendering should consume snapshots or change summaries without blocking session reads longer than necessary.

The exact async runtime and channel types are open implementation choices.

## Rendering Contract

The renderer needs access to a stable cell-space view containing at least:

- Visible rows and columns.
- Grapheme or display-cell content.
- Cell width and continuation information.
- Foreground, background, and text attributes.
- Cursor location, visibility, and style.
- Selection and hyperlink metadata when supported.
- Dirty rows or regions.

`TerminalSnapshot` is the M4 immutable borrowed view, while
`TerminalRenderCache` is the presentation-side changed-row cache. The first
renderer may redraw more than the theoretical minimum, but this interface
avoids forcing full-grid copies for every frame. Its diagnostics report frame
duration, requested dimensions, rows refreshed, core input outcome/queue
depth, content-free active-session input counters, session lifecycle/pressure
metrics, and input-to-paint-submission time without recording terminal
content. Paint-submission timing ends after the grid's shapes are submitted to
egui; it is not presentation timing.

## SSH Interoperability and Testing

fesTerm should interoperate with OpenSSH without using the OpenSSH client as its runtime backend.

OpenSSH configuration should be parsed or imported into an internal profile representation. Unsupported directives must be surfaced rather than silently misinterpreted. The internal model may extend beyond OpenSSH concepts.

SSH testing should use three layers:

1. Unit tests and fake transports for connection-state and reconnect logic.
2. In-process client/server tests when the selected Rust library supports both roles.
3. A small set of containerized OpenSSH `sshd` interoperability tests covering PTY allocation, resize, host-key handling, authentication, shell I/O, disconnect, and reconnect behavior.

Integration tests must create and own their server configuration, keys, users, and lifecycle. They must not depend on a developer's machine-wide SSH configuration.

## Diagnostics and Observability

Debugging facilities should be designed in early but remain disabled or low-overhead by default:

- Structured logs with subsystem targets and levels.
- Optional escaped or hexadecimal protocol traces with redaction controls.
- Terminal-operation traces for parser/state debugging.
- Session lifecycle and reconnect events.
- Frame timing, dirty-cell counts, queue depth, and input-to-paint-submission
  latency metrics (not presentation timing).
- Reproducible diagnostics bundles that exclude secrets by default.

Raw terminal streams can contain credentials and private data, so protocol logging must be explicit and clearly warned.

## Continuous Integration

CI should be introduced with the foundation work and run on every change. The initial matrix should remain small but cover Windows, macOS, and Linux where practical.

The first checks should include:

- Formatting.
- Linting.
- Unit and golden tests.
- Documentation build or link checks where practical.
- A Linux-hosted OpenSSH interoperability job once the SSH crate exists.

Performance benchmarks should initially report trends rather than block every commit until stable thresholds are established.

## Architectural Invariants

1. Terminal protocol semantics do not live in GUI widgets.
2. Session backends do not mutate terminal state directly.
3. Secrets do not appear in normal configuration, workspace, or exported/shared metadata.
4. Test fixtures are deterministic and repository-owned.
5. A failed configuration reload does not destroy the last valid configuration.
6. One terminal state has one logical writer.
7. Optional future systems do not enter the critical path before they are required.
8. Product-level actions converge on the application command model rather than
   duplicating policy across GUI invocation surfaces.

## Experimental remote session bridge

ADR 0084 stage 4 adds the opt-in `festerm-host-setup::lifecycle` policy and
standalone `app/festerm-host` composition root without GUI/graphics dependencies.
Both desktop and headless callers can use its protected, origin-scoped
configuration/consent journal, operation epochs and exact-owned cleanup receipts.
Consent commits the exact foreground-approved settings under the journal lease;
persisted operation tickets remain recoverable after interruption. Initial
journals and staged CLI/helper files share atomic no-replace publication.
It reuses sessiond private storage, the bridge's reviewed official-CLI pins and
the shared authenticated-artifact verifier. Verified helper entries and their
exact signed provenance are staged immutably without execution;
default legacy/mobile installer-review dependencies remain unchanged.
The optional setup-library `native-services` feature, selected by the default
headless binary, adds a
[same-user native control component](docs/native-host-services.md):
sealed updater-authenticated executable receipts, protected exact-owned
registration and epoch-fenced native observations/control.
macOS admission pins the release Developer ID team and hardened runtime; Windows
admission pins the owner-approved Artifact Signing subject/profile and Microsoft
chain, with trusted RFC3161 and online-enabled fail-closed revocation.
Windows registration/start require the compiled runtime watcher; stop observes
protected lifecycle revocation, exclusive run-lease release and exact native task
quiescence, never destructive task termination. Native execution is unqualified. The native
component itself owns neither endpoint readiness nor shell launch. The headless
controller now composes explicit setup, separate owner-key/permission approval,
run, disable, authenticated stopped-version update and exact-owned removal with
those APIs. Update retains signed old/new helper provenance and a protected
reservation/unknown-outcome journal, never automatic rollback or access reenabling.
Configure defaults disabled;
compilation, native Running and last-run history confer no consent or Ready claim.
Default legacy/mobile dependencies remain unchanged. See the
[headless boundary and adapter contract](docs/headless-host.md).

The [Dev Tunnels experiment](docs/adr/0053-separate-dev-tunnels-session-bridge.md), included in default desktop builds,
adds a separate network bridge, never cloud dependencies to the daemon/core/UI.
The opt-in bridge `host-routing` module and paired
`festerm-tunnel-client::host_control` transport reuse the shared HOST contracts
for bounded framed control RPC. The bridge consumes non-serializable verified
owner proof and delegates reservation/grant checks to the session backend;
the client owns immutable account/endpoint/host correlation, not authority.
Creation RPCs retain an opaque issued operation bound to that destination and
the original negotiated namespace across reconnects; lifecycle RPCs never
inherit a creation scope. Correlation includes the exact echoed namespace.
`host_control::native::NativeControl` uses actual sessiond reservations and owned
policy guards; the headless binary's default-enabled `owner-routing` composition
adapts protected enrollment, not portable configuration. The setup library's
optional `host-runtime`
feature implements the runtime policy traits directly: each admission reacquires
the explicitly selected credential store and returns an owned enrollment lease,
not cached authority. The app wrapper delegates; its default remains native vault
with no file-store fallback. Owned native fixtures cover
control routing/retry without frontend leases. Explicit headless `run` consumes
these modules only after authenticated release provenance, current protected
account/host identity and separate local owner approval. That source integration
does not establish signed native-service or first-connect acceptance.
The `host_terminal` module pairs bounded binary terminal framing with
an immutable client handle and an opt-in actual native attachment actor. Recovery
adoption and observed native writer admission precede Attached; exact temporary
leases distinguish intentional detach from transport loss. FIFO output turns
are released before network I/O; current authorization can cancel slow output.
The native actor retains original opaque generation/attachment authority, and
freshly checks an intentional-detach receipt before publishing stopped success;
no client lease fields can mint replacement authority.
`HostRuntime` consumes proof on each actual signed SSH stream and pairs these
control/terminal adapters over the same protected backend. Only that paired
constructor advertises current lifecycle capabilities and the backend-reserved
issuer; standalone control stays inventory-only. No claimed issuer or cached
permission can replace protected current admission. Every frame/action and output
publication revalidates the opaque attachment against fresh protected state.
Key/host/session revocation closes the remote and native attachment, not the
persistent daemon. A 20-second heartbeat renews the daemon's 60-second connected
temporary deadline; unexpected closure uses original-authority TransportLost,
leaving grace/expiry daemon-owned. Persistent unticketed local attachment remains
available; temporary attachment always requires its actual ticket.
Independent exact-head consumer review, native-vault/first-connect/service
qualification and fair control-under-load remain gates; legacy exact-generation
relay and independent grants are unchanged.

The [ADR 0084 stage-1 contract](docs/host-protocol.md) introduces the small
`festerm-host-protocol` serde/UUID crate for shared HOST identities, bounded
messages, authorized pagination and reservation/freshness helpers. This avoids
a sessiond-to-bridge dependency cycle; it has no PTY, GUI, cloud or native-store
dependency. The bridge's non-default `owner-auth` feature provides enrolled-owner
Ed25519 challenge/admission primitives, not provider identity or a listener.
Legacy bearer connectivity is not principal proof. Protected enrollment,
pinned-channel integration and independent security review are mandatory before
new remote capabilities; all new capabilities default denied. Stage 2 adds
`festerm-sessiond -> festerm-host-protocol`, never a dependency on the bridge.
Its local `host_control::SessionControl` journals owner/logical identity,
immutable launch intent, reserved generations, operation results and revocation
revisions through the existing private no-follow/atomic storage boundary. It
uses the real detached native launcher and single-writer daemon admission.
HOST reservations require their exact native launch journal, preserving
ADR 0085's refusal to adopt an unrelated live generation from metadata alone.
Read-only private native checkpoint snapshots do not hold the mutation lock
during file/ACL I/O; serialized mutation CAS retains its validated directory
handle across publication. Private global-registry mutations compute their
change once under the lock, prepare and sync a private checkpoint after releasing
it, then reacquire it to revalidate before atomic publication. Disjoint concurrent
changes rebase without replaying effects, with at most four preparation attempts;
a conflicting same-name lineage refuses rather than replacing another generation.
The directory stays retained through preparation/publication; final directory sync
runs outside the lock. Abandoned checkpoints remove only their owned staging file:
Unix retains its directory-relative name, Windows its exact open file handle.
Legacy nonprivate reads and mutations remain locked.
Read-only reconciliation cannot create a fresh reservation. A durable issuance
namespace bounds the entire 128-entry operation ledger without receipt archives;
old captured scopes become Stale, never fresh creation. The 64 logical records
use a fixed native slot pool, with generation artifacts reclaimed only after
sealed supervision and positive native absence proof. Capacity reclamation takes
only one requesting-owner known-Absent record, never another owner's or a revoked
fence. Revoked live shells remain accounted and locally manageable; exact local
end and new explicit consent are required before retiring their fence.
Namespace publication reauthorizes the actual post-rotation epoch under the
same reservation and retains that policy guard through the durable checkpoint.
At capacity, authorized inventory/inspection may first reconcile owned dead
Running/Ending records, followed by retry of Busy with the original operation.
Opaque attachment authorities retain current policy and per-generation
grant/writer/liveness guards across nonblocking local actions, without parsing or
locking the global journal. Intentional detach yields a publication-only receipt,
not renewed terminal access. Missing/unsupported authority journals fail closed;
there is no automatic namespace reset or prerelease-state migration.
The daemon's own loop enforces temporary heartbeat/grace deadlines from
generation/lease-fenced private records, independently of bridge/client lifetime.
Temporary ownership changes commit beside the writer replacement only after
recovery adoption; a pending attachment is not an Attached success. Persistent
shells never acquire temporary cleanup merely through attachment. Endpoint
routing must not stall an existing writer while a candidate adopts: the native
daemon queues one candidate, bounds its post-snapshot output/control delta to
512 KiB including queue allocation headroom, independently of the live 64-frame
queue, and cancels only that candidate on saturation. Committed delta drains
before newer output. New-client admission/refusal errors cannot terminate the
daemon or persistent PTY; listener errors still propagate. The candidate's input
worker waits for the daemon's actual writer commit before accepting commands.
The existing writer remains usable until that commit. Endpoint
routing, protected enrollment/channel integration, migration and native
reboot/device qualification remain gated; this is not remote exposure.
Legacy bearer connectivity is not principal proof. The non-default
`festerm-host-setup/owner-enrollment` feature persists exact-installation
owner/key/account policy under the lifecycle lease, with compact native-vault
revision/digest fencing, durable prepared-record forward recovery and monotonic
revocation revisions. Bridge `ClientOwnerKey` uses opaque credential references;
native storage remains default and GUI policy unchanged. An explicit headless-only
protected-file credential/anchor backend binds origin/storage/installation;
whole protected-store/home rollback is not resisted by that weaker mode.
Enrollment does not enable access or creation; settings deny masks creation
without blocking key revocation or permission removal.
Persistent consent contains no runtime issuer. Its distinct protected-permission
verifier masks creation until an immutable projection receives the current issuer
inside protected backend admission; public runtime policy validation remains
strict. A projection retains its receiver's in-memory issuer watermark without
changing permission epochs or enrolled keys. Fresh enrollment reloads have no
such watermark; rollback protection across reservations belongs to the protected
durable backend, not cached owner policy.
Vendored russh now derives immutable bindings from its signed original session
identifier and role-ordered actual channel IDs, excluding no-KEX outer relays.
Client signing requires its privately captured actual pin and matching current
stream; server authentication transfers that stream and owner/guard together.
The opt-in bridge `host_runtime` composes that exact stream with bounded owner
challenge/proof framing, then a control or terminal role. Its host shares one
aggregate budget and FIFO output scheduler; protected policy work retains client
admission through cancellation. Paired `HostRuntime` advertises protected
lifecycle support/current backend issuer; standalone `NativeControl` remains
inventory-only. Separately proven terminal channels use the exact-generation
adoption/receipt adapter. No raw socket, principal string or fabricated owner can enter the
runtime. The portable client handshake creates no key and changes no trust.
The default headless build now composes that runtime through explicit foreground
setup/approval/run with the fixed protected adapter and corrected session source.
There is no automatic listener or portable-import authority; current permissions
remain default denied. Independent combined-source review, real release/native
vault/service and client first-connect/device qualification remain mandatory.
The enrollment/channel helpers alone grant no routing, creation, migration or
lifetime authority; those effects require the separate current protected backend.
The app's peer profile and `ApplicationSession::DevTunnel` retain normal
Session/controller ownership. Its shared `remote_files` capability routes
detected and explicit file targets to live SFTP or negotiated bridge reads;
provider/session-bound snapshots are not local document authority.
Default desktop builds include the experiment; the current mobile host does
not integrate it. `festerm-tunnel-client` owns portable native GitHub device
authorization/refresh and bounded official-service discovery using rustls,
without CLI, browser implementation, native FFI, daemon or GUI dependencies.
The host opens the fixed system-browser device URL and supplies native secure
storage. The app serializes credential rotation, validates profile/account
bindings before publication and asks Cancel-default account/host consent.
OAuth identity/refresh and scoped connection credentials use distinct opaque
references; only scoped connect authority reaches the session runtime.
Read-only pinned bridge discovery precedes atomic daemon admission.
Project registration/native service acceptance and future iOS runtime/Keychain
integration remain gated by [#429](https://github.com/fes/fesTerm/issues/429).

## Open Questions

- Exact async runtime and cancellation model.
- Parser implementation strategy and whether to use any parsing crate.
- Unicode width and grapheme segmentation sources.
- M9 implementation follows [ADR 0017](docs/adr/0017-bounded-logical-scrollback-and-anchored-viewports.md);
  viewport projection and efficient reflow data structures remain to be proven.
- Concrete rendering cache and shaping architecture.
- Configuration migration policy before a stable release.
- Platform serial-device discovery, permissions, and reconnect policy for the
  later first-class serial transport.
- Future scripting/MCP boundaries are proposed in
  [ADR 0065](docs/adr/0065-mcp-and-javascript-automation.md) and the
  [automation design](docs/automation-design.md): JavaScript, an application-owned
  authority broker, and platform-qualified isolated execution. These are design
  proposals, not implemented crate boundaries. Plugin trust remains separate.


## Opt-in mobile SSH composition (experimental)

Campaign #422/#424 extends the isolated mobile root under ADR 0042's
architecturally reviewed live-SSH extension, without native/product acceptance.
The `live-ssh` feature adds the existing session and SSH
crates, never the desktop root/PTY/sessiond/updater. The app is the sole
presented terminal writer and processes at most 32 session events per frame.
The pump runs even while Files/README is selected, once per frame rather than
per layout pass. The worker notifier wakes the UI; bounded backend queues own payload admission.
Encoded keyboard input and core protocol replies use that same admission path.
Refusal retires the connection with no automatic replay, and one pending resize
coalesces geometry changes. Replacement drops prompt/transport ownership and
rejects stale work. A main-thread suspension handle lets the native callback
retire plain-SSH transport and wipe sensitive prompts without a redraw;
it never mutates the terminal. Existing SshSession Drop is nonblocking.
Established mobile Dev Tunnel attachments are retained instead (ADR 0057):
active and parked tabs gate input until an attachment-owned asynchronous
authenticated-host ping completes within 8 seconds. Failure uses the existing
bounded same-client recovery, pinned to the original durable generation.
Cancellation/replacement invalidates late results. App lock/privacy remains
independent; neither transport retention nor a healthy ping unlocks content.

Transient zeroizing authentication and memory-only host pins stay outside
configuration; iOS persistent native storage remains Unsupported. This changes
the mobile composition dependency graph and requires the ADR/architecture
review before merge. It does not accept device networking, native input or
physical iOS feasibility. Desktop contracts are unchanged.
