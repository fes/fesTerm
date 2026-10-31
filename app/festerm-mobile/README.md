# iOS rendering spike

Experimental host for the existing fesTerm Rust core and egui renderer. The default
build is an offline fixture; the opt-in `live-ssh` prototype connects one
transient SSH session, pending native/live-service qualification. It combines the original bounded terminal probe
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

The generated Simulator app bundle is a local development artifact, not a
physical-device app, IPA or TestFlight build. Device compilation can be checked
on a Mac with:

```sh
rustup target add aarch64-apple-ios
IPHONEOS_DEPLOYMENT_TARGET=15.0 cargo check --locked -p festerm-mobile --target aarch64-apple-ios
```

## Opt-in live SSH prototype (campaign #422/#424)

Build and run the shared mobile host with a transient SSH connection UI:

```sh
cargo run --locked -p festerm-mobile --features live-ssh
# On macOS/Xcode, create a separate Simulator bundle:
python3 scripts/build-ios-spike.py --live-ssh --run
```

The feature bundle/archive is under `target/ios-spike-live/`, separate from
the offline smoke bundle. CI links and publishes it as
`festerm-ios-live-ssh-simulator`. Offline nightly launch evidence continues
to exercise the default fixture, and does not establish live SSH acceptance.

Choose **Connect** in the compact header, enter host/port/username, and choose
**Connect** in the form. This replaces the previous connection with a fresh shell.
Review the host fingerprint before entering a password. Approved fingerprints
are pinned only for that canonical host/port during this app run; changed
keys are rejected. Passwords are transient with bounded sensitive/no-undo
editing. No saved profiles, persistent trust or credentials are implemented.

The connected terminal uses the existing persistent keyboard, Ctrl/Alt/Esc/Tab
and directional gesture helper. Connection/auth controls temporarily own input;
their text must never reach the terminal. The shared core encodes input and
protocol replies. Refused backend input disconnects with a notice rather than
silently dropping a command suffix or replaying it. Resize is coalesced; output
work per frame and backend queues are bounded. Disconnect retains the visible
terminal until a new connection or **More → Use offline demo** is selected.
Suspension requires an explicit new connection; it does not promise remote
process survival or uninterrupted background networking.

Termius for iOS is the interaction reference. This first form deliberately
uses one transient connection rather than saved-host navigation. Persistent
keyboard and the existing arrow helper are retained; paste, native selection
handles and broader mobile affordances remain #425 follow-up work. Files and
README stay offline previews. Before acceptance, exercise MOB-06 against an
owned SSH server on Simulator and provisioned iPhone/iPad, including a remote
TUI such as Copilot CLI. Feature compilation/linking and fake-session tests
do not prove native UI, authentication or network operation.

The header keeps only the current surface, a short mode indicator, Connect/Cancel
and More. Preview navigation, demo reset and content-free lifecycle/input counters
live under More; the counters are in Diagnostics. The form grows to its content
within 45% of measured safe space and uses 44-point field/button targets. Short
views scroll vertically. Menu interaction is kept out of terminal input/gestures;
the existing persistent keyboard and accessory/helper policy remain. Native
keyboard/touch usability remains MOB-06.

## Development-signed physical-device build

