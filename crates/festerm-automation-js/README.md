# Isolated JavaScript worker

ADR 0065 P1–P3 adapter only. Nothing here enables scripting, grants authority,
connects to sessions, starts MCP/HTTP, watches files, or schedules jobs.
The application owns approval, authorization, concurrency, recipients, privacy
events, result retention, packaging/provenance, and default-off settings.

## Stable integration API

Public paths/signatures:

```text
snapshot::ApprovedBundle::new(SourceSnapshot) -> Result<ApprovedBundle, Error>
ApprovedBundle::{digest() -> &str, manifest() -> &Manifest,
                 snapshot() -> &SourceSnapshot,
                 validate_parameters(&Parameters) -> Result<(), Error>}
WorkerProgram::from_install_owned_absolute(&Path) -> Result<WorkerProgram, Error>
Supervisor::spawn(&WorkerProgram, &ApprovedBundle, Parameters, Limits)
    -> Result<Supervisor, Error>
Supervisor::try_event(&self) -> Result<Option<Event>, Error>
Supervisor::host_result(&self, id: u32, HostResult) -> Result<(), Error>
Supervisor::cancel(&self)
Supervisor::shutdown(self) -> Result<(), Error>
worker_entry(input: impl Read, output: impl Write) -> Result<(), Error>
platform::availability() -> Result<&'static str, Error>
cleanup_failed() -> bool
```

The composition root must branch on **exactly** `--automation-worker` before
initializing GUI, threads, credentials, networking, or other application state:

```rust,ignore
let mut args = std::env::args_os().skip(1);
if args.next().as_deref() == Some(std::ffi::OsStr::new("--automation-worker"))
    && args.next().is_none() {
    let result = festerm_automation_js::worker_entry(
        std::io::stdin().lock(), std::io::stdout().lock());
    std::process::exit(if result.is_ok() { 0 } else { 70 });
}
```

Use the install-owned absolute application executable (or the provided
`festerm-automation-js-worker` binary) as `WorkerProgram`. The caller verifies
install/signature provenance and protects every parent directory; this crate
rejects relative/noncanonical paths, nonfiles and group/world-writable Unix
executables. It never uses a shell or PATH lookup. Linux additionally refuses
an executable whose initial process mappings already exceed 128 MiB.

Events are `HostCall { id, request }`, `Finished { outcome }`, and `Failed(Error)`.
For **every** host call, authorize the live run, exact target generation,
readable sources and all reachable recipients in the trusted parent broker.
Only then dispatch through existing application/controller APIs and deliver a
typed `HostResult`. Method/schema validity is **not** authorization. Manifest
capabilities are only declared maxima and are never interpreted as grants here.
P2 observation-only callers should deny all write/open/invocation variants.

Use `cancel_with` to close broker admission synchronously before cancellation
on revocation, lock/suspension, caller disconnect, human takeover, or parent
return. Cap the broker run deadline at `Supervisor::deadline()`; the watchdog
closes worker admission and reaps independently at that deadline. Already
admitted effects cannot be undone. `cancel()` prevents delivery of queued new
host-call events; at most 250 ms cooperation precedes forced kill. The
independent watchdog does not depend on GUI polling or a busy admission gate.
`shutdown()` reports cleanup errors; drop also cleans up. An unreaped worker
or watchdog panic quarantines subsequent launches for the entire application
process; surface `cleanup_failed()` and require a clean restart. Reaping has a
2-second bound.

`Outcome::Completed { value }` contains **script-controlled authorized result
data**, not diagnostics. Gate retention/disclosure through the broker, just as
an observation. No script exception/stack/message is exposed. `Event` and
`Outcome` Debug redact payloads. Wire values and source/parameter structures
are not diagnostic records and must not be logged.

## Snapshot and language profile

Exact engine pin: **rquickjs = 0.14.0**, verified with `cargo info` and actual
builds. It embeds QuickJS-NG; this is not Node, a browser or a promise of full
latest-ECMAScript conformance. Qualified profile v1 includes static ES modules,
ordinary computation, Promises, async/await, JSON, typed arrays, regexes, and
an exported `main({ fesTerm, params })` (sync or async).

