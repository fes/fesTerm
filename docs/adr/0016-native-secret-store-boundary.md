# ADR 0016: Native Secret Store Boundary

- **Status:** Accepted
- **Date:** 2026-08-12

## Context

ADR 0003 requires secrets to remain outside ordinary configuration and future
synchronization metadata. M8 needs a cross-platform persistence boundary
without coupling GUI or TOML code to operating-system keychain APIs.

## Decision

`festerm-secret-store` owns GUI-independent secret storage. It creates
cryptographically random UUID-v4 opaque references and exposes only
content-free error categories. `SecretBytes` owns and zeroizes its allocation;
neither secrets nor references are formatted or logged by this boundary.

The native implementation uses `keyring-core` directly with the macOS Keychain,
Windows Credential Manager, and the Linux Secret Service implementation. It
uses the fixed `io.github.fes.festerm` namespace. On Linux, the target and
user-visible label are fixed, non-sensitive constants. It deliberately has no
file, keyutils, plaintext, or in-memory production fallback. The deterministic
memory implementation is injection-only for tests and future app composition.

The M8 password-credential slice persists only validated opaque SSH-password
references in TOML. `festerm-ssh` resolves the secret on its background worker
immediately before password authentication. It does not add persistence for
private keys, passphrases, agents, key files, or host trust.

## Consequences

- Missing, locked, or unavailable platform storage remains actionable instead
  of silently weakening confidentiality.
- A logged-in desktop Secret Service provider and session D-Bus are required
  on Linux; KWallet is usable only through its Secret Service integration.
- TOML remains secret-free: it can contain only an opaque reference for a
  native stored SSH password.

### Dedicated read-only diagnostic process fence

The macOS `owner-readiness` CLI permanently disables process Keychain UI and
verifies false readback before selected-store construction. Setter/readback
failure refuses before observation. The shared native store and foreground GUI
getters never install this fence. It changes only the diagnostic process, grants
no item access, and retains per-query noninteractive matching.

### Additive selected macOS HOST item mechanism

The unwired `MacHostCreationContext` binds one already reserved HOST purpose and
opaque reference to this executing app and one caller-authenticated immutable
HOST executable. The current app may be a valid retained ad-hoc development
build; this does not establish production stable-DR upgrade qualification.
The native mechanism captures both exact trusted applications and verifies
standard legacy ACL plaintext-read trust plus absence of automatic HOST ACL
administration. Standard all-app ENCRYPT is not a plaintext-read grant. It does
not promise resistance to hostile coordinated same-user modification of both
native and private policy state (the existing ADR0053 account-trust boundary).

`festerm-unix-security` owns the macOS public-API FFI and RAII handles; its small
responsibility extension contains no HOST lifecycle, sign-in or publisher
policy. `festerm-secret-store` remains unsafe-free and owns fixed namespace,
reference/size checks and finite errors. Create consumes one bound context and
refuses collisions; updates retain the exact selected logical item and never
upsert, recreate missing items, SetAccess or migrate existing ACLs. Post-create
verification failure leaves the reserved item intact for interrupted-publication
recovery. No current HOST or ordinary credential path selects this facade.
No shared GUI process UI fence, dependency package, permission grant, helper
activation or read/export API is added. Future wiring must retain the reviewed
stage/consent/reference journal binding and dedicated-process noninteraction.

## Validation impact

Initializer ordering/refusal and setter/readback fixture logic,
plus isolated CLI invocation, cover the boundary without proving native credential
access. Native prompt-free qualification remains separate; see
[ADR 0084](0084-host-based-dev-tunnels.md#dedicated-read-only-diagnostic-process-fence).

The additive mechanism has bounded policy/create/update-order fixtures and
CoreFoundation-only retained-output cleanup tests; the facade checks exact
purpose/reference binding and finite errors. Tests do not query the host user's
Keychain. Actual two-identity native create/read/update, retained-item deletion,
ACL preservation, locked/denied behavior and signed-DR updates remain opt-in
qualification under ADR0084; no live Mac fix or successful Enable is claimed.
