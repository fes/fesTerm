# Read-only remote observation protocol

This is the implementation contract for #584 and ADR 0070. It extends the
host filesystem foundation in #585 without granting general browsing, writes,
terminal output, terminal control, JavaScript, credentials or remote execution.
The application-owned broker is the required MCP authority; the filesystem
observation integration described below is not yet wired into that broker.

## Current implementation status

The legacy bridge implements the rooted observation protocol and a
connection-owned `ObservationRequestor`. This does not establish an available
Desktop, mobile or MCP file-watch workflow:

- `transport::ClientSession` carries the provider alongside its terminal, but
  Desktop `start_tunnel` currently retains only `files` and `session`, dropping
  `observation` (`app/festerm/src/tunnel_reconnect.rs`).
- The host-oriented workflow returns `HostSession` from
  `ApprovedAttachment::start`; Desktop and mobile install that terminal without
  an observation provider. HOST terminal frames explicitly confer no filesystem,
  observation or MCP capabilities (`host_client.rs` and `host_terminal.rs` in
  `crates/festerm-tunnel-bridge/src`).
- The current MCP catalog in `crates/festerm-automation-mcp/src/schema.rs` exposes
  terminal observation and controller operations, without filesystem observation
  tools or a `files.observe` grant. Existing terminal observation is separate
  from filesystem observation.

Consequently, session/generation/root fencing below describes the implemented
legacy protocol and the required future composition. It does not prove that a
new HOST session can watch files, or that an MCP caller can obtain those events.
Native end-to-end HOST/file-watch/MCP qualification remains unrun. Windows/Linux
watch support is implemented at the protocol layer; macOS advertises
`watch: false`.

## Host consent and lifetime

`--observe-root ABSOLUTE_DIRECTORY` is repeatable, up to four roots, and grants
only observation. `--disable-observation` revokes every observation root.
Managed installer equivalents are `-ObserveRoot` and `-DisableObservation`.
An omitted option preserves consent. Ordinary `--file-root` / writes never
imply observation consent, and observation never implies those grants.
Owned workers stop before policy changes become runnable; tunnel/key/shell
identity stays unchanged. A different helper release requires explicit upgrade.
Daemon recovery uses the same installer lock, reloads current policy before
mutation and holds it through launch/publication, preventing a stale worker
snapshot from restoring a journaled revocation.

Each service incarnation assigns an opaque root ID to a retained, trusted,
no-follow directory capability. Root IDs survive reconnect to that same service
incarnation, but are not authority; a new connection requires a freshly bound
provider and broker approval even when an ID repeats. Policy change/restart
rotates root IDs. Revisions, cursors and subscriptions are stream-local and never
survive reconnect. Duplicate configured root paths are rejected lexically, not
by native identity or alias equivalence. Labels are untrusted display text,
not root authority; host absolute paths are never published. Paths are virtual
absolute POSIX paths under the selected root, with the same ambiguity and
link/reparse rejection as the rooted filesystem service.

Every opening and data operation checks the exact selected daemon generation
against its live registry record and generation lease. Observation opens no
daemon terminal attachment and cannot take over. Watches also end on a failed
generation/liveness check. A connection/bridge failure becomes an explicit
ended result in the client adapter; it never resumes or switches providers.

## Negotiation and frames

Legacy discovery versions 1 and 2 remain unchanged. Discovery version 3 uses
the existing DISCOVER/DESCRIPTION framing and reports the exact target, optional
existing filesystem capabilities, and optional observation capabilities.
Disabled or older hosts advertise/refuse observation rather than falling back
to unrestricted reads, local files, shell commands or another connection.

Observation uses separate forwarded streams on the already verified,
authenticated host transport. No new network endpoint or credential is exposed.
The relay host keeps at most four concurrently open forwarded channels per authenticated client
(terminal, observation control and two watches) to the one configured port.
Closing a channel releases its slot; resubscribing cannot exhaust a lifetime counter.
Serving tasks retain channel and client admission through completion, so
disconnect/reconnect churn cannot bypass the four-client/sixteen-stream bound.
Frame kinds are 40 OPEN, 41 REQUEST, 42 RESPONSE, 43 DATA and 44 NOTICE.
Headers remain kind u8 plus big-endian payload length u32. OPEN is at most
4096 bytes; JSON frames at most 65536; DATA is request ID u32 big-endian followed
by at most 65536 bytes. Unknown fields, directions and operations refuse before
unbounded allocation. Requests have nonzero, increasing u32 IDs; no pipelining,
replay or offset retry after an uncertain result.