Bundles are constructed **only from owned UTF-8 strings**: `SourceSnapshot`
contains a strict `Manifest` and `BTreeMap<canonical_path, source>`. There is no
filesystem loader in this worker crate, bytecode, native loader or cwd lookup.
The separate, unintegrated workflow composition crate has a
[single-file local intake](../../docs/automation-design.md#user-selected-single-file-intake)
which opens every path component without following links and constructs a
frozen snapshot before approval. Module names are ASCII, case-distinct, sorted, root-
relative bundle names ending `.js`, e.g. `helpers/read.js`; imports use exactly
those names, not `./`, `..`, URLs or package aliases. The manifest module list
must exactly equal the reachable static graph, including re-exports/cycles.
Missing, extra, duplicate, unreachable and case-colliding modules are refused.

A real tree-sitter JavaScript syntax tree rejects dynamic imports, including
inside templates, and unsupported syntax; engine compilation remains the final
syntax check. Top-level await and import attributes are excluded from v1.
`eval`, `Function`, and ordinary/async/generator constructor access are removed
before source executes, so generated code cannot bypass import qualification.
There is no `require`, console, DOM, filesystem/environment, fetch, network,
process, package download or secret-store host API.

SHA-256 covers v1 canonical serde JSON: fixed struct field order, sorted map
keys, sorted manifest paths and capability set, exact source strings; no
whitespace. This includes the manifest's typed parameter schema and capability
maxima. **Actual parameter values are separately validated and moved into the
immutable per-launch message; the broker must bind their exact values to its
approval/grant.** Values are text, safe integers, booleans or opaque target
tokens, with no unknown/extra parameters. JSON maps reject duplicate keys.
Each run gets a new runtime and new process; no module/global cache survives.

Every `fesTerm` method returns a Promise, including malformed calls:

```js
export async function main({ fesTerm, params }) {
  const observation = await fesTerm.observe();
  const binding = observation.targets.find(item => item.target.id === params.target);
  if (!binding) throw new Error("target unavailable");
  const screen = await fesTerm.readScreen({
    target: binding.target,
    maxBytes: 4096
  });
  return { observation, screen };
}
```

Typed requests: `Observe`, `ReadScreen`, `ReadScrollback`, `GetSelection`,
`WaitForChange`, `SendText`, `SendKeys`, `Open`, `InvokeWorkflow`.
Wire method names/options are camelCase. Targets contain `id` and `generation`;
numeric parameters/revisions/generations must fit JavaScript's safe integer
range. Responses are `Observation`, `Text`, `Change`, `Accepted`, `Opened`,
`Workflow`, or a content-free typed `Error`. No executable callback crosses IPC.
Wait deadlines are at most 30 seconds; the parent implements waits from its
notification stream, not polling. This crate has no direct session/kernel access.

Root Promise settlement closes admission immediately. Outstanding host work
produces `UnsettledWork`, not a successful result. No remaining microtasks are
drained after completion, including while serializing the result. Unawaited
work that was already broker-admitted remains the broker's cleanup obligation.
After forwarding calls, the worker pauses JavaScript until each is either
rejected with a result or acknowledged after its exact broker admission.
An acknowledgement permits other JavaScript to proceed while an admitted
asynchronous call is still waiting for its result; it is not that result.
This prevents an unadmitted effect from racing the worker's completion cut.
The application must use the supervisor's bounded `with_admission` boundary,
prepare the exact operation, and consume its one-shot permit around the actual
broker/controller commit. The atomic claim orders that admission against
watchdog closure; an already claimed effect may physically execute later. If
the claim fails, abort the prepared request or cut the run in the broker. Use
`cancel_with` for the synchronous broker cut and still enforce all scopes,
recipients and lifecycle checks. A successful host-result/acknowledgement send
only queues a message, not a delivery receipt.

## Native policy and support

| Platform | Status |
|---|---|
| Linux ARM64, kernel 6.8, glibc (local Ubuntu VM) | Real engine + supervised subprocess + native hostile probes passed. |
| Linux x86-64 | Same implementation and runtime qualification; native x86-64 CI/distribution evidence still required before shipping enablement. |
| Other Linux architectures, or missing kernel prerequisites | Explicit unavailable, no fallback. |
| macOS | Explicit unavailable. Seatbelt alone cannot enforce the mandatory hard 128 MiB process ceiling; no advisory RSS limit or polling approximation is substituted. No unconfined production engine execution. |
| Windows | Explicit unavailable pending reviewed native AppContainer containment. A Job Object alone is insufficient. |
| iOS | Explicit unavailable: no qualified separate helper/stdio P1 transport. |

Linux qualification runs **before Ready/source reception/engine construction**:
cleared environment (only `MALLOC_ARENA_MAX=1` allowed), inherited pipe-only
stdio, parent-death SIGKILL, nondumpability, `close_range(2, UINT_MAX)` (requires
Linux 5.9+), hard `RLIMIT_AS=128 MiB`, hard cumulative `RLIMIT_CPU=5s`, no core
files/file growth/new descriptors, `no_new_privs`, seccomp-BPF TSYNC.
Unexpected setup failure disables that launch, not the policy.

The architecture-checked default-deny syscall allowlist permits only
read(stdin), write(stdout), non-executable memory allocation/protection,
private-process futex waits/wakes, local clocks/sleep, entropy for hash/engine
seeds, signal runtime support and exit. It forbids filesystem operations,
network/raw/Unix sockets, process creation/exec/signals to other processes,
debugging/process-memory access, shared futex IPC, shared memory/eventfd/pipe
creation, descriptor duplication, and resource/policy changes. No Landlock,
container profile, missing JS globals or absent privileges are assumed to
provide the boundary.

Every worker actually probes denied file read/write-open, TCP/raw/Unix socket
creation, clone/exec, ptrace/process memory, signals, executable mappings,
IPC, unexpected descriptors, shared futex operations and foreign clocks. It
requires `EPERM`; an outer policy or native privileges can also produce that
error, so these probes complement filter review and testing with the container
profile disabled. Process and signal probes target the worker itself. A native
mmap probe must fail with `ENOMEM` beyond the hard ceiling. Probes do not
modify files, contact services, or spawn grandchildren.

Limits: 64 MiB QuickJS heap, 1 MiB JS stack, 128 MiB process address space;
5s cumulative process CPU, at most 5 minutes/run. Engine interrupt callbacks
also check process CPU and wall deadlines. The native ceiling/watchdog covers
uncooperative regex/native calls. Frames are big-endian u32 length plus JSON,
1–65,536 bytes; source transfers once in at most 8,192-byte chunks with a
2-second loading deadline and recomputed digest. At most 64 outstanding calls,
1,024 calls/run, 16 KiB input writes, 1 MiB total source/64 modules, 64 KiB
manifest, 2 MiB encoded snapshot. Structured results have depth <=16,
<=4,096 nodes, <=256 array elements, <=64 object members, <=16 KiB strings,
and <=49,152 encoded bytes. Oversize/backpressure fails closed.

## Verified tests (2026-11-03)

- macOS: 12 unit + 5 subprocess/API tests passed; the unsupported-platform
  test actually launches the helper and verifies explicit unavailable/no script.
- Linux ARM64 / Ubuntu kernel 6.8.0-117 / Rust 1.97.1: 14 unit + 5 subprocess
  tests passed, including a compromised-native tight loop killed by the kernel's
  cumulative CPU ceiling, active native denial/memory probes, and setup failure.
- The same Linux suite passed with Docker's seccomp profile **disabled** for the
  owned qualification container, proving the crate's own filter supplies denial.
- Infinite loop, recursion, regex backtracking, allocation storm, microtask
  storm, unawaited host calls, private exceptions, cancellation and a watchdog
  with no application event polling are covered.
- `cargo clippy -p festerm-automation-js --all-targets -- -D warnings` passed
  on macOS and Linux; crate formatting passed.

The container/VM is test infrastructure, not a shipping confinement dependency.
Broader distro, native x86-64, release packaging, GUI grants and end-to-end
authority/lifecycle qualification remain parent-owned acceptance gates.
