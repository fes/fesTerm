# ADR 0088: JavaScript Terminal Program Sessions

- **Status:** Proposed
- **Date:** 2026-11-17
- **Supersedes:** None; proposes a separate application execution mode alongside ADR 0065
- **Scope classification:** Deferred product capability; owner-requested design and PR only
- **Related:** ADRs [0004](0004-componentized-testable-terminal-core.md),
  [0010](0010-preserve-future-extension-seams.md),
  [0014](0014-window-workspace-tab-session-ownership.md),
  [0033](0033-moving-tabs-between-windows.md),
  [0065](0065-mcp-and-javascript-automation.md);
  [application command model](../application-command-model.md)

## Context

The owner asked what fesTerm would need to run a JavaScript port of the BBS
game TradeWars 2002 directly in a new session tab. An ANSI terminal game needs
to receive that tab's input, produce its output, retain game state and remain
alive through a normal play session. It need not start a shell, open a local
Telnet listener or connect through SSH to accomplish those tasks.

fesTerm's existing JavaScript support supplies a useful interpreter and
confinement foundation, but its public host API serves a different purpose:
an approved workflow observes and controls an existing terminal. In particular,
`fesTerm.sendText()` sends input to a target session; it does not print text to
the workflow's own display. Reading a rendered screen cannot substitute for
receiving the player's input stream.

This decision is required because a long-lived program introduces new host
capabilities, storage ownership and resource policies. Merely adding a write
method to the automation adapter would conflate shell control with display
output and leave input, save consistency and worker lifetime unresolved.

### Existing implementation and gaps

The following baseline describes `main` at `1084c6ad` when this proposal was
written, not a claim that every platform has shipping qualification:

| Area | Existing seam | Missing for a terminal program |
| --- | --- | --- |
| JavaScript | [`festerm-automation-js`](../../crates/festerm-automation-js/README.md), pinned `rquickjs = 0.14.0` / QuickJS-NG; static modules, Promises and async/await | A distinct program entry contract and host API |
| Host calls | [`HostRequest`](../../crates/festerm-automation-js/src/protocol.rs): observation, terminal reads/waits, input, profile open and workflow invocation schemas | Own-session output, input/resize delivery, timers, assets and saves; schema presence alone never authorizes a call |
| Worker policy | Isolated supervised process; 64 MiB engine heap, bounded stack and platform-specific 128 MiB process policy; at most 300,000 ms/run, 5,000 ms cumulative CPU and 1,024 host calls | Program mode supplies separate profile negotiation and replenishing active-turn budgets without changing this automation policy |
| Native limits | [`platform.rs`](../../crates/festerm-automation-js/src/platform.rs) and [`windows.rs`](../../crates/festerm-automation-js/src/windows.rs) enforce cumulative CPU ceilings as well as engine checks | Program-mode Linux/macOS/Windows hard fallback and parent-owned rolling enforcement, pending packaged qualification |
| Desktop workflow | [`automation/javascript.rs`](../../app/festerm/src/automation/javascript.rs) selects a local PTY target; current run is capped at 295,000 ms and terminal sends require separate input review | A launch into a new terminal without selecting or controlling a shell |
| Session backend | [`Session`](../../crates/festerm-session/src/lib.rs) exchanges bounded input, output, resize, shutdown and lifecycle events | An implementation backed by the JS worker |
| Tab and display | [`ApplicationSession`](../../app/festerm/src/tabs.rs), `SessionTab` and [`SessionController`](../../app/festerm/src/session_controller.rs) already compose ordinary terminal sessions | A program variant, typed launch intent, capability policy and metadata |
| Storage | Worker has no ambient filesystem, environment, secret-store or network APIs | Private host-mediated assets and transactional game saves |

The existing interpreter is neither Node.js nor a browser. A port that assumes
`require`, npm installation, DOM, `fetch`, `process`, filesystem calls or
browser timers needs adaptation. This ADR does not promise those environments
or compatibility with an unspecified existing JavaScript port.

## Decision

Introduce an opt-in **JavaScript terminal program** execution mode, with
TradeWars as the motivating application. Reuse qualified engine, immutable
source-intake and supervision components where their contracts fit. Give this
mode its own versioned worker protocol, capability model and lifetime policy;
do not relax the existing automation workflow policy globally.

The first delivery target is a local, offline, single-player ANSI game on
qualified desktop platforms. This ADR describes host support, not the game's
rules implementation or authorization to redistribute original code/assets.
Game source and asset provenance must be established before distribution.
Shared universes, remote players and unattended maintenance are separate work.

