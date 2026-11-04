# ADR 0067: Rooted Dev Tunnel Host Filesystem

- **Status:** Proposed
- **Date:** 2026-11-04
- **Supersedes:** None; extends ADR 0053's explicitly granted filesystem subset

## Context

The owner requested host-side directory enumeration and streaming transfers,
with desktop/iOS clients implemented separately. The owner selected an explicit
configured root and create-new uploads with explicit conflict-checked atomic
replacement. Existing optional file snapshots are neither a directory sandbox
nor a general transfer protocol.

## Decision

Add a separately negotiated filesystem stream, never terminal input or an SFTP
fallback. Keep protocol-v1 terminal discovery and attachment byte-compatible.
Publish portable wire types and an integration contract in
`docs/dev-tunnel-files-protocol.md`. Directory recursion belongs to clients:
hosts expose bounded enumeration, metadata, chunked read/write, mkdir and close.
Delete, arbitrary rename, links, permission editing and reconnect replay are
not part of this grant.

All access is default off. `--file-root` grants rooted reads and enumeration;
`--allow-file-write` additionally grants uploads/mkdir.
`--disable-files` revokes the grant. Managed reruns can change these grants
without changing tunnel identity, key or daemon generation. They stop only
owned workers before changing policy; switching helpers still needs upgrade.
An omitted policy preserves an existing installation's consent.

Use capability-relative `cap-std` directory handles for enumeration and path
resolution, with no-follow component traversal. Reuse repository-owned native
private creation, identity and no-replace publication primitives. Host-only
dependencies stay behind `relay`; portable wire types contain no native I/O.
Reject ambiguous paths, unsupported objects and roots/parents writable by
untrusted OS principals. Remote `/` means the configured root, not the host
volume root. Root absolute paths never enter public destination metadata.

Use bounded chunks, pages, handles, admitted streams and blocking jobs. Uploads
are sequential into private same-parent staging files, never sparse arbitrary
writes. Disconnect/close cancels incomplete uploads. Creation never overwrites.
Replacement names an existing open-read handle from this connection, verifies
identity/change stamps, captures the original without replacement, then
publishes atomically without replacing a concurrent new destination. Conflicts
restore the captured original without overwrite; failed restoration preserves
it under an explicitly reported recovery name instead of deleting evidence.

## Alternatives considered

- Expose the whole OS account filesystem: rejected by the owner.
- Add fields to strict legacy discovery JSON: breaks deployed clients.
- Shell commands or an installed SFTP server: changes authority/dependencies.
- Reopen directory pathnames for enumeration: loses the retained root boundary.
- Check existence then overwrite: can destroy a concurrently created file.

## Consequences

The wire contract is separate from UI policy. Client integration remains a
separate PR. Native filesystem behavior, signed service policy changes and
real-service desktop/iOS transfers need qualification; mocks do not qualify them.
Interrupted uploads do not resume automatically. No on-disk transfer journal
claims power-loss recovery; a killed host can leave private staging/recovery
files for explicit operator cleanup. Authorized file sizes consume OS storage
quota, not unbounded RAM or queues. New uploaded files are private and do not
preserve source ACLs, executable bits or extended metadata.

## Validation impact

- **Invariants introduced or changed:** ADR 0053's host filesystem grant now
  includes explicitly rooted enumeration and writes. Terminal ownership,
  host verification, generation pins, secret boundaries and bounded queues
  remain unchanged.
- **GUI/action edges affected:** `TUNNEL-01`, `TUNNEL-03`, `TUNNEL-05`;
  client UI implementation remains separate.
- **Automated tests required:** rooted traversal/listing/transfer/overwrite and
  cleanup tests, stream negotiation/frame bounds, legacy discovery, portable
  client-only dependency checks and managed policy migration/revocation tests.
- **Native/manual evidence required:** DT-05 signed host enable/disable and
  cross-platform filesystem conflict qualification; DT-07 physical iOS and
  desktop integration once their separate clients exist.
- **Coverage superseded:** None.
