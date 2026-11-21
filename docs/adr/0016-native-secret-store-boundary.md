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

Background HOST readiness renewal uses explicit `get_noninteractive` and
`update_noninteractive` operations; unsupported backends refuse rather than
calling an interactive method. Updates retain the existing opaque reference.
Linux updates exactly one unlocked Secret Service item through `SetSecret`,
without unlock, creation or prompts. Windows retains the existing serialized
generic CredRead/CredWrite contract, including its external-deletion race;
this is not compare-and-swap. macOS updates require the dedicated HOST process
to disable Keychain UI before runtime/vault initialization and verify that
process-local fence. GUI and foreground setup/operator processes never set it.
Native work remains owned and drained; a failed credential persistence never
publishes the refreshed identity or authorizes replay of token refresh.

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

The additive `MacHostCreationContext` binds one already reserved HOST purpose and
opaque reference to this executing app and one caller-authenticated immutable
HOST executable. Before setup reserves state or starts OAuth, both the actual
executing app and authenticated HOST must be valid, non-ad-hoc Developer ID Application code
with the same nonempty Team identifier. Trusted applications alone do not cross
the legacy creator partition: an ad-hoc GUI's cdhash partition refuses the signed
HOST's Team partition. Fresh creation revalidates these identities. This signing
precondition does not establish stored-item access or stable-DR native evidence.
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
recovery. Ordinary account credential paths do not select this facade.

### Fresh Desktop HOST purpose wiring

On macOS only, HOST owner-anchor and HOST OAuth factories retain selected-item
updates. Fresh foreground creation additionally requires an authenticated held
HOST stage and a reservation minted in that exact operation. An owner
INITIALIZING receipt reloaded from disk never authorizes fresh creation, even
when the native reference is absent. Only the live cloud Foreground that first
published its reference can consume creation authority, before the native call;
a failed or unknown create never rearms it. The existing receipt formats and
non-Mac/headless selection stay unchanged. No legacy ACL migration occurs.

After supported recovery of an unsubmitted, unauthenticated initial Desktop
reservation, a strictly newer explicit Enable may reserve a **different** native
reference. The exact old journal bytes/reference are retained in the private
`host-cloud-initial-authorization-history.json` before atomically publishing the
new current record. This version-1 metadata history is limited to eight entries,
128 KiB total and 16 KiB per original record. It never reads, deletes, updates or
asserts absence of the old native item. Existing authenticated/bound/resource
paths and Headless behavior remain unchanged.

Only a live caller that confirms both durable publications receives fresh
creation authority; reopening the already-current reference always receives
none. Archive-only persistence, collision, corruption, full history or uncertain
publication fails closed without eviction, rollback or automatic repair. The
history preserves unknown references for owner review, not an executable retry
or recovery plan. Historical tickets are typed, ordered metadata; only the new
Store-validated ticket supplies current approval. `Record` v1 is unchanged and
absence of this optional private file remains compatible with older state.

Actual macOS Desktop start runs in the authenticated registered helper, whose
entry disables Keychain UI before constructing stores. The GUI never changes
its process-wide UI policy. Serving entry points also install this dedicated
process fence before native reads. HOST retained updates never upsert. Static
capability refusal occurs before a new setup ticket or device sign-in. This
source wiring supplies no signed/native acceptance or new permissions.

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

Secret-store unit regressions cover unsupported backends without fallback,
existing-reference updates, size/missing refusal, the exact unlocked-item
selector and failed macOS fence setup/readback. Synthetic HOST regressions
cover persistence before readiness, changed scope, cancellation, retained-work
draining and fatal uncertain renewal without replay. Platform typechecks do
not establish native vault behavior.

DT-05 additionally requires actual native noninteractive renewal in the same
protected run, with no prompt and unchanged session/policy identity. macOS
qualification must use the dedicated serving entrypoint; GUI and foreground
setup remain unfenced. These native gates remain unrun for this change.

Composed Desktop HOST renewal retains the selected purpose facade: the owned
blocking observer preparation selects `HostPurposeStore` on macOS Desktop.
Noninteractive persistence first verifies the dedicated process fence, then
opens and updates the exact retained item. It never routes these HOST items
through generic get/set update semantics; missing/denied items refuse without
upsert, ACL mutation, prompt, or refresh replay. The deterministic pre-open
admission regression does not establish actual native Keychain renewal.