### 1. Preserve terminal and command ownership

Add a backend provisionally named `JavaScriptSession` implementing `Session`,
and an `ApplicationSession::JavaScript` variant in the composition root.
Keep JavaScript and GUI dependencies out of `festerm-core` and the common
session contract. A focused runtime/backend crate may depend on
`festerm-session` and reusable JS components; the application injects scoped
host services and owns launch authorization. Exact crate extraction is an
implementation review decision, not permission for a broad automation rewrite.

```text
Launcher / profile / palette
        -> typed application command and reviewed program launch
        -> SessionTab + SessionController + JavaScriptSession

keyboard / paste -> normal core encoding -> bounded session input -> JS worker
JS worker -> bounded SessionEvent::Output -> SessionController -> Terminal
resize / close -> existing session lifecycle -> worker protocol
assets / saves / timers -> scoped trusted host services -> typed worker results
```

The app remains the single writer of the presented terminal. No JS code,
worker or backend receives a mutable `Terminal`, egui object or renderer
handle. ANSI/VT semantics and query generation remain in the core. The normal
global ingest budget, per-session fairness and event notifier apply to game
output as they do to other sessions. Do not wake the UI by periodic polling.

Human input is ordinary input to this program; it is not automation taking
over another principal's shell. It therefore does not require approval for
each keystroke or each screen write. External automation of a game tab, if
later supported, still requires its own ADR 0065 enrollment and input lease.
Launching a game does not enroll it or grant control of any existing session.

### 2. Define an own-session terminal API

The proposed entry is `export async function main({ terminal, assets, storage,
clock, params })`, with validated immutable launch parameters and one fresh
runtime per launch. Its Promise remains pending while the game is running.
An awaited input/event read suspends execution; it does not poll. Returning
ends the program. No source-controlled callback or live host object crosses
IPC; the worker exposes local wrappers around typed messages.

The following names illustrate the proposed v1 contract. They are not existing
methods or final wire schemas:

| API | Proposed semantics |
| --- | --- |
| `terminal.write(text)` | UTF-8 text including supported ANSI sequences; resolves when accepted into the bounded output path, not when painted |
| `terminal.events()` | One ordered async consumer (`next()`) of input bytes, accepted resize events and cooperative close requests; the repository-owned synchronous callback fixture remains compatible |
| `terminal.size` | Immutable initial columns/rows; subsequent size changes arrive as ordered resize events |
| `assets.read(id)` | Bounded read of an exact read-only asset included in the reviewed bundle; rejects unknown or oversize inventory entries |
| `storage.load(slot)` | Load a private save with schema metadata and an opaque revision |
| `storage.commit(slot, expectedRevision, value, operationId)` | Atomically publish one bounded save envelope with conflict detection and recoverable outcome |
| `clock.now()` | Host wall-clock Unix milliseconds |
| `clock.sleep(ms)` | Monotonic delay bounded to 60,000 ms; cancellation/fence revocation cuts the wait |

The accepted host-services slice implements only `assets.read`, `clock.now`
and `clock.sleep`. `params` is currently an empty frozen compatibility object;
reviewed launch payloads are deferred. The ambient `Date` API is unavailable,
so wall time crosses only the explicitly reviewed `clock.now` capability. It does **not**
implement saves, a file picker, arbitrary paths, network/filesystem access or
general host timers. The repository-owned async fixture uses the same
terminal/event ordering as the synchronous echo fixture while exercising a
frozen asset, one sleep and split UTF-8 input.

The save integration adds exactly `storage.load(slot)` and
`storage.commit(slot, expectedRevision, schemaVersion, value, operationId)`.
Opaque slots are application-bound: no path or handle crosses the worker.
Launch parameters are immutable and host-created with explicit `new` or
`resume` authority. Save frames stay strictly below the framing ceiling.

Input is a byte stream after the existing keyboard/IME/paste encoding path,
not raw OS keydown events. Chunks may split UTF-8, escape sequences and pasted
commands; the port must decode incrementally. Document Enter, Backspace,
Escape, control keys, bracketed paste and any enabled mouse reporting. Reuse
terminal modes and normal paste review rather than creating a second native
keyboard implementation. A line editor or `readLine` helper can be a bundled
JS library; host-side line editing is not required for v1.

The existing session input path also carries terminal query replies. For v1,
retain this terminal-stream behavior: the program's input decoder must consume
device/status replies as protocol data rather than player commands. Do not
promise a separate human-input or paste-origin event that `Session` does not
currently carry. If a future API requires origin tags, review an additive
controller/session seam separately. Initial terminal size and resize events
avoid a need to query the terminal just to discover its dimensions. Do not
claim backend reply ownership or add a second terminal mirror for this mode.

