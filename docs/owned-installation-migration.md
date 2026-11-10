# Owned-installation migration

ADR [0084 §11](adr/0084-host-based-dev-tunnels.md#11-migration-and-rollout),
`HOST-06`, remains **partial**. The shared `owned_migration` library and headless
commands implement protected inventory, reservation, authenticated in-place
binding of explicitly selected capability-enabled current daemons, retained-root
routing, explicit switch, same-operation resume and exact release/rollback.
Installed cross-platform, old-release, native-vault, cloud and GUI acceptance
remain separate gates. A local switch publishes HOST records; it does not
provision a provider endpoint or enable the disabled installation.

## Explicit selection

Create a regular owner-private JSON file in a trusted private directory:

```json
[
  {
    "source": {
      "kind": "desktop-profile",
      "installation": "11111111-1111-4111-8111-111111111111"
    },
    "logical_id": "22222222-2222-4222-8222-222222222222",
    "label": "work"
  }
]
```

Replace the example IDs with the original protected installation ID and an
explicitly chosen new logical UUIDv4. `standalone-script` selects only the
canonical script-owned installation. Custom old script state locations are not
discovered or adopted. One to eight distinct installations/logical IDs may be
selected; duplicate exact native generations refuse rather than deduplicating
by session/host names. Labels are metadata only. Selection files, recipes,
destination exports and account strings never grant authority.

```text
festerm-host migration-inventory --selection /private/selection.json --account-id 42
festerm-host configure --state-dir /private/new-host --name "My computer"
festerm-host migration-preview --state-dir /private/new-host --selection /private/selection.json --account-id 42
festerm-host migration-reserve --state-dir /private/new-host --selection /private/selection.json --account-id 42 --preview-id EXACT_PREVIEW_SHA256 --consent reserve
festerm-host migration-status --state-dir /private/new-host
festerm-host migration-resume --state-dir /private/new-host
festerm-host migration-defer --state-dir /private/new-host
festerm-host migration-rollback --state-dir /private/new-host --consent rollback
```

No command enumerates an account, signs in, obtains credentials, invokes a
service manager or contacts a provider. The supplied account must match every
selected protected provisioning/ownership receipt; this is **retained local
account evidence**, not a fresh provider authentication. Source private keys are
loaded only to verify their retained stable host ID and installation fingerprint.
They are never exported, copied, replaced or imported as owner keys.

Inventory validates the canonical protected installation and key, original
consent/cloud ownership/available incarnation and exact selected target.
Desktop consent must match the installation account, cloud ID, ownership label,
spawn fingerprint, origin and generation. Script ownership must match its
original installation/key/cloud ID/label. Missing, foreign, corrupt or untrusted
evidence refuses. Cleanup/revocation/incomplete rows cannot reserve.

For eligible rows, native evidence must be the exact private registry generation
with its lifetime lease; name/PID-only compatibility is not accepted. Desktop
rows additionally verify retained spawn evidence. A valid live generation is
reported as `authenticated-binding-available` only after a generation-local
authenticated challenge, not from metadata or a version number. Older helpers
without the startup capability remain `authenticated-binding-unavailable`;
bound generations are `already-bound`, not candidates for a second installation.
Current daemons already managed by another HOST watch also refuse import; a
valid capability alone is not advertised as import eligibility.
Preview negotiation is display evidence only: reservation keeps its original
immutable receipt hash, and bind/switch reacquire protected authority. Missing
or untrusted evidence refuses rather than becoming absence-based creation.
No migration command acquires a frontend.

## Protected transaction and rollback

Preview hashes exact source receipt bytes, explicit mapping, account and the
fresh disabled destination snapshot/settings/epoch. Changed intent requires
another preview. Reservation commits an immutable intent before creating the
destination's normal `host-identity`, then journals progress and claims each
source under existing installer/consent locks. It preserves original receipt
bytes and issued IDs, cloud incarnation, source key fingerprint and narrow
legacy permissions as evidence only. No owner principal, key enrollment,
session grant, sharing, creation, temporary lifetime or remote access is minted.

`reserving` records partial progress. `awaiting-native-binding` means **only**
all selected local claims/identity are reserved. It is not migrated, switched,
Ready or a successfully bound session. `deferred` leaves that reservation and
legacy use intact. Resume reloads the same operation/host identity and checks the
original source bytes, exact generation and destination snapshot; it never
refreshes stale epochs, creates another host or launches a shell. Missing claim
evidence after its checkpoint, released/foreign claims and revocation refuse.
The source claim prevents another destination from simultaneously reserving it.

Enable/setup refuses until every explicitly selected generation is authenticated
and switched. It then uses the reserved identity through the normal separately
consented setup path, never migration consent as provider or service authority.
`migration-status` is protected history and may be read after source revocation
or loss; it is not fresh source authority. An interruption before the first
journal can be resumed from its immutable intent or explicitly rolled back.

Rollback first journals `rollback-pending`, then releases only matching local
claims and checkpoints each result before `rolled-back`. Partial failures retain
the operation/identity/evidence for retry. It never restores the old consent,
reenables the destination, changes a legacy target, signals a daemon, repairs
permissions, deletes an identity/vault entry or makes a cloud deletion request.
An already enabled destination must be explicitly disabled first. Existing
pending provider cleanup remains pending under its own lifecycle APIs; rollback
is not proof of remote absence. The approved explicit owner provider-side
cleanup policy is not implemented or widened here.

The target key/receipts are retained after rollback; no source exposure is
resurrected even if the source was revoked while migration was deferred. A
rolled-back operation cannot resume or regenerate a missing identity. New
independently reviewed reservations can claim a previously released source.

There is **no legacy stop command** in this flow. Declining to reserve, deferring
or rolling back leaves normal exact-generation legacy attachment available.
Setup/sharing/reservation consent never authorizes stop or restart. A future
live stop requires its own separate, default-No confirmation tied to that exact
generation (ADR 0085); no `--yes`, takeover, stop or restart flag is accepted.
Legacy client protocol/strict-attach behavior is unchanged.

## Concrete missing binding boundary

At base `9b1d2a688e35cad8dead2afa4b228c152c2cadb9`,
`festerm-sessiond::host_control::SessionControl` has no `bind_existing` API.
`ensure` is a launch/reservation API, not adoption. Its records reconstruct
native endpoints relative to `self.root`; the controller selects
`state/daemon`. Ordinary daemons initialize `DaemonWatch` only when launched
with the HOST-control startup contract. Publishing a lease beside an ordinary
live daemon cannot authenticate a binding or install that watch.

That frozen reservation-only boundary is now supplied by the following narrow
current-source implementation; it is not retrofitted into old daemons.

## Authenticated binding and retained-root boundary

```text
SessionControl::bind_existing(binding, fresh_protected_admission)
SessionControl::switch_binding(binding, fresh_protected_admission)
SessionControl::release_existing(binding, fresh_exact_reduction_admission)
```

The migration composition supplies actual anchored owner enrollment, source
consent/ownership/key/claim guards and exact current operation/installation/
account/principal/owner epoch/issuer namespace checks. The local callback is not
a wire credential. Guards are dropped before native IPC and reacquired before
each protected publication. Owner initialization is separate, zero-grant and
account/operation-bound. Another explicit key-fingerprint confirmation approves
only inventory/attach/detach: never creation/end/temporary or imported permissions.

```text
festerm-host migration-owner-init --state-dir /private/new-host --operation ORIGINAL_OPERATION --account-id 42 --owner-storage native-vault --consent initialize-owner
festerm-host migration-owner-approve --state-dir /private/new-host --operation ORIGINAL_OPERATION --public-key OWNER_PUBLIC_KEY_HEX --fingerprint CONFIRMED_OWNER_FINGERPRINT --owner-storage native-vault --consent approve
festerm-host migration-bind --state-dir /private/new-host --owner-storage native-vault --consent bind
festerm-host migration-binding-status --state-dir /private/new-host
festerm-host migration-switch --state-dir /private/new-host --owner-storage native-vault --consent switch
festerm-host migration-resume --state-dir /private/new-host --owner-storage native-vault
festerm-host migration-rollback --state-dir /private/new-host --consent rollback
```

`headless-protected-file` is an explicit alternate owner-storage selection, never
a native-vault failure fallback. Owner-routing support is required for these
commands. The remote verifier retains the lifecycle Disabled master gate even
after local owner approval and switch.

Capability-enabled ordinary daemons create a private generation/PID-bound
HMAC-SHA256 capability while holding their original lifetime lease, before Ready
publication. `FSB1` exchanges fresh nonces and domain-separated request/reply
MACs over that actual generation's native endpoint. Native registry, lifetime
and spawn proof are independently checked. The daemon authenticates and installs
a persistent watch on its existing sole writer loop, outside frontend admission.
The transport reuses Unix sockets or the existing protected Windows pipe API;
Windows execution and installed-platform qualification remain open.

The on-disk capability is a live-generation secret, including for ordinary
daemons that are never migrated. Its exclusive creation returns a separate
resource owner: normal exit and failed-start unwind explicitly remove only that
retained file identity, before releasing the lifetime lease. Reading/deserializing
a capability never transfers cleanup ownership. Partial publication cleans only
its own file; cleanup failures are reported and preserve generation artifacts and
an exact retry record. Dead-generation recovery requires the exclusive exact
lifetime lease and matching protected identity/spawn fingerprint; corrupt,
foreign, redirected or untrusted keys refuse cleanup without permission repair.
No directory sweep is performed. Release while the daemon is alive preserves
its capability and PTY; released receipt tombstones are deliberately retained.

The prepared native lease is private/revoked at initial grant epoch one. Switch
journals its original intent and activates only that preparation; later revoked
revisions cannot be reactivated. A lost acknowledgement resumes the same
operation, immutable descriptor and namespace, not another host or shell.
Prepared bindings can be deferred; a requested switch instead requires exact
rollback. A namespace anchor fences lost/replaced host journals. Imported records
route existing inventory/attachment to the exact retained root; `ensure`,
recreate, end and artifact retirement are forbidden for them. Native admission
and signed shared-client authorization remain the existing path.

Every imported remote operation/frame freshly fences original source receipt,
ownership/consent, private key and claim bytes. Observed changes seal the native
remote grant; source restoration cannot authorize switch again. Missing/corrupt
state denies remote access but never stops the imported persistent shell.
Rollback journals its intent, revokes the exact lease, authenticates release,
retains a one-way generation-local tombstone, and only then releases original
claims. Resume finishes an already journaled rollback without new stop consent.
An uncertain result stays pending for the same operation; there is no automatic
retry, native restart, provider cleanup or authority expansion.

Ordinary shell exit does not strand a prepared or active binding. Release keeps
the protected operation/owner/namespace/issuer checks and revokes an existing
HOST lease first. Only the existing exact-identity/fingerprint native-generation
observation returning authoritative absence can replace an unavailable native
release reply, or permit an already-missing HOST lease. Unknown, Busy, foreign,
corrupt and failed observations remain pending, never inferred absence from an
I/O error. An absent generation needs no new native tombstone: the protected
released binding checkpoint and original claim release remain one-way. Rollback
can then finish and unblock the separate enable gate without recreating a shell.

Focused deterministic fixtures cover reservation/receipt preservation, account/
key/origin/name and generation conflicts, stale epochs, missing/corrupt/released
claims, interruption, rollback after revocation, untrusted files and default-No.
The opt-in current-source native fixture
`owned_migration_native_reservation_deferral_rollback_preserve_pid_and_existing_frontend`
checks a real independent daemon and original frontend through reservation,
deferral, refusal and rollback. It is deliberately **not** real supported-daemon
binding, old-release compatibility, signed-service, native-vault, GUI or complete
cross-platform migration acceptance.

The separate opt-in
`owned_migration_native_authenticated_binding_switch_resume_release_preserve_frontend`
executes real current-daemon bind/defer/switch/resume/revocation/release/rollback
and original PTY I/O, checking unchanged PID/generation/frontend, foreign/stale
requests, held native lease and no duplicate identity. This is controlled
current-source local evidence, not an installed old-release or cloud exposure
acceptance claim.

`owned_migration_native_authenticated_binding_preserves_local_frontend` reuses
that same complete authenticated sequence with an original **Local** frontend.
It checks Local admission explicitly and retains original PID/generation,
current namespace authority, PTY I/O and unchanged live capability bytes through
bind/switch/resume/source revocation/release/rollback. This is actual binding,
unlike the earlier Local reservation-only case; it is still not Desktop GUI
sharing or installed-platform acceptance. The separate Prepared/Active ordinary
shell-exit regression covers authoritative-absence rollback and pending
Busy/corrupt observations.

Ticketed HOST admission uses the new imported grant; the ordinary generation's
pre-existing unticketed exact-generation local/legacy path is unchanged, including
its original independently enforced bridge consent. Sealing HOST access never
retires a pre-existing unticketed legacy frontend or restores a revoked legacy grant.
The release tombstone continues to retire only the ticketed HOST frontend even
after its imported watch is removed. Lost native publication acknowledgements
and local binding/release checkpoints resume only the exact existing receipt;
missing or backward native receipt state is never reconstructed.

Run the authenticated fixture after building both binaries from this exact source:

```sh
cargo build --locked -p festerm-sessiond -p festerm-host --bins --features festerm-host/owner-routing
FESTERM_ADR84_MIGRATION_HELPER="$CARGO_TARGET_DIR/debug/festerm-sessiond" \
FESTERM_ADR84_MIGRATION_HOST="$CARGO_TARGET_DIR/debug/festerm-host" \
cargo test --locked -p festerm-host-setup --features owner-enrollment --lib \
  owned_migration::tests::owned_migration_native_authenticated_binding_switch_resume_release_preserve_frontend \
  -- --ignored --exact --nocapture --test-threads=1
```

Set an explicit private `CARGO_TARGET_DIR`, `CARGO_BUILD_JOBS=1`,
`CARGO_PROFILE_DEV_DEBUG=0`, `CARGO_PROFILE_TEST_DEBUG=0` and
`CARGO_INCREMENTAL=0`, using the supported pinned compiler and sufficient host
memory. It creates only project-local disposable protected fixtures, starts a
test-owned shell/helper, uses the original frontend and performs exact-generation
fixture cleanup; it never consults an installed runtime or service.
`scripts/run-optional-validation.sh` aggregates it only with the additional
`FESTERM_RUN_OWNED_MIGRATION_NATIVE=1` opt-in and explicit binary paths. Windows
aggregation records this Unix-only fixture as skipped, not native acceptance.
