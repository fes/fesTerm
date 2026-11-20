<!-- agent-navigation: {"domain": "tunnels-enterprise", "generator": "2", "model": "mai-code-1.1-flash", "sources": "8f065d1535cfffad3d2d253d6f01fdd4dab2ebf57b0b23a01262ae0978d03d6e"} -->
# Dev tunnels, enterprise, and PowerShell

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Portable tunnel bridge discovery, attachment, and validated transport for host-backed dev tunnels and PowerShell sessions.

Canonical docs: [docs/dev-tunnels-experiment.md](../../docs/dev-tunnels-experiment.md), [docs/dev-tunnel-files-protocol.md](../../docs/dev-tunnel-files-protocol.md), [docs/remote-observation-protocol.md](../../docs/remote-observation-protocol.md), [docs/powershell-enterprise.md](../../docs/powershell-enterprise.md), [docs/host-release-artifacts.md](../../docs/host-release-artifacts.md), [docs/adr/0084-host-based-dev-tunnels.md](../../docs/adr/0084-host-based-dev-tunnels.md)

## Host discovery

Use for: Which bridge host description is accepted without opening daemon IPC or issuing a live attachment?

- [`discover_stream`](../../crates/festerm-tunnel-bridge/src/wire.rs) — read-only host description handshake over the bridge stream
- [`serve_with_files`](../../crates/festerm-tunnel-bridge/src/transport.rs) — advertises target identity, file consent, and endpoint metadata

Evidence:
- [`bridge_discovery_is_bounded_read_only_and_never_opens_daemon_ipc`](../../crates/festerm-tunnel-bridge/src/transport.rs) — proves discovery is bounded and read-only
- [`bridge_discovery_refuses_incompatible_or_nonbridge_responses_without_admission`](../../crates/festerm-tunnel-bridge/src/transport.rs) — proves invalid host descriptions are rejected before admission

Caveats:
- Discovery validates target generation and endpoint identity but is not live daemon proof.
- Foreign or incompatible peers are rejected before any attachment or IPC is opened.

## Tunnel attach

Use for: How does a client negotiate a session hello, snapshot, and reclaim before adoption?

- [`Hello`](../../crates/festerm-tunnel-bridge/src/protocol.rs) — request payload: target, schema, takeover, optional client id
- [`Description`](../../crates/festerm-tunnel-bridge/src/protocol.rs) — host response: target, files, reclaim, and takeover state
- [`establish_negotiated`](../../crates/festerm-tunnel-bridge/src/mobile.rs) — retries previous snapshot schema once on a fresh stream
- [`TunnelSession`](../../crates/festerm-tunnel-bridge/src/mobile.rs) — adopts the recovered terminal and drives session lifecycle and output

Evidence:
- [`establish_negotiated_retries_the_previous_schema_on_a_fresh_stream`](../../crates/festerm-tunnel-bridge/src/mobile.rs) — proves schema fallback and SafeFraming guardrail
- [`remote_snapshot_roundtrips_and_rejects_malformed_and_oversized_state`](../../crates/festerm-tunnel-bridge/src/protocol.rs) — proves snapshots are validated, bounded, and not replayed after adoption

Caveats:
- A host older than SafeFraming is retried once, but the previous schema never gets a SafeFraming snapshot.
- Malformed snapshots or incompatible hellos fail the handshake before a session is adopted.

## Bridge transport

Use for: How are input, output, and file requests bounded on the validated bridge session?

- [`start_client_with_takeover`](../../crates/festerm-tunnel-bridge/src/transport.rs) — creates validated transport, session, and optional file requestor
- [`client_io_with_files`](../../crates/festerm-tunnel-bridge/src/transport.rs) — handles Hello, snapshot, file reads, and terminal I/O
- [`ValidatedStream`](../../crates/festerm-tunnel-bridge/src/transport.rs) — bounded queue and cancellation without retry on dropped transport
- [`FileAccess`](../../crates/festerm-tunnel-bridge/src/protocol.rs) — explicit file consent flags and path style gating

Evidence:
- [`remote_session_adopts_before_live_output_and_never_owns_query_replies`](../../crates/festerm-tunnel-bridge/src/transport.rs) — proves file reads stay separate from terminal output and adoption is gated
- [`saturated_transport_is_bounded_and_drop_cancels_without_retry`](../../crates/festerm-tunnel-bridge/src/transport.rs) — proves queue saturation is bounded and drop cancels the session transport

Caveats:
- Frame direction and size are checked before allocation; wrong-direction or oversized packets are rejected.
- A file result cannot satisfy another request or be mistaken for terminal output.