Resize acceptance and generated output must have defined ordering. Every
accepted resize has a monotonic sequence and is delivered in that order;
the program redraws for the latest delivered dimensions. Use the normal
terminal resize path and document any coalescing before acceptance. A fixed
80-by-25 game layout must give a useful small-window message or adapt; it must
not silently change unrelated tabs' geometry.

Output is UTF-8. Convert legacy CP437 artwork into reviewed Unicode assets or
provide an explicit game-side conversion library. Do not add a global legacy
decoder or change existing terminal encoding to accommodate one game.
Qualify box drawing, colors, cursor movement, erase, wrapping and supported
screen-buffer usage with deterministic fixtures.

### 3. Extend the worker protocol without weakening isolation

Program launch selects a distinct protocol/mode before source execution.
Version its handshake, schemas, maximum sizes and available capabilities;
refuse incompatible peers rather than falling back to automation execution.
The host binds each message to an exact program session and worker generation.
Late output or save results from an old worker cannot reach a restarted tab.

The program manifest declares its kind/API version, entry and exact module
graph, display metadata, typed parameter schema, read-only asset inventory
with lengths/digests, save-schema compatibility and requested capability maxima.
Freeze asset bytes as well as source before review; rereading a mutable path
after approval cannot substitute different data. Preserve separate source,
asset, frame and aggregate intake limits. A manifest's claimed program identity
is descriptive until the trusted installation/launch policy binds it to a
storage namespace. No declared capability is itself a grant.

Add typed messages for output, input, resize, delay completion, asset reads,
save operations and cooperative close. Do not reuse `SendText` for display
output. Define bounded outstanding calls, frame sizes, per-direction byte and
event capacities, and completion/error semantics before implementation.
Large assets and saves require bounded streaming or an explicit smaller limit;
they cannot bypass the existing 65,536-byte automation framing ceiling by
being embedded in oversized JSON messages.

The current implementation accepts at most eight program host calls
outstanding. Asset inventory and bytes are frozen before review, bounded to
12 KiB per asset, and covered by the bundle digest. Typed host requests/results
carry the exact worker generation and call ID. Mismatched, duplicate or late
results are protocol failures. Genuine host wait time does not consume the
JavaScript computation deadline, while cancellation, child exit and native CPU
evidence remain actively monitored. Result delivery re-enters the
application-owned generation/privilege fence at the physical IPC write.

`write` backpressure must suspend its Promise or reject with a typed error,
never drop arbitrary bytes from an ANSI sequence. Preserve order across
accepted writes, and account for buffered output on both sides of IPC.
Stop reading worker output when the session queue is full. Bound human input
and expose pressure through the existing session metrics/errors. Reserve a
bounded control path for cancel/close so saturated data queues cannot prevent
shutdown. Reject flooding by unawaited calls before it allocates unbounded
Promise state. A malicious program can ignore its API; supervision must still
kill and reap it without blocking the UI.

The new host API is an authority boundary, not just an engine object. Bind its
capabilities to the approved bundle digest, parameters, session generation,
asset set, save namespace and lifetime. Reuse existing broker primitives
where applicable, but add explicit own-session capabilities rather than
manufacturing a shell-control grant. A worker has no target-discovery,
arbitrary profile-open, workflow-invocation or other-tab read/write APIs in
this mode. Receiving an arbitrary target token cannot expand authority.

Authorization must hold both when a host operation is admitted and when its
result is delivered. Input, save data, asset chunks and other host results use
an application-owned delivery fence bound to the exact live worker generation,
capability epoch and privacy state. Recheck at each nonblocking IPC write,
using the existing `DisclosureGuard`/admission pattern or a separately qualified
equivalent. Revocation discards queued undelivered results; an old authorization
check or a queued acknowledgement is not permission for later disclosure.
Already admitted storage publication may finish without delivering its result
to a revoked worker; resolve that outcome through the trusted save service.

Keep source snapshots immutable, validate the complete module graph and
freeze the capabilities and assets with the launch. No dynamic package
download, native extensions, arbitrary filesystem loader, eval fallback or
ambient network is required. A bundle update is a new digest requiring a new
launch review; it does not replace code inside a running game.

