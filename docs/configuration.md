# Configuration Foundation

`festerm-config` owns versioned, strict TOML parsing, explicit file I/O, and
in-memory transactional load state. It also owns a metadata-only workspace
persistence model. The `festerm` application discovers and loads one
configuration file during startup, then supplies its immutable profile metadata
to the Launcher. Workspace state (the open tab list, its order, and the active
tab), the five Settings interface preferences (chip layout, status bar
visibility, session details in chips, live-session close confirmation, and
workspace restoration), known-host trust decisions, and profile CRUD all save
automatically the moment they change - there is no
manual save/reload action anywhere in Settings. File watching (reacting to
edits made outside the running app) is intentionally not part of this slice.

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
running with an empty configuration, while Settings explains the action
needed (restarting fesTerm, e.g. after fixing the file by hand). There is no
in-app "Reload configuration" action: fesTerm does not watch, poll, or re-read
the selected location while running, so external edits only take effect on
the next startup. It also never edits, renames, or repairs a file that is
unavailable or invalid - it starts from safe defaults for that run instead and
leaves the on-disk file untouched.

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
credential_id = "550e8400-e29b-41d4-a716-446655440000"

[profiles.persistence]
provider = "tmux"
session_name = "main"

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

[settings]
chip_layout = "single-row-scroll"
status_bar_visible = true
show_session_details = true
confirm_session_close = true
restore_workspace = false
```

Local profiles pass `executable`, `arguments`, and the optional
`working_directory` directly to `festerm_pty::LocalProfile`; they do not
contain environment overrides. A saved Local profile may add the same optional
`persistence` table shown above to select a local `tmux` or `screen` session.
When present, the provider command replaces the profile executable for that
launch (`tmux new-session -A -s <name>` or `screen -xRR <name>`); the working
directory still applies. The built-in **Local Shell** is not a profile and
never enables persistence.

SSH profiles convert host, port, username, terminal type, and initial
dimensions to `festerm_ssh::SshConnectionProfile`. Their optional
`persistence` table selects the corresponding provider and validated remote
session name. Omitting `persistence` on either profile kind preserves ordinary
plain-shell behavior.

SSH defaults are port `22`, terminal type `xterm-256color`, and an initial
size of `80` columns by `24` rows. Persistent session names are 1-64 bytes and
may contain only ASCII letters, digits, `-`, `_`, or `.`.

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

## Interface settings

The optional `[settings]` table holds five interface preferences:
`chip_layout` (`"wrap"` or `"single-row-scroll"`, default
`"single-row-scroll"`), `status_bar_visible` (default `true`),
`show_session_details` (default `true`), `confirm_session_close` (default
`true`), and `restore_workspace` (default `false`). They mirror the current
Settings controls for chip layout, the bottom status bar, session-detail
visibility, live-session close confirmation, and explicit workspace
restoration.

fesTerm writes the whole configuration document through immediately whenever
one of these controls changes (or after an explicit Settings **Reset interface
settings to defaults** action), using the same selected startup location and
atomic replacement path as workspace and profile saves. The in-memory UI
change always applies immediately regardless of whether the write succeeds; a
failed write only means the change will not survive a restart, and Settings
shows a content-free diagnostic in that case.

`[settings]` is omitted entirely from a saved document while all fields are
at their defaults, so a configuration that has never customized these
preferences serializes and reloads identically to one written before this
table existed. A configuration file without a `[settings]` table parses using
these same defaults.

## Startup workspace restoration


When `workspace_enabled = true`, startup restores `workspace.tabs` in saved
order instead of adding the normal startup Launcher. Every restored
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

Workspace state saves automatically the moment the open tab list, its order,
or the active tab changes - there is no manual "Save workspace" action. Each
save snapshots only the restorable metadata described above in current tab
order: Launcher, Settings, authentication-required restored SSH profile
surfaces, and sessions launched from configured local profiles. It omits
default/ad-hoc local sessions and live SSH sessions. If nothing is eligible, it
saves one Launcher descriptor. The saved focus is always a captured descriptor
(or the first descriptor when the active tab was omitted). Workspace
descriptor IDs are fresh deterministic metadata, never runtime `TabId`s.

Saving preserves the existing profile list, enables `workspace_enabled`, and
uses the selected startup configuration source atomically. It is the only
kind of save that may create a missing native configuration directory; an
explicit `FESTERM_CONFIG_PATH` override never has its parent created. Profile
editing (create, update, delete, reorder) also saves automatically on each
change, the same way. fesTerm still never watches the file or reacts to edits
made outside the running app, and it never persists secure storage/trust
material into this TOML.

## Secret boundary and reload behavior

Do not place passwords, passphrases, private keys, tokens, key-file paths, or
credential values in this TOML. The parser rejects known secret-bearing field
names, private-key material, and recognizable credential options throughout
profiles and workspaces; unknown fields are also errors.

An SSH profile may instead include one optional `credential_id`. In this M8
slice it is **only** a canonical lowercase UUID-v4 opaque reference to a
native stored SSH password produced by `festerm-secret-store`; it does not
identify a private key, passphrase, agent response, key file, trust record, or
arbitrary credential. The parser rejects malformed, noncanonical, or non-v4
references. No other credential field or metadata is allowed in profiles or
workspaces. The SSH transport resolves the opaque reference on its background
worker immediately before password authentication; UI event handling never
retrieves the password.

The Launcher exposes stored-password actions only for an existing saved SSH
profile. A user may explicitly enter a password and select **Remember this
password in native secure storage**; the password is written on a background
worker, then the generated opaque reference is atomically saved into that
profile. If configuration persistence fails, fesTerm removes the newly created
native secret where possible and leaves the old profile reference unchanged.
One-off SSH connections remain transient. Restored workspace SSH surfaces
never auto-connect: users must explicitly choose the stored-password action
or enter a fresh password.

Native storage uses macOS Keychain, Windows Credential Manager, or a Linux
Secret Service provider available through the logged-in session D-Bus. KWallet
is supported only when its Secret Service integration is active. Locked or
unavailable storage must produce an actionable error; fesTerm does not fall
back to a file, keyutils, plaintext, or in-memory storage.

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
