# Windows test exit diagnostics

This is a validation-blocker diagnostic, not a product/runtime fix or a crash
upload feature. It preserves Cargo's tests, filters, package working directories,
default libtest concurrency, pixel assertions and nonzero failure behavior.
There is no retry, debugger replay, global serialization, installed application,
desktop input, registry/WER change, or process-name-based termination.

## The tracked residual failure

[#330](https://github.com/fes/fesTerm/issues/330) recorded run `37250376126`,
attempt 1, Windows job `111576603780`, at head
`748e97cc7530202ddbf133241dbfd3fc9e75e83b`. At `2026-10-05T01:15:10Z`,
`cargo test --workspace` reported `festerm-ui-egui` exit **2173 / 0x0000087d**
without an assertion or identified failing test. Artifact `11321066752`
contained only 28 PNGs. Attempt 2 passed at the same head: that is neither a
cause nor a repair. The separately diagnosed Direct2D `0xc0000005` wrapped-target
retirement repair (#336, superseded by #329) is not a repair of exit 2173.
The owner later accepted the intermittent exit as a separately tracked residual
and closed #330 so unrelated exact-head-green changes would not remain blocked
indefinitely. That disposition is not a causal diagnosis or repair; later
occurrences and their receipts remain evidence for further investigation.

## Required CI and local execution

Windows CI calibrates the runner with non-GPU deterministic fixtures, then uses
it for the workspace, retained-renderer and excluded font-atlas Cargo tests.
Linux/macOS commands and the existing level-zero CI dev/test debug-info policy
are unchanged. Native receipts retain module offsets when local symbols are
unavailable; local level-1 builds can provide PDB-backed symbol names without
changing the CI workload. Neither PDBs nor executables are uploaded by this
integration.

From a PowerShell session, pass an actual argument array:

```powershell
.\scripts\run-windows-tests.ps1 -CargoArguments @('test', '--workspace')
```

Use `-OutputDirectory <owned-directory>` and `-TimeoutSeconds <1..14400>` to
override `target\windows-test-diagnostics` and the 1800-second
**per-executable** deadline. The caller's `CARGO_TARGET_DIR` is respected. The
Python runner accepts only executables inside this checkout or that explicitly
selected build directory; it cannot attach to an existing PID. Unique receipt
names permit parallel Cargo invocations without overwriting evidence.

The implementation uses installed Windows `kernel32` debug APIs and System32
`dbghelp`, not a required SDK/CDB download. It reuses and extends the controlled
2173 C# exit-calibration fixture from `diagnose/windows-direct2d-native-crash`.
That older replay's CDB command files, raw register/stack/event-log transcripts
and debugger `q` exit semantics are deliberately not copied into CI.
The `feature/crash-exit-diagnostics` application journals remain local and
separate; their raw logs/panic reports are not safe automatic CI artifacts.

## Evidence and privacy boundary

Each root test process is launched under `DEBUG_ONLY_THIS_PROCESS`, suspended
until the debugger owns its startup. Descendants are **not** debugged or
recorded. The child receives a native Unicode environment-block copy with only
`_NO_DEBUG_HEAP=1` overridden: Windows must not enable its debugger-default heap
validation and change allocation behavior compared with ordinary, uninstrumented
tests. Hidden drive entries and UTF-16 values are preserved. The parent
environment and registry are untouched, and environment contents are never
included in receipts. The safe `debug_heap_policy` label records this policy.
The runner observes normal debug events, passes application
exceptions through unchanged, and captures a second-chance fault before
Windows terminates the process. One-shot entry probes at
`ntdll!RtlExitUserProcess` and `ntdll!NtTerminateProcess` capture a nonzero
exit/termination code and stacks; original instructions and the thread's PC
are restored before continuation. The debugger never substitutes its own
success exit for the test status.

Receipts contain source HEAD/tree/dirty flag, executable basename/SHA-256/size,
allowlisted GitHub run identity, root PID, timestamps, unsigned/signed/hex
Windows status, exception code/chance/address, termination API, thread IDs,
bounded PC/module-offset stacks, optional local symbol names, module basenames,
PE timestamp/image size and available PDB GUID/age (not its embedded path).
Symbols are looked up locally; environment symbol-server paths are ignored.
No symbol-server download is configured.

Ordinary test stdout/stderr still reaches the existing CI console. The runner
does **not** upload or persist that transcript. Its only parsed output is
libtest's compile-time test name/result lines. Raw memory, dumps, exception
parameters, arbitrary register values, stack arguments/locals, debug strings,
source/file paths, command arguments, environment dumps, machine/user names,
Application/WER event messages and installed-user journals are excluded.
Debugger memory reads are used internally for PE metadata/unwinding and probe
instruction restoration; those bytes are never written into receipts.

The `windows-native-test-metadata` artifact uploads only receipt JSONs, even
on failure, with seven-day retention. Existing snapshot artifacts remain
unchanged. Do not broaden the upload glob to PDBs, dumps or ordinary logs.

## Classification, bounds and exact limitations

- `passed`: original target status 0.
- `rust_test_failure`: status 101 without an observed second-chance exception.
  It is the libtest convention, not proof of which assertion failed.
- `native_exception`: observed second-chance exception; original status retained.
- `native_exit_unknown_cause`: other nonzero exit, including 2173; **not**
  automatically labeled an access violation or diagnosed cause.
- `timeout`: owned execution exceeded its deadline; runner status 124.
- `diagnostic_tool_unavailable`: required API/architecture unavailable; target
  is not started and runner status is 125, never a successful fallback.
- `runner_error`: diagnostic execution/persistence failed; status 125, never
  inferred success. Invalid CLI arguments return 2 before launching a target.

Cargo still returns its ordinary aggregate test failure status (usually 101);
the runner itself preserves the original target DWORD, including statuses above
`INT_MAX`. The receipt separates target status from timeout/runner status.
Optional unwinding/module/probe limitations are explicit within the receipt and
do not turn a failing target into a pass. Missing stack initialization or exit
probes marks capture `partial`, rather than claiming complete native evidence.

Capture currently supports Windows AMD64 with 64-bit Python. Stable parallel
libtest does not publish test-start/thread attribution: completed test names,
native threads and stacks are retained, but **no last-reported test is claimed
as the failing test**. Buffered output may be lost on abrupt exit. A thread in
optimized/managed/generated code may have only a PC; PDBs may be absent, and
module offset/GUID/hash metadata needs matching local binaries/symbols for
further analysis. Doctests and child executables started directly by a test do
not go through Cargo's target runner and are not covered.

Probes are one-shot and perturb scheduling like any debugger. A termination
from another process or kernel may provide only the root exit event, not a
caller stack. An earlier `NtTerminateProcess` invocation targeting a different
process can consume that probe; it is not proof that the root exited there.
No debugger can retrospectively establish the cause of the original run.

### Collector-induced syntax budget failure

The first cumulative gate at
`e7a9a902be5e01160c36238a928745a450ce078a` retained a real status-101 failure:
`festerm-syntax::tests::only_the_range_asked_for_is_queried` produced no spans.
This is separate from #330's unexplained exit 2173. Its original failed gate
and receipt remain evidence, not replaced by a green rerun.

Matched controls used the **same original executable**, SHA-256
`dc7cfc3e5c0621c98cd88654468db8a960a59ff4e1872d4a24c8a9e03357b744`,
without rebuilding or changing the syntax source, its 40 ms parse budget, test
assertions or default test concurrency. Both bare full-scope controls passed;
both original-wrapper full-scope controls failed the same assertion. The
single exact test also passed bare and failed under the original wrapper.
Event-service timing showed no debug events between creation of that test
thread and its assertion panic; startup module handling was sub-millisecond.
This did not support a live symbol/stack-handler pause as the cause.

Changing only `_NO_DEBUG_HEAP=1` made the original wrapper's full and exact
scopes pass. An independent owned fixture read the native heap-validation flag
mask: bare **0**, original debugger **0x70**, fixed debugger **0**. The new
child-only environment policy therefore restores bare heap behavior instead
of increasing the product's deadline or hiding its failure. The fixed wrapper
passed the same full/exact scopes with unchanged event, stack and module
handling. Debugger scheduling overhead still exists; this is not a claim of
zero observation cost or complete workspace qualification.

Limits: 512 event and module records; eight stack captures, 64 threads and up to
24 frames per capture; 8192 reported test names; 512-byte output-line parsing
buffers; a 4 MiB receipt ceiling with explicit truncation. Root execution has
the configured deadline, followed by at most ten seconds of debug-exit draining;
owned descendant exit waits have a shared five-second budget and stdout/stderr
reader cancellation is bounded. System debug/unwind/file APIs themselves are
not forcibly interrupted; no remote symbol requests are made.

Timeout/error cleanup snapshots the descendant tree, validates creation times,
pins exact process handles, terminates descendants before the root and checks
their exits. It never terminates by executable name, inspects unrelated process
content, changes Job Object policy, or kills intentional surviving descendants
after a normal test exit. Depth beyond 32 generations is not a supported test
fixture. The debugger's own process must remain alive to collect a receipt;
whole-runner/job termination can prevent final persistence.

## Focused validation

### Opt-in hosted sampler lifecycle investigation

The main run `38176532395` at `ff580b2e6dda51cbf2ce05b9280afc9d08911eb4`
captured a second-chance `0x0000087d`. Matching public Microsoft SDK-layer
symbols and bounded disassembly narrow its faulting thread to
`CreateSampler` / `HandleToDescriptor`, reporting
`INVALID_DESCRIPTOR_HANDLE` (message 646). The shared report site cannot
distinguish a handle outside known heaps from an invalid/misaligned offset,
and concurrent heap creation on another thread is not proof of a race.

A sample of CI run IDs created from October 7 through October 12, 2026,
including original failed attempts behind green reruns, contains four
exit-2173 failures in 95 completed Windows workspace-test attempts; main has
one in 20. The older two have no native receipt proving the same exception.
The fourth, from PR #378, has the same SDK-layer module identity and captured
`CreateSampler` stack as the retained main occurrence.
These changing revisions/workloads are not a matched CI/local probability
comparison. Local concurrent sampler and mixed Direct2D/egui stress have not
reproduced the native fault. The [explicit hosted diagnostic run
38186043227](https://github.com/fswiderski/fesTerm/actions/runs/38186043227)
at `b3b5d92d57b3b765cb97f28b5eb9ddb165b63c95` also passed all four cases while
loading the same D3D12Core and SDK-layer identities as the main fault. This
removes the local module-identity mismatch for that bounded control, not the
workspace/environment/workload mismatch. Original findings remain in
[#330](https://github.com/fes/fesTerm/issues/330#issuecomment-6114523822).

Exact repaired-head dispatch
[38194411645](https://github.com/fes/fesTerm/actions/runs/38194411645)
then added a fifth exit-2173 failure in 96 completed Windows workspace-test
attempts and the first bounded-control reproduction. The shared-instance,
sampler-only case exited 2173 after 17.969 seconds; independent sampler-only
and both mixed cases passed. Its exception has the exact SDK-layer frame
offsets above, followed by symbolized `ID3D12Device::CreateSampler` and wgpu
sampler creation, with the same D3D12Core and SDK-layer identities as the
simultaneous workspace failure. One occurrence does not establish that shared
instance ownership is causal or estimate a matched failure rate, but the
focused suite is now a demonstrated reproduction route rather than only
non-reproduction stress.

The ignored `windows_sampler_lifecycles` integration test is diagnostic, not
a product repair or a replacement for the original workspace workload:

```powershell
$env:FESTERM_RUN_OPTIONAL_VALIDATION = '1'
$env:FESTERM_RUN_WINDOWS_SAMPLER_DIAGNOSTICS = '1'
.\scripts\run-windows-sampler-diagnostics.ps1
```

The optional-validation PowerShell aggregate includes the same explicitly
enabled suite. The shell aggregate records a Windows-x64 prerequisite skip.
The launcher runs four **distinct** cases once: independent/shared instances
with sampler-only/mixed work. Defaults are 16 workers and 32 device rounds
each; `-Threads` is bounded to 1..32 and `-Rounds` to 1..128. Each round
asserts a CPU DX12 adapter, creates 32 distinct samplers, submits/polls and
retires resources. Mixed work also creates/draws/retires a native Direct2D
surface and initializes egui, uploads fonts, then renders/readbacks two
160 x 96 frames with dimension/nontransparent-output assertions. These are
not pixel-equivalence, latency, or freeze acceptance.

The existing native runner enforces a 600-second per-case deadline and
retains original status/source/executable/module identity under
`target\windows-sampler-diagnostics\<run-id>\<workload>-<topology>`. The launcher
restores its four parent settings, keeps the first failure nonzero while
running the other distinct controls, gives each invocation a fresh run
directory so old receipts cannot be confused with new ones, and does not retry
any case.
Validation is always enabled and system runtime selection is not overridden.
There is no desktop input, installed application, personal fixture, or
persisted image. Hosted execution writes to its fresh runner-temporary
directory. Only the existing content-free receipt JSONs are uploaded, with
seven-day retention; ordinary console output is not added to that artifact.

To run on a reviewed diagnostic branch, manually dispatch the existing `CI`
workflow with boolean input `windows_sampler_diagnostics=true`. Its separate
75-minute Windows job uses the existing level-zero build profiles and non-GPU
runner calibration. The ordinary CI jobs still run unchanged. Ordinary PR,
main-push and default manual runs do not opt in. Record loaded module GUIDs
and each case's actual completion before claiming same-environment evidence;
a later `windows-latest` image may differ from the original incident.
Passing stress is non-reproduction, never proof of a causal repair.

```powershell
python -m unittest discover -s scripts\tests -p test_windows_test_runner.py -v
python -m unittest discover -s scripts\tests -p "test_windows_*.py" -v
python scripts\check_validation_traceability.py
```

The native suite compiles the owned fixture with the installed Windows Framework
C# compiler; it requires no GPU or desktop interaction. It proves original
0/101/2173/high-bit status propagation, genuine second-chance native exception,
termination stacks, bounded owned-tree timeout cleanup with a same-name foreign
sentinel, argument rejection, explicit unavailable/failed diagnostics,
content-free completed-test context, PowerShell/Cargo argument boundaries,
bare/debugger heap-policy parity and preservation of parent, Unicode and
hidden-drive environment entries without recording their contents.
These are runner proofs, **not** a reproduction of the unexplained UI exit.
