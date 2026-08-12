# Configuration Foundation

`festerm-config` owns versioned, strict TOML parsing, explicit file I/O, and
in-memory transactional reload state. It also owns a metadata-only workspace
persistence model. The `festerm` application discovers and loads one
configuration file during startup, then supplies its immutable profile metadata
to the Launcher. Settings can explicitly save a fresh workspace snapshot while
preserving the manually authored profiles. File watching, profile editing,
credential-store references, and saved SSH autoconnect are intentionally not
part of this slice.

## Startup discovery

At startup, `festerm` uses the native per-user configuration location supplied
by the maintained `directories` crate for the project identity
`com.fes.fesTerm`, with `config.toml` as the final filename. It does not create
the directory or file.

`FESTERM_CONFIG_PATH` is an explicit support and test override. When present,
it must be a non-empty Unicode file path and takes precedence over the native
location. A non-Unicode or unusable override is ignored safely: fesTerm starts
with an empty configuration and reports a content-free Settings diagnostic.
Diagnostics never expose the selected path or source TOML.

A missing configuration file is normal and starts fesTerm with
`Configuration::empty()`. An unreadable or invalid file also leaves the app
running with an empty configuration, while Settings explains the action needed.
Settings offers an explicit **Reload configuration** action that reads the
same location selected during startup. It validates a complete replacement
before applying it. A valid replacement affects only future Launcher choices;
it does not stop or reconfigure existing sessions. A missing file replaces the
active configuration with `Configuration::empty()`. An unreadable, invalid, or
unavailable selected location retains the last known configuration and shows a
content-free actionable diagnostic. There is no file watching, polling,
configuration editing, or automatic persistence.

## Schema version 1

Every document begins with `schema_version = 1`. Unknown fields are rejected.
Profile and workspace tab identifiers are unique within their respective
collections and use one to 64 lowercase ASCII letters, digits, and internal
hyphens.

```toml
schema_version = 1
workspace_enabled = true

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

[workspace]
focused_tab_id = "build-tab"

[[workspace.tabs]]
kind = "launcher"
id = "launcher"

[[workspace.tabs]]
kind = "local_session"
id = "dev-tab"
profile_id = "dev-shell"

[[workspace.tabs]]
kind = "ssh_session"
id = "build-tab"
profile_id = "build-host"

[[workspace.tabs]]
kind = "settings"
id = "settings"
```

Local profiles pass `executable`, `arguments`, and the optional
`working_directory` directly to `festerm_pty::LocalProfile`; they do not
contain environment overrides. SSH profiles convert only host, port, username,
terminal type, and initial dimensions to
`festerm_ssh::SshConnectionProfile`.

SSH defaults are port `22`, terminal type `xterm-256color`, and an initial
size of `80` columns by `24` rows.

## Workspace metadata

`workspace_enabled` defaults to `false`. When it is `false`, `[workspace]`
must be omitted and no workspace is persisted. When it is `true`, a
non-empty `[workspace]` is required. This explicit opt-in makes a profile-only
document deterministic and avoids silently persisting window state.

`workspace.tabs` is the saved display order. Every tab has a stable `id` and
one of these exact strict shapes:

| `kind` | Fields | Meaning |
| --- | --- | --- |
| `launcher` | `id` | The Launcher application surface; no session. |
| `settings` | `id` | The Settings application surface; no session. |
| `local_session` | `id`, `profile_id` | A fresh local session from an existing `local` profile. |
| `ssh_session` | `id`, `profile_id` | An authentication-required surface for an existing `ssh` profile. |

`profile_id` must name an existing profile, and the tab kind must match that
profile's kind. Tab IDs must be unique. If `focused_tab_id` is present, it
must name one saved tab; when it is omitted, restoration deterministically
focuses the first tab in order. A workspace cannot be empty. The application
must replace a final closed tab with a Launcher tab before taking a later
snapshot, consistent with ADR 0014.

This model stores only tab identities, tab order, optional focus, surface
kind, and profile references. It does not store terminal screen content,
scrollback, local processes, SSH channels, remote process memory, transport
attempts, window integration state, authentication, keys, host trust, or
credentials. Ad-hoc sessions and mutable launch definitions have no schema
representation yet: they are omitted rather than serialized. Unknown tab
kinds and fields are rejected.

## Startup workspace restoration

When `workspace_enabled = true`, startup restores `workspace.tabs` in saved
order instead of adding the normal default local-shell tab. Every restored
runtime tab receives a fresh process-local `TabId`; the saved tab `id` is
metadata used only to preserve order and select focus. The saved
`focused_tab_id` selects its fresh tab, or the first saved tab is selected
when focus is omitted.

Launcher and Settings entries restore as application surfaces. A local-session
entry starts a fresh local PTY from its referenced profile, preserving that
profile identifier as the stable label and using the ordinary visible
no-session startup-failure handling if creation fails. It never restores the
old shell process or terminal output.

An SSH-session entry restores as an **SSH authentication required** surface
at its saved position. It retains non-secret destination metadata and
pre-fills the existing transient authentication form, but starts no SSH
connection. The user must explicitly supply fresh password or in-memory
private-key authentication; host trust is requested anew when needed. No
credentials, key material, connection/channel, process state, or host trust
is persisted or recreated.

Settings has an explicit **Save workspace** action. It snapshots only the
restorable metadata described above in current tab order: Launcher, Settings,
authentication-required restored SSH profile surfaces, and sessions launched
from configured local profiles. It omits default/ad-hoc local sessions and
live SSH sessions. If nothing is eligible, it saves one Launcher descriptor.
The saved focus is always a captured descriptor (or the first descriptor when
the active tab was omitted). Workspace descriptor IDs are fresh deterministic
metadata, never runtime `TabId`s.

Saving preserves the existing profile list, enables `workspace_enabled`, and
uses the selected startup configuration source atomically. It is the only
operation that may create a missing native configuration directory; an explicit
`FESTERM_CONFIG_PATH` override never has its parent created. Profile editing
remains manual. fesTerm does not auto-save, watch the file, or persist secure
storage/trust data.

## Secret boundary and reload behavior

Do not place passwords, passphrases, private keys, tokens, key-file paths, or
credential values in this TOML. The parser rejects known secret-bearing field
names, private-key material, and recognizable credential options throughout
profiles and workspaces; unknown fields are also errors. Secure-storage
references are not implemented yet.

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
