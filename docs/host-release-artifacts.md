# Authenticated helper release artifacts

This is the independent authenticity foundation of [ADR 0084 §9](adr/0084-host-based-dev-tunnels.md#9-packaging-integrity-and-upgrades),
tracked in [#681](https://github.com/fes/fesTerm/issues/681), not completion of
stage 7. The existing helper package contains sessiond, tunnel bridge and setup
(plus the Windows worker), VERSION, LICENSE and THIRD_PARTY_NOTICES.txt.
The new host-management CLI is **not** claimed to be in this package.

## Wire contract and trust

For each unchanged `fesTerm-tunnel-VERSION-PLATFORM-ARCH.tar.gz` (Windows `.zip`),
publish its unchanged `.sha256`, an additive `.manifest.json`, and
`.manifest.json.sig`. Schema 1 is strict JSON with exactly these typed fields:

```json
{"schema":1,"package":"festerm-helpers","version":"1.2.3","platform":"linux","architecture":"aarch64","filename":"fesTerm-tunnel-1.2.3-linux-aarch64.tar.gz","length":12345,"sha256":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"}
```

The example's length/hash are illustrative, not download pins. Versions are
exact stable numeric semver (no prefix, prerelease, build suffix or leading
zeroes); platforms are `linux`, `macos`, `windows`; architectures are `x86_64`,
`aarch64`. Package identity is only `festerm-helpers`. Unknown/duplicate fields,
unknown schema/package/target values and noncanonical hashes are rejected.

`scripts/build_host_manifest.py` binds the actual archive length and SHA-256
after the existing package/notice checks. The protected release publish job
requires the existing updater private key/password and requires the configured
public key to equal `packaging/updater.pub`. Pinned cargo-packager 0.11.8 signs
the **exact manifest bytes**, using its existing base64-encoded Minisign
signature format (including authenticated trusted comment). The independent
verifier uses the same `minisign-verify` implementation as the desktop updater,
requires modern prehashed signatures and embeds that same public key. It
accepts no key supplied alongside a download.

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
The verifier enforces the exact six Unix/seven Windows allowlisted entries,
nonempty regular files, exact 0755/0644 modes and exact VERSION contents.
It refuses duplicates, missing/extra/nested/absolute/traversal names,
symlinks, hardlinks, special files, tar extension records, encrypted ZIPs,
ZIP64/multidisk/commented ZIPs, oversized headers and trailing gzip streams.
Central-directory count/size is bounded before ZIP parser allocation.
No archive path is passed to a filesystem extraction API.

An offline command uses the same API and reads each input with a size cap:

```text
festerm-verify-artifact 1.2.3 linux aarch64 fesTerm-tunnel-1.2.3-linux-aarch64.tar.gz
```

It reads the adjacent manifest/signature, validates, and **only reports**;
it never extracts, installs or executes anything. Release publication runs it
against all six signed packages before uploading assets.

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

Legacy filenames, archives, checksums and scripts are unchanged. Existing
scripts still use their legacy integrity/native-signer path and do not gain
independent Linux publisher authentication from these additive assets. New
typed consumers fail if a manifest/signature is absent, including old releases.
Explicit protected recovery of a pre-authentication tag retains its old payload
contract and emits a notice, not an authenticated-helper claim. Normal releases
missing this tooling fail. Recovery still pins tagged runtime source and keeps
its existing tooling-only change allowlist and CI requirements.

## Evidence and remaining work

Rust synthetic-key tests cover six valid targets, unsigned/wrong-key/tampered
metadata, wrong version/platform/architecture/filename/length/hash, strict schema
and size limits, signed unsafe TAR/ZIP entries, duplicate/missing entries,
links, invalid modes/VERSION and oversized headers. Python fixtures cover
six generated manifests, unchanged legacy bytes, overwrite refusal, target
rejection and workflow signing/verification order. Recovery fixtures exercise
the explicit old-tag exception and fail-closed ordinary releases.

No actual release key, release dispatch or live installation is used by tests.
Native protected-release signing/download/rejection evidence on all six targets,
headless CLI package inclusion, installer integration, private atomic staging,
service switching/rollback, legacy migration and independently trusted first
Linux installation remain required under HOST-06/DT-05/DT-08 and #681.
Reviewed Microsoft CLI pins/acquisition remain separate shared-setup work.