OPEN names protocol version 1 and the exact target. READY returns capabilities:
the opaque approved roots and display labels, read/page limits, watch support
and generation-correlation method support (the actual marker may be absent).
REQUEST carries an
ID, exact opaque root ID and one typed operation. One result or fixed error is
returned. Errors contain no raw OS text, content or host path.

## Operations

- `stat(path)` returns kind, native stable file identity, unsigned decimal size,
  optional UTC epoch modification time, and an opaque current revision.
- `open_directory(path)` / `read_directory(cursor, limit)` return bounded native
  pages with names, kinds and same-handle identity/size/time metadata. Unsupported
  or vanished entries are omitted and aggregately counted in `Page.skipped`;
  cursors are stream-local. An exactly full final page can require an additional
  empty page to establish `done`. `close_directory(cursor)` releases the iterator;
  completed cursors remain allocated until close or stream retirement.
- `read(path, offset, length, expected)` accepts an explicit identity/revision
  from stat or a preceding changed response; the entire expected metadata is
  compared. The host opens without following
  links and compares the same retained file before and after bounded I/O.
  A changed identity, revision or truncation returns metadata-only `changed`,
  not bytes under the old offset. A client may deliberately accept the newly
  reported version and read the same saved offset; append changes are not
  confused with command completion. No whole-file size cap is involved.
- `watch()` subscribes to the approved root and its immediate subdirectories,
  not arbitrary recursive trees. Notices are content-free created, modified,
  removed or renamed paths with canonical unsigned sequence numbers. Native
  events are coalesced/rate limited with bounded buffering. Overflow, ambiguous
  native events, unsafe directory changes or a coverage limit emit
  `rescan_required` and retire the subscription rather than silently losing
  changes. Top-level create/remove/rename events conservatively require rescan,
  including ordinary files; new directory coverage is retired rather than
  dynamically expanded. Disconnect/restart/expiry ends; reconnect requires a new
  subscription. Windows/Linux implement native watches; macOS/other Unix hosts
  advertise `watch: false`. No polling is misrepresented as a lossless watch.
- `correlation()` returns the daemon-injected, generation-bound
  `FESTERM_SESSION_CORRELATION` value only when the selected daemon actually
  injected it. This is helper 6's permitted alternative to process-tree
  enumeration. Descendants inherit it, but it is non-secret evidence only:
  it never enrolls a target or authorizes an operation. Existing daemon shells
  without the marker report unavailable; no shell is restarted or hook installed.
- Internal `ping()` keeps an otherwise idle request stream alive every ten seconds.

Sizes/offsets are canonical decimal u64 strings, avoiding JSON precision loss.
Read results contain metadata and the exact following DATA length; empty EOF
still has a DATA frame containing its request ID. Zero-length reads are permitted,
so empty data establishes EOF only for a positive requested length. Outcomes
have no separate offset or EOF field. A changed response has no
DATA. Identity is obtained from the same handle (Unix device/inode; Windows
volume/file index), not names or unrelated stat calls. Revisions are scoped to
the stream-local retained-object/cache token and change on mutation or replacement,
preventing a
retired handle from authorizing bytes after replacement or cache eviction.
Mutation is detected from identity, size and change time, so a same-size
rewrite within one filesystem timestamp tick (about 15.6 ms on Windows, the
coarse kernel clock on Linux) cannot be distinguished; appends, truncation and
replacement are always detected.
Clients do not infer an append-only history after a gap: they rescan/reconcile.

## Client library and planned broker contract

Portable client types live in `festerm_tunnel_bridge::observation_client`.
`ObservationRequestor` is a cloneable, bounded, connection-owned provider:

- `capabilities()` returns currently negotiated capabilities or `None` while
  pending, unsupported or retired; `ClientSession.observation` can be `Some`
  while negotiation is pending. `available()` and `cancel()` expose lifetime,
  not additional authority.
- `stat(root, path)`, `open_directory(root, path)`,
  `read_directory(root, cursor, limit)`, `close_directory(root, cursor)`,
  `read(root, path, offset, length, expected)` and `correlation(root)` perform
  bounded operations on that one attachment.
- `watch(root)` returns an `ObservationWatch` with `try_recv()`,
  `recv_timeout(Duration)` and explicit close. A receive timeout does not retire
  the subscription. A retired parent connection terminates all watches; terminal
  outcomes are latched outside the queue, discard queued notices and remain
  observable on subsequent receives.
