# ADR 0073: Persistent MCP Client Identity and Capability Grants

- **Status:** Proposed
- **Date:** 2026-11-05
- **Supersedes:** None; extends ADR 0065

## Context

The desktop MCP beta matches a per-connection verification code, and its broker
grants are bound to an ephemeral connection principal. Grants expire within
five minutes and controller input is reviewed in Settings for each requested
action. This is unsuitable for a continuously connected hardware controller:
read-only status should remain current, and a user-approved controller action
set should not require a separate Settings approval on every button press.

An MCP client name, `clientInfo`, requested permission list, terminal label,
registration ID, or claim that a hardware gesture occurred is not proof of
client identity or user consent. Persistent grants must not silently follow a
closed or replaced terminal. Terminal input remains the target account's
execution authority, even when its encoded action is a short command.

## Decision

1. Define a versioned, host-owned capability catalog. MCP clients may request
   catalog entries before use; they cannot define permission semantics or
   assert approval. Unknown, unsupported, or newly introduced entries are
   denied until native review. A requested list is a review default, never
   authority.
2. Present requested actions by default in native Settings. An Advanced view
   lets the user disable individual requested actions and review their
   permission groups. Show the client identity, exact target, effect, data
   disclosure, lifetime and consequences. Do not silently add an action
   because a later request uses it.
3. Keep independent grants for read-only session metadata/status, terminal
   contents, terminal input, presentation/focus and remote file observation.
   Status grants expose no screen, scrollback, selection or input. In
   particular, changing the control grant must not interrupt an independently
   authorized status grant.
4. Status authorization may be **until revoked**. Control authorization offers
   a 15-minute or **until revoked** choice. A control grant covers only the
   user-approved actions and exact target binding; the latter choice removes
   only its timer, not its revocation conditions. It does not make an arbitrary
   client-provided gesture claim authoritative. An action outside the approved
   set requires a new native review.
5. Durable grants require a stable, authenticated client identity established
   by proof of possession of a client-owned Ed25519 key through a versioned
   authentication extension. The local adapter sends a fresh 32-byte
   challenge; the client signs a domain-separated versioned message. The
   native pairing flow approves the key; the server stores its public identity
   and permission policy, not the private key. `--mcp-keygen` creates a seed
   for the MCP host to provide to `--mcp` through `FESTERM_MCP_CLIENT_KEY`;
   the secret is not accepted in argv. `clientInfo`, display names,
   command-line labels and client-provided approval flags never identify a
   principal. Missing authentication retains only connection-scoped behavior;
   malformed or mismatched proofs fail closed.
6. Reconnect authenticates the same client and can restore its approved policy,
   but never restores pending requests, queued effects, writer leases, or
   execution outcomes. Runtime grants are reconstructed only after current
   privacy and backend checks. A closed, replaced or otherwise new target
   generation is unavailable until the user explicitly binds it again; labels,
   positions and matching host names cannot retarget a saved grant.
7. Manual revocation, key rotation/revocation, privacy loss, app shutdown,
   target replacement and the existing human-input/IME safety cuts remain
   effective regardless of the selected lifetime. A cut never replays input.
   Until-revoked authorization is durable policy, not a promise that a session
   or active writer survives shutdown or a lifecycle cut.

The authentication handshake now has a versioned wire shape and deterministic
cryptographic tests. The native identity registry, durable grant restoration
and platform qualification remain implementation gates. No production
permission feature is enabled by this proposal.

## Alternatives considered

- Reuse `clientInfo` or a caller-supplied stable ID: spoofable and not an
  authenticated principal.
- Persist a grant by connection ID, terminal label or tab position: these are
  ephemeral or retargetable and cannot safely identify a client or target.
- Keep approving each controller input in Settings: preserves the beta's
  friction and prevents a practical hardware-controller workflow.
- Treat a claimed device gesture as consent: the client can fabricate the
  claim, and the MCP host cannot verify the physical device.
- Persist the active lease or queued input: would restore stale authority and
  permit delayed effects after a disconnect or restart.
- Give all MCP clients an indefinite, broad grant: exceeds the requested
  capability set and would turn status authorization into terminal execution.

## Consequences

MCP permissions become a reusable, host-enforced capability system rather than
a controller-specific exception. The client must authenticate and declare the
capability set; native Settings remains the authority and can narrow the
request. Read-only status can remain available without periodic reapproval,
while input remains a separate explicit execution permission.

The existing desktop beta remains unchanged until implementation and review.
The first implementation stage authenticates and displays the key fingerprint,
but does not trust or persist the key and does not restore authority. The
authentication extension must be usable by the Rust controller without
JavaScript and must fail closed for clients that claim a proof but cannot
verify. Persistence does not imply persistence of a PTY, session, target
generation, presentation, lease or pending action. Cross-platform native
privacy, pairing and revocation evidence remain required before claiming
production support.

## Validation impact

- **Invariants introduced or changed:** Add authenticated durable MCP client
  identity and host-defined capability grants; preserve exact-target
  generation binding, least privilege, independent metadata/control scopes,
  default-off operation, privacy revocation, no retargeting and no replay.
- **GUI/action edges affected:** `AUT-03`, `AUT-04`, `AUT-07`, `AUT-08`; add a
  dedicated permission-review edge when implementation is accepted.
- **Automated tests required:** Authenticated reconnect restores only reviewed
  policy; untrusted client names/claims and unknown capabilities refuse;
  Advanced selections narrow authority; status remains independent of control;
  target replacement requires rebind; revocation, privacy loss and disconnect
  cut runtime authority and queued delivery; 15-minute expiry and until-revoked
  policy obey distinct lifecycle rules.
- **Native/manual evidence required:** Extend desktop MCP checks for key
  pairing, requested/default and Advanced permission review, reconnect,
  revocation, lock/sleep and target replacement on each supported platform.
  Until those checks pass, durable permissions and controller input remain
  unqualified.
- **Coverage superseded:** None.
