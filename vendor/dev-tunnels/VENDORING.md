# Pinned Dev Tunnels SDK subset

- Upstream: https://github.com/microsoft/dev-tunnels
- Revision: `b73b0eb774b86a5b519c300f2f41c9072a66f663`
- Source: `rs/`, plus upstream `LICENSE` (MIT, retained).
- Experimental consumers: desktop bridge (`relay`) and portable native
  identity/discovery client (management/contracts only).

Local modifications:

- The legacy `relay_tunnel_host`/`relay_tunnel_client` modules are not compiled.
  Their unbounded queues, per-client spawning and old OpenSSL russh dependency
  are not part of this product's relay path.
- `connections` exports only the WebSocket adapter/helpers and errors; it uses
  modern vendored russh and rustls. The product uses only direct connections.
- Direct WebSockets cap message/frame size at 1 MiB and write buffer at 2 MiB.
- Direct/proxied TLS WebSockets select aws-lc explicitly per connector with
  native roots and normal certificate/hostname verification, without setting
  a process-wide crypto provider. The full desktop app also enables ring;
  implicit Rustls provider selection would otherwise panic on first relay use.
- The management JSON success path reads at most 64 KiB; errors omit body
  content on the endpoint-registration path. The caller disables redirects
  and sets a 20-second HTTP timeout.
- Endpoint registration and contract types remain the official SDK APIs.
  Ports are preprovisioned. Runtime never calls create/delete-port APIs.
- Management Authorization headers are marked sensitive for request Debug
  redaction; the first-party WebSocket caller does the same.
- Native discovery uses official GitHub authorization and connect-only token
  scopes. Retained authorization, header temporaries and bounded JSON bodies
  are wiped on drop; raw service errors never reach product diagnostics.
- User-Agent uses compile-time OS/architecture only, not OS inspection helpers,
  subprocesses or Windows registry/partner identifiers. `os_info` is removed
  so the native client has no desktop-helper dependency for this purpose.
- The bounded modern-russh relay handshake is in the first-party bridge crate,
  derived from the upstream MIT implementation. One host/client/port scope,
  pinned host binding and bounded queues replace generic forwarding.

Keep the upstream license/headers and these modifications on updates. Re-run
bridge protocol, real in-memory encrypted transport, lifecycle/adoption,
host-key denial, saturation and independent security review gates.
