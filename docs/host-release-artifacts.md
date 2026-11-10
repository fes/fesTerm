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
