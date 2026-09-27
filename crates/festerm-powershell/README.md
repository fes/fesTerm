# festerm-powershell

Bounded native PowerShell/PSRP backend for fesTerm.

Current scope:

- WSMan/WinRM over HTTPS only
- explicit NTLM credential mode only
- one persistent runspace pool and one active pipeline at a time
- incremental PSRP stream events with bounded command/event budgets
- non-interactive host behavior only
- explicit private CA trust via PEM when required

Not claimed yet:

- SSH transport
- CredSSP or delegation
- GUI/sessiond bridge integration
- live interoperability evidence beyond repository-owned tests

Example:

```sh
cargo +1.98.0 run --manifest-path crates/festerm-powershell/Cargo.toml --example psrp-shell -- <host> <username> [domain] [ca-pem-file]
```

The example prompts for the password with terminal echo disabled and reuses the
same runspace for repeated commands until `exit`.
