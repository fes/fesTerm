<!-- agent-navigation: {"domain": "tunnels-enterprise", "generator": "2", "model": "mai-code-1.1-flash", "sources": "d0b78bdc2d1fd49eaa580ca459f44ca11c5eee7bfa79224dfee2be2378e32d3a"} -->
# Dev tunnels, enterprise, and PowerShell

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Owns authenticated host admission, rooted file-transfer semantics, and the trusted filesystem boundaries that attach HOST policy to concrete local roots.

Canonical docs: [docs/dev-tunnels-experiment.md](../../docs/dev-tunnels-experiment.md), [docs/dev-tunnel-files-protocol.md](../../docs/dev-tunnel-files-protocol.md), [docs/remote-observation-protocol.md](../../docs/remote-observation-protocol.md), [docs/powershell-enterprise.md](../../docs/powershell-enterprise.md), [docs/host-release-artifacts.md](../../docs/host-release-artifacts.md), [docs/adr/0084-host-based-dev-tunnels.md](../../docs/adr/0084-host-based-dev-tunnels.md)

## Host runtime

Use for: Which authenticated stream path is chosen and how are proofs revalidated?

- [`HostRuntime`](../../crates/festerm-tunnel-bridge/src/host_runtime.rs) — Shared aggregate budget and per-role dispatch for authenticated host sessions
- [`serve_reserved`](../../crates/festerm-tunnel-bridge/src/host_runtime.rs) — Authenticates then dispatches control, terminal, or files by proven role
- [`authenticate_ordinary`](../../crates/festerm-tunnel-bridge/src/host_runtime.rs) — Revalidates route binding, denied keys, and proof before acceptance

Evidence:
- [`authenticated_runtime_routes_actual_ssh_control_and_rechecks_revocation`](../../crates/festerm-tunnel-bridge/src/host_runtime/tests.rs) — Control routing follows the authenticated stream and revocation is rechecked
- [`native_authenticated_runtime_creation_retains_issuer_and_exact_end_authority`](../../crates/festerm-tunnel-bridge/src/host_runtime/tests.rs) — Runtime creation preserves issuer and exact end authority

Caveats:
- Route selection is explicit and relies on the proven encrypted channel state.
- Protected policy is reloaded and revalidated; cached state is not authority.
- A cancelled or stale proof can reject before control or terminal dispatch.

## Rooted files

Use for: How does the client handle uploads, aborts, and connection-local file handles?

- [`FilesEndpoint`](../../crates/festerm-tunnel-bridge/src/files_endpoint.rs) — Shared Files adapter that tracks transfer state, generation, and review markers
- [`Client`](../../crates/festerm-tunnel-bridge/src/file_client.rs) — Rooted sequential file protocol with strict replacement and path validation
- [`Handle`](../../crates/festerm-tunnel-bridge/src/file_client.rs) — Connection-local transfer handle; close is explicit and does not prove remote completion

Evidence:
- [`rooted_client_interrupted_upload_closes_handle_and_keeps_stream_usable`](../../crates/festerm-tunnel-bridge/src/file_client.rs) — Abort closes the local handle but keeps the transfer stream usable
- [`files_endpoint_returned_cancel_and_source_error_do_not_leave_unknown_markers`](../../crates/festerm-tunnel-bridge/src/files_endpoint.rs) — Cancel and source errors do not leave stale unknown state behind

Caveats:
- Handle IDs are connection-local and cannot authorize a matching remote object elsewhere.
- A close or abort does not imply unlink success or publication completion.
- Failure markers and state are retained only under explicit review and strict cancellation rules.

## Filesystem trust

Use for: What local roots and start paths are accepted before a HOST files session can run?

- [`Filesystem`](../../crates/festerm-tunnel-bridge/src/filesystem.rs) — Prepared root and allowed-location policy for HOST filesystem access
- [`validate_start_directory`](../../crates/festerm-tunnel-bridge/src/filesystem.rs) — Rechecks the configured start path is real, not symlinked, and not a privileged escape

Evidence:
- [`start_directory_is_nofollow_navigation_not_an_allowed_root`](../../crates/festerm-tunnel-bridge/src/filesystem.rs) — Shows start-directory validation rejects nofollow, missing, file, and symlink cases

Caveats:
- A start directory is metadata-checked; path grants are not a substitute for local root trust.
- Allowed locations and root proofs must be current and consistent with the request scope.
- Review-only metadata cannot widen a filesystem root or bypass mutation checks.

## Discovery inventory

Use for: Which local sessions are discovered and when does a refresh publish a new inventory?

- [`HostBinding`](../../crates/festerm-tunnel-client/src/service.rs) — Selected tunnel binding identifies the concrete bridge host and public key
- [`Discovery`](../../app/festerm/src/discovery.rs) — Polls native and hosted session roots and coalesces refreshes
- [`Inventory`](../../app/festerm/src/discovery.rs) — Reports native, tmux, screen, and hosted inventory with errors

Evidence:
- [`changed_inventory_is_published_and_periodic_discovery_continues`](../../app/festerm/src/discovery.rs) — Verifies refresh coalescing, publication, and continued polling after a visible change

Caveats:
- Inventory is observational, not a grant of access or host authority.
- Refreshes intentionally coalesce stale results; a stale scan is discarded.
- Hosted roots are tracked separately from default native roots and do not imply identity equivalence.
