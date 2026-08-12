# Configuration Foundation

`festerm-config` is the first M8 configuration slice. It owns versioned,
strict TOML parsing and in-memory transactional reload state. File watching,
workspace persistence, credential-store references, and GUI/profile-launch
integration are intentionally not part of this slice.

## Schema version 1

Every document begins with `schema_version = 1`. Unknown fields are rejected.
Profile identifiers are unique and use one to 64 lowercase ASCII letters,
digits, and internal hyphens.

```toml
schema_version = 1

[[profiles]]
kind = "local"
id = "dev-shell"
executable = "/bin/zsh"
arguments = ["-l"]
working_directory = "/work/project"

[[profiles]]
kind = "ssh"
id = "build-host"
host = "build.example"
port = 2200
username = "alice"
terminal_type = "xterm-256color"
initial_columns = 132
initial_rows = 43
```

Local profiles pass `executable`, `arguments`, and the optional
`working_directory` directly to `festerm_pty::LocalProfile`; they do not
contain environment overrides. SSH profiles convert only host, port, username,
terminal type, and initial dimensions to
`festerm_ssh::SshConnectionProfile`.

SSH defaults are port `22`, terminal type `xterm-256color`, and an initial
size of `80` columns by `24` rows.

## Secret boundary and reload behavior

Do not place passwords, passphrases, private keys, tokens, key-file paths, or
credential values in this TOML. The parser rejects known secret-bearing field
names, private-key material, and recognizable credential options; unknown
fields are also errors. Secure-storage references are not implemented yet.

`ConfigurationState::reload` parses and validates a complete replacement
before changing active state. A rejected candidate leaves the previous valid
configuration active and records a content-free diagnostic. A successful
replacement atomically replaces active state and clears that diagnostic.
