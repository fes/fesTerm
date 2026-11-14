# fesTerm VM Evidence Adapter

This directory is the product-owned half of fesTerm's VM evidence harness. It
is installed by a pinned `vm-evidence-lab` controller and runs only after that
controller has restored the VM, staged an exact fesTerm source bundle, and
validated this adapter's pinned commit.

The adapter accepts exactly one source named `festerm`, an empty payload, and
one of these modes:

- `native-smoke`
- `os-input-smoke`
- `optional-validation`
- `running-session-stress`
- `keyboard-routing-check`
- `keyboard-routing-native`
- `windows-conpty-prompt-stress` (Windows only)
- `dev-tunnel-workflow` (explicit real-service/native-vault prerequisites)

Each mode invokes the existing repository-owned runner and writes its
content-free status file beneath the artifact directory supplied by the shared
relay. It never accepts commands, arguments, paths, environment variables, or
output destinations from a job.

`running-session-stress` runs the same isolated Running Sessions harness as
optional validation with fixed `--batch 8 --cycles 3` limits. It exercises native
sessiond on every platform and installed tmux/screen providers on Unix, without
opening a GUI or requiring a working GPU. Provider skips remain explicit in the
guest log; a backend pass is not native-window or OS-input qualification. The
private host configuration must explicitly allow this mode and pin a reviewed
adapter commit that supports it. Use a short guest checkout path for Unix
domain socket limits.

On Unix the Running Sessions harness uses an owned private XDG runtime
directory, or a fresh `0700` leaf beneath a trusted home `.fsm` parent.
Shared-writable ancestors, symlinks and foreign/private-mode mismatches fail
without permission repair; it no longer stages daemon runtime state in shared
temporary storage. Keep the home/runtime path short enough for Unix sockets.

Windows build modes initialize the existing Visual Studio Build Tools
environment and select `clang-cl` when installed, otherwise `clang`. OS-input
builds keep that environment in their child PowerShell process. Cargo stderr
progress is not itself a failure, but its nonzero exit still fails the run.
Portable source/fixture contracts do not qualify native Windows execution.

`dev-tunnel-workflow` invokes the bounded real-service HOST client runner with
an empty job payload. It selects only the controlled `fes-festerm1` identity
and the operator's isolated configuration/native receipt. The two operator-selected
creation-policy phases exercise shared profile start/resume, private profile exclusion,
generic create permission, actual encrypted terminal roundtrips, exact detach and
durable-generation survival. It never automates password login, enrolls an owner,
accepts secrets from a job, deletes cloud resources or ends unrelated shells.
Windows execution is explicitly **blocked** until an owned Job capability
is implemented; a Windows host can be tested from a Unix client. Missing
operator metadata, account credentials or owner approval is **blocked**,
never pass. GUI acceptance is always separately marked **unrun**. See
[the operator procedure](../../docs/dev-tunnel-workflow-lab.md) before adding this
mode to a host's private allowlist and updating its adapter pin.

The historical fixed November ADR 0084 campaign modes are not in the current allowlist.
Historical source `62b1a47b` and its immutable private evidence retain those
operators; see the [scope and outcome record](../../docs/manual-validation.md#historical-adr-0084-vm-evidence-and-retained-delivery-scope).
No old source result qualifies a new head or authorizes another VM window.

`keyboard-routing-check` invokes `scripts/check_keyboard_routing.py` without
arguments. It runs synthesized production routing/configuration/controller
checks on the guest platform; it does not open a window or establish native
input delivery. `keyboard-routing-native` enables the repository-owned
keyboard scenario in the existing OS-input driver and keeps its content-free
result in the job artifact directory. These are separate modes so a working
backend cannot mask missing desktop, graphics, or Accessibility prerequisites.
The native scenario checks no-selection Copy, palette capture, an identified
palette Paste request, and controlled terminal bytes. It overwrites the test
desktop clipboard with a fixed fixture without reading or restoring previous
contents: use a dedicated desktop with guest/host clipboard sharing disabled.
It is not selected-URL clipboard, AltGr, or IME qualification.
Both modes require an empty payload and a reviewed adapter pin.

`windows-conpty-prompt-stress` repeats the exact ConPTY prompt-return
regression 20 times. Each attempt retains the production test's three-second
readiness deadline and adds an outer 45-second process bound. It stops on the
first failure instead of retrying it, so an intermediate blank command-echo row
cannot be hidden by a later pass. The fixed VM mode replaces agent-authored
Cargo loops and accepts no cycle count or other payload.

`vm-evidence-lab` owns the host controller, Parallels provider, guest relay,
exact-source bundles, locks, result records, and manifests.

From a fesTerm checkout, bootstrap its reviewed shared-lab dependency with:

```sh
./scripts/bootstrap-vm-evidence-lab.sh
```

Run the host-independent Unix adapter contract check with:

```sh
./scripts/vm-evidence-adapter/tests/run.sh
```