Treat terminal escape output as untrusted too. Audit core-generated actions
and application handlers: a game tab must not inherit a local-shell working
directory, local-file-link authority, automatic clipboard access, credential
access or executable launch privileges merely because it is local. Titles and
links remain bounded untrusted presentation, subject to application policy.
Any host effect outside ordinary rendering needs its own explicit policy;
unimplemented/denied OSC features stay denied. Retain ordinary user-initiated
copy, selection and consent-gated recording behavior without granting program
clipboard or screen-reading access.

### 4. Qualify an interactive lifetime and resource policy

The automation limits are deliberately short and cumulative. Increasing only
`MAX_RUN_MS`, renewing an authorization grant or resetting a JS counter cannot
overcome the native cumulative CPU limit. This mode therefore needs a reviewed
platform policy as well as changes to the engine and supervisor.

Use a persistent isolated worker per live game session, retaining JS state
between inputs. Keep finite engine heap, stack, process memory, IPC, asset and
save quotas. Start qualification with the existing memory ceilings; measure
representative universe sizes, parsing and serialization peaks before deciding
whether any increase is necessary. Idle input waits must not consume CPU or
regularly wake every tab.

Replace the lifetime host-call count with bounded outstanding work and
rate/window budgets for this mode only. Enforce finite runnable work per turn
and over a rolling interval using monotonic process CPU accounting, engine
interrupt checks and an independent supervisor capable of forced termination.
Bound timers and their maximum delay; waiting for input/storage/timers does not
consume an active-computation deadline, but must not conceal a spinning engine.
Count microtasks, serialization and native engine execution in the work budget.
Do not trust a worker's self-reported idle state or allow it to renew its own
resource allowance. Universe generation may require chunked computation or a
separately bounded startup budget, with the same cancellation guarantees.

The default-off developer process policy now uses explicit, separate profiles
(`linux-seccomp-stdio-program-rolling-v1`,
`macos-seatbelt-program-rolling-v1`, and
`windows-appcontainer-program-job-v1`). It retains the automation mode's
128 MiB process policy, no filesystem/network/process authority, descriptor
closure, no core files and one-process boundary. The program policy has a
finite 3,600-second native CPU fallback, a 200 ms active CPU / 1 second wall
budget per turn, a 250 ms CPU budget in each rolling 1-second window and a
500 ms close cut. Automation continues to use its original profiles and
5-second cumulative CPU ceiling.

QuickJS interrupt checks enforce the active-turn limits. Independently, the
parent samples process CPU every 10 ms (`/proc` on Linux,
`proc_pid_rusage` on macOS and `GetProcessTimes` on Windows), expires old
samples, and force-terminates/reaps a worker that exhausts the rolling window.
Only CPU deltas enter the rolling window, and program-mode input waits block on
the existing inherited pipe instead of polling. Thus idle input does not
consume active work and a later command can use replenished capacity. Missing
or regressing CPU evidence and cleanup failure fail closed. The finite native
ceiling is a last resort if all higher-level accounting fails, not a grant to
run for an hour.

The application owns one process-wide lifecycle/quota coordinator shared by
all tabs and windows. Its explicit developer default admits at most four
pending, live or closing programs and accounts 128 MiB for each (512 MiB
total). The process supervisor independently enforces the aggregate count and
memory proxy before spawning. Permits release on every error, completion,
cancellation and reap path. This is finite accounting, not aggregate RSS or
CPU measurement, and does not change automation-worker admission.

These values close the source-level runaway policy gap, not the launch
qualification gate. Record measured normal workloads, hostile-loop results,
aggregate multi-tab limits and each packaged native enforcement mechanism
before enablement. Existing kernel/job cumulative limits must not be changed
for automation mode. If a platform cannot qualify the extended-lifetime
profile, keep program mode unavailable there. Do not silently run in-process
or impose an undocumented five-minute exit.

Application lock, suspension or capability revocation cuts new host-operation
admission immediately when detected, including save reads and input delivery.
On a detected privacy cut, terminate the worker and retain the last committed
checkpoint; a later explicit resume creates a fresh worker. This conservative
v1 policy avoids inventing a trusted pause protocol. Already admitted storage
publication may finish; report its real outcome. Existing OS event-detection
timing limitations still apply. Do not claim synchronous knowledge of lock or
revoke acknowledged effects retroactively.