- Read outcomes distinguish data from changed metadata; watch outcomes
  distinguish notices, rescan-required and ended. The provider exposes neither
  authentication tokens nor raw SSH/relay handles.

  DTOs are re-exported from `observation_client`. Metadata is
  `{kind: File|Directory, identity: String, size: String,
  modified_ms: Option<String>, revision: String}`; pages contain
  `Entry {name, metadata}`, `done` and `skipped`. Cursor IDs are strings and page
  limits are `u16`. Reads return `Data {metadata, bytes}` or `Changed {metadata}`.
  Watch notices contain a decimal sequence and Created/Modified/Removed/Renamed,
  RescanRequired or Ended; correlation is `Correlation {value: String}`.

The legacy transport constructs the provider on the same exact `ClientSession`
as its terminal; cancellation/displacement retires it. A metadata probe cannot
keep a retired terminal transport alive. The Desktop adapter does not yet retain
this provider, and new HOST attachments do not supply it. Future application
integration must retain the exact attachment-owned provider, copy only capability
metadata into native review and perform I/O off the UI thread. No terminal
protocol frame carries observation content.

An independent observation expiry fence conservatively extracts bounded `exp`
from the already service-validated scoped token. Missing/unusable expiry makes
observation unavailable, not the terminal. Monotonic deadline checks enforce
expiry during availability/receive operations independently of timer scheduling.
This host-token fence does not replace broker grant/caller expiry.

The planned MCP integration requires a separate `files.observe` native grant,
bound to authenticated caller, exact enrolled tab/attachment, approved opaque
root/provider incarnation,
revocation epoch, deadline and quotas. No terminal.observe/control or JavaScript
grant is required or implied. The future native review must explain that event
logs can contain prompts/tool arguments/responses and explicitly confirm the
chosen roots.
Every queued request and completion must revalidate the binding before disclosure.
Closure, human revocation, lock, expiry, app/attachment replacement or caller
disconnect must discard pending content and end subscriptions. Labels/cwd/PIDs or
correlation values alone never authorize or retarget.

Planned MCP tools must provide capability negotiation, approved roots,
stat/list/read,
watch/next/close and correlation; metadata/bytes must be delivered only to their
authorized caller. Unsupported SSH or other backend combinations must advertise
unavailable until their connection-owned, approved-root no-follow provider is
qualified. SSH may advertise polling-only; no shell or new SFTP connection
fallback is allowed.

## Evidence and limits

Bounds are four roots, four host streams, four shared blocking jobs, 64-KiB
chunks, 64-entry pages, 4096-length paths, 64 pinned metadata objects, eight
cursors, 32 watched directories, 64 native/client notices, two watch streams per
provider and 4096 watch-setup iterator inspections. Windows retains at most 64
ancestor locks. Client open/request/frame waits and host request/watch-generation
jobs have 30-second deadlines; the initial host OPEN generation job has no
separate timeout wrapper. A synchronous request timeout cancels the entire
observation provider without replay. Generation/coalescing checks run on a
100-ms tick and again before notices/DATA release. Idle watch receiving has no
ordinary frame timeout. Native watch
generation checks share the blocking-job budget; contention can conservatively
end a subscription even when the generation is still live. Clients must handle
explicit retirement and deliberately resubscribe rather than infer continuity.
Native watch
coverage limits refuse explicitly; filesystem permissions are never repaired.
No remote content enters application configuration, logs, ordinary caches,
workspaces or status persistence. The external controller owns status derivation
and privacy; host/broker implementation is not a Copilot log parser.

The full contract requires deterministic tests of strict negotiation, default-off
independence,
same-handle identities, large/empty offset reads, append/change/truncation and
replacement refusal, root/link safety, event overflow/gaps/disconnect,
generation/caller/root revocation and JavaScript-disabled MCP delivery.
Native signed-host watch/policy migration, desktop approval/lock and external
controller/physical-device usability remain explicit qualification gates.
Earlier Windows native isolated identity/watch results cover the protocol layer,
not the missing application/MCP composition. This status review did not rerun
those tests or qualify Linux/macOS execution. macOS native watch support remains
a named implementation prerequisite; the protocol supports stat/list/offset-read
without watches. Availability in the new HOST workflow still requires the
provider and approval integration described above.
