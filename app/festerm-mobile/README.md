# iOS rendering spike

Experimental host for the existing fesTerm Rust core and egui renderer. This
is **not yet an SSH client**. It combines the original bounded terminal probe
with synthetic Files/SFTP and Markdown workspaces so the responsive mobile
interaction design can be tested before credentials, transport, persistence or
real file access exist. See
[ADR 0042](../../docs/adr/0042-ios-rendering-spike-host.md).

## Run on an iOS Simulator

Requires macOS, full Xcode selected with `xcode-select`, an installed iOS
Simulator runtime, Rust stable, and Python 3. Boot one iPhone or iPad Simulator
using Xcode, then from the repository root:

```sh
python3 scripts/build-ios-spike.py --run
```

The script installs the host-appropriate Rust Simulator target, builds using
the selected SDK and iOS 15 deployment floor, creates
`target/ios-spike/fesTermSpike.app`, signs it ad hoc, then installs and launches
it. Use `--simulator <UDID>` when multiple Simulators are booted. Omit `--run`
to build only. It does not create or erase Simulators.
The adjacent `fesTermSpike-simulator.tar.gz` preserves executable permissions
for CI artifact downloads; extract it before installing the `.app` bundle.

For software-keyboard evidence in Xcode 27's Device Hub, select the intended
Simulator and open **Device > Keyboard**, then uncheck **Simulate Hardware
Keyboard**. The submenu is populated when opened; querying it while closed
can misleadingly report only a disabled software-keyboard toggle.
**Keyboard Capture** is a separate control, not the hardware-keyboard setting.
Older Simulator.app preferences do not configure Device Hub.

The generated app bundle is a local development artifact, not an IPA or
TestFlight build. Physical devices still require a separately provisioned and
signed application; that packaging is outside this slice. Device compilation
can be checked on a Mac with:

```sh
rustup target add aarch64-apple-ios
IPHONEOS_DEPLOYMENT_TARGET=15.0 cargo check --locked -p festerm-mobile --target aarch64-apple-ios
```

## Automated Simulator launch evidence

After building on macOS, run:

```sh
python3 scripts/smoke-ios-simulator.py --run
# Or build first in the same command:
python3 scripts/smoke-ios-simulator.py --run --build
```

This creates fresh iPhone and iPad Simulators using an installed iOS runtime
matching the selected Xcode Simulator SDK,
installs only the offline spike, checks process survival after launch and
relaunch, and captures both screens. It shuts down and deletes only its own
devices. Existing Simulators are never selected for mutation. Every command
has a deadline; failures and cleanup errors fail the run. No runtime downloads,
provisioning, credentials, or physical devices are involved.

Each run writes `target/ios-smoke/<run-id>/manifest.json`, four PNGs on success,
and a command/diagnostic log. The manifest includes the commit, Xcode version,
selected runtime/model, process IDs and screenshot dimensions. A process can
remain alive while rendering incorrectly: a passing result requires visual
review and does not qualify keyboard/IME, gestures or background/resume.

The iOS workflow runs this smoke nightly at 08:17 UTC on the default branch
and through its manual **Run workflow** action. It publishes these files as
`festerm-ios-simulator-evidence`, including partial evidence on failure.
Affected PRs still run mobile unit/dependency checks, device compilation and
Simulator app builds, but skip native preparation and runtime smoke.
The opt-in aggregate validation scripts run the suite on macOS and report it
skipped on other platforms.

This separates experimental Simulator/hosted-runner failures from desktop
merge gates; it does not call a failed smoke successful. Scheduled/manual
smoke failures still fail their runs and remain tracked in #303, with no
retry or relaxed deadlines, device ownership, UI/liveness or capture checks.
Restore an explicit runtime merge gate through review when mobile product
support is accepted; passing screenshots alone do not accept that support.

## Portable checks and desktop harness

```sh
cargo test --locked -p festerm-mobile --lib
cargo clippy --locked -p festerm-mobile --all-targets -- -D warnings
python3 scripts/build-ios-spike.py --check-dependencies
cargo run --locked -p festerm-mobile
```

The last command opens the same UI in a phone-sized desktop window. Desktop
success does not establish iOS rendering, touch, keyboard or lifecycle support.

## Current behavior and remaining gate

- A compact top strip switches among Terminal, Files and README workflow
  previews. Each workspace keeps its in-memory state while another workspace
  is active. These are fixed preview categories, not full create/rename/reorder/
  close session tabs.
