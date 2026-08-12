# Configuration Foundation

`festerm-config` is the first M8 configuration slice. It owns versioned,
strict TOML parsing, explicit file I/O, and in-memory transactional reload
state. File watching, configuration-path discovery, workspace persistence,
credential-store references, and GUI/profile-launch integration are
intentionally not part of this slice.

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

## Explicit file I/O

Callers explicitly select a path with `Configuration::load_from_path` and
`Configuration::save_to_path`; the crate does not choose or search for config
locations. Loads read and validate the complete document through
`Configuration`. `ConfigurationState::reload_from_path` has the same
transactional state behavior as `reload`: an unreadable or invalid candidate
does not change the active configuration.

Saves serialize and validate before writing a new file in the target's parent
directory, sync it, then rename it into place. On Unix the parent directory is
also synced after replacement. On Windows, where a rename cannot overwrite an
existing target, the prior target is moved aside and restored if the final
rename fails; no partially written target is exposed. Temporary files are
cleaned up on failures where possible.

`ConfigurationFileError` reports stable, content-free categories for missing
files, reads, parse/validation, serialization, temporary-file writes, and
replacement steps. It deliberately retains neither TOML content nor
caller-supplied paths; parse and validation details are available separately
as the existing content-free `ConfigError`.
