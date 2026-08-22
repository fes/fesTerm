# M10 distribution sketch: `cargo-dist` + GitHub Releases

Implements ADR-0021. This is a planning sketch, not yet wired in — no
`cargo dist init` has been run against this repository. Config shown here is
illustrative and should be regenerated/reconciled against whatever
`cargo-dist` version is current when M10 packaging work starts.

## Prerequisites before running `cargo dist init`

- [ ] `cargo-dist` installed (`cargo install cargo-dist` or via its own
      installer script) on a dev machine — not required in CI, which pins its
      own version.
- [ ] Windows Authenticode code-signing certificate provisioned and stored as
      a GitHub Actions secret (e.g. `WINDOWS_CERT_PFX` + `WINDOWS_CERT_PASSWORD`).
      See "Where to get certificates" below for vendor/format options.
- [ ] Apple Developer ID Application certificate + notarization credentials
      (App Store Connect API key) stored as GitHub Actions secrets
      (e.g. `APPLE_CERTIFICATE`, `APPLE_CERTIFICATE_PASSWORD`,
      `APPLE_API_KEY`, `APPLE_API_ISSUER`).
      See "Where to get certificates" below.
- [ ] Decide the release tag format (e.g. `v0.1.0`) and confirm it matches
      `workspace.package.version` in `Cargo.toml`.

## Where to get certificates

**Windows Authenticode:**
- Purchase from a CA/reseller: DigiCert, SSL.com, Sectigo, GlobalSign, or
  Certum. Roughly $70–$400/year depending on vendor and validation level
  (OV vs. EV).
- **EV (Extended Validation) is strongly recommended** for a new publisher —
  EV certs get instant SmartScreen reputation, while OV certs must build
  reputation over time via install volume, so early users would see warnings
  regardless of signing.
- EV certs increasingly require hardware-backed storage (USB HSM token or a
  cloud HSM) rather than a plain `.pfx` file — plan the CI signing step
  around whichever storage the chosen vendor requires.
- **Alternative worth evaluating first:** **Azure Trusted Signing**
  (Microsoft's own pay-as-you-go signing service). No separate CA purchase,
  integrates directly into CI, and grants SmartScreen trust comparable to
  EV — may avoid buying/managing a certificate at all.

**Apple Developer ID (macOS signing + notarization):**
- Requires an **Apple Developer Program** membership — $99/year, enrolled at
  developer.apple.com (individual or organization account).
- Once enrolled, the "Developer ID Application" certificate is generated free
  via Xcode or the Apple Developer portal (Certificates, Identifiers &
  Profiles).
- Notarization uses the same account: generate an **App Store Connect API
  key** (also free, under App Store Connect → Users and Access →
  Integrations) for CI-based `notarytool` submission — no cost beyond the
  $99/year membership.

## Sketch: `Cargo.toml` additions

```toml
[workspace.metadata.dist]
# cargo-dist writes/maintains most of this block itself via `cargo dist init`
# and `cargo dist generate`; shown here for review, not hand-authored.
cargo-dist-version = "0.X"          # pin to whatever is current at M10
ci = ["github"]
installers = ["shell", "powershell", "msi"]
targets = [
    "x86_64-pc-windows-msvc",
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
]
# Binary lives in app/festerm, not the workspace root — point at it explicitly.
dist-workspace-members = ["app/festerm"]
pr-run-mode = "plan"
install-updater = true              # bundles axoupdater alongside the binary
```

Notes:
- `aarch64-apple-darwin` + `x86_64-apple-darwin` covers Apple Silicon and
  Intel Macs; `cargo-dist` can also emit a universal binary if preferred —
  decide once notarization is set up, since universal binaries change the
  signing/notarization invocation slightly.
- Linux `aarch64` is aspirational; drop it from `targets` if no ARM Linux
  support is committed to for M10 and CI minutes are a concern.
- `install-updater = true` is what pulls in `axoupdater` integration — this
  is the option that ties the build step to ADR-0021's update-check decision.

## Sketch: generated release workflow (`.github/workflows/release.yml`)

`cargo dist init` generates this file; do not hand-write it. Expected shape,
reusing the same three-OS runners already in `ci.yml`:

```yaml
name: Release

on:
  push:
    tags:
      - 'v[0-9]+.[0-9]+.[0-9]+*'
  pull_request:  # cargo-dist's "plan" mode dry-runs on PRs that touch config

jobs:
  plan:
    runs-on: ubuntu-latest
    # computes what would be built/released; posts a summary on PRs

  build-local-artifacts:
    strategy:
      matrix:
        include:
          - target: x86_64-pc-windows-msvc
            runner: windows-latest
          - target: aarch64-apple-darwin
            runner: macos-latest
          - target: x86_64-apple-darwin
            runner: macos-latest
          - target: x86_64-unknown-linux-gnu
            runner: ubuntu-latest
    runs-on: ${{ matrix.runner }}
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      # ... cargo-dist-managed build/sign/package steps per target
      # signing steps reference the GitHub secrets listed above

  host:
    needs: build-local-artifacts
    runs-on: ubuntu-latest
    # uploads all artifacts to a single GitHub Release for the tag

  publish-homebrew-formula:  # optional, evaluate later
    needs: host
    if: false  # not enabled initially
```

Signing is added as extra steps `cargo-dist` supports via its config
(`[workspace.metadata.dist.github-custom-runners]` or signing hooks,
version-dependent) rather than hand-patched into the generated file, so
regenerating the workflow after a `cargo-dist` upgrade doesn't silently drop
signing.

## Sketch: in-app update check (`axoupdater`)

Not yet implemented; sketch of the intended shape once M10 reaches the UI
work:

```rust
// crates/festerm-ui-egui or app/festerm, exact home TBD at implementation time
let updater = axoupdater::AxoUpdater::new_for("festerm");
if let Some(release) = updater.query_new_version().await? {
    // Surface as a passive notice; never auto-apply.
    // User-initiated action triggers updater.run() to download + swap.
}
```

Per ADR-0021's stated invariant, this must never install without explicit
user confirmation — the update check populates UI state only; a distinct
user action (not yet given a stable action-graph edge ID) triggers the
actual download/replace.

## Open questions to resolve before implementation

1. Which vendor/path to actually use: Windows EV cert from a traditional CA
   vs. Azure Trusted Signing; and whether the Apple Developer Program
   enrollment is under an individual or organization account.
2. Whether Linux ships `.deb` only, or also an AppImage/`.rpm` — affects
   `installers` list above.
3. Where in `festerm-ui-egui`'s existing screen/command model an
   update-available notice and "check now" action should live, and what
   stable action-graph edge ID(s) (`docs/gui-action-graph.md`) it needs.
4. Release cadence/tagging convention (who cuts a tag, how `Cargo.toml`
   version bumps are sequenced with the tag push).
