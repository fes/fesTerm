# Configuration Foundation

`festerm-config` owns versioned, strict TOML parsing, explicit file I/O, and
in-memory transactional load state. It also owns a metadata-only workspace
persistence model. The `festerm` application discovers and loads one
configuration file during startup, then supplies its immutable profile metadata
to the Launcher. Workspace state (the open tab list, its order, and the active
tab), the current Settings interface preferences, known-host trust decisions,
and profile CRUD all save automatically the moment they change - there is no
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
credential_kind = "password"

[profiles.persistence]
provider = "tmux"
session_name = "main"

[[known_hosts]]
host = "build.example"
port = 2200
sha256_fingerprint = "SHA256:AAAAB3NzaC1yc2EAAAADAQABAAAB..."

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
terminal_font = "jetbrains-mono"
terminal_ligatures = false
emoji_presentation = "color"
```

Local profiles pass `executable`, `arguments`, and the optional
`working_directory` directly to `festerm_pty::LocalProfile`; they do not
contain environment overrides. A saved Local profile may add the same optional
`persistence` table shown above to select one of three local durable-session
providers: `festerm-sessiond`, `tmux`, or `screen`. For
`festerm-sessiond`, the saved executable/arguments/working directory remain
the launched shell inside the detached daemon and the session later
reattaches by name. For `tmux` and `screen`, the provider command replaces
the profile executable for that launch: tmux attaches or creates the named
session, hides its status bar, and uses the latest attached client for window
sizing; GNU Screen starts with `-c /dev/null` so user screenrc
captions/hardstatus do not wrap the shell. The working directory still
applies in every case. The built-in **Local Shell** is not a profile and
never enables persistence by itself.

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
| `local_session` | `id`, `profile_id` | A session launched from an existing `local` profile; ordinary profiles start fresh while durable providers may attach or create by name. |
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

The optional `[settings]` table currently holds thirteen interface preferences:
`chip_layout` (`"wrap"` or `"single-row-scroll"`, default
`"single-row-scroll"`), `status_bar_visible` (default `true`),
`show_session_details` (default `true`), `confirm_session_close` (default
`true`), `restore_workspace` (default `false`), `terminal_font`
(`"jetbrains-mono"`, `"iosevka-term"`, `"julia-mono"`, or `"maple-mono"`;
default `"jetbrains-mono"`), `terminal_ligatures` (default `false`),
`emoji_presentation` (`"color"` or `"monochrome"`; default `"color"`),
`scroll_speed` (`"very-slow"`, `"slow"`, `"normal"`, `"fast"`, or
`"very-fast"`; default `"normal"`), `quick_switch_overlay` (default `false`),
`compact_launcher_grid` (default `false`), `pulse_new_output_dot`
(default `false`), and `show_resumable_sessions` (default `false`). They
mirror the current Settings controls for chip layout, chip details, the
status bar, live-session close confirmation, workspace restoration, compact
Launcher layout, background-output chip pulsing, resumable local-session
surfacing, terminal-only typography, keyboard quick-switch overlays, and
scrollback scroll speed. Font choice never changes application chrome.
Enabling ligatures shapes only eligible adjacent ASCII cells; the terminal
grid remains authoritative for cursor, selection, mouse, and resize geometry.
Emoji presentation switches only between the bundled color renderer and owned
monochrome fallback; it never changes cell allocation or discovers arbitrary
system/user font files.

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
entry launches from its referenced profile, preserving that profile identifier
as the stable label and using the ordinary visible no-session
startup-failure handling if creation fails. Ordinary local profiles start a
fresh PTY. A saved Local profile using `festerm-sessiond`, `tmux`, or
`screen` follows that provider's usual attach-or-create semantics instead, so
the restored tab may reattach to an existing durable session rather than
always creating a new shell.

An SSH-session entry restores as an **SSH authentication required** surface
at its saved position. It retains non-secret destination metadata and
pre-fills the existing transient authentication form, but starts no SSH
connection. The user must explicitly supply fresh authentication — a typed
password/key, or the profile's stored credential if it has one. When the
connection actually starts, host-key verification runs again as it would for
any other launch: a matching persisted `known_hosts` entry (above) is
accepted silently, otherwise the user is prompted anew. No live
connection/channel or process state is persisted or recreated by workspace
restoration itself.

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
made outside the running app, and it never persists secret material (only
opaque secret-store references) into this TOML; non-secret host-key trust
records are the one exception stored directly, per the "Persistent host-key
trust" section below.

## Secret boundary and reload behavior

Do not place passwords, passphrases, private keys, tokens, key-file paths, or
credential values in this TOML. The parser rejects known secret-bearing field
names, private-key material, and recognizable credential options throughout
profiles and workspaces; unknown fields are also errors.

An SSH profile may instead include one optional `credential_id` plus an
optional `credential_kind` (defaulting to `"password"` when omitted, the only
other value being `"private_key"`). `credential_id` is always a canonical
lowercase UUID-v4 opaque reference produced by `festerm-secret-store`
([ADR 0016](adr/0016-native-secret-store-boundary.md)); `credential_kind`
says only which native-stored secret shape it names — an SSH password, or an
OpenSSH private key plus its optional passphrase packed together
([ADR 0024](adr/0024-native-secret-store-stored-private-keys.md)). Neither
field ever contains a password, key, or passphrase directly. The parser
rejects malformed, noncanonical, or non-v4 references. No other credential
field or metadata is allowed in profiles or workspaces. The SSH transport
resolves the opaque reference on its background worker immediately before
authentication; UI event handling never retrieves the secret.

The Launcher/profile editor exposes stored-credential actions only for an
existing saved SSH profile. A user may explicitly enter a password or an
OpenSSH private key (with optional passphrase) and select **Remember this
password/private key in native secure storage**; the secret is written on a
background worker, then the generated opaque reference and its
`credential_kind` are atomically saved into that profile. If configuration
persistence fails, fesTerm removes the newly created
native secret where possible and leaves the old profile reference unchanged.
One-off SSH connections remain transient. Restored workspace SSH surfaces
never auto-connect: users must explicitly choose the stored-credential action
or enter fresh authentication.

Native storage uses macOS Keychain, Windows Credential Manager, or a Linux
Secret Service provider available through the logged-in session D-Bus. KWallet
is supported only when its Secret Service integration is active. Locked or
unavailable storage must produce an actionable error; fesTerm does not fall
back to a file, keyutils, plaintext, or in-memory storage.

## Persistent host-key trust

Host public keys and their SHA-256 fingerprints are not secret, so they use
ordinary configuration rather than `festerm-secret-store`
([ADR 0020](adr/0020-persistent-host-key-trust.md)). The document may include
a top-level `known_hosts` array of `{ host, port, sha256_fingerprint }`
entries, at most one per `host:port`. Accepting and remembering a host's key
(or accepting a deliberate, explicitly typed key change) upserts that host's
entry; there is no automatic merge. A future connection to a `host:port` with
a persisted, matching fingerprint is accepted without prompting; any other
presented key still prompts, flagged as a changed-key warning when a record
already exists. No UI currently exposes revoking a `known_hosts` entry other
than accepting a legitimate key change; removing one otherwise requires
editing this TOML directly.

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
