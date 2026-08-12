# VM-Based Native Evidence Framework

**Status:** Automation foundation implemented; first manual evidence run completed 2026-08-10 (see [Evidence Run Log](#evidence-run-log)). Guest baselines and relay installation remain environment provisioning work.
**Scope:** Deterministic Windows, Linux, and macOS desktop evidence executed as virtual machines on a macOS host  
**Primary hypervisor:** Parallels Desktop  
**Secondary hypervisor:** VMware Fusion where the requested guest is supported

This document defines a VM-based evidence lab for fesTerm. The goal is not to
replace normal GitHub-hosted CI, headless egui tests, snapshots, or the existing
native-smoke workflow. The goal is to add repeatable evidence from logged-in,
real desktop sessions where the operating system compositor, focus model,
window manager, input stack, DPI behavior, PTY/ConPTY implementation, and
production eframe/winit window all participate in the same run.

The initial implementation target is a Mac that hosts three clean test VMs:

- Windows desktop;
- Linux desktop; and
- macOS desktop.

The host is the controller. Each guest is disposable between evidence runs and
is restored to a known baseline before checking out and testing an exact fesTerm
commit.

This framework is intentionally deterministic. Copilot may implement,
orchestrate, inspect, and diagnose the framework, but it must not improvise
acceptance criteria or silently convert a failed run into a pass.

## Relationship to Existing Validation

This framework extends, rather than replaces, the current validation layers:

- [`ui-test-plan.md`](ui-test-plan.md) defines the tiered test architecture.
- [`m6-validation-gate.md`](m6-validation-gate.md) defines M6 acceptance scope.
- [`native-smoke-policy.md`](native-smoke-policy.md) defines P4/Tier 6 policy,
  including no silent retries and sanitized failure artifacts.
- [`milestone-acceptance-record.md`](milestone-acceptance-record.md) is the
  auditable acceptance record.
- `scripts/run-optional-validation.sh` and
  `scripts/run-optional-validation.ps1` are the repository-owned aggregate
  optional-validation entry points.
- `scripts/run-windows-os-input-smoke.ps1` is the current independently driven
  Windows desktop-input proof.

The VM lab should invoke these existing seams wherever possible. Do not copy
their logic into a separate VM-only test implementation.

## Why a VM Lab Exists

GitHub-hosted runners are sufficient for most deterministic tests, but they do
not cover every native desktop condition that fesTerm needs to prove.

The VM lab should specifically provide:

1. a logged-in desktop with a real compositor/window manager;
2. independently driven native focus, resize, keyboard, and pointer behavior;
3. a repeatable graphics/display environment;
4. screenshots captured outside the fesTerm process;
5. exact candidate-SHA execution on a clean OS image;
6. reproducible guest metadata for the milestone evidence record; and
7. a local environment that Copilot can rerun while diagnosing a native defect.

For Linux this closes the gap represented by issue #21: Xvfb is intentionally
unfocused and is not native-focus evidence. For Windows it supplements hosted
CI with a persistent real desktop and externally driven window/input evidence.
For macOS it upgrades the current advisory native-window path into a controlled,
logged-in desktop run with explicit focus and native-input evidence.

## Non-Goals

The VM lab is not intended to:

- replace PR-blocking core, UI, PTY, or headless-frame CI;
- make hypervisor behavior the product specification;
- use a developer's normal shell profile, clipboard, credentials, or terminal
  contents as fixtures;
- hide flakiness by retrying tests automatically;
- treat an emulated or architecture-mismatched Windows VM as the sole oracle
  for an architecture-specific Windows defect;
- make screenshots the only assertion for terminal correctness; or
- allow an AI agent to decide that a required check can be skipped because it
  appears unnecessary.

## Host Requirements

Use a dedicated macOS user account for the VM evidence lab when practical.
The account should remain logged in while evidence runs execute.

Required host capabilities:

- supported macOS release for the chosen hypervisor;
- enough RAM and disk for three independent desktop VMs;
- `git`, `gh`, `jq`, `ssh`, `scp`, and standard POSIX shell tools;
- a GitHub self-hosted Actions runner registered under the logged-in user if
  GitHub workflow dispatch is used;
- the chosen hypervisor CLI on `PATH` or discoverable at a configured absolute
  path; and
- a host-side evidence directory outside the repository checkout.

Recommended GitHub runner labels:

```text
self-hosted
macOS
festerm-vm-lab
```

The self-hosted runner is the host controller. Do not register the same runner
inside all three guest VMs for the first implementation; that complicates
snapshot identity, registration state, and desktop-session ownership.

## Hypervisor Choice

### Parallels Desktop: primary

Implement the Parallels provider first.

Reasons:

- `prlctl` provides machine lifecycle and snapshot control;
- `prlctl capture` can capture the VM display as PNG from the host;
- `prlctl send-key-event` can inject keyboard events from outside the guest;
- current Parallels releases support macOS virtual machines on Apple Silicon;
  and
- the same provider can manage the Windows, Linux, and macOS lab machines.

On Apple Silicon, Parallels macOS VMs use Apple's Virtualization Framework.
Treat a guest running the same major macOS version as the host as the safest
baseline unless current Parallels documentation explicitly supports the desired
host/guest combination.

Parallels documentation currently notes that snapshots of macOS guests on
Apple Silicon require macOS Sonoma 14 or newer on the host. If the lab cannot
use snapshots, the provider must support an equivalent clean-clone/template
reset strategy rather than running against a dirty guest.

### VMware Fusion: secondary

Define a provider interface for VMware Fusion, but do not block the first
implementation on it.

On Apple Silicon, Fusion supports ARM guest operating systems but Broadcom's
current compatibility guidance states that ARM macOS guests are not supported
in Fusion VMs. Therefore, on an Apple Silicon host, Fusion cannot satisfy this
framework's three-guest Windows/Linux/macOS requirement. It may still be a
useful Windows/Linux provider.

On an Intel Mac, Fusion may be usable for compatible x86 macOS guests according
to the Fusion/guest support matrix, but the concrete guest release must be
verified before treating it as an accepted lab target.

The framework must record provider and provider version in every evidence
manifest so results from Parallels and Fusion are not conflated.

## Host Architecture Policy

Record the Mac architecture in every run.

### Apple Silicon host

Preferred guest set:

- Windows 11 ARM for routine Windows ARM evidence;
- ARM64 Linux desktop for Linux compositor/focus evidence; and
- ARM64 macOS guest through Parallels for macOS native evidence.

If Parallels x86 emulation is used for a Windows x86 guest, record it explicitly
as emulated evidence. Do not silently equate it with native x64 Windows
hardware. The issue #3 Windows rendering history originated on Windows native
behavior; retain GitHub `windows-latest` and/or a physical x64 Windows run as a
separate architecture oracle until the acceptance record explicitly changes
that policy.

### Intel host

Preferred guest set:

- x64 Windows;
- x86_64 Linux desktop; and
- a macOS guest version supported by the selected hypervisor.

## VM Inventory

Use stable logical names independent of provider-specific VM IDs.

```text
festerm-windows
festerm-linux
festerm-macos
```

Each logical VM maps to provider configuration stored outside secrets, for
example:

```text
~/.config/festerm-vm-lab/config.json
```

Example shape:

```json
{
  "provider": "parallels",
  "artifact_root": "/Users/festerm/VM-Evidence",
  "vms": {
    "windows": {
      "name": "festerm-windows",
      "baseline": "festerm-evidence-clean-v1",
      "guest_host": "festerm-windows.local",
      "guest_user": "festerm"
    },
    "linux": {
      "name": "festerm-linux",
      "baseline": "festerm-evidence-clean-v1",
      "guest_host": "festerm-linux.local",
      "guest_user": "festerm"
    },
    "macos": {
      "name": "festerm-macos",
      "baseline": "festerm-evidence-clean-v1",
      "guest_host": "festerm-macos.local",
      "guest_user": "festerm"
    }
  }
}
```

Do not commit real hostnames, usernames, VM IDs, SSH private keys, passwords, or
license data.

## Clean Baseline Contract

Every VM must have a clean baseline containing only the OS, build/runtime
prerequisites, and test automation dependencies.

The baseline should contain:

- OS updates intentionally selected for that baseline;
- Git and GitHub CLI if needed;
- Rust bootstrap prerequisites;
- native compiler/build dependencies;
- graphics/runtime dependencies;
- SSH server for host-to-guest control;
- a dedicated, non-administrator test user where practical;
- a logged-in desktop session configured to start automatically for the lab;
- a small interactive desktop command relay described below; and
- platform UI-automation tools required by the accepted smoke path.

The baseline must not contain:

- a developer's Git credentials;
- personal SSH keys;
- production host keys or SSH configuration;
- personal shell dotfiles;
- personal clipboard/history data;
- a mutable fesTerm checkout intended to survive across runs; or
- prior evidence artifacts.

Prefer provisioning dependencies once and restoring the baseline before each
run. The tested repository checkout itself should be recreated or hard-reset to
an exact candidate SHA on every run.

**Known gap (2026-08-10):** the first manually operated run of this lab found
that Parallels' default guest templates left **Shared Profile** (host
Desktop/Documents bind-mounted into the guest), shared clipboard, and shared
cloud content enabled on one or more of the three VMs, and a leftover ad hoc
host-folder share on another. These were hardened manually (`prlctl set
--shared-profile off`, `--shared-clipboard off`, `--shared-cloud off`, and
removing the stray share) but that fix is not yet captured in a repeatable
provisioning step or confirmed to survive a snapshot revert. See
[#36](https://github.com/fes/fesTerm/issues/36).

## Interactive Desktop Command Relay

A remote shell is not sufficient proof that an OS-input test ran in the active
desktop session. Windows services and SSH sessions can be isolated from the
interactive desktop; macOS Accessibility events are associated with the
logged-in GUI session; Linux focus/input tools need the actual display and
session environment.

Each guest therefore needs a minimal repository-owned command relay running as
the logged-in desktop user.

The relay must:

- start automatically when the dedicated test user logs in;
- execute only a small allowlisted set of fesTerm evidence commands;
- receive a job containing candidate SHA, run ID, and requested evidence tier;
- run commands in the active GUI session;
- write machine-readable status and log paths;
- never accept arbitrary shell text from an untrusted job payload; and
- never persist credentials or terminal contents in its status channel.

Suggested implementation choices:

- Windows: an interactive Scheduled Task started at user logon;
- Linux: a user service/autostart entry in the graphical session;
- macOS: a per-user `LaunchAgent`.

The host may deliver a signed/validated job file over SSH/SCP. The interactive
relay polls a private guest spool directory, validates the schema, invokes the
repository-owned platform script, and writes a completion file. This keeps
control traffic separate from GUI-session ownership.

A simpler first implementation may use an already-running interactive terminal
or runner inside the guest, but it must not claim native input evidence if the
actual UI-driving command executed outside the logged-in desktop session.

## Proposed Repository Layout

Copilot should implement the framework using a narrow provider/guest interface.
A suggested layout is:

```text
scripts/
  vm-evidence/
    host.sh
    config.example.json
    providers/
      parallels.sh
      fusion.sh
    guest/
      common.sh
      linux.sh
      macos.sh
      windows.ps1
    relay/
      README.md
      linux-install.sh
      macos-install.sh
      windows-install.ps1
  run-macos-os-input-smoke.sh
.github/workflows/
  vm-evidence.yml
docs/
  vm-evidence-framework.md
```

Exact names may change if the existing script conventions make another layout
clearer, but preserve the separation between:

1. host orchestration;
2. hypervisor-specific operations;
3. guest build/test operations; and
4. interactive desktop input operations.

## Host CLI Contract

The primary human/Copilot interface should be one bounded command.

Suggested commands:

```text
scripts/vm-evidence/host.sh status
scripts/vm-evidence/host.sh windows <sha>
scripts/vm-evidence/host.sh linux <sha>
scripts/vm-evidence/host.sh macos <sha>
scripts/vm-evidence/host.sh all <sha>
scripts/vm-evidence/host.sh reset windows
scripts/vm-evidence/host.sh reset linux
scripts/vm-evidence/host.sh reset macos
scripts/vm-evidence/host.sh collect <run-id>
```

`all` must execute platform runs independently and preserve all three results.
A failure on Windows must not prevent Linux/macOS evidence from being collected
unless the host infrastructure itself is unusable.

The command exits nonzero if any requested platform test fails or if the
required evidence manifest is incomplete.

## Provider Interface

Each provider must expose equivalent host operations:

```text
provider_validate
provider_vm_exists <platform>
provider_reset <platform>
provider_start <platform>
provider_wait_running <platform>
provider_capture <platform> <png-path>
provider_send_key <platform> <key-spec>
provider_stop <platform>
provider_metadata <platform>
```

Optional operations:

```text
provider_clone_clean <platform>
provider_delete_clone <platform>
provider_guest_ip <platform>
```

The first Parallels implementation should wrap documented `prlctl` operations
rather than scattering raw `prlctl` commands throughout the host workflow.

Provider-specific failures are infrastructure failures, not product failures.
Record them as such in the manifest and do not fabricate a product test result.

## Per-Run Lifecycle

For every requested platform:

1. Allocate a unique `run_id`.
2. Record host/provider metadata before mutating the VM.
3. Restore the VM's clean snapshot or recreate it from the clean template.
4. Start the VM.
5. Wait for the OS and the interactive desktop relay to report ready.
6. Confirm the logged-in test desktop/session identity.
7. Create a fresh checkout or fetch/reset to the exact candidate SHA.
8. Record the resolved full SHA from inside the guest.
9. Install/update only repository-declared dependencies that intentionally vary
   per run; do not opportunistically upgrade the OS.
10. Run the platform's deterministic preflight tests.
11. Run the production native-window self-smoke.
12. Run independently driven OS focus/input/resize evidence.
13. Capture required host-side screenshots.
14. Collect content-free status files and sanitized logs.
15. Write and validate the evidence manifest.
16. Stop the VM.
17. Leave the VM dirty only long enough for explicit diagnosis; the next normal
   run always restores the clean baseline.

Do not retry a failed product test automatically. If a human or Copilot elects
to rerun after investigation, create a new `run_id` and retain both outcomes.

## Common Guest Preflight

Each guest should run the repository's normal deterministic validation before
native evidence:

```text
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo check --workspace
git diff --check
```

Where this duplicates already-green CI, the workflow may support a narrower
`native-only` mode for diagnosis. Release/milestone evidence should default to
the full mode unless the acceptance record explicitly links equivalent CI for
the same candidate SHA.

## Windows VM Evidence

### Required environment

- logged-in Windows desktop;
- real Explorer/DWM session;
- expected display scale recorded;
- ConPTY support;
- PowerShell available;
- current fesTerm Windows build prerequisites; and
- interactive desktop relay running as the test user.

### Required execution

Use the existing Windows validation entry point:

```powershell
$env:FESTERM_RUN_OPTIONAL_VALIDATION = '1'
pwsh -NoProfile -File scripts\run-optional-validation.ps1
```

This retains the existing reviewed ConPTY staging path and Windows OS-input
smoke rather than reimplementing them in VM code.

For P4 evidence, also retain the production self-smoke result contract:

```text
FESTERM_NATIVE_WINDOW_SMOKE=1
FESTERM_NATIVE_SMOKE_RESULT_PATH=native-smoke-window-result.txt
```

### Additional host evidence

Capture the VM display from the host:

- once after the desktop is ready;
- once while the controlled fesTerm window is visible;
- at the final resize generation; and
- immediately on failure.

The screenshot scenario must use repository-owned/sanitized terminal content.

### Windows pass conditions

A VM run is `pass` only when all required checks for the selected mode pass,
including:

- ConPTY/session smoke;
- bounded shutdown;
- native-window result `status=pass`;
- independently driven Windows OS-input smoke;
- expected artifacts exist;
- guest-reported SHA matches the requested SHA; and
- no infrastructure failure occurred.

Record guest architecture separately. On Apple Silicon, Windows ARM evidence is
valid for the ARM build/runtime path but does not by itself replace native x64
Windows evidence where architecture matters.

## Linux VM Evidence

### Required environment

Use a real Linux desktop session. Do not use Xvfb for the acceptance path in
this framework.

Initial recommended baseline:

- Ubuntu Desktop;
- GNOME;
- Xorg for the first implementation because mature focus/window-control tooling
  is straightforward;
- real window manager/compositor;
- `xdotool` and `wmctrl` or an equivalent maintained native automation tool;
- build prerequisites listed in [`development-handoff.md`](development-handoff.md);
  and
- interactive desktop relay running in the graphical user session.

Wayland should be added as a separate evidence target after the Xorg path is
stable. Do not silently treat an Xorg pass as a Wayland pass.

### Required execution

Run the Unix PTY smoke tests and production native-window self-smoke without the
Xvfb exception:

```sh
cargo test -p festerm unix_pty_smoke_flow_with_test_child_and_issue3_resizes -- --include-ignored --nocapture
cargo test -p festerm unix_pty_bounded_shutdown_terminates_process_tree -- --include-ignored --nocapture
FESTERM_NATIVE_WINDOW_SMOKE=1 \
FESTERM_NATIVE_SMOKE_RESULT_PATH=native-smoke-window-result.txt \
./target/debug/festerm
```

Do **not** set `FESTERM_NATIVE_SMOKE_ALLOW_UNFOCUSED=1`.

For the aggregate suite:

```sh
FESTERM_RUN_OPTIONAL_VALIDATION=1 scripts/run-optional-validation.sh
```

### Independently driven Linux evidence

The interactive desktop driver must independently:

1. locate the fesTerm top-level window;
2. activate/focus it through the desktop/window manager;
3. verify that the application reports focused state;
4. resize the native window through the issue #3 sequence;
5. send the agreed keyboard inputs through the OS input path;
6. confirm the controlled PTY observed the expected input; and
7. close the application through a bounded path.

Record desktop environment, display protocol (`x11` or `wayland`), compositor,
window manager, scale factor, and virtual display size.

### Linux pass conditions

A VM run is `pass` only when:

- both Unix PTY smokes pass;
- native-window result is `status=pass` with focused state required;
- the independent desktop driver completes;
- required host-side screenshots exist;
- guest SHA matches the requested SHA; and
- the environment is a real desktop rather than Xvfb.

A successful Xorg run closes the current real-desktop/X11 evidence gap. Wayland
remains a separately labeled target.

## macOS VM Evidence

macOS is a first-class VM target in this framework.

### Hypervisor requirement

On Apple Silicon, use Parallels for the macOS guest. Current VMware Fusion
compatibility guidance does not support ARM macOS guests in Fusion VMs.

For Parallels on Apple Silicon, provision the macOS guest from an appropriate
`.ipsw` image through the documented macOS VM workflow. Prefer the same major
macOS release as the host for the initial stable lab baseline unless the
current compatibility documentation verifies another combination.

### Required environment

- macOS guest running on Apple hardware through the selected hypervisor;
- logged-in dedicated test user;
- normal WindowServer desktop session;
- Rust/build prerequisites;
- Remote Login/SSH for control-plane operations;
- interactive desktop relay installed as a per-user LaunchAgent; and
- Accessibility permission granted only to the specific guest-side UI driver
  that needs to synthesize native input.

Prefer host-side hypervisor screenshots. Avoid requiring guest Screen Recording
permission unless a future diagnostic requires guest-side capture.

### Required execution

Run the existing Unix PTY and native-window paths inside the guest:

```sh
cargo test -p festerm unix_pty_smoke_flow_with_test_child_and_issue3_resizes -- --include-ignored --nocapture
cargo test -p festerm unix_pty_bounded_shutdown_terminates_process_tree -- --include-ignored --nocapture
FESTERM_NATIVE_WINDOW_SMOKE=1 \
FESTERM_NATIVE_SMOKE_RESULT_PATH=native-smoke-window-result.txt \
./target/debug/festerm
```

Do not mark the macOS VM accepted merely because the current aggregate Unix
script passes. Add independently driven native macOS input evidence analogous
to `run-windows-os-input-smoke.ps1`.

### New macOS OS-input smoke

Implement `scripts/run-macos-os-input-smoke.sh` or an equivalent narrowly scoped
helper.

The helper must run in the logged-in GUI session and use native macOS
accessibility/event APIs through a small auditable driver. AppleScript/System
Events may be used for simple activation/keystrokes; a small Swift or other
native helper using supported accessibility/Quartz event APIs is preferred if
it produces more reliable focus, pointer, resize, and keyboard control.

The smoke should:

1. launch the controlled fesTerm native-window smoke mode;
2. identify the fesTerm process/window without scraping terminal text;
3. bring the window to the foreground;
4. click inside the terminal content area;
5. verify native focused state;
6. perform deterministic native window resizes corresponding to the issue #3
   sequence;
7. send Tab, an arrow key, controlled text, and Enter through the OS input path;
8. prove the controlled PTY observed the expected byte/input sequence using
   content-free counters or markers;
9. close the window/application; and
10. emit only content-free `status=pass`/`status=fail` evidence plus bounded
    diagnostics.

Do not grant broad Accessibility access to unrelated tools merely to make the
smoke easier to implement.

### macOS host-side evidence

Capture the VM display from the hypervisor at the same bounded checkpoints used
for Windows/Linux. The host screenshot establishes that an independently
observable VM display contained the real fesTerm window; the product's own
state/result file remains the structural assertion.

### macOS pass conditions

A macOS VM run is `pass` only when:

- both Unix PTY smokes pass;
- native-window result is `status=pass` with focused state required;
- the macOS independent OS-input smoke passes;
- required host-side screenshots exist;
- guest SHA matches the requested SHA; and
- host/guest macOS versions and architecture are present in the manifest.

Until the macOS OS-input helper exists and executes in the real logged-in guest
desktop, macOS VM status is `validation pending`, not accepted P4 native-input
evidence.

## Screenshot Policy

Screenshots are diagnostic/observational evidence, not the sole correctness
oracle.

For each platform, use a bounded capture set:

```text
00-desktop-ready.png
10-festerm-window.png
20-resize-final.png
90-failure.png        # only on failure
```

Additional captures may be added for a diagnosed defect, but do not turn every
run into a recording of terminal output.

Requirements:

- use repository-owned controlled content for screenshot scenarios;
- do not expose developer shell history, clipboard data, credentials, SSH host
  data, or arbitrary command output;
- capture from the hypervisor/host when practical;
- compute SHA-256 for each screenshot and record it in the manifest; and
- keep video disabled by default, consistent with current native-smoke policy.

## Evidence Bundle

Host artifacts should use a stable layout:

```text
vm-evidence-artifacts/
  <run-id>/
    run.json
    windows/
      manifest.json
      optional-validation-result.txt
      native-smoke-window-result.txt
      logs/
      screenshots/
    linux/
      manifest.json
      optional-validation-result.txt
      native-smoke-window-result.txt
      logs/
      screenshots/
    macos/
      manifest.json
      optional-validation-result.txt
      native-smoke-window-result.txt
      macos-os-input-result.txt
      logs/
      screenshots/
```

Missing required artifacts are a failed/incomplete evidence run.

## Evidence Manifest

Every platform run must emit structured JSON. Suggested minimum schema:

```json
{
  "schema_version": 1,
  "run_id": "2026-08-09T170500Z-8977cf5-windows",
  "requested_sha": "8977cf5899ebe531a852b44a7528b0a491bb5eaa",
  "guest_sha": "8977cf5899ebe531a852b44a7528b0a491bb5eaa",
  "status": "pass",
  "failure_class": null,
  "host": {
    "os": "macOS",
    "version": "...",
    "architecture": "arm64",
    "model": "..."
  },
  "hypervisor": {
    "provider": "parallels",
    "version": "...",
    "vm_name": "festerm-windows",
    "baseline": "festerm-evidence-clean-v1"
  },
  "guest": {
    "platform": "windows",
    "os_version": "...",
    "architecture": "arm64",
    "desktop": "...",
    "display_protocol": "...",
    "compositor": "...",
    "display_size": "1920x1200",
    "scale_factor": 1.0
  },
  "checks": {
    "workspace": "pass",
    "pty": "pass",
    "native_window": "pass",
    "os_input": "pass",
    "screenshots": "pass"
  },
  "artifacts": [
    {
      "path": "screenshots/20-resize-final.png",
      "sha256": "..."
    }
  ],
  "started_at": "...",
  "completed_at": "..."
}
```

`failure_class` should distinguish at least:

```text
product
infrastructure
provisioning
timeout
artifact
configuration
```

A timeout in a product smoke is still a product failure when the environment
was healthy and the test's bounded deadline expired. Do not classify it as
infrastructure merely because a timeout occurred.

## Result Vocabulary

Use the repository's existing acceptance vocabulary plus one evidence-run
state:

- `pass`: all required checks for this run completed successfully;
- `fail`: at least one required product check failed;
- `infra-fail`: required environment/provider/relay operation failed before a
  product conclusion was possible;
- `not-run`: explicitly requested component could not execute and therefore is
  not evidence; and
- `validation pending`: documentation-level milestone state when required
  evidence has not yet produced a qualifying pass.

Never translate `not-run` or `infra-fail` to `pass`.

## GitHub Actions Integration

Add `.github/workflows/vm-evidence.yml` after local host orchestration is stable.

Initial triggers:

```text
workflow_dispatch
nightly schedule
```

Do not make the VM workflow PR-blocking initially. Follow the same reliability
policy as existing native smoke: measure repeated clean execution first.

Suggested workflow inputs:

```text
sha               # default: workflow ref SHA
platform           # windows | linux | macos | all
mode               # full | native-only
provider           # default from host config; optional override
preserve_on_failure # false by default
```

The job runs on:

```text
[self-hosted, macOS, festerm-vm-lab]
```

The workflow should:

1. resolve the full candidate SHA;
2. invoke `scripts/vm-evidence/host.sh`;
3. always upload the bounded evidence bundle;
4. publish a concise job summary table;
5. fail if any requested platform is `fail` or `infra-fail`; and
6. never rerun a failed guest test automatically.

Use concurrency so only one workflow mutates a given VM inventory at a time.

## Copilot Operating Model

Copilot is a consumer and implementer of this framework, not the source of
truth for whether evidence passes.

Copilot may:

- implement provider adapters and guest scripts;
- trigger `host.sh <platform> <sha>`;
- inspect manifests, sanitized logs, screenshots, and existing test failures;
- diagnose a defect;
- edit fesTerm or the harness;
- rerun under a new `run_id`;
- add deterministic regressions for discovered defects; and
- prepare updates to the milestone acceptance record after qualifying evidence
  exists.

Copilot must not:

- call raw hypervisor commands throughout unrelated scripts when the provider
  wrapper exists;
- alter a baseline during an evidence run;
- disable a failed check to obtain a green result;
- use `--yolo`-style unrestricted host access as a design requirement;
- silently retry a product failure;
- infer a pass from a screenshot when a structural result file failed;
- weaken the focus requirement on real Linux/macOS desktop runs;
- set `FESTERM_NATIVE_SMOKE_ALLOW_UNFOCUSED=1` outside the explicit Xvfb CI
  scenario; or
- commit secrets or machine-specific VM configuration.

The ideal Copilot loop is:

```text
change
  -> normal CI
  -> deterministic VM evidence command
  -> structured evidence bundle
  -> diagnosis
  -> regression/fix
  -> new run ID
  -> acceptance-record update after pass
```

## Safety and Permissions

Keep host and guest privileges narrow.

- Use a dedicated SSH key for the lab.
- Prefer guest-local privilege escalation only for provisioning, not for test
  execution.
- Do not expose the hypervisor CLI to untrusted repository content.
- Validate `platform`, `sha`, `mode`, and provider arguments before use.
- Treat candidate SHA as data, never as shell text.
- Do not execute arbitrary commands read from fixture files or job JSON.
- Keep macOS Accessibility permission scoped to the dedicated native-input
  driver.
- Store host config and SSH keys outside the repository.
- Do not upload VM disk images or memory snapshots as GitHub artifacts.

## Failure Preservation and Diagnosis

Default behavior after a failure:

1. capture the bounded failure screenshot;
2. collect status/log artifacts;
3. record the failure manifest;
4. stop the VM cleanly when possible; and
5. preserve the dirty guest only when `preserve_on_failure` was explicitly
   requested.

Preserving a failed guest is a diagnostic convenience, not evidence. A later
qualifying run must start again from the clean baseline.

## Baseline Versioning

Treat VM baselines as versioned infrastructure.

Suggested identifier:

```text
festerm-evidence-clean-v1
```

When OS updates, desktop changes, graphics stack changes, or automation-tool
updates materially alter the environment, create `v2` rather than silently
mutating the accepted baseline.

Record the baseline ID in every manifest and in milestone evidence when the VM
lab is used for acceptance.

## Initial Implementation Sequence for Copilot

Implement in dependency order and keep each step reviewable.

### Phase 1 — Host/provider skeleton

- Add configuration parsing and schema validation.
- Add the Parallels provider.
- Implement `status`, `reset`, `start`, `capture`, and `stop`.
- Add a dry-run mode.
- Add provider unit/shell tests that do not require a VM where practical.

**Exit:** host can reset/start/capture/stop each configured VM without running
fesTerm.

### Phase 2 — Guest checkout and content-free evidence

- Add SSH/SCP control-plane support.
- Create a fresh exact-SHA checkout per run.
- Record guest OS/architecture/display metadata.
- Collect result files and write manifest JSON.

**Exit:** a no-op test job produces a complete manifest on Windows, Linux, and
macOS.

### Phase 3 — Existing repository validation

- Wire Windows to `run-optional-validation.ps1`.
- Wire Linux/macOS to `run-optional-validation.sh`.
- Preserve individual PTY/native result files.
- Add host-side screenshot capture.

**Exit:** existing optional validation executes unchanged inside all three VMs.

### Phase 4 — Interactive desktop relay

- Install per-platform logged-in-user relay.
- Ensure jobs are allowlisted and schema-validated.
- Prove commands execute in the real GUI session.

**Exit:** the relay can launch the controlled fesTerm native smoke and report
active desktop/session metadata on all three platforms.

### Phase 5 — Linux real-desktop P4 evidence

- Add independent focus/resize/input driver.
- Run without the unfocused Xvfb escape hatch.
- Record Xorg desktop evidence and issue #21 acceptance data.

**Exit:** one clean Linux VM run satisfies the documented real-desktop focus
criteria.

### Phase 6 — macOS native-input evidence

- Add `run-macos-os-input-smoke.sh` and the narrow native UI driver.
- Provision Accessibility permission for the dedicated driver.
- Prove focus, resize, OS keyboard input, and bounded exit.

**Exit:** macOS has evidence parity with the independently driven Windows/Linux
native-desktop layer.

### Phase 7 — GitHub Actions orchestration

- Add `vm-evidence.yml`.
- Add workflow inputs and concurrency.
- Upload bounded artifacts on success/failure.
- Add summary table.

**Exit:** a manually dispatched workflow can produce a three-platform evidence
bundle for a selected candidate SHA.

## Implemented Automation Foundation

The repository now contains the first bounded implementation at
`scripts/vm-evidence/`:

- `host.sh` validates a user-local configuration, resets/starts a Parallels VM,
  waits for SSH only as a control plane, verifies the graphical relay through
  a disposable readiness probe, submits an allowlisted relay job, captures the
  display through `prlctl`, writes a JSON manifest, and cleanly stops the VM
  on every terminal path. Its configuration template is `config.example.json`;
  real VM names, addresses, keys, and artifact paths remain outside Git.
- The Unix relays run only in a graphical session. Linux qualifying execution
  requires Xorg explicitly; the macOS relay requires the console user's
  `gui/<uid>` launchd domain. Both reject arbitrary job fields and accept only
  a Git SHA plus `readiness-probe`, `native-smoke`, or
  `optional-validation`.
- The Windows relay is executed through Parallels as the active console user.
  It can automate ConPTY and CPU-rendered diagnostic evidence, but the
  Parallels Windows-on-ARM guest remains `diagnostic`: Parallels does not
  expose a hardware-capable wgpu backend there.

Install each relay using `scripts/vm-evidence/relay/README.md`, then place a
real configuration at `~/.config/festerm-vm-lab/config.json`. A normal run is:

```sh
scripts/vm-evidence/host.sh linux <full-candidate-sha>
scripts/vm-evidence/host.sh macos <full-candidate-sha>
scripts/vm-evidence/host.sh windows <full-candidate-sha>
```

`all` preserves individual platform failures and returns nonzero after all
three attempts. No failed guest test is retried automatically.

### Watchdog and cleanup contract

Before submitting a product job, the controller restores the configured
baseline, starts the VM, waits for SSH, captures a ready desktop, and requires
the GUI relay to complete a `readiness-probe`. That probe verifies the relay's
actual graphical context and the guest's `git`, `cargo`, and `rustc`
prerequisites; it is not product evidence.

The controller also derives a relay-source tree hash from the checked-in
`scripts/vm-evidence/relay/` package. After each reset it compares that hash
with the guest spool's relay marker and synchronizes the platform relay before
publishing a job when they differ. Any unclaimed prior jobs are retained under
an `infrastructure-failed-` name, so a reset cannot cause an interrupted
validation to execute during a later run.

Relays atomically update a structured running record while progressing through
`queued`, `preflight`, `checkout`, `build`, and `app`. The controller enforces
separate configurable deadlines for readiness, checkout, build, app execution,
and the entire run. A deadline expiry preserves a controller-failure record,
screenshots, and provider metadata, then stops the VM instead of waiting
indefinitely. The next run restores its snapshot rather than trying to remove
repository or build output from a guest.

The local `watchdog` configuration may override these defaults:

```json
{
  "watchdog": {
    "ssh_seconds": 120,
    "readiness_seconds": 180,
    "checkout_seconds": 300,
    "build_seconds": 1200,
    "app_seconds": 180,
    "overall_seconds": 1800,
    "poll_seconds": 2
  }
}
```

The host controller intentionally does **not** install guest dependencies,
grant Accessibility/TCC permissions, disable Parallels sharing, or alter a
baseline. Those are explicit one-time provisioning tasks and must be captured
in the next clean baseline before the controller is trusted for acceptance.

### Phase 8 — Reliability qualification

- Run at least seven consecutive scheduled executions per platform.
- Track failures without silent retries.
- Classify product vs infrastructure failures.
- Only then decide whether any VM evidence should become release- or PR-blocking.

## Acceptance Record Integration

After a qualifying run, update
[`milestone-acceptance-record.md`](milestone-acceptance-record.md) with:

- candidate SHA;
- run ID;
- provider/version;
- host architecture;
- guest OS/version/architecture;
- Linux desktop protocol/compositor;
- Windows architecture classification;
- macOS host/guest version pair;
- P3/P4/P5 checks actually executed;
- result;
- artifact/workflow link; and
- explicit remaining gaps.

Do not change a milestone from `validation pending` to `accepted` merely because
one VM run passed if another gate condition remains open.

## Evidence Run Log

This section records actual runs of the lab. Entries here are evidence, not
framework design; keep them append-only and short. The full schema-validated
manifest/bundle described above is not yet implemented (see Definition of Done
below) — these entries describe manually operated runs performed directly
against the three VMs while that automation does not yet exist.

### 2026-08-10: manual VM lab execution

**Candidate SHA:** `bcfd7a7` (`main`). **Operator:** Copilot CLI, driven
interactively by the repository owner (VM GUI logins performed by the human
operator; all builds, tests, and screenshots driven by the assistant over
SSH/`prlctl`).

**Pre-run isolation hardening:** discovered and fixed a lab-isolation gap
(host Desktop/Documents/clipboard/cloud sharing enabled by VM template
defaults) before collecting evidence; see [#36](https://github.com/fes/fesTerm/issues/36)
for making that fix durable.

| Platform | Build | PTY/bounded-shutdown | Native-window smoke | Notes |
| --- | --- | --- | --- | --- |
| Linux (Xvfb) | pass | pass (both required tests) | **fail** | `resize_count=4` correct but `generations.len()=5`; see [#33](https://github.com/fes/fesTerm/issues/33). |
| Linux (real GNOME/Wayland desktop) | — | — | **fail** | `focus=true` achieved (first real-desktop focus evidence for [#21](https://github.com/fes/fesTerm/issues/21)); timed out in `AwaitInitialOutput` before any PTY output was observed, alongside an `eglCreateContext`/`EGL_BAD_MATCH` warning; see [#35](https://github.com/fes/fesTerm/issues/35). |
| macOS | pass | pass (advisory) | **pass** | Required running the binary as the console-logged-in user, not the SSH-only build user, for the process to attach to the real WindowServer session; passed with real focus and all 4 resize generations after that. |
| Windows | pass | pass | **could not execute** | No working GPU surface under Vulkan, DX12, or GL; see [#32](https://github.com/fes/fesTerm/issues/32). |
| Windows (ConPTY retention smoke, `stage-conpty.ps1 -RunSmoke`) | — | — | **fail** | Staged correctly (after installing portable `pwsh` 7 to match CI's `shell: pwsh`, since the VM's Windows PowerShell 5.1 cannot `Expand-Archive` a `.nupkg`), but the bundled retention smoke failed a visible-cell assertion; see [#34](https://github.com/fes/fesTerm/issues/34). |

None of these findings are confirmed product regressions. They require
correlation against real CI and/or real (non-VM) hardware before being treated
as anything more than VM-lab environment evidence. Full logs and screenshots
for this run are retained outside the repository per the no-sensitive-content
policy below; see the linked issues for reproduction commands and detail.

## Definition of Done for the VM Framework

The framework itself is implemented when:

- one host command can target Windows, Linux, macOS, or all three;
- every target starts from a reproducible clean baseline;
- the tested guest commit exactly matches the requested SHA;
- Windows runs the existing ConPTY/native/OS-input evidence;
- Linux runs in a real desktop session with required focus and independent OS
  input/resize evidence;
- macOS runs in a real guest WindowServer session with required focus and a new
  independent OS-input smoke;
- host-side screenshots are captured from all three platforms;
- every run produces schema-validated machine-readable manifests;
- no terminal/user-sensitive content is collected by default;
- no product test is silently retried;
- infrastructure failures remain distinguishable from product failures;
- the GitHub workflow can dispatch the lab by candidate SHA; and
- documentation records exactly what VM evidence can and cannot prove.

## External Hypervisor References

These links are implementation references, not fesTerm acceptance criteria.
Verify the currently installed hypervisor version against its current
documentation before implementing provider-specific behavior.

- Parallels CLI VM creation, including macOS `.ipsw` creation and Apple Silicon
  constraints: <https://docs.parallels.com/landing/parallels-desktop-developers-guide/command-line-interface-utility/manage-virtual-machines-from-cli/general-virtual-machine-management/create-a-virtual-machine>
- Parallels snapshot switching:
  <https://docs.parallels.com/landing/parallels-desktop-developers-guide/command-line-interface-utility/manage-virtual-machines-from-cli/snapshot-management/reverting-to-a-snapshot>
- Parallels host-side VM screen capture:
  <https://docs.parallels.com/parallels-desktop-developers-guide/command-line-interface-utility/manage-virtual-machines-from-cli/general-virtual-machine-management/capture-a-screen-area>
- Parallels macOS VM overview:
  <https://docs.parallels.com/landing/pdfm-ug/v20-en-us/parallels-desktop-for-mac-20-users-guide/advanced-topics/using-other-operating-systems-on-your-mac/running-macos-virtual-machines>
- VMware Fusion Apple Silicon guest compatibility, including the current lack
  of ARM macOS guest support:
  <https://knowledge.broadcom.com/external/article/315602/compatibility-considerations-for-arm-gue.html>

## First Copilot Instruction

A useful first handoff prompt is:

```text
Read docs/vm-evidence-framework.md, docs/native-smoke-policy.md,
docs/ui-test-plan.md, docs/milestone-acceptance-record.md, and
scripts/run-optional-validation.{sh,ps1}. Implement Phase 1 of the VM evidence
framework only. Preserve the existing validation semantics. Use Parallels as
the first provider, keep provider calls behind the documented interface, add a
dry-run path and tests where possible, and do not add or change milestone
acceptance claims until real evidence exists.
```
