# Owned-installation migration reservation

ADR [0084 §11](adr/0084-host-based-dev-tunnels.md#11-migration-and-rollout),
`HOST-06`, remains **partial**. The shared `owned_migration` library and headless
commands implement explicit protected inventory, preview, identity reservation,
cross-installation claims, recovery, deferral and local rollback. They do **not**
bind an ordinary legacy daemon to HOST, switch exposure or qualify migration
acceptance. This is executable reservation/recovery source, not a migration plan
or an adapter claiming success without performing binding.

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
reported as `authenticated-binding-unavailable`, **not** compatible with HOST
merely because current-source metadata can be read. Missing generations remain
unavailable, never absence-based creation. No command opens a frontend.

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

Enable/setup refuses while this migration reservation is outstanding: the
missing binding step cannot be bypassed by provisioning the reserved identity.
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

The runtime/session owner must supply a narrow authenticated API equivalent to:

```text
bind_existing(
  current_owner_policy_guard,
  logical_id,
  exact_native_identity_and_retained_runtime_root,
  protected_source_installation_consent_account_key_receipt,
  original_operation_and_current_issuer_epoch
) -> retained_exact_binding_receipt
```

It must negotiate actual daemon support without acquiring/changing the
frontend, restarting, creating a shell or fabricating launch evidence; preserve
owner/principal/generation/account/source/revocation fences; route inventory and
strict attachment to the retained root; and offer exact release/rollback without
reviving revoked grants. The migration layer must then consume that receipt,
fresh protected destination owner enrollment and current issuer before a real
switch. Neither a public injectable callback nor a synthetic fixture is that
contract.

Focused deterministic fixtures cover reservation/receipt preservation, account/
key/origin/name and generation conflicts, stale epochs, missing/corrupt/released
claims, interruption, rollback after revocation, untrusted files and default-No.
The opt-in current-source native fixture
`owned_migration_native_reservation_deferral_rollback_preserve_pid_and_existing_frontend`
checks a real independent daemon and original frontend through reservation,
deferral, refusal and rollback. It is deliberately **not** real supported-daemon
binding, old-release compatibility, signed-service, native-vault, GUI or complete
cross-platform migration acceptance.

Run only that fixture after building both binaries from this exact source:

```sh
cargo build --locked -p festerm-sessiond -p festerm-host --bins
FESTERM_ADR84_MIGRATION_HELPER="$CARGO_TARGET_DIR/debug/festerm-sessiond" \
FESTERM_ADR84_MIGRATION_HOST="$CARGO_TARGET_DIR/debug/festerm-host" \
cargo test --locked -p festerm-host-setup --features lifecycle --lib \
  owned_migration::tests::owned_migration_native_reservation_deferral_rollback_preserve_pid_and_existing_frontend \
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
