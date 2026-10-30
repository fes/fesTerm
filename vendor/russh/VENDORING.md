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

The Dev Tunnels outer relay explicitly uses `Preferred::anonymous_none()`:
KEX/cipher/MAC/compression `none`, with an empty typed key preference as the
no-host-key sentinel. Only that no-KEX/no-cipher preference advertises and
accepts the literal SSH wire host-key name `none`; it cannot be constructed as
an `ssh-key::AlgorithmName`, which requires extension-name syntax. Negotiated
host keys are represented as `Option<Algorithm>` internally, and signed
exchanges require `Some`. All ordinary preferences and the pinned encrypted
inner tunnel remain unchanged. This mode is only for independently
authenticated/encrypted transports (the allowlisted TLS relay); it is not an
SSH fallback. Bridge regressions perform an actual outer no-key handshake and
prove that a normal encrypted client refuses that same peer.
Only the anonymous client/server skip-exchange path with no negotiated host key
omits `NEWKEYS`, matching the upstream SDK's no-key exchange. Existing no-KEX
preferences with a host-key algorithm retain their `NEWKEYS` packet, including
the raw malformed-input regression fixtures. Their connection helper also has
a bounded handshake deadline so negotiation regressions fail rather than hang
the test suite. Signed/encrypted key exchanges still emit and require `NEWKEYS`
unchanged.
The pinned legacy SDK's default MAC list includes `none`; modern ordinary
defaults do not. The explicit anonymous preference advertises only `none`
rather than accidentally omitting the relay's only supported MAC.
