# Dev Tunnel host filesystem protocol

This is the host/client integration contract for ADR 0067. Host implementation
lives in `festerm-tunnel-bridge`; desktop/iOS clients are a separate change.
Portable Rust DTOs/constants are in `file_protocol.rs`. Do not send these
operations as terminal input, invoke a remote shell, or fall back to SFTP/local
files. No desktop/iOS file-manager integration is claimed by this host PR.

## Authorization and existing installations

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
No delete, general rename, chmod, execution or remote content persistence in
ordinary fesTerm configuration/workspaces is introduced.
