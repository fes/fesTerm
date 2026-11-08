# festerm-powershell

Bounded native PowerShell/PSRP backend for fesTerm.

Current scope:

- WSMan/WinRM over HTTPS and native PowerShell SSH subsystem transports
- explicit NTLM credential mode only
- explicit endpoint configuration selection; default `Microsoft.PowerShell`,
  with deliberate `PowerShell.7` and custom/JEA names (no silent fallback)
- one persistent runspace pool and one active pipeline at a time
- incremental PSRP stream events with bounded command/event budgets
- non-interactive host behavior only
- explicit private CA trust via PEM when required

Not claimed yet:

- opening SSH connections directly from this crate
- CredSSP or delegation
- GUI/sessiond bridge integration
- JEA authorization or arbitrary interactive-host support
- cross-host or corporate interoperability qualification

Selecting a configuration maps to the endpoint's WSMan resource URI
(`http://schemas.microsoft.com/powershell/<name>`) for the whole shell
lifecycle. Name input is bounded and validated, and the resource URI is
XML-escaped by the transport. Selecting a configuration does not grant its
endpoint permissions.

Example:

```sh
cargo +1.98.0 run --manifest-path crates/festerm-powershell/Cargo.toml --example psrp-shell -- \
  [--configuration NAME] <host> <username> [domain] [ca-pem-file]
```

The example prompts for the password with terminal echo disabled and reuses the
same runspace for repeated commands until `exit`.

Noninteractive SSH example:

```sh
PSRP_PASSWORD=... cargo +1.98.0 run --manifest-path crates/festerm-powershell/Cargo.toml --example psrp-ssh-shell -- \
  --fingerprint SHA256:... --password-env PSRP_PASSWORD [--port 22] <host> <username> "'hello from ssh psrp'"
```

The SSH example requires an explicit pinned host-key fingerprint and never
prompts or auto-accepts trust.

## PSRP over SSH transport seam

`PowerShellSshSession::connect_ssh_subsystem` opens the native `powershell`
SSH subsystem through `festerm-ssh`, then runs structured PSRP over that
bounded no-PTY stdio stream. SSH authentication, explicit host-key
trust/pinning, local source binding, channel queues, cancellation and cleanup
remain owned by `festerm-ssh`; this crate owns only PSRP runspace/pipeline
protocol state above the byte stream.

`PowerShellSshSession::connect_stream` remains available for deterministic
wire tests and future already-opened subsystem adapters. It accepts any bounded
`Read + Write + Send` stream that is already connected to a PowerShell SSH
subsystem.

This worktree has deterministic byte-stream/reassembly regressions. End-to-end
native SSH PowerShell support still requires controlled interoperability
evidence against a real PowerShell SSH server.
