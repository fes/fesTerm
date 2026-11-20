# Dev Tunnel host filesystem protocol

This is the host/client integration contract for ADR 0067. Host implementation
lives in `festerm-tunnel-bridge`. Its portable `client` feature also exposes the
sequential `file_client::Client` engine. Desktop composition uses ordinary
`open_files_reviewed` (browsing/review) plus pinned `open_files` (transfers)
and the existing SFTP file-manager presentation and queue for browse,
upload/download, mkdir and bounded read-only previews. Reviewed deletion uses the separate Open3 extension below; rooted v1 write
consent never authorizes deletion. Rename, links and chmod remain unavailable. Mobile
integration and native/provider Desktop qualification remain pending. Portable
Rust DTOs/constants are in `file_protocol.rs`. Do not send these operations as
terminal input, invoke a remote shell, or fall back to SFTP/local files.

## Legacy rooted authorization and existing installations

Everything is default off. On either a new installation or an existing host:

- `--file-root ABSOLUTE_DIRECTORY` enables enumeration and downloads.
- Add `--allow-file-write` to enable uploads and mkdir.
- Rerun with `--file-root` without the write switch to change to read-only.
- `--disable-files` revokes all rooted filesystem access.
- Omit these flags to preserve the installed policy. `--allow-file-write`
  requires `--file-root`; disable cannot be combined with either.
- A different helper release still needs `--upgrade`. Policy changes stop only
  the exact owned hosting workers, then restart with the new grant; tunnel,
  host key, account and daemon generation remain unchanged. In-flight filesystem
  streams close rather than retaining their old grant. Revocation does not
  require a live daemon or available cloud account.

PowerShell installer equivalents: `-FileRoot`, `-AllowFileWrite`, `-DisableFiles`,
and `-Upgrade`. The POSIX installer forwards the long flags above.
Do not invoke these options against a live installation without its owner's
approval. These flags require a release containing this implementation.

Roots must exist and be absolute local directories. Ancestors and traversed
directories must satisfy the repository's trusted-owner/non-foreign-writable
policy; permissions are never repaired. The configured absolute root is private
installation metadata, not public destination JSON.

The legacy `--allow-file-read` remains separate and cannot be combined with a
rooted grant. Rooted mode keeps legacy `file_read: false`: old clients must not
gain unrestricted snapshot reads as a side effect of enabling a root.

## Transport and negotiation

Use the existing host-key-verified relay endpoint and configured virtual port.
Open a **separate forwarded stream**, independent of terminal attachment. File
streams never acquire, displace or reclaim the daemon's terminal attachment.
The host still pins the connection to its advertised exact target identity.

Frames retain the bridge framing: one kind byte, four-byte **big-endian** payload
length, then exactly that many bytes.

| Kind | Direction | Payload |
| --- | --- | --- |
| `13 DISCOVER` | client → host | Two-byte big-endian `2` |
| `14 DESCRIPTION` | host → client | Filesystem discovery JSON below |
| `30 OPEN` | client → host | Filesystem open JSON |
| `31 REQUEST` | client → host | Request JSON, at most 65536 bytes |
| `32 RESPONSE` | host → client | Response JSON, at most 65536 bytes |
| `33 DATA` | either | Four-byte big-endian request ID, followed by at most 65536 data bytes |

Discovery remains a one-request stream which the host then closes. Version 2
discovery replies:

```json
{"version":2,"target":{"name":"example","pid":123,"generation":"1793737311317"},"filesystem":{"version":1,"write":true,"chunk_limit":65536,"page_limit":64,"handle_limit":8}}
```

`filesystem: null` means disabled. An older host may refuse discovery version 2:
report unsupported; ordinary terminal use can continue via unchanged version 1
discovery. Never infer filesystem permission from terminal authentication or the
legacy snapshot-read flag. Existing version 1 discovery JSON, HELLO,
CAPABILITIES and terminal frames are unchanged.

On a new file stream, send kind 30:

```json
{"version":1,"target":{"name":"example","pid":123,"generation":"1793737311317"}}
```