Normal tab, window and application close cuts new input first, allows the
500 ms cooperative path only for a bounded same-universe checkpoint commit,
then settles or reports the operation, forces termination if needed, joins the
worker and releases the writer. App quit observes one aggregate cleanup
deadline rather than waiting once per program. Privacy loss instead cuts
storage, input and output immediately, discards queued output, skips
checkpoint close and terminates/reaps. An uncompleted lease or reap is an
explicit cleanup failure, never an inferred successful save. The cooperative
and force paths are bounded for the worker/control plane. A local storage
operation that remains inside uninterruptible native I/O cannot be forcibly
resolved without an uncertain publication policy; quit remains cancelled with
the writer retained until that operation returns. Packaged filesystem timing
must be qualified before enablement rather than claiming a universal I/O
deadline.

### 5. Supply private assets and transactional saves

Add an application-owned private local data service. Workers receive opaque
asset/slot identifiers, not host paths or directory handles. Validate namespace
components, ownership, links and resource limits through a reusable trusted
storage boundary. Storage I/O and serialization that can block the GUI belong
on bounded workers. Do not synchronously save on the UI thread.

The host assigns each installed program a stable storage identity and each
universe a separate instance identity. Bundle digests identify executable
versions, not an automatic new save directory on every update. A reviewed
update must explicitly retain the same trusted program identity to access an
existing namespace; a new bundle cannot claim another game's ID in its
manifest. The selected save/universe is part of launch review.

For v1, use one versioned save envelope per universe containing all state
that must commit together: sectors, players, ships, ports, turn counters,
maintenance markers and RNG state as appropriate to the port. Game logic owns
the schema and invariants; the host treats payloads as bounded untrusted data.
Atomic replacement of unrelated files individually is not a universe
transaction. If whole-envelope saves are too large or slow, a transactional
database design needs separate measurement and review.

Hold an exclusive writer lease across processes for that universe, and check
the expected save revision at commit. Opening another tab/instance must refuse
a second writer or explicitly create a separate universe; it must not silently
overwrite a live game's state. Bind the lease to actual host ownership, not a
PID file or JS-supplied session name alone. Release after admitted writes have
settled and the worker has stopped, including crash cleanup.

Publish using a protected temporary file, required durability steps and atomic
replacement on a qualified local filesystem. Define acknowledgement as durable
publication under that platform's documented guarantees, distinguish conflicts,
disk-full, corrupt/future saves and uncertain publication, and retain the last
valid checkpoint on failure. No automatic reset after a load error. Operation
IDs and a payload digest retained atomically with the envelope let the host
resolve an interrupted commit's outcome without publishing a different state;
serialize commits per slot and refuse new publication while the previous
operation is uncertain. Bound retained operation metadata. Explicitly separate
ordinary process-crash recovery from stronger power-loss guarantees.

Checkpoint at meaningful committed game actions, not only during close. After
a restart, resume from the last confirmed save and regenerate the screen;
terminal scrollback is not authoritative game state. Versioned migrations must
validate before publication and retain a recoverable previous save. A changed
bundle cannot silently run a migration during workspace restoration. Import,
export, delete and backup policy are explicit user operations; save data does
not enter ordinary config, workspace metadata, logs or cloud synchronization.

The implementation reviews schema 2 as current and schema 1 as approved for
explicit migration. Future, corrupt and unapproved schemas refuse; downward
publication refuses. Resume publishes a durable pre-migration envelope before
the replacement. New/refused-existing and resume/refused-missing are distinct
launch results, and the exclusive writer is held for the runtime lifetime.

Unexpected worker exit is observed through a supervisor termination wake,
drives session cleanup, drops the runtime-owned writer and allows a later
explicit Resume to start a fresh generation. The portable storage fixture
proves that reopening after release retains only a confirmed save; packaged
process-crash and power-loss evidence remains native qualification. Old
input, queued output, host calls and terminal contents are not replayed.

### 6. Define clocks, randomness and game maintenance

Expose separate wall time and monotonic delays, with bounded cancellation and
test injection. Document milliseconds/epoch, timezone/calendar policy and
behavior when the system clock moves backward or forward. Daily turns and
maintenance are game rules, not generic timer callbacks supplied by fesTerm.
The game must store its last completed maintenance marker in the same atomic
transaction as the resulting changes, so restart or a missed timer does not
apply rewards/penalties twice.

For a local v1 game, perform bounded catch-up on explicit launch or the next
interaction; no guarantee exists that a closed or suspended tab runs work at
midnight. Persist a versioned seeded PRNG state if reproducible saves/replays
are required. This is not cryptographic randomness or cheat resistance;
players controlling the local machine can change its clock and save files.
An online authoritative economy needs server time and server-owned state.

### 7. Integrate launch, close and recovery into ordinary tabs

