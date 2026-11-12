<!-- agent-navigation: {"domain": "tunnels-enterprise", "generator": "2", "model": "mai-code-1.1-flash", "sources": "52298d05e65eadc9535b09952ed61542ec7c5d38d8632d49de3cc11aed3ad554"} -->
# Dev tunnels, enterprise, and PowerShell

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Owns dev-tunnel discovery, managed-host consent, enterprise/PowerShell workflows, and release artifact verification for desktop remote sessions.

Canonical docs: [docs/dev-tunnels-experiment.md](../../docs/dev-tunnels-experiment.md), [docs/dev-tunnel-files-protocol.md](../../docs/dev-tunnel-files-protocol.md), [docs/remote-observation-protocol.md](../../docs/remote-observation-protocol.md), [docs/powershell-enterprise.md](../../docs/powershell-enterprise.md), [docs/host-release-artifacts.md](../../docs/host-release-artifacts.md)

## Session discovery

Use for: When a remote session inventory or refresh must be extended or bounded

- [`Inventory`](../../app/festerm/src/discovery.rs) — coalesced discovery across native, tmux, screen, and hosted roots
- [`Discovery`](../../app/festerm/src/discovery.rs) — UI refresh loop cancels superseded workers and only repaints on visible change

Evidence:
- [`native_inventory_keeps_same_name_sessions_in_their_exact_roots`](../../app/festerm/src/discovery.rs) — proves same-name sessions are kept distinct by runtime root and hosted source

Caveats:
- hosted roots are only considered behind the feature gate and runtime namespace
- generation and refresh coalescing drop superseded results
- errors from one provider remain per-root, not global

## Local bind

Use for: When choosing SSH local source address or validating connection source policy

- [`LocalBindDraft`](../../app/festerm/src/local_bind.rs) — resolves Automatic vs fixed source IP and persists policy for SSH
- [`parse_local_bind_address`](../../app/festerm/src/local_bind.rs) — rejects invalid or scoped addresses before backend session creation

Evidence:
- [`malformed_and_scoped_source_addresses_are_rejected_before_connecting`](../../app/festerm/src/local_bind.rs) — protects against wildcard, multicast, link-local, and mapped addresses

Caveats:
- fixed bind only sets source IP, not interface, VPN, route, or DNS path
- Ask mode fails closed until the user picks an address
- discovery bounds results to 64 addresses and filters unsupported addresses

## Hosted profile consent

Use for: When enabling or revoking managed-host tunnel profiles and restart semantics

- [`Controller`](../../app/festerm/src/profile_hosting.rs) — desktop hosting workflow, grant checks, and restart or attach orchestration
- [`Store`](../../crates/festerm-tunnel-bridge/src/profile_hosting.rs) — private installation records hold consent, selected account, and target identity

Evidence:
- [`hosted_preparation_publishes_only_complete_records_without_overwriting`](../../crates/festerm-tunnel-bridge/src/profile_hosting.rs) — proves consent and account selection are fenced before installation records are committed

Caveats:
- consent string must exactly match the binding before a grant is accepted
- Target identity is validated before bind or deferred restart
- revocation and cleanup are separate from initial provisioning and require explicit consent

## Enterprise and PowerShell

Use for: When adding enterprise Dev Center inventory or PSRP session flows

- [`EnterpriseState`](../../app/festerm/src/enterprise.rs) — Dev Center auth and bounded inventory tab lifecycle across browser handoff
- [`PowerShellState`](../../app/festerm/src/powershell.rs) — PSRP and SSH worker states with bounded event and value presentation

Evidence:
- [`docs/powershell-enterprise.md`](../../docs/powershell-enterprise.md) — documents the trust boundary between enterprise sign-in, remote-session output, and consented takeover

Caveats:
- all worker-side data stays off the UI thread and budgets are enforced
- structured output is sanitized to bounded text and value depth
- browser handoff and token ownership live in the worker, not persisted in the tab

## Artifact verification

Use for: When validating host helper release archives before install or upgrade

- [`Manifest`](../../crates/festerm-host-setup/src/artifacts.rs) — signed metadata for exact package version, platform, and architecture
- [`verify`](../../crates/festerm-host-setup/src/artifacts.rs) — authenticates the manifest, validates the signature, and checks archive bounds

Evidence:
- [`python_produced_six_bundles_and_manifests_authenticate_in_rust`](../../crates/festerm-host-setup/src/artifacts.rs) — proves signed artifacts from repository fixtures verify under the pinned updater key

Caveats:
- manifest signature is over the exact JSON bytes, not reserialized JSON
- archive paths, duplicate entries, symlinks, and size bounds are rejected before extraction
- expected version and platform are caller-controlled trust decisions separate from signed metadata
