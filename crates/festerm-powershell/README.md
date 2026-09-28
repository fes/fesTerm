# festerm-powershell

Bounded native PowerShell/PSRP backend for fesTerm.

Current scope:

- WSMan/WinRM over HTTPS only
- explicit NTLM credential mode only
- explicit endpoint configuration selection; default `Microsoft.PowerShell`,
  with deliberate `PowerShell.7` and custom/JEA names (no silent fallback)
- one persistent runspace pool and one active pipeline at a time
- incremental PSRP stream events with bounded command/event budgets
- non-interactive host behavior only
- explicit private CA trust via PEM when required

Not claimed yet:

- SSH transport
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