Add typed intents provisionally named `StartJavaScriptProgram`,
`RestartJavaScriptProgram` and existing close routing extensions. Launcher,
profile editor, palette and other invocation surfaces converge on the same
coordinator; widgets never spawn interpreters or manipulate save leases.

Launch review shows program/version, immutable digest, chosen universe,
parameters and requested own-tab/storage capabilities in understandable form.
Selecting, importing or saving a profile does not execute code. Retain the
existing static module/source review machinery where useful, but do not make
Settings' local-PTY workflow selector the game-playing surface. First delivery
can be explicit local-bundle launch; a download catalog is not a prerequisite.
Keep the feature opt-in and show an actionable unsupported-platform error.
The developer slice binds a `JavaScriptProgramLaunch` to the repository-owned
program's exact immutable digest, API version, exact terminal capability set
and one fresh worker generation. The central `StartJavaScriptProgram` command
creates that reviewed value; UI code cannot select a path, import a bundle or
convert an automation grant into program authority.

Use ordinary tab identity, title, status indicators and focus behavior. Move
between panes/windows transfers presentation without creating a second worker
or writer lease. Starting and failures remain visible on the originating tab.
`main` settlement closes admission immediately. Returning with outstanding
host calls produces `UnsettledWork`, not normal completion, preserving the
existing worker rule. A `write` already acknowledged as accepted may still
have display bytes queued; those bytes drain within a finite bound without
admitting another host call. Normal completion requires all host-call Promises
to have settled. Exceptions, resource exhaustion and protocol faults produce
a content-free failure while retaining the last durable save. Pending
unadmitted calls cannot survive completion. An admitted commit is settled or
reported uncertain, never changed into a success by worker exit.

Closing requests cooperative shutdown with a bounded opportunity to checkpoint,
then reaps through the supervisor. A close-request event is advisory: it cannot
force the game to save. The UI distinguishes last confirmed checkpoint,
unsaved progress, failed save and an in-flight uncertain commit; never display
"Saved" because the worker exited. If saving fails, offer the existing
application-level cancel/return path where still possible or explicit close
with loss. Forced quit/crash recovers only committed state. Application quit
uses a bounded total cleanup policy across game tabs, not an unbounded wait
per tab. No implicit worker respawn or input replay follows a crash.

During cooperative close, stop new player input and allow only the narrowly
retained same-universe checkpoint work until the host deadline; privacy loss
or revocation takes precedence and cuts that admission too. The host knows
confirmed revisions and pending commits, but cannot infer arbitrary dirty JS
state. Any program-reported dirty marker is advisory; absence of a marker is
not proof that all progress is durable. Surface the last confirmed checkpoint
and conservatively describe possible loss when the worker is unresponsive.

Workspace persistence records only program/profile identity and a local save
reference. Restoration presents a resumable placeholder; explicit resume
validates the installed digest, approved identity, save schema and platform
before creating a new worker. It does not serialize a QuickJS heap, rerun
initialization automatically or assume a game is a persistent `sessiond` shell.
Native session daemon wire/recovery schemas do not change for this local mode.

### 8. Keep multiplayer as a separate authority model

Multiple tabs do not create a safe shared universe. A future multiplayer
design needs one authoritative game service owning transactions, player
authentication, concurrent actions, disconnect semantics, server time,
maintenance, quotas and recovery independently of each terminal frontend.
The frontend might use an existing transport or a separately scoped protocol,
but arbitrary sockets and client-side save writes are not required by v1.

A loopback BBS server or external Node executable remains a valid way to host
a different deployment. It is not evidence that the confined own-tab JS
runtime supports multiplayer. Remote hosting, headless execution, unattended
schedules, package acquisition and public game discovery are outside this ADR.

## Alternatives considered

| Alternative | Assessment |
| --- | --- |
| Run the port under Node in an ordinary local PTY | Smallest route to a playable prototype today; existing tabs already work. Requires an external runtime and its authority/dependency model, rather than the requested integrated confined JS program mode. Keep this option available. |
| Run a BBS/game server and connect through a supported transport | Good fit for a persistent multiplayer universe. Adds server provisioning and networking to a local offline use case. It does not eliminate server-side game work. |
| Add output to the automation workflow object | Leaves raw input, storage and lifetime unsolved and risks conflating own-display writes with authority to control shells. Use a distinct program contract. |
| Give the worker filesystem/network/Node APIs | Unnecessary authority for local play; defeats the existing restricted host boundary and introduces a much larger compatibility/security surface. |
| Embed the interpreter in the GUI process | Simplifies calls but violates ADR 0065's separately confined untrusted-worker direction and exposes UI availability to game faults. |
| Use a WebView/canvas game tab | A different graphical application platform with a new renderer/accessibility model; unnecessary for an ANSI game and does not reuse the terminal/session contract. |
| Restart a short-lived worker after every command | Retains existing cumulative limits but requires explicit complete continuations and checkpoint serialization at every command. Async input, menus and uncommitted state make this a materially different game API. Reconsider if persistent confinement cannot be qualified. |

