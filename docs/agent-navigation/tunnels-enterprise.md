<!-- agent-navigation: {"domain": "tunnels-enterprise", "generator": "2", "model": "mai-code-1.1-flash", "sources": "dee1e2d317322710d89a8615377c1482d29b60a6008d2f78bec196b454b42ad6"} -->
# Dev tunnels, enterprise, and PowerShell

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

This domain owns authenticated host runtime admission, route selection, and rooted file-transfer semantics that preserve ownership and cancellation invariants.

Canonical docs: [docs/dev-tunnels-experiment.md](../../docs/dev-tunnels-experiment.md), [docs/dev-tunnel-files-protocol.md](../../docs/dev-tunnel-files-protocol.md), [docs/remote-observation-protocol.md](../../docs/remote-observation-protocol.md), [docs/powershell-enterprise.md](../../docs/powershell-enterprise.md), [docs/host-release-artifacts.md](../../docs/host-release-artifacts.md), [docs/adr/0084-host-based-dev-tunnels.md](../../docs/adr/0084-host-based-dev-tunnels.md)

## Host runtime

Use for: Which authenticated stream path is chosen and how are proofs revalidated?

- [`HostRuntime`](../../crates/festerm-tunnel-bridge/src/host_runtime.rs) — Shared aggregate budget and per-role dispatch for authenticated host sessions
- [`serve_reserved`](../../crates/festerm-tunnel-bridge/src/host_runtime.rs) — Authenticates then selects control, terminal, or files by proven stream role
- [`authenticate_ordinary`](../../crates/festerm-tunnel-bridge/src/host_runtime.rs) — Verifies route binding, denied keys, and ordinary proof before acceptance

Evidence:
- [`authenticated_runtime_routes_actual_ssh_control_and_rechecks_revocation`](../../crates/festerm-tunnel-bridge/src/host_runtime/tests.rs) — Confirms control role follows the authenticated stream and revocation is rechecked
- [`native_authenticated_runtime_creation_retains_issuer_and_exact_end_authority`](../../crates/festerm-tunnel-bridge/src/host_runtime/tests.rs) — Shows the runtime keeps issuer and exact end authority through creation

Caveats:
- Route selection is explicit and relies on proven encrypted channel state.
- Protected policy is reloaded and revalidated; cached state is not authority.
- A cancelled or stale proof can reject before control or terminal dispatch.

## Rooted files

Use for: How does the client handle uploads, aborts, and connection-local file handles?

- [`FilesEndpoint`](../../crates/festerm-tunnel-bridge/src/files_endpoint.rs) — Shared Files adapter that tracks transfer state, generation, and review markers
- [`Client`](../../crates/festerm-tunnel-bridge/src/file_client.rs) — Rooted sequential file protocol with strict replacement and path validation
- [`Handle`](../../crates/festerm-tunnel-bridge/src/file_client.rs) — Connection-local transfer handle; close is explicit and does not prove remote completion

Evidence:
- [`rooted_client_interrupted_upload_closes_handle_and_keeps_stream_usable`](../../crates/festerm-tunnel-bridge/src/file_client.rs) — Abort closes the local handle but leaves the transfer stream usable
- [`files_endpoint_returned_cancel_and_source_error_do_not_leave_unknown_markers`](../../crates/festerm-tunnel-bridge/src/files_endpoint.rs) — Shows cancel and source errors do not leave stale unknown state behind

Caveats:
- Handle IDs are connection-local and cannot authorize a matching remote object elsewhere.
- A close or abort does not imply unlink success or publication completion.
- Failure markers and state are retained only under explicit review and strict cancellation rules.
