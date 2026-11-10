# Authenticated helper release artifacts

This is the independent authenticity foundation of [ADR 0084 §9](adr/0084-host-based-dev-tunnels.md#9-packaging-integrity-and-upgrades),
tracked in [#681](https://github.com/fes/fesTerm/issues/681), not completion of
stage 7. The separate schema-2 host package contains sessiond, tunnel bridge, setup and
the GUI-free `festerm-host` CLI (plus the Windows worker), VERSION, LICENSE and
THIRD_PARTY_NOTICES.txt. These six native targets use the existing helper build,
macOS signing/notarization and Windows Authenticode paths; Linux uses the shared
updater signature. Native release qualification is not established by fixtures.
Normal CI runs `python scripts/check_host_notices.py` against actual native
Cargo metadata, including `app/festerm-host` workspace membership.

## Wire contract and trust

Legacy `fesTerm-tunnel-VERSION-PLATFORM-ARCH.tar.gz` (Windows `.zip`) keeps its
exact published helper entry allowlist and schema-1 `festerm-helpers` identity.
Old installers fetch those names and reject additional entries. The additive
`fesTerm-host-VERSION-PLATFORM-ARCH.tar.gz` (Windows `.zip`) includes the host
CLI and its own dependency notices. Each family has a `.sha256`,
`.manifest.json`, and `.manifest.json.sig`. Schema 2 is strict JSON:

```json
{"schema":2,"package":"festerm-host","version":"1.2.3","platform":"linux","architecture":"aarch64","filename":"fesTerm-host-1.2.3-linux-aarch64.tar.gz","length":12345,"sha256":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"}
```

The example's length/hash are illustrative, not download pins. Versions are
exact stable numeric semver (no prefix, prerelease, build suffix or leading
zeroes); platforms are `linux`, `macos`, `windows`; architectures are `x86_64`,
`aarch64`. Package/schema/filename identities cannot substitute for each other.
Unknown/duplicate fields,
unknown schema/package/target values and noncanonical hashes are rejected.
Schema 1 is not accepted as schema 2: it never guaranteed host CLI inclusion.

`scripts/build_host_manifest.py` binds the actual archive length and SHA-256
after the existing package/notice checks. The protected release publish job
requires the existing updater private key/password and requires the configured
public key to equal `packaging/updater.pub`. Pinned cargo-packager 0.11.8 signs
the **exact manifest bytes**, using its existing base64-encoded Minisign
signature format (including authenticated trusted comment). The independent
verifier uses the same `minisign-verify` implementation as the desktop updater,
requires modern prehashed signatures and embeds that same public key. It
accepts no key supplied alongside a download.
Cargo installation/build and manifest generation run in earlier steps without
signing secrets. Only the signing step receives the private key/password and
runs the sign commands; verification runs afterwards without those secrets.

## Verification and staging API

The optional `festerm-host-setup/artifacts` feature enables the GUI-free
`festerm_host_setup::artifacts::verify(manifest, signature, archive,
&ExpectedArtifact { version, platform, architecture, package })` API, returning a
private-constructor `VerifiedArtifact`. The legacy/mobile default dependency
graph remains unchanged. Expected metadata must come from
trusted installation policy, not from the untrusted manifest. Authentication
precedes parsing metadata and archive decoding; all expected identity fields,
exact filename, byte length and digest must match. There is no unsigned or
checksum-only fallback.

`VerifiedArtifact::manifest()` exposes authenticated metadata;
`entries()` exposes immutable `VerifiedEntry::{name,bytes,executable}` getters.
`manifest_bytes()`, `signature_bytes()` and `archive_bytes()` retain the exact
original authenticated provenance for restart verification; reserializing JSON
is not equivalent. On restart, reverify that provenance and compare each staged
file to the resulting verified entries before execution (or restage privately).

Desktop primary Setup receives that owned capability directly from official
acquisition and calls existing `TrustedStage::publish`, retaining real native
publisher qualification and installation/controller authority. Explicit Advanced
offline input uses `staging::verify_release_files(manifest, signature, archive)`:
bounded private/no-follow reads, the fixed current native target/version and
embedded root, returning owned bytes without staging or execution. Neither path
reopens the input archive after authentication, serializes a capability or accepts
a caller's URL/version/key. Manual files are never automatic error fallback.
The archive hash alone does not authenticate a subsequently modified extracted
file. The descriptor owns the exact validated file bytes. A consuming setup/staging
owner must write these bytes, not reopen or separately extract the downloaded
path. It must create private owner-controlled directories/files using the
existing platform security facade's no-follow/exclusive/handle-bound prior art,
sync and publish without replacement atomically. Authentication alone does not
authorize service switching, execution, pruning referenced generations or
grant changes. Native executable signer checks remain additional requirements.

Bounds: manifest/signature each 4 KiB, compressed archive 256 MiB, expanded
archive 512 MiB, executable entry 128 MiB, each metadata/notice entry 16 MiB;
version at most 32 bytes. Downloaders must enforce the same limits while
receiving bytes and apply their own time/cancellation limits.
The verifier enforces the exact seven Unix/eight Windows host entries or the
unchanged six Unix/seven Windows legacy helper entries,
nonempty regular files, exact 0755/0644 modes and exact VERSION contents.
It refuses duplicates, missing/extra/nested/absolute/traversal names,
symlinks, hardlinks, special files, tar extension records, encrypted ZIPs,
ZIP64/multidisk/commented ZIPs, oversized headers, nonzero data after TAR end
markers and trailing gzip streams.
Central-directory count/size is bounded before ZIP parser allocation.
No archive path is passed to a filesystem extraction API.

An offline command uses the same API and reads each input with a size cap:

```text
festerm-verify-artifact --host 1.2.3 linux aarch64 fesTerm-host-1.2.3-linux-aarch64.tar.gz
```

It reads the adjacent manifest/signature, validates, and **only reports**;
it never extracts, installs or executes anything. Release publication runs it
against both families on all six targets before uploading assets. Omit `--host`
only for the legacy helper identity. Producers likewise default to the legacy
ABI; `--host` explicitly selects the separate host archive/manifest.

## Bootstrap, rotation, downgrade and compatibility

The first verifier must come from an already authenticated installation or a
source revision obtained and reviewed through an independently trusted channel,
then built locally with the locked dependencies. Building the standalone
`festerm-verify-artifact` target (with `--features artifacts`) does not depend
on the desktop/GUI. Downloading
both verifier and public key over the same unauthenticated channel does **not**
establish trust. A GitHub download URL or adjacent checksum is not this trust
root. No new Linux zero-trust `curl | sh` bootstrap is claimed.

Key rotation requires reviewed source changes to the existing shared updater
trust root and an independently authenticated verifier/package update. No
network-fetched key, automatic alternate key, or trust-on-first-use is allowed.
Compromise recovery requires an independent trust channel; signatures under a
compromised key cannot by themselves authenticate its replacement.

There is no automatic version selection, downgrade or retry against an older
release. Exact-version verification is not a freshness service: the installation
owner must compare against installed policy and reject lower versions by default.
Any future explicit rollback needs separate compatibility/state/grant checks;
this API does not authorize rollback or override revocation.

Legacy filenames, helper entries, checksums and scripts are retained unchanged;
only the separate host archives add the CLI and expanded notices. Existing
scripts still use their legacy integrity/native-signer path and do not gain
independent Linux publisher authentication from these additive assets. New
typed consumers fail if a manifest/signature is absent, including old releases.
Explicit protected recovery of a pre-authentication tag retains its old payload
contract and emits a notice, not an authenticated-helper claim. Normal releases
missing this tooling fail. Recovery still pins tagged runtime source and keeps its existing tooling-only
change allowlist and CI requirements. The protected workflow detects schema 2
from that exact source, never the download. Schema-1/pre-authentication recovery
retains the old helper set without claiming host inclusion; the reviewed macOS
packager override defaults to that same legacy set.
That payload cannot receive a schema-2 manifest.

## Official host-bundle acquisition

The optional `festerm-host-setup/acquisition` feature exposes the GUI-free
`acquisition::host_bundle::acquire_current_host_bundle(&CancellationToken)`
boundary. Its installation policy is deliberately fixed: this trusted build's
`CARGO_PKG_VERSION` and native compilation OS/architecture, schema 2,
`Package::Host`. Unknown targets/invalid versions refuse before requests. There
are no caller-selected versions, URLs, keys, latest lookup, downgrade, older
retry or legacy helper fallback. The six target policies include Windows ARM64;
this does not change Microsoft CLI acquisition's separate ARM64 refusal.

It constructs exactly
`https://github.com/fes/fesTerm/releases/download/vVERSION/fesTerm-host-VERSION-PLATFORM-ARCH.EXT`
and the adjacent `.manifest.json` and `.manifest.json.sig`, never `.sha256`.
Only HTTPS redirects to port 443 on the exact
`release-assets.githubusercontent.com` host under
`/github-production-release-asset/` are allowed, at most three hops. Userinfo,
fragments, HTTP downgrade and all other hosts/paths refuse. Host acquisition
reads raw Location itself before request construction; automatic
redirect following is disabled so normalization cannot erase credentials or
fragments before policy checks. Missing/duplicate/relative locations refuse.
The shared bounded transport disables proxies and automatic decompression and requests identity
encoding; the host client disables Referer and sends no authentication headers.
Redirect signed queries and response/transport details never enter typed errors.

Explicit callers must use a blocking worker outside a Tokio runtime. One owned
current-thread runtime bounds all three receives together to 120 seconds,
with 15-second connection and 120-second request limits. Cancellation drops
the asynchronous work, not a detached DNS/download thread. Manifest/signature
are each capped at 4 KiB while streaming; archive at 256 MiB, including unknown
length/chunked responses. Empty, oversized, encoded, truncated and non-200
responses refuse. `HostBundleError::NotPublished(asset)` on 404 is an explicit
publication **BLOCK**, not successful absence or a reason to fetch old helpers.

The existing embedded `packaging/updater.pub` and `artifacts::verify` authenticate
the exact owned bytes and apply the existing expanded/entry allowlists and
limits. Cancellation/deadline are checked before and after synchronous bounded
verification; it is not preempted mid-decoder and no capability is returned
after cancellation/deadline. The result is only `VerifiedArtifact`: no extraction
to disk, staging, chmod, execution, registration, enable, grants, account/provider
or OS changes. Existing first-verifier trust and release producer/signing
workflow are unchanged. Desktop primary Setup now explicitly consumes this
library seam on its bounded worker and delegates to the existing native
publisher/staging/controller boundary; headless commands retain their explicit
signed-file path. Desktop Advanced manual paths are a separate explicit choice,
never automatic error fallback. Genuine release/native acceptance is not implied.

Controlled loopback fixtures use a private test-only signer and verifier for all
six targets, exact identity/path/bytes, missing assets, unsafe redirects,
stream/length/encoding limits, timeout/cancellation and tamper/wrong-key refusal.
The production embedded-key verifier rejects that synthetic signer. These are
source-only acquisition tests, not genuine published release, native-publisher
or older-release compatibility evidence. Genuine schema-2 availability is not
qualified: unpublished assets/404 remain blocked until a protected release
actually publishes them. No real release asset download is part of these tests.

## Microsoft CLI acquisition

The optional shared `acquisition` feature and explicit headless `acquire-cli`
command download **only** pinned raw Microsoft bytes into protected,
non-executable staging. See [bounds and command semantics](headless-host.md).
The versioned origin is
`https://tunnelsassetsprod.blob.core.windows.net/cli/1.0.2115+e16833f284/`;
no mutable `aka.ms` redirect, version-output inspection or adjacent metadata
is a trust source. Existing reviewed executable SHA-256 values are retained.
The Windows x64 raw asset is `devtunnel.exe`; Unix assets are
`osx-{x64,arm64}-devtunnel` and `linux-{x64,arm64}-devtunnel`.
Windows ARM64 is explicitly unsupported by **new acquisition only**. Existing
legacy hosting/offline verification retains its reviewed Windows x64-emulation
selector; the native-only acquisition selector never uses that fallback.
The macOS package smoke downloads both pinned raw assets and verifies their
exact executable hashes, then applies the unchanged strict native codesign
Apple-generic anchor requirement to each raw file. It does not execute them.

Textual provenance, verified without downloading/executing dependency binaries:
[version-specific winget manifest](https://github.com/microsoft/winget-pkgs/tree/master/manifests/m/Microsoft/devtunnel/1.0.2115%2Be16833f284)
for Windows x64, and
[commit-pinned Homebrew source](https://github.com/Homebrew/homebrew-cask/blob/fb0b862b488a16d3413d0087f2a495ed39069e8a/Casks/d/devtunnel.rb)
for macOS/Linux. These sources corroborate the build's existing reviewed hashes;
they are not an independent Microsoft native-publisher qualification or a grant
of redistribution rights. No dependency binary is bundled.

The acquired path is **not** a trusted-executable/service-registration receipt.
The shared staging owner does not execute, chmod, sign in, modify services or
grant access. Future execution must additionally enforce platform-native
publisher/signing policy on retained verified handles before executable
publication. This qualification and the authenticated delivery of the initial
Fes verifier/host CLI remain blocked; downloaded binaries cannot authenticate a
new trust root fetched beside themselves.

## Evidence and remaining work

Rust synthetic-key tests cover six valid targets, unsigned/wrong-key/tampered
metadata, wrong version/platform/architecture/filename/length/hash, strict schema
and size limits, signed unsafe TAR/ZIP entries, duplicate/missing entries,
links, invalid modes/VERSION and oversized headers. Python fixtures cover
both manifest families on six targets, unchanged legacy bytes, overwrite refusal, target
rejection, workflow signing/verification order and signer-secret isolation.
Recovery fixtures exercise
the explicit old-tag exception and fail-closed ordinary releases.
`scripts/tests/test_tunnel_bundle.py` also sends real UTF-8 subprocess output
through a forced CP1252 default transport decoder, checking non-ASCII Cargo
metadata is preserved without lossy decoding.

`python_produced_six_bundles_and_manifests_authenticate_in_rust` invokes the real
Python bundle **and** manifest producers for both families on all six targets, signs their exact
manifest bytes with a synthetic test identity, and passes those archives and
signatures through the Rust verifier. This cross-language test runs with the
artifact feature on every CI platform; it does not use Rust-built archives.
These artifact-feature tests require Python 3.11+ on PATH (`python` on Windows,
`python3` elsewhere); this is a test dependency, not a verifier runtime dependency.
The separate Linux normal-CI signer job installs **cargo-packager 0.11.8
--locked**, then explicitly runs
`pinned_cargo_packager_signatures_authenticate_python_produced_manifest`.
That test generates its own throwaway key using the real CLI, signs all twelve
Python-produced manifests, checks modern prehashed mode and verifies with the
private test-only key seam. It also confirms the shipping embedded-key API
rejects the throwaway publisher and that changed signed bytes fail. The
expensive external-tool test is ignored in ordinary Rust runs, not omitted
from normal CI. No production trust override is exposed.

No actual release key, release dispatch or live installation is used by tests.
Native protected-release signing/download/rejection evidence on all six targets,
native installer/executable-publication qualification,
service switching/rollback, legacy migration and independently trusted first
Linux installation remain required under HOST-06/DT-05/DT-08 and #681.
The [shared setup boundary](headless-host.md) implements private atomic
no-replace staging for authenticated helper entries and reviewed Microsoft CLI
pins, with bounded HTTP acquisition covered by local synthetic fixtures.
Native installation/publisher qualification and service switching remain
deferred; cache files are not executable publication.
The off-default [native service component](native-host-services.md) has a
separate sealed executable-receipt factory for exact verified `Package::Host`
bytes. Ordinary staging/acquisition paths remain data only; the component is
not wired to incomplete endpoint/CLI setup and does not qualify native releases.
macOS promotion requires Apple's Developer ID chain, the pinned release team
and hardened runtime. Windows promotion requires the approved native policy below;
any trusted signer is not substituted.

### Windows native publisher policy

The owner independently approved publisher
`C=US, ST=wa, L=Seattle, O=Frank Swiderski, CN=Frank Swiderski` and
`fesLabs-CodeSign` / `fesTermPublicTrust`. The profile-specific EKU is
`1.3.6.1.4.1.311.97.303022872.891206086.435023656.127014475`; it identifies only
this profile lifetime, not the signing account, and changes on profile
recreation. Standard CodeSigning and Artifact Signing PublicTrust EKUs are also
mandatory. Same-subject/profile/approved-chain leaf renewal is allowed; any
publisher/profile/chain change requires review.

The compiled DER SHA256 pins were independently derived from Microsoft's public
PKI certificate downloads, never candidate certificates or manifest metadata:

| Chain certificate | Authoritative Microsoft source | DER SHA256 |
| --- | --- | --- |
| Microsoft ID Verified CS AOC CA 03 | [public certificate](https://www.microsoft.com/pkiops/certs/Microsoft%20ID%20Verified%20CS%20AOC%20CA%2003.crt) | `acaa07d57d4274b290d86c156aa0d6c5633e3cd92d43751e6a0f797c5b2dff4d` |
| Microsoft ID Verified Code Signing PCA 2021 | [public certificate](https://www.microsoft.com/pkiops/certs/Microsoft%20ID%20Verified%20Code%20Signing%20PCA%202021.crt) | `3d29798cc5d3f0644a7e0dc9cb1cade523ea5ec83b335109b605bfeaa7d5f5c1` |
| Microsoft Identity Verification Root Certificate Authority 2020 | [public certificate](https://www.microsoft.com/pkiops/certs/microsoft%20identity%20verification%20root%20certificate%20authority%202020.crt), [Microsoft repository](https://www.microsoft.com/pkiops/docs/repository.htm) | `5367f20c7ade0e2bca790915056d086b720c33c1fa2a2661acf787e3292e1270` |

These are additional restrictions on OS chain trust, not custom trust anchors.
[Microsoft certificate-management guidance](https://learn.microsoft.com/en-us/azure/artifact-signing/concept-certificate-management)
documents the 72-hour leaf lifetime, profile-specific EKU scope and timestamp
longevity. A genuine OS-trusted RFC3161 timestamp within leaf validity preserves
correctly signed binary validity beyond that leaf's expiry. Native verification
enables online revocation and fails closed on revoked/unknown/stale/unavailable
evidence; no custom 24-hour offline grace exists. See the
[complete policy/runtime contract](native-host-services.md).

Updater authentication, current whole-bundle checks, retained private file
identity and every executable helper's native publisher check remain independent
requirements. Synthetic policy tests and x64 MSVC check/strict Clippy of only
`festerm-windows-security --features host-service` are not real Windows
publisher/runtime acceptance or Windows compilation of the host-setup integration.
The [exact commands and SDK limitation](milestone-progress.md#windows-native-admission-without-destructive-stop)
are recorded separately. This policy neither authorizes signing/releases,
OS changes or VM execution nor grants host-owner/client trust.

### Opt-in Windows public PKI fixture

The existing CI workflow has a default-false `windows_native_pki` dispatch
input for the existing ignored
`windows_publisher_native_opt_in_approved_signed_host_and_unsigned_rejection`
test. It selects only the bounded `windows-native-pki` hosted `windows-latest`
job; ordinary CI, privileged security tests and sampler/GUI jobs are excluded
from this dispatch. PR/push checks are unchanged. Mixed sampler/PKI requests
fail before acquisition. The Ubuntu `pinned helper signer compatibility` job
tests synthetic cargo-packager interoperability, **not Windows native trust**.

**Operator gate:** first publish/review the exact source and obtain all three
exact-source gates, then obtain a separate owner authorization for one
read-only PKI run. This source change is not that authorization. After it:

```sh
gh workflow run ci.yml --ref <reviewed-ref> -f windows_native_pki=true -f windows_sampler_diagnostics=false
```

The repository-owned `scripts/run_windows_publisher_validation.py`, also reached
through `scripts/run-optional-validation.ps1 -WindowsPublisherOnly`, acquires only:

| Public read-only data fixture | Fixed value |
| --- | --- |
| URL | `https://github.com/fes/fesTerm/releases/download/v0.16.0/fesTerm-tunnel-0.16.0-windows-x86_64.zip` |
| ZIP SHA256 | `3edfbf5a0725b70a0bc56448d81bb3745e25d745a2cd670978ecc1f33f51e896` |
| Exact member | `festerm-sessiond.exe` |
| Member SHA256 | `4cd3e1b12026412717848cef2a872e110c50039bc0a6464ce8bb05a2781a481b` |

These public research pins provide **integrity only**, not independent updater
authentication; cohosted checksums are not fetched or promoted to authentication.
The existing compiled approved-publisher policy and native OS trust are the
authentication check. The legacy binary is opened as data and **never executed**.
It is not a new `Package::Host` bundle or a first-verifier/release qualification.

Acquisition has a 150-second parent-owned hard deadline, a 120-second body
deadline and 15-second network timeouts. HTTPS redirects are limited to the
public GitHub release-assets authority. ZIP/member bytes are capped at 128 MiB,
total declared expansion at 512 MiB and entries at 64; all entries must be
plain regular root files without duplicates, traversal or links. Only the exact
member is extracted, exclusively into a new checkout-relative directory; stale
evidence is never overwritten or reused. Evidence paths with nonempty anchors
(POSIX absolute or Windows rooted, drive-relative/absolute or UNC) are rejected
lexically before any checkout
join or filesystem access. The selected facade-only Cargo test
has a 600-second limit and the hosted job a 20-minute limit. No SDK workaround,
policy waiver, alternate fixture or automatic retry is provided.

Both `FESTERM_RUN_OPTIONAL_VALIDATION=1` and
`FESTERM_RUN_ADR84_PUBLISHER_NATIVE=1` are required for local opt-in.
The single-suite aggregate returns before other optional/native/GUI suites.
Python resolution selects the first PATH-priority application before reading
its `Source`; multiple executable matches are never joined or retried.
The existing offline preflight includes owned-fixture PowerShell regressions
for two matches, first-only execution and actual nonzero exit preservation.
They explicitly skip where PowerShell 7 is unavailable locally, but missing
PowerShell on Windows fails preflight rather than silently skipping.
Once the Python runner starts, the evidence artifact retains `report.json`,
source/tool-version/command logs,
actual exit codes, timeouts and SHA256 log hashes even on failure; binaries and
build outputs are not uploaded. Verify its exact source HEAD against the
reviewed head and its run ID/attempt against the authorized dispatch; old
reports are not new evidence. Zero selected tests cannot pass; genuine-fixture/policy failures
remain failures. A hosted PKI pass would prove only this public signed fixture
and unsigned-test-binary rejection under that runner's current OS trust and
online revocation—not ordinary-user VM, Task Scheduler, daemon, GUI, whole-bundle
or new-host/release acceptance.

The separately authorized historical [run 42876689716, attempt 1](https://github.com/fes/fesTerm/actions/runs/42876689716)
at exact `2ff42e255742019c8201b36ffeedcf1e41e09437` failed during PowerShell
launcher resolution of hosted CPython plus WindowsApps. Its 22 hosted Windows
source contracts passed, but the Python runner/acquisition/native PKI never
started. Only a failure summary was uploaded: no `report.json`, downloaded
fixture or PKI result exists. Raw logs/artifact and their hashes remain retained;
that one-run authorization is consumed. The source-only launcher correction is
not a retry or PKI acceptance and requires three fresh exact-source gates and a
separate new one-run authorization before any further dispatch.

A separate grant at exact `3030bcd7008e300a90b67fa6c6bc6c0a37ce88c1`
produced [run 42881664804, attempt 1](https://github.com/fes/fesTerm/actions/runs/42881664804):
25 preflight passes / zero skips, including both real PowerShell 0/37 fixtures;
acquisition exit 0 and both exact integrity hashes verified. The facade built,
then one genuine signed-positive test started / completed 0 / aborted 1 at
`native.rs:416:20`, a misaligned `CRYPT_TIMESTAMP_INFO.ftTime` read.
Native Cargo/report exit was `3221226505` (`0xc0000409`); publisher step/watch
exit 1, dispatch CLI exit 0. The unsigned negative was unreached; no publisher,
RFC3161 or signer/TSA-revocation acceptance follows. Only PKI ran; six other
jobs skipped. The legacy EXE was never executed. This grant is consumed too.

Retained SHA256 provenance:

| Evidence | SHA256 |
| --- | --- |
| Original job log | `7a781b4ccd6b018e24b75aaa414d10d7019fcf2d76bb8ff82253ee1fd1daea2e` |
| Native test log | `8493cc729f0b9e41cc6ef48b17f81632b23d826eff0dec4c704609d996b7c2c2` |
| `report.json` | `cfa7486458612b8df4db8cf1295d959d3c40e10194350ea20f561d1ed91ec63a` |
| Original artifact ZIP | `533a127051f8dc4e73763d7bf50ba043c4f4a602c9b68ddfa643eb5bb847f714` |

The [source-only raw-field correction](native-host-services.md) preserves the
SDK layout/owned lifetime and every cryptographic/revocation check; its new
alignment regression is cross-compiled, not executed locally. No new dispatch,
fixture download or genuine Windows execution accompanies that correction. Both
historical failures remain failures; fresh exact-source reviews and a separate
new one-run grant remain mandatory. Required three-platform CI is separate.