## Consequences

- The reusable result is a narrow terminal-program host, not a TradeWars-specific
  renderer or a general plugin platform. The game port still supplies gameplay,
  universe generation, menus/input decoding, balance and migrations.
- No existing terminal ownership invariant is removed. Proposed extensions are
  the JS host contract, native resource profile, private storage service and
  application launch/lifecycle policy. They require review before implementation
  and acceptance before enablement under the architecture-stability policy.
- Workflow bundles and approvals retain their existing meaning, short limits
  and default-off policy. Version the new program manifest/protocol explicitly;
  old workers/builds refuse unsupported program profiles rather than executing
  them as workflows or falling back to a shell.
- Save state is device-local, potentially private and application-managed.
  Profile/workspace schema changes must remain metadata-only and use existing
  strict versioning; no automatic configuration or save conversion is implied.
- CPU/memory/IPC and save latency need aggregate measurements, including several
  idle/background games beside active shell sessions. Large redraws cannot
  monopolize ingestion, and storage cannot block rendering or shutdown.
- macOS, Windows and Linux each need fresh native evidence for the extended
  worker and storage policy. Existing automation probes are a foundation, not
  qualification of this mode. iOS has no qualified separate worker here;
  Android also needs its own feasibility/security/lifecycle assessment before
  enablement. No mobile fallback is implied.

## Delivery and acceptance gates

1. **Contract and threat-model review:** approve the distinct manifest,
   capabilities, byte/event semantics, save identity/lease model, output-action
   restrictions and native interactive resource mechanisms. Record exact
   numeric quotas and compatibility negotiation. Do not enable execution yet.
2. **Minimal terminal vertical slice:** a repository-owned echo/menu program
   launches in a normal tab with no shell, receives incremental input/resizes,
   writes ANSI output and exits/reaps reliably under bounded pressure. Cover
   multiple tabs/windows and preserve ordinary shell behavior.
3. **Assets and durable state:** prove scoped immutable assets, cross-process
   writer exclusion, transactional save/revision behavior, conflict/fault
   recovery and explicit resume using a small deterministic universe fixture.
4. **TradeWars integration:** adapt or implement the JS game against this API;
   validate generation, trading, navigation, combat and maintenance rules in
   the game project. Use seeded input/output/save fixtures and establish code
   and asset distribution provenance. A working terminal host alone does not
   mean the game port is complete.
5. **Platform and usability qualification:** exercise the actual packaged
   confined worker, real local filesystems, lock/suspend/quit/crash behavior,
   resource exhaustion, keyboard/IME/paste and ANSI presentation on each
   supported desktop. Review native evidence before enabling that platform.

The current implementation branch closes none of the delivery gates. It
contains only a default-off developer vertical slice: one repository-owned
QuickJS echo program, ordinary session-tab composition, exact encoded
input/resize/output checks, bounded control and a typed reviewed launch.
The reviewed save fixture now adds opaque-slot load/commit, immutable start or
resume parameters, exclusive writer ownership and recoverable schema migration.
Per-program persistence is independently denied by default in Settings and is
keyed by the approved program digest, not a mutable display name or source path.
The reader may allow one bounded JSON byte envelope, choose a 4/8/12 KiB cap,
inspect its disk size, reveal the exact private directory, or delete its
confirmed payload while no writer is active. Disabling or deleting storage does
not alter a captured bundle, source file or another digest's namespace.
An application-owned delivery fence binds OS privacy/lock monitoring, launch
generation and cancellation. It is checked before worker IPC, before session
queue publication and when queued output is consumed; revocation stops and
reaps the process and suppresses queued terminal bytes. A separate
default-off process profile adds deterministic rolling-window and idle-
survival limits plus force-bounded hostile-input coverage while automation
retains its original 5-second policy. The lifecycle slice now shares one
application-owned aggregate coordinator across windows, moves live tabs
without re-creating workers/writers, bounds normal/quit cleanup, skips close
checkpointing on privacy loss, and refuses a fifth 128 MiB accounted program
before worker spawn. The branch does not make the mode generally enabled or
qualify a packaged production worker. Keep packaged/native
qualification, measured actual aggregate resource use, general launch review,
TradeWars and platform acceptance in separately reviewed delivery slices.

