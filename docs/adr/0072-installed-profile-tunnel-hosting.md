# ADR 0072: Installed-Profile Dev Tunnel Hosting

- **Status:** Proposed
- **Date:** 2026-11-05
- **Supersedes:** None; extends ADRs 0025, 0053 and 0066

## Context

[Issue #618](https://github.com/fes/fesTerm/issues/618) requests an installed
desktop profile checkbox instead of the standalone installer for publishing a
local native persistent session. The owner selected activation on Save,
including starting the configured daemon without opening a terminal tab.
Existing local profile Save is metadata-only, the native start API attaches a
frontend, and standalone host setup creates a separate default shell. None is
an appropriate implicit implementation of this request.

The issue contains the reviewed product/state-machine contract. This ADR owns
the architectural changes rather than duplicating that design.

## Decision

The application composition root owns explicit profile-host consent, save
outcomes, foreground browser authentication and a bounded host coordinator.
Widgets return typed intent; they do not manage services, keys or cloud state.
Only a saved Local profile selecting `festerm-sessiond` can request hosting.
Other Save paths remain metadata-only.

Profile configuration contains non-secret desired state and host-local consent
identity. Protected per-installation state authorizes actual exposure through
Prepared, Requested, Provisioning, Exposed and revocation/cleanup states.
Configuration alone cannot create a host. Revocation and operation epochs fence
late completions, recovery and update. Profile rename preserves ownership;
duplication/import cannot transfer a hosting grant.

Add a non-attaching native-provider `ensure_detached` operation. It verifies
effective spawn parameters, reuses only a matching live daemon, or journals one
exact generation before starting an absent daemon. It never acquires a frontend
lease or displaces a local attachment. Persist versioned spawn-fingerprint
evidence at daemon start without changing terminal framing. Older records
remain readable for existing attach behavior but cannot authorize unverified
hosting. Uncertain launch or a subsequently dead generation requires separately
confirmed fresh-shell consent; ordinary Retry never creates a replacement.

The host helper has a separate bind-existing entry point that revalidates the
exact native identity and spawn evidence. It never creates a shell. Transport,
takeover and terminal ownership remain with the existing bridge/daemon
boundaries; no terminal semantics enter GUI controls.

Desktop-origin host records are explicitly versioned and stored in protected
UUID directories. Services execute verified per-user versioned helper copies,
never installed application paths. The app stages and reconciles only its own
enabled hosts after update, preserving live daemon/tunnel/account/key/grants.
Standalone setup and its explicit upgrade path remain independent. Old helper
copies are conservatively retained; reference-aware cleanup is deferred to
[#619](https://github.com/fes/fesTerm/issues/619).

Standalone setup, upgrade and explicit legacy repair refuse desktop-origin
records before repair or update mutation. A desktop fresh-shell replacement
must pass its protected profile-consent coordinator; a script command cannot
replace a desktop target or bypass revoked consent.

Only the foreground app starts the pinned official CLI's browser sign-in;
workers and startup reconciliation return SignInRequired. CLI credentials stay
with their existing owner, not configuration, command arguments or logs.
Downloads are bounded and pinned; no fetched script execution is introduced.

One shared removal operation journals revocation, confirms the exact owned
relay is stopped, and deletes only the recorded cloud identity under the
recorded account. Offline/account/identity failures retain cleanup evidence.
CLI inventory/delete-all, checkbox disable, profile deletion and deletion of a
locally owned Running Tunnels entry converge on it. Daemon shells, unrelated
services, global credentials and foreign tunnels are never removal targets.
Unmanaged cloud disappearance is distinct from a local revocation tombstone.

The desktop implementation supplies profile and owned-browser removal plus a
bounded local cleanup inventory. The standalone `--manage` and installer
entry points cover only the separate schema-1 script namespace, sharing exact
native-service teardown and cloud lookup verification without weakening the
desktop consent contract. Desktop removal remains canonical in the GUI.
Script deletion journals revocation before stopping and retains cleanup receipts;
new creates reserve a private random cloud ownership label before submission.
Original script records require an additional typed exact-qualified-ID deletion
confirmation, not automatic/bulk ownership adoption. A separate deletion-only
receipt pins the returned cloud incarnation when available. The pinned CLI can
omit creation time: explicit null/unavailable evidence requires renewed full-ID
confirmation for every attempt, never inferred incarnation continuity or a
retroactive pin. A previously pinned incarnation cannot disappear/change.
Completed,
locally stopped and cloud-gone state may be explicitly archived with a private
no-replace rename before a separately requested fresh setup; uncertain evidence
is never archived. Destructive script management and canonical script setup share
a protected namespace installer lock. Archive verifies completed receipts under
the state-local lock too, releasing that child handle only on Windows before
rename while retaining the namespace lock (Windows refuses open-child directory
renames). Opt-in management-tool bootstrap stages verified immutable
helpers without servicing/provisioning an existing host. An active unreferenced grant may
belong to another configuration document: inventory warns and requires explicit confirmed
revocation rather than adopting it or automatically stopping exposure.

Linux packaged-source paths must not be group/world-writable; custom
`/usr/local/bin` installations with shared-write permissions are refused.
macOS permits only the narrowly verified root/admin source-directory case;
private helper destinations remain strict on every platform.

## Alternatives considered

- Run the standalone installer from a widget: creates an unrelated shell and
  bypasses profile, save and authentication ownership.
- Start an attached native session and drop it: can displace the local writer
  and introduces a hidden frontend admission.
- Reconstruct grants from enabled config on startup: resurrects externally
  revoked hosts and treats portable metadata as authority.
- Execute services from the app installation directory: interferes with Windows
  loaded images and package replacement.
- Delete cloud resources by name prefix or account-wide list: exceeds ownership.

## Consequences

The opt-in Local Save path intentionally gains visible background effects.
Ordinary profiles, old native attach compatibility and standalone installer
behavior remain unchanged. An unverifiable old live daemon is preserved and
hosting is refused with guidance, not silently adopted or terminated.

There are new private consent/operation records and additive native spawn
evidence. New host schema versions refuse older helpers rather than discarding
ownership fields. A staged update can fail without changing the live shell.
Native sign-in, signed package servicing, user-service behavior and physical
client reconnection still require separate qualification.

## Validation impact

- **Invariants introduced or changed:** Explicit opted-in Local Save can start
  one detached daemon and host; protected consent, exact generation binding,
  non-attaching ensure, independent service lifetime and revocation precedence
  preserve single-writer, secret and bounded-work ownership.
- **GUI/action edges affected:** PROF-01, PROF-02, PROF-03, PROF-06 and
  TUNNEL-01, TUNNEL-03, TUNNEL-05, TUNNEL-11, TUNNEL-15 and TUNNEL-16.
- **Automated tests required:** Detached ensure/fingerprint/launch-reservation
  fixtures; configuration, editor and composition save/disable/delete tests;
  tagged management-request generation round trips through `u64::MAX`,
  malformed/oversized generation rejection at the real helper boundary and
  unchanged full-width persisted targets;
  structured helper ownership, update, no-resurrection and teardown fixtures;
  installer and package regressions specified in #618. Missing registration
  directories still require exact native-service absence; unsafe ancestors
  and unknown/foreign loaded state refuse cleanup. Executable publication
  sets private execution permissions before rename, and cached CLI recovery
  repairs only an already pin-verified handle. The detached native fixture
  selects a directory accepted by production private-storage policy rather
  than assuming Windows runner checkout ancestors are trusted.
- **Native/manual evidence required:** DT-08 signed desktop lifecycle,
  GitHub browser sign-in, live helper updates and physical-client reconnect;
  deterministic tests do not promote acceptance.
- **Coverage superseded:** None.
