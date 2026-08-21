# ADR 0013: `russh` for Native SSH Transport

- **Status:** Accepted
- **Date:** 2026-08-07
- **Refined by:** ADR-0018 (SSH Liveness, Reconnect, and Persistent Session
  Recovery) restates reconnect as a policy layered on top of the mechanism
  decided here: for a plain SSH session, only the manual/user-initiated
  reconnect path described below is enabled by default, and an automatic
  bounded-backoff policy is not currently constructible for it. The `russh`
  transport selection, bounded-backoff mechanism, and host-trust decisions in
  this ADR remain in force.

## Context

M7 requires a cross-platform, in-process SSH session with host-key
verification, interactive PTY allocation and resize, multiple authentication
methods, application-owned reconnect behavior, and controlled OpenSSH
interoperability tests. The existing `festerm-session` contract already
separates bounded byte/lifecycle transport from terminal-core mutation.

The SSH implementation must not execute a system `ssh` binary, depend on a
developer's SSH configuration, or silently weaken host-key or cryptographic
policy.

## Decision

fesTerm will use `russh` with its supported `ring` crypto backend as its
Tokio-native SSH client in a new `festerm-ssh` transport crate. Do not enable
its insecure legacy `des` or `dsa` features. The default `aws-lc-rs` backend
is not selected because its Windows build requires NASM, an unnecessary native
toolchain prerequisite for fesTerm.

`festerm-ssh` will emit the existing bounded `SessionEvent` values and accept
the same input/resize operations as local sessions. It will not mutate
terminal-core state directly.

Host trust is application policy, not a library default:

- unknown or changed host keys require an explicit application decision;
- malformed trust data, persistence failures, cancellation, and prompt
  timeouts reject the connection;
- the user-facing decision uses a canonical host-and-port identity and a
  SHA-256 fingerprint; and
- private keys, passwords, agent responses, and host-key material are never
  included in diagnostics.

M7 initially supports a documented safe OpenSSH-config import subset:
`Host`, `HostName`, `Port`, `User`, and `IdentityFile`. Unsupported,
ambiguous, or process-spawning directives such as `ProxyCommand` are reported
rather than applied.

Reconnect is owned by the application state machine. It uses bounded backoff,
is cancellable, repeats host-key verification, and creates a new connection
and remote PTY; it does not claim to restore an ordinary remote shell process.

## Consequences

- The application needs an asynchronous prompt bridge for host-key and
  authentication decisions; it must never block the GUI thread.
- Public-key and password authentication are M7 baseline paths. Agent support
  requires deliberate platform adapters and tests; Windows OpenSSH Agent must
  not be assumed equivalent to Pageant.
- Test layers include fake transport/state-machine tests and a pinned,
  repository-owned OpenSSH container fixture. They must own keys, users,
  ports, and lifecycle.
- `ssh2`/libssh2 and wrappers are not selected because their synchronous,
  native-FFI model is a weaker fit for the bounded asynchronous session pump.
- User-visible profiles and persistent trust storage remain coordinated with
  M8 configuration and secure-storage work; M7 defines their transport
  boundary without storing secrets in TOML.