The exact advertised target must match. Success is kind 32 with ID 0:

```json
{"id":0,"result":{"type":"ready","capabilities":{"version":1,"write":true,"chunk_limit":65536,"page_limit":64,"handle_limit":8}}}
```

A refused/unsupported opening closes the stream. Subsequent requests use
nonzero, strictly increasing **u32** IDs. Process one request at a time; no
pipelining, reconnect replay, handle reuse across streams, or transfer resumption.
All JSON field names/types and enum spellings are defined by the portable DTOs.
Unknown fields/operations and malformed framing refuse. Each frame/read/job has
a 30-second deadline. `ping` can keep an otherwise idle stream alive.

## HOST Files opening amendment (reviewed contract)

This architecture-reviewed server extension has separate source and native
acceptance gates. It does not describe qualified deployed HOST support.
The preceding discovery/opening and installer switches remain the legacy
session-bound contract; they are not HOST settings or a HOST fallback.

HOST uses existing ordinary-v1 authentication on its verified provider route.
Add only role `files`, signed byte `2`, to `StreamRole`; existing `control`/`terminal`
and bytes `0`/`1`, proof domain, challenge fields and accepted/denied envelopes
stay unchanged. Ordinary dispatch uses the signed role, with no extra role byte.
Advanced enrolled-owner byte `2` refuses. The protected HOST settings must be
ProviderOwner with an explicitly configured filesystem grant to open file
operations. An authenticated ordinary Files probe may learn that files are off.
Unknown-role rejection/closure on an older host never means files are enabled
or absent, and never authorizes a different transport. No fields are added to
strict existing HOST terminal/control capability responses.

After ordinary Accepted, switch to the existing rooted framing (one kind byte,
four-byte big-endian length). The host sends new kind **34 HOST_FILES_DESCRIPTION**
frame (at most 4096 bytes), strict JSON:

```json
{"version":2,"scope":{"host":"11111111-1111-4111-8111-111111111111","installation":"22222222-2222-4222-8222-222222222222","epoch":"29"},"filesystem":{"version":1,"write":false,"chunk_limit":65536,"page_limit":64,"handle_limit":8}}
```

`host`/`installation` use the existing validated HOST ID representations. `epoch`
is a canonical nonzero decimal u64 string taken from current lifecycle
`Snapshot.epoch`, not `OrdinaryHostInfo.policy_epoch` (enrollment revision) and
not `SessionPolicy.host_epoch`'s request namespace. The scope must match the
actual authenticated host route and installation. Keep account, absolute root,
credentials and daemon/session identifiers out of this message. `filesystem:null`
confirms off for this scope and the host closes after the greeting; a connection
failure or missing/malformed greeting does not confirm off.

With non-null capabilities, the client sends kind **30 OPEN**, strict JSON at
most 4096 bytes, on that same stream:

```json
{"version":2,"scope":{"host":"11111111-1111-4111-8111-111111111111","installation":"22222222-2222-4222-8222-222222222222","epoch":"29"}}
```

All three scope fields must exactly match both greeting and current protected
serving scope, and ordinary authorization must still hold. Version 1 or a legacy
`target` field refuses; no PID/session lookup is permitted. A stale/denied opening
may receive existing kind 32 `{"id":0,"error":"denied"}` and then closure;
malformed/unsupported openings close. On success, send the **unchanged** rooted
v1 kind-32 ready response (ID 0), whose capabilities must match the greeting.
Subsequent kinds 31/32/33, operation schemas, increasing request IDs, handles,
chunk/page limits, errors and timeouts remain exactly rooted v1. HOST opening
version 2 is not a change to legacy discovery version 2 or rooted operation
version 1. Kind 34 is legal only after authenticated Files-role dispatch; neither
legacy discovery nor Control/Terminal streams receive it.

The HOST server factors the existing rooted operation loop behind this opening
rather than manufacturing a legacy `Target`. Its single shared rooted service
uses one existing two-stream pool across all clients of that exact protected
scope and retains native-job permits until work actually ends. Prepared roots
cannot be rebound to a newer lifecycle scope even when the root path is equal. Client opening is a distinct entry to the
same bounded operation engine, preserving legacy `Client::open` behavior.
Handles never cross stream/scope boundaries. There is no automatic reconnect,
resume, finish replay or scope refresh within an existing stream.

