## Vendored russh

- Upstream crate: `russh` 0.62.5
- Upstream commit: `4882af71cf27ea5293636bf4985ef296dcf20896`
- crates.io archive SHA-256:
  `da7c230e0ed9cbeb92fbad6c8848985d6df2a1464c0dc247a021abd666e9005e`
- License: Apache-2.0

Local changes keep password and passphrase owners zeroizing through the
authentication queue, wipe plaintext packet assembly on flush and drop, redact
authentication/session debug output, and replace packet-content logging with
type/name/length metadata. Local warning accommodations mark
`MAXIMUM_DECOMPRESSED_PACKET_LEN` as dead-code-tolerant for feature combinations
that do not consume it and explicitly consume the PKCS#5 `sec` value when RSA
support is disabled. `LICENSE-APACHE` was added from the canonical Apache-2.0
text because the published crate declares that license but does not include a
license file. Two trailing spaces in the upstream README were removed to
satisfy the repository diff policy.

Keep these changes when updating the vendored crate and rerun the fesTerm SSH,
credential-lifecycle, security, vendored-russh, and workspace Clippy gates.

The experimental Dev Tunnels adapter additionally opts into
`client::Config::maximum_pending_replies = Some(16)`. Its synchronous
authentication/control-reply queue fails closed on overflow instead of allowing
an unsolicited-reply flood to allocate without bound. The default remains
`None`, preserving ordinary SSH clients. Channel-open failures now propagate a
failed reply send instead of ignoring it. The bounded queue has a focused
regression in the client tests.