Campaign [#422](https://github.com/fes/fesTerm/issues/422) targets a real SSH
terminal on iPhone/iPad, with Termius for iOS as the interaction reference.
The default host remains offline; select `--live-ssh` explicitly to package
the transient SSH prototype. Neither mode establishes native feasibility.
The packaging path under
[#426](https://github.com/fes/fesTerm/issues/426) uses existing local signing
credentials rather than waiting for a TestFlight pipeline.

On a Mac with full Xcode selected, prepare an **Apple Development** identity
with its private key in the local keychain, an explicit registered bundle ID,
and an unexpired **iOS App Development** provisioning profile containing that
app, certificate and physical device UDID. An App Store/ad-hoc distribution
profile, wildcard app ID or macOS Developer ID identity will be refused.
Select the exact local certificate hash with:

```sh
security find-identity -v -p codesigning
```

Run preflight before spending time compiling. Substitute your local values;
keep the profile outside the repository and do not share its decoded contents:

```sh
python3 scripts/build-ios-device.py \
  --live-ssh \
  --profile /private/path/festerm-development.mobileprovision \
  --identity YOUR_40_HEX_CERTIFICATE_SHA1 \
  --team YOURTEAMID \
  --bundle-id your.registered.festerm.app \
  --device YOUR_PROVISIONED_DEVICE_UDID \
  --preflight-only
```

Remove `--preflight-only` to build `aarch64-apple-ios` with the selected
`iphoneos` SDK, embed the profile, sign with minimal development entitlements
and verify the signature. Keep `--live-ssh` on both preflight and build: it
checks the feature-enabled mobile dependency graph, compiles that feature and
includes the selected-host local-network usage description. It adds no extra
signing entitlements. Omit `--live-ssh` from both commands for the offline fixture.

The script prints a fresh app path below
`target/ios-device/development-live-ssh-*/fesTermSpike.app` for live mode, or
`target/ios-device/development-*/fesTermSpike.app` for the offline fixture.
Both use your explicit bundle ID, so installing one replaces an installed app
with that same ID. This private directory
contains provisioning/device metadata: do not upload it as a CI artifact or
commit it. Its `build-receipt.json` records source commit/dirty state,
executable hash, toolchain, `prototype_mode` and `cargo_features`, with
`native_validation: not-run`; it excludes
profile content, device UDID and signing identity. Only clean-source receipts
can be attributed solely to a commit.

Connect and trust the selected device, enable Developer Mode when required,
and use Xcode's device-management interface to install the printed `.app` on
that provisioned device. Launch it on-device and record MOB-01–06 results,
including the persistent keyboard and Termius-inspired touch interactions.
In live mode, open Connect, enter an owned test host, verify its fingerprint
through a separate trusted channel, approve it and authenticate. Exercise the
MOB-06 remote shell/TUI, resize, disconnect and suspension checklist; keep
credentials and private host/device identifiers out of demo artifacts.
The script never installs, launches, enrolls an account, downloads profiles,
or uploads a build. Local signing can require keychain approval. Native build,
install and rendering remain pending until exercised on a provisioned Mac
and device; portable packaging tests do not certify them.

## Headless UI review images

For campaign #422's screenshot review, render the actual mobile widgets with
repository-owned fixtures, without a native window, user data or SSH worker:

```sh
python3 scripts/capture-mobile-ui.py
```

This opt-in generator needs Rust and a working headless wgpu adapter. It writes
four PNGs (phone/tablet terminal and empty connection form) into a fresh
`target/mobile-ui-captures/<attempt>/` directory. `--output <new-directory>`
selects another fresh destination; existing attempts, including failures, are
never overwritten. The manifest records exact Git commit/dirty state, host OS,
viewport, image hashes and capture method. The optional aggregate runners
include it when `FESTERM_RUN_MOBILE_UI_CAPTURE=1` is selected alongside their
global opt-in. PR iOS CI publishes `festerm-mobile-headless-ui-review` separately
from Simulator artifacts.
CI scopes the destination by run ID and attempt so restored build-cache
directories cannot collide with or be published as a fresh capture.

These are **non-native headless review images**, with a pinned Dark preference
and a simulated reserved keyboard area. The native system keyboard is not
rendered. They do not show a live SSH shell or establish UIKit layout, native
keyboard/touch, Simulator/device rendering or MOB-01–06 acceptance. Inspect
the PNGs and source attribution before sharing them in chat/PR review. Native
captures remain separately required; Termius interaction policy is unchanged.

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
Their check stays `simulator-build`; nightly/manual runs use the distinct
`experimental-simulator-smoke` check, including manual runs on a PR branch.
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

The dependency command checks both the offline mobile host and the real
`festerm-ssh` normal/build graphs on device and ARM64 Simulator targets.
The iOS workflow separately runs:

```sh
cargo check --locked -p festerm-ssh --target aarch64-apple-ios
```

This exposes transport portability blockers before connection-UI integration.
Desktop secret-service adapters are excluded from iOS, and native persistent
storage returns Unsupported until a reviewed iOS Keychain backend exists.
No in-memory/plaintext persistent fallback is substituted. The host remains
offline; successful graph inspection/compilation does not establish transport
linking, network connectivity, explicit host trust, keyboard usability or a
working device SSH session. Live integration is campaign #422/#424.

The last command opens the same UI in a phone-sized desktop window. Desktop
success does not establish iOS rendering, touch, keyboard or lifecycle support.

## Current behavior and remaining gate

- More switches among Terminal, Files and README workflow
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
  retain no typed text; nothing executes. More → Reset demo returns to a known fixture.
- Native resume/suspend/memory-warning counters under More → Diagnostics. No rendering while suspended;
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

The owner-provided iOS product boards guide the mobile-only navy/blue visual
pass, two-line host identity, labelled connection fields and Esc/Tab/Ctrl/Alt
order. See [scope and deliberate prototype differences](../../docs/mobile-layout-design.md#owner-provided-product-mock-ups).
Actual-widget headless captures support review; native appearance/input and
the deferred product screens are not established by this styling pass.

Disconnected terminals retain local scroll/selection/Copy while typing, paste,
mouse/focus transport reports and the native terminal keyboard stay disabled.
A bounded recovery card offers a fresh-shell form; opening/cancelling it or
failing synchronous validation preserves history. Explicit Connect replaces
history when its worker starts, before authentication. Details remain available
without a verbose top strip; earlier-execution warnings stay visible. Native
touch/clipboard, dropped-network recovery and signed-device SSH remain MOB-06
checks, not established by synthetic disconnected gallery images.