Protected HOST consent is `Settings.filesystem`, off by default and omitted when
off; enabled is the existing strict `{root,write}` policy in protected storage.
Root/write/off changes follow confirmed Disable, Disabled-only configuration
and newly reviewed Enable. Ordinary Disable alone preserves configured settings.
Existing Desktop/headless surfaces without file-consent review refuse an existing
nonempty policy rather than silently erasing or re-enabling it; typed protected
configuration is only the server primitive, not completed user-facing setup.
The Files stream rechecks current protected scope/policy for each native request
and reply/data disclosure, plus a final admission before upload publication.
Unknown/busy/revoked scope closes/refuses, never reuses a cached grant. See
[ADR 0067's cutover rule](adr/0067-rooted-dev-tunnel-host-filesystem.md#revocation-and-publication-cutover):
work/disclosure already admitted before cutover can finish afterward. Leases are
not held over native or network I/O, Disable cannot undo an OS operation, and a
lost/suppressed finish response remains an unknown publication outcome. Existing
safe cleanup/restore may run after cutover; it cannot authorize another write.

Implementation acceptance requires the finite tests listed in ADR 0067, including
strict old-message compatibility, proof-role separation, no-daemon operation,
exact-scope refusal, bounded admission and publication/cutover interleavings.
Real signed HOST policy changes and desktop/iOS UI remain separate native gates.

## Reviewed DELETE extension

**Source implementation; native/packaged qualification pending.** Ordinary Files role2 and the initial kind34
HOST description2/capabilities1 stay unchanged. Legacy rooted v1, HostOpen2 and
its Ready/operation bytes stay unchanged; Open2 refuses the new operations even
when protected delete consent exists. No new auth role/crypto domain is added.

After the base greeting, explicit kind30 HostOpen3 echoes the same exact scope.
Only this mode receives distinct kind32/ID0 ReadyV3:

```json
{"id":0,"result":{"type":"ready_v3","capabilities":{"version":3,"write":true,"operations":[],"chunk_limit":65536,"page_limit":64,"handle_limit":8}}}
```

ReadyV3 uses a bounded explicit `operations:["delete"]` list, not a new policy
language: at most 16 names, each 1–32 UTF-8 bytes without control characters.
Unknown optional names are ignored and grant nothing; duplicate known names
refuse. Only implemented, explicitly consented operations are advertised.
Common write/limit fields must match the greeting and existing bounds. Open2
accepts only old Ready; Open3 only ReadyV3. Wrong-mode replies refuse before any
operation. Delete requires protected delete=true and write=true; upgrading grants
nothing. New clients may require Open3; old helpers refuse it before mutation.
No automatic downgrade/reconnect/fallback/replay. An unframed close/timeout is
unavailable negotiation, not proof of old-helper/off/absence.

Proposed strict operations, retaining existing frame/ID/path/JSON limits:

```json
{"id":1,"operation":{"op":"review_entry","path":"/notes.txt","purpose":"delete"}}
{"id":2,"operation":{"op":"delete","review":1}}
```

Review returns `entry_review`, a handle and actual File/Directory metadata,
retaining native identity/rooted parent/type/change stamp. Each purpose-bound
review occupies one existing handle slot; Close releases it, Delete consumes
its valid token once. Foreign/stale/wrong-purpose tokens cannot consume another
review. Only regular files and empty directories qualify; links/reparse refuse
without following them. Native removal rechecks emptiness and returns
`not_empty` for new children. Observed success returns `deleted`.

Fresh request/disclosure/publication admission surrounds exact capture,
verification and removal. Pre-removal refusal restores without replacing a
concurrent entry; failed restore retains the capture and uses existing
`recovery_required`/validated bounded virtual path. Never unlink a foreign
replacement at observable boundaries. Unix relies on the trusted reserved-capture
namespace, not identity-atomic unlink against arbitrary same-user OS writers.
Missing/dropped/suppressed results are terminal unknown, no replay
or inferred success/rollback; existing native-job charges outlive async timeout.

Protected delete defaults false/omit-false and requires write, full Disabled-only
foreground consent and fresh Enable epoch. Existing static `capabilities` stays
byte-identical; separate `capabilities files-delete` returns strict schema1 plus
`host_files_delete_v3` boolean before setup/update effects, with existing bounded
authenticated read-only probing. This proves compiled support, not permission.

Desktop prepares no more than eight direct children, all-or-no-dialog, with
worker-owned handles; Busy preserves transfers and Cancel closes all reviews.
Confirm runs sequentially, stops at first non-success and reports confirmed,
unknown and untouched entries separately. See [ADR 0067](adr/0067-rooted-dev-tunnel-host-filesystem.md)
for native primitive gaps and required evidence. Rename/chmod, links, recursive
or oversized-batch delete, Mobile and full CLI parity remain queued.

## Portable client engine

`file_client::Client` consumes an already host-key-verified **separate legacy
bridge stream** and an exact target. Discovery consumes its own stream and
checks that same target; disabled grants return no filesystem capabilities.
The engine does not acquire credentials, connect to a relay, authenticate an
ordinary HOST role, attach a terminal, launch a tab or authorize a local path.
It adds no file operation or grant. `Client::open_host` is the separate opening
for an already ordinary-Files-authenticated HOST stream, validating the host and
installation plus the server's lifecycle scope before echoing it. Account/relay
client composition and top-level desktop/iOS entry points remain separate.

The client exposes bounded directory pages, stat/read handles, streamed
uploads/downloads, mkdir, ping and explicit close. Handles are bound to their
connection and issuance; a foreign or retired handle cannot name a new object.
At most the advertised eight handles and one upload are tracked. Frames,
canonical offsets, response IDs, chunks, metadata and names are checked before
use; unexpected frame kinds/oversized headers refuse before payload allocation.
No directory or file is buffered in full. Empty nonfinal directory pages with
skips must be continued. EOF does not release a handle.

Local source/sink ownership belongs to the caller, including safe staging and
publication. Upload sources must contain exactly the declared size followed by
EOF; failures abort the stream. Cancellation, deadline, malformed input and a
dropped in-flight operation close the affected stream; dropped high-level
transfers also abort while waiting on local source/sink I/O. `cancel()` closes
an idle client immediately. A supplied cancellation token interrupts active
work and is checked before another operation. There is no reconnect or replay.
An unacknowledged/malformed finish returns `UncertainFinish`; a dropped finish
future is also an unknown publication outcome, and that client cannot be reused.
A reported virtual recovery path is for deliberate host-local operator recovery,
never an automatic client delete/rename/retry instruction. Capability metadata
alone does not establish a still-live grant.

The duplex regressions exercise this engine against the existing legacy server;
scripted peers cover malformed replies, empty pages and lost finish responses.
They do not qualify ordinary HOST authentication, relay service, signed native
filesystem behavior, packaged UI or crash recovery.

## Paths, handles and operations

Paths are UTF-8 **virtual absolute** POSIX-style paths: `/` is the configured
root, `/notes/a.md` is below it even on Windows. Do not send drive letters,
host-absolute paths or a source cwd. Reject `.`/`..`, repeated/trailing separators,
backslashes, controls, Windows ambiguous/reserved names, components over 255
UTF-8 bytes and total paths over 4096 bytes. `.festerm-transfer-*` names are
reserved. Symlinks/reparse points are not followed. Unsupported entries are
marked `unsupported`; unrepresentable/reserved names are counted as skipped.
Directory iteration is native order, not a sorted or immutable snapshot.

Requests have this shape:

```json
{"id":1,"operation":{"op":"open_directory","path":"/"}}
```

Success: `{"id":1,"result":{...}}`. Failure:
`{"id":1,"error":"conflict"}`. Exactly one of result/error is present. Fixed error
codes: `invalid`, `denied`, `not_found`, `exists`, `conflict`, `busy`, `io`,
`recovery_required`. No raw OS error text or host absolute paths are returned.

| Operation fields | Success result (`type` plus fields) |
| --- | --- |
| `stat`, `path` | `stat`, `metadata` |
| `open_directory`, `path` | `directory`, `handle` |
| `read_directory`, `handle`, `limit` (1–64) | `entries`, `entries:[{name,kind}]`, `done`, `skipped` |
| `open_read`, `path` | `file`, `handle`, `metadata` |
| `read`, `handle`, `offset`, `length` (1–65536) | `read`, `offset`, `length`, `eof`; immediately followed by one DATA frame, including an empty DATA payload after its ID at EOF |
| `begin_upload`, `path`, `size`, `expected` (null or open-read handle) | `upload`, `handle` |
| `write`, `handle`, `offset`, `length` (1–65536); immediately followed by one matching DATA frame | `written`, `offset` (next upload offset) |
| `finish_upload`, `handle` | `finished`, `size` |
| `close`, `handle` | `closed` |
| `mkdir`, `path` | `created` |
| `ping` | `pong` |

Metadata is `{kind:"file"|"directory",size:"decimal",modified_ms:null|"decimal"}`.
Directory sizes are zero; modification times are milliseconds since UTC epoch.
Offsets and sizes are **canonical unsigned decimal strings** (u64, no leading
zeroes except `"0"`), avoiding JSON floating-point precision loss. IDs/handles
and chunk lengths are JSON integer u32s. DATA's request ID must match its
request/response; chunk length must match the JSON declaration.

At most eight handles and one upload exist per stream; at most two filesystem
streams are admitted per host. Blocking jobs retain their admission permit even
after a timeout, preventing accumulation of abandoned blocking I/O. `close` is
required after finishing an enumeration/download; it aborts an upload. EOF does
not implicitly close a read or directory handle. Enumeration may return an
empty page with `done:false` and skipped entries: continue until `done:true`.
No entire directory/file is buffered; recursive transfers are client-driven
enumeration plus mkdir and individual file transfers.

## Uploads, replacement and cancellation

Create is the default (`expected:null`): an existing destination refuses, both
at begin and at atomic publication. Upload offsets must be sequential starting
at zero, without sparse gaps or retries. `finish_upload` requires the exact
declared size and syncs staging contents before publication. Empty files need
begin plus finish, without a write frame.

To explicitly replace, first `open_read` the same path and retain that handle;
show its metadata in the client's overwrite confirmation. Pass its handle as
`expected`. The host verifies file identity and change stamps at begin/finish.
A changed version refuses rather than overwriting silently. Handles are
connection-local version tokens, not persistent hashes or reusable permissions.
Downloads also refuse a changed open file instead of mixing file generations.

Replacement captures the original without overwriting another name, then
publishes the new file atomically without replacing a concurrently created
destination. A conflict restores the captured original without overwrite.
If restoration/cleanup cannot be proven safe, preserve evidence and return
`recovery_required` with `recovery_path` in the same virtual namespace. This
reserved name is for deliberate **host-local operator recovery**, not a client
delete/rename API. Never blindly retry a finish after an uncertain connection
loss; rediscover/stat and obtain fresh explicit consent.

Disconnect, close or cancellation cleans incomplete staging files when the
process can run cleanup. A force-kill/power loss can leave private staging or
recovery files; this is not durable crash-resumable transfer. New uploaded files
and directories are current-user-only; source ACLs, executable bits, timestamps,
links and extended metadata are not copied. Large transfers are streamed and
consume the OS account's disk quota; there is no RAM-sized whole-file limit.

This is not isolation from hostile code already running as the same OS user,
administrator/root, or external filesystem changes by those principals.
Symlink/reparse traversal and foreign-writable directory parents refuse.
Legacy rooted-v1/Open2 grants introduce no delete, general rename, chmod or
execution. Separately consented Open3 reviewed deletion is specified above;
general rename/chmod remain unavailable. Remote content is never persisted in
ordinary fesTerm configuration/workspaces.
