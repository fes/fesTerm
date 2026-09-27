# iOS rendering spike

Experimental Phase 1 host for the existing fesTerm Rust core and egui
renderer. This is **not yet an SSH client**. It displays a bounded ANSI/Unicode
fixture, routes input into a counting/discarding sink and exposes lifecycle
counters. See [ADR 0040](../../docs/adr/0040-ios-rendering-spike-host.md).

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

This creates fresh iPhone and iPad Simulators using an installed iOS runtime,
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

The iOS workflow publishes these files as `festerm-ios-simulator-evidence`,
including partial evidence on failure. The opt-in aggregate validation scripts
run this suite on macOS and report it skipped on other platforms.

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

- Shared terminal fixture with ANSI colors, combining/wide glyph coverage and
  emoji; scrollback limited to 256 KiB, resize handled by the existing view.
- A docked Esc/Tab/Ctrl/Alt row and persistent native keyboard request
  use the same vertical arrangement on iPhone and iPad, including landscape
  and Split View. A UIKit keyboard-layout guide measures available space;
  hardware-keyboard use reclaims system-keyboard space but retains the row.
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
  paste/link handling, SSH, SFTP, profiles, secrets or background sockets yet.
- No product-level performance or accessibility claim. Safe areas, rotation,
  touch behavior, software keyboard feasibility, foreground Metal recovery,
  memory pressure and process death need native evidence under `MOB-01`–`04`.

`.github/workflows/ios-spike.yml` checks device compilation and builds the
Simulator bundle, then runs the isolated launch evidence suite. Build or
process-survival success is not a passed native interaction gate. Use
the scenarios in [manual validation](../../docs/manual-validation.md); record
failures and reassess the hosting path before Phase 2 if input/lifecycle would
require an upstream fork.