- Shared terminal fixture with ANSI colors, combining/wide glyph coverage and
  emoji; scrollback limited to 256 KiB, resize handled by the existing view.
- A docked Esc/Tab/Ctrl/Alt row and persistent native keyboard request
  use the same vertical arrangement on iPhone and iPad, including landscape
  and Split View. A UIKit keyboard-layout guide measures available space;
  hardware-keyboard use reclaims system-keyboard space but retains the row.
  The mobile host retains egui focus without re-requesting existing focus or
  blurring on accessory taps. Repeated focus requests interrupt egui IME
  composition and make UIKit repeatedly hide/reopen the keyboard, even when
  the app sets `should_interrupt_composition` to false inside its UI callback.
  The regression checks the final platform output after egui completes the
  frame, including accessory taps, keyboard events, geometry changes and resume.
  One-shot Ctrl/Alt chords and ordinary hardware input use the core encoder. Counters
  retain no typed text; nothing executes. Reset returns to a known fixture.
- Native resume/suspend/memory-warning counters. No rendering while suspended;
  the next frame after a memory warning rebuilds view caches while preserving
  text size. OS termination
  starts a new fixture, with no restoration claim.
- Hold the terminal for 450 ms, then drag to send arrows. A temporary helper
  highlights direction; dragging farther selects one of three repeat speeds.
  Return to the center to pause; release to dismiss. There are no permanent
  arrow buttons. Early drags reach the shared renderer; cancellation stops
  repeats without leaking mouse reports. Native gesture feel remains unverified.
- Place two fingers on the terminal and pinch to resize its text (8–32 points).
  Chrome and keyboard stay the same size. A second finger takes over a pending
  hold or arrow gesture; after either finger lifts, input stays captured until
  both are up. An ordinary drag already in progress retains ownership: lift
  first, then start the pinch. Reset fixture restores the default text size.
- Standard renderer selection/scrolling is exposed for native testing. No
  double-tap Tab, native selection handles,
  paste/link handling, SSH, profiles, secrets or background sockets yet.
- Files uses repository-owned synthetic Local and Remote listings. Wide space
  renders a horizontal split, Compact space stacks Remote above Local, and
  Minimal space shows one focused pane. Selection controls upload/download
  direction; fake transfers have bounded progress history and filename
  collisions require Replace, Keep both or Skip. Nothing is read, written,
  uploaded or downloaded.
- README uses the shared bounded `festerm-markdown` parser over a synthetic
  runbook. Preview and Source modes are available; Wide space keeps a contents
  rail beside the document and narrower space opens Contents above it. Heading
  activation selects and scrolls to the section. Links/resources stay inert.
- No product-level performance or accessibility claim. Safe areas, rotation,
  touch behavior, software keyboard feasibility, foreground Metal recovery,
  memory pressure and process death need native evidence under `MOB-01`–`04`.

`.github/workflows/ios-spike.yml` checks device compilation and builds the
Simulator bundle on affected PRs; nightly/manual runs additionally execute
the isolated launch evidence suite. Build or
process-survival success is not a passed native interaction gate. Use
the scenarios in [manual validation](../../docs/manual-validation.md); record
failures and reassess the hosting path before Phase 2 if input/lifecycle would
require an upstream fork.

### Native checkpoint

The SDK-matched iOS 18.5 run at `7670d52` did not pass: iPhone launch timed
out and iPad launch/relaunch screenshots were black. The runner now captures
app stdout/stderr and rejects a live process that never builds its first UI.
The diagnostic run found a rejected GPU limit on both devices (16 requested
inter-stage variables versus 15 supported). The mobile configuration now uses
wgpu's downlevel limits and preserves adapter texture dimensions; native rerun
and visual review are still required. See ADR 0042 and issue #261;
rendering, keyboard and gesture acceptance remain open.

A later local iPhone 17 / iOS 26.5 run using the Xcode 27 SDK paints the
fixture and reproduces the repeated-focus keyboard restart loop described
above. With the focus correction and simulated hardware keyboard disconnected,
the software keyboard stays visible while idle, after native character,
Return and Delete actions, and after background/foreground. This is bounded
local evidence, not SDK-matched CI or iPad/physical-device qualification;
native accessory-touch, complex IME and gesture acceptance remain open.

The workflow extension has also rendered Compact Files and Markdown/Contents
on that iPhone and the Wide terminal surface on an isolated iPad Pro 13-inch
(M5) Simulator. Those local captures are bounded visual evidence, not
qualification of iPad workflow interaction, rotation, multitasking,
accessibility or a physical device.
