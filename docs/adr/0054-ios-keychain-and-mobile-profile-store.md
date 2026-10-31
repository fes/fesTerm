# ADR 0054: iOS Keychain Secret Backend and Mobile Profile Store

- **Status:** Accepted
- **Date:** 2026-11-01
- **Supersedes:** None

## Context

The iOS app (ADR 0042) holds one transient SSH connection with memory-only
pins and credentials, and `festerm-secret-store::native_store()` returned
`Unsupported` on iOS (ADR 0016/0024). The mobile port plan (Phase 4) needs
persistent profiles, host-key trust, SSH keys and Dev Tunnel accounts without
ever writing secrets to ordinary files.

`festerm-config` owns desktop profiles but depends on `festerm-pty`, which the
iOS dependency gate forbids, so mobile cannot reuse it directly.

## Decision

1. **Keychain backend.** On `target_os = "ios"`, `NativeSecretStore` is backed
   by the established `apple-native-keyring-store` crate, `protected` feature
   only (it wraps `security-framework`, so our crates keep
   `#![forbid(unsafe_code)]`). Items are `kSecClassGenericPassword`,
   `kSecAttrAccessibleWhenUnlockedThisDeviceOnly`, with iCloud sync disabled,
   service `SERVICE_NAMESPACE` and a random UUID account. Secrets are zeroized,
   bounded to 64 KiB (`SecretStoreError::TooLarge`), and Security framework
   statuses map to `LockedOrUnavailable` (-128, -25291, -25293, -25308) or
   `Unsupported` (-34018, missing entitlement). The legacy macOS `keychain`
   feature stays forbidden on iOS. The backend accepts any keyring store, so
   tests inject the keyring mock on the host.
2. **Dependency policy.** `scripts/build-ios-spike.py --check-dependencies`
   now allows `apple-native-keyring-store` only with `protected` and without
   `keychain`, and also gates the new `festerm-mobile-profiles` crate.
3. **New crate `festerm-mobile-profiles`**, UI-agnostic:
   - `ProfileStore`: versioned secret-free `profiles.json` (SSH, Dev Tunnel,
     SFTP), Dev Tunnel accounts, atomic 0600 writes, order, recents, unique
     names, bounds, per-entry corruption tolerance with quarantine copy,
     newer-version refusal, migration hook. Secrets are opaque `SecretRef`s,
     deleted with the profile; failed deletions are retried.
   - `KnownHosts`: separate `known_hosts.json`; a changed key is the typed
     `HostKeyError::Changed` and only `forget` can replace it.
   - `keys`: import OpenSSH/PKCS#8/PuTTY text (Ed25519 and ECDSA; RSA is not
     enabled), optional passphrase that is never retained, normalised to
     unencrypted OpenSSH inside the Keychain (protected by the device
     Keychain) so `festerm-ssh` stored-key auth works; generate Ed25519.
   - `PublicProfileImport`: validates the one-line installer's
     `festerm-profile.json`; Dev Tunnel profiles default `take_over_if_attached`
     to on.
   - Dev Tunnel shapes and validation are mirrored from `festerm-config` and
     the bridge rather than shared, because of the forbidden `festerm-pty`
     dependency; a later refactor may extract a shared pure crate.

## Alternatives considered

- Direct Security-framework FFI: needs `unsafe`; the maintained binding is
  smaller risk.
- Extending `festerm-config`: drags PTY code into iOS.
- Storing secrets in profile JSON or iCloud-synced items: violates ADR 0016.

## Consequences

Secrets stay on-device and locked-only. After a device restore the Keychain
items are absent (`CredentialStatus::Missing`) and the UI must ask again.
Real-Keychain behaviour needs an entitled app process; host tests cover logic
through the injected store, and the real path is compile-checked for device
and Simulator targets.

## Validation impact

- **Invariants introduced or changed:** secrets never reach profile files;
  changed host key is never overwritten; iOS keychain feature set is gated.
- **GUI/action edges affected:** None; no UI changes in this ADR.
- **Automated tests required:** `mobile_profiles` integration tests,
  `festerm-secret-store` unit tests, `test_ios_ssh_dependencies.py`.
- **Native/manual evidence required:** Simulator/device Keychain round trip
  remains pending (MOB-06 class acceptance).
- **Coverage superseded:** The iOS-unsupported secret-store test.