## Validation impact

- **Invariants introduced or changed:** Own-session capabilities,
  digest/capability-bound reviewed launches, generation-fenced worker
  messages, independent developer-runtime budgets, application-owned privacy
  delivery fencing and explicit close. The save slice adds private transactional
  ownership and explicit resume while preserving the single app
  terminal writer, core-owned protocol semantics, bounded queues, event-driven
  wakes, restricted host API, centralized commands and metadata-only workspaces.
  The lifecycle slice adds shared aggregate admission, one runtime through
  cross-window moves, privacy-first termination, bounded normal close/reap,
  explicit cleanup failure and idle-exit wakeup. Fresh-runtime Resume and
  storage release/reopen are modeled separately; digest-bound Settings policy,
  quota refusal, reveal and deletion are explicit. Real process-crash evidence
  remains pending.
- **GUI/action edges affected:** `JSP-01` launches only the repository-owned
  developer echo program through the typed command path; `JSP-02` carries
  ordinary encoded input, resize and output; `JSP-03` preserves close and
  backpressure behavior. `JSP-04` exercises reviewed start/resume, writer
  exclusion, save faults and schema migration; packaged/native filesystem
  qualification remains open. Revalidate existing `ROOT-03`, `ROOT-04`,
  `CLOSE-01`, `CLOSE-04` and
  applicable move/window/input edges without reinterpreting them as game policy.
- **Automated tests required:** The developer slice includes strict manifest
  and protocol bounds, stale-generation refusal, ordered QuickJS echo/resize,
  bounded backpressure, the reviewed-launch exact digest/capability/generation
  binding, worker reaping after revocation, queued-output suppression, 10 ms
  query gating, deterministic 200 ms turn-budget interruption, rolling
  CPU-window expiry, compliant cumulative work beyond five seconds,
  hostile-input forced termination, native-profile separation, idle survival
  and the app-level
  `javascript_program_launches_without_shell_or_automation_authority` route.
  Aggregate parallel supervisor admission, app refusal/release, privacy
  termination without checkpoint close, moved-tab adoption, source-window
  removal and multi-program aggregate quit are now covered.
  Add the following concrete paths to the trace relationships as the remaining
  delivery slices land:
  - `javascript_program_launches_without_shell_or_target_authority`;
  - `javascript_program_stream_input_handles_split_utf8_escape_and_replies`;
  - `javascript_program_resize_and_output_preserve_accepted_order`;
  - `javascript_program_backpressure_preserves_bytes_and_shutdown_progress`;
  - `javascript_program_stale_generation_cannot_publish_output_or_saves`;
  - `javascript_program_revocation_fences_each_queued_result_chunk`;
  - `javascript_program_return_with_unsettled_host_work_cannot_succeed`;
  - `javascript_program_cannot_invoke_automation_or_local_file_actions`;
  - `javascript_program_commit_is_atomic_revision_checked_and_recoverable`;
  - `javascript_program_universe_writer_is_exclusive_across_processes`;
  - `javascript_program_idle_waits_sleep_and_runaway_work_is_reaped`;
  - `javascript_program_lock_close_and_crash_retain_confirmed_checkpoint`;
  - `javascript_program_restore_requires_explicit_digest_and_save_review`;
  - `javascript_program_update_cannot_claim_foreign_storage_identity`.
  Add hostile-frame/source tests, save corruption/disk-full/publication-cut
  injection, aggregate quotas, incompatible-version refusals and ANSI golden
  fixtures. Game rules, seeded PRNG and maintenance idempotency need separate
  game-level tests. Existing workflow tests must prove their limits and grants
  have not expanded.
- **Native/manual evidence required:** `JSP-01` records the owner-run packaged,
  keyboard/IME/paste, multi-window, pressure, shutdown, privacy-lock and
  confinement checks required before enablement. Existing `AUTO-01` workflow
  consent evidence does not qualify this execution mode. Portable process
  tests exercise the new rolling/idleness/hostile behavior on the current
  host; macOS, Windows and Linux packaged profile qualification, real
  lock/suspend timing, packaged confinement and actual aggregate memory/CPU
  measurements remain open. GUI/native usability is also unqualified. Mobile
  remains deferred behind a separately qualified worker and lifecycle design.
- **Coverage superseded:** None. `validation/traceability.json` registers the
  implemented developer slice as partial and retains packaged confinement,
  aggregate quotas, lifecycle, save, resume and platform evidence as
  prerequisites.
