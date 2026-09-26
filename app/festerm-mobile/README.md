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
- A docked Esc/Tab/Ctrl/Alt/arrow row and persistent native keyboard request
  use the same vertical arrangement on iPhone and iPad, including landscape
  and Split View. A UIKit keyboard-layout guide measures available space;
  hardware-keyboard use reclaims system-keyboard space but retains the row.
  One-shot Ctrl/Alt chords and ordinary hardware input use the core encoder. Counters
  retain no typed text; nothing executes. Reset returns to a known fixture.
- Native resume/suspend/memory-warning counters. No rendering while suspended;
  the next frame after a memory warning rebuilds view caches. OS termination
  starts a new fixture, with no restoration claim.
- Standard renderer selection/scrolling is exposed for native testing. No
  advanced mobile gesture remapping, native selection handles,
  paste/link handling, SSH, SFTP, profiles, secrets or background sockets yet.
- No product-level performance or accessibility claim. Safe areas, rotation,
  touch behavior, software keyboard feasibility, foreground Metal recovery,
  memory pressure and process death need native evidence under `MOB-01`–`03`.

`.github/workflows/ios-spike.yml` checks device compilation and builds the
Simulator bundle. Build success is not a passed native interaction gate. Use
the scenarios in [manual validation](../../docs/manual-validation.md); record
failures and reassess the hosting path before Phase 2 if input/lifecycle would
require an upstream fork.
