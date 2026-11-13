# Configuration Foundation

ADR 0084's headless host settings are **host-local**, not portable profile TOML.
See [headless lifecycle configuration](headless-host.md): `festerm-host configure`
saves disabled intent with creation denied and manual startup by default. It
does not enable access, register startup or create a shell. Existing desktop and
headless origin journals cannot adopt each other.

The off-default [native service component](native-host-services.md) keeps
exact resource/executable/creation-epoch receipts in protected host-local state,
not TOML. It accepts approved journal settings and a sealed authenticated
executable receipt; a configured path is not launch authority. Manual/sign-in
registration does not qualify pre-login availability or endpoint readiness.
macOS executable promotion requires the pinned release Developer ID team and
hardened runtime; Windows promotion refuses pending publisher policy.
Windows registration/start and running-task stop also refuse
pending authenticated graceful-stop support that preserves independent daemons.

Owner enrollment is separately protected host-local policy
([authentication API and blockers](host-protocol.md#enrollment-and-secret-boundary-transport-integration-still-gated)).
Public-key import requires exact fingerprint confirmation; it never grants
remote access or creation. Revocation revisions and the selected policy anchor
cannot be reconstructed from TOML/startup metadata. Client owner seeds and
approved bindings use native-vault opaque references by default. Deliberate
headless-only protected-file storage records origin/installation/storage consent
separately from portable configuration; it is not a GUI option or automatic
native-vault-failure fallback. Whole private-store/home rollback is not resisted
in that weaker mode. No seed belongs in profiles, recipes or connection JSON.

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

If `Configuration::save_to_path` returns an error during an automatic save,
the application cannot determine whether the candidate reached the selected
file. It disables
further in-process saves, retains potentially referenced resources from that
attempt, and warns all live windows. A subsequent save is rejected before the
write and reported separately; candidate credentials and tunnel destinations
whose exclusive ownership is not established are retained for manual
reconciliation. Missing configuration paths, failure to create a parent
directory, session-name validation failures, and busy refusals occur before that
call and do not themselves latch the gate. Validation errors returned by
`save_to_path` are still indeterminate for this app-side policy. A busy request
may be retried after the other
save completes only if the gate is not latched. Reconcile
the configuration and affected resources manually, then restart the entire
application; an individual window cannot clear the latch. A poisoned save gate
also requires reconciliation and restart. Windows opened later consult the
shared gate and receive the unresolved warning; while a save is in progress
they receive a provisional, conditional notice instead. Blocked-resource and
failed destination-cleanup warnings are broadcast even if the initiating
window closes; inspect references before removing any retained destination.
Repeated warnings renew their visibility period without duplicating text.

This is an incomplete in-memory safety layer, not a proof of filesystem
installation, path/alias safety, durable recovery, or safe retirement of old
credentials. It does not coordinate external writers or stale documents and
live preferences across windows. Restart clears the latch without proving that
manual reconciliation occurred.

## Schema version 1

Every document begins with `schema_version = 1`. Unknown fields are rejected.
Profile and workspace tab identifiers are unique within their respective
collections and use one to 64 lowercase ASCII letters, digits, and internal
hyphens.

### Public host recipes (ADR 0084 foundation)

Issue [#681](https://github.com/fes/fesTerm/issues/681) adds an **offline
metadata foundation**, not the stage 6 desktop/mobile connection UI.
`festerm-client-recipes` owns the same validated public model and commands for
desktop, iOS and Android without importing desktop PTY/configuration code.
Desktop stores a separate `host_recipes` collection in schema version 1;
mobile stores the same collection in its version 1 `profiles.json`.
Documents without it remain unchanged and omit the empty collection on save.

```toml
[[host_recipes]]
id = "550e8400-e29b-41d4-a716-446655440000"
name = "Project shell"
logical_session = "550e8400-e29b-41d4-a716-446655440001"

[host_recipes.host]
id = "550e8400-e29b-41d4-a716-446655440002"

[host_recipes.host.account]
provider = "github"
account_id = 42

[host_recipes.launch]
shell = "/bin/zsh"
args = ["-l"]
cwd = "/remote/project"

[host_recipes.lifetime]
kind = "persistent"

[host_recipes.behavior]
kind = "resume_or_create"
```

The recipe ID is client-local. Host and logical-session IDs use the shared
host protocol's canonical UUIDv4 identities; neither is an authorization
credential. The numeric service-account reference is a public lookup hint,
**not a verified principal or account binding**. Login, token, host grants,
consent and trust decisions are not part of this model. Existing device-local
account/key stores retain only opaque vault references in configuration;
public recipes do not copy even those references.

The behaviors are `choose_session`, `resume_or_create`, `resume_only`,
`always_create` and `attach_only`. `resume_only` preserves the saved logical
identity across authorized generations but never requests creation. New
persistent recipes and omitted new-recipe behavior
default to `resume_or_create`. Attach-only also requires `generation = "123"`
(a canonical nonzero decimal generation), never a PID or friendly name.
Temporary lifetime is explicit (`kind = "temporary"`,
`reconnect_grace_seconds = 60`) and inherits the shared protocol's current
validation, not a native lifecycle guarantee. Shell/argv/cwd are bounded public
metadata with no local shell expansion, filesystem lookup or path rewriting.
Do not put secrets in argv; obvious secret-bearing options/private-key material
are refused, but arbitrary user text cannot be automatically classified.

`MetadataCommand::{Save, Edit, Duplicate, Import, Export, Delete}` returns a
validated replacement and typed outcome. Desktop's `AppCommand::HostRecipe`
persists through the existing save gate before publishing success; mobile's
`ProfileStore::apply_host_recipe_command` uses its existing atomic writer.
Export is read-only. Failed saves do not publish candidate state. These
commands cannot sign in, discover, connect, create/end a shell or install a
host. Deleting a recipe removes only metadata; editing launch/lifetime/behavior
affects future actions and never mutates an attached session's snapshot.
Host/logical identity edits require a new recipe rather than silently
retargeting an existing one.

Adapters preserve content-free `RecipeError` categories for invalid metadata,
identifier conflicts, missing recipes, unsupported portable versions, oversize
imports and the 200-recipe limit. Export success/failure is reported separately
from saving; export never reports a configuration save. Mobile loading recovers
entries individually with the shared validation, uniqueness and limit rules:
valid entries survive an unrelated later save, rejected entries are counted in
`LoadReport::dropped_entries`, and the existing quarantine/report mechanism
preserves the rejected input rather than silently defaulting the whole book.

Default duplication creates both a fresh recipe ID and logical-session ID.
Duplicating an attach-only recipe as a separate shell drops its old generation
pin and selects resume-or-create. Deliberate `ExistingSession { generation }`
duplication keeps the logical target but makes the copy strict attach-only.
Portable JSON uses `festerm_host_recipe_version = 1`, has a 64 KiB bound and
rejects unknown/protected fields. Import preserves the advertised identities;
an existing recipe ID is a conflict, not a silent edit or new shell. Account
and host trust must be revalidated independently before any future connection.

**Import is not execution consent.** An imported recipe can choose arbitrary
shell/argv/cwd and resume/create behavior. Before its first execution, the
future Connect UI must present that launch definition, lifetime and behavior
for explicit review/reconfirmation, bound to verified host/account/principal
identity. A portable document cannot supply that approval. This gate remains
required for later stage 6 wiring; there is no reachable executor in this
foundation.

**Legacy is not migrated implicitly.** Existing `kind = "dev_tunnel"` profiles
and public setup imports retain their exact-generation destination, settings
and launch path. They do not acquire a host recipe, remote-create permission,
or local hosting exposure. Legacy automatic discovery preferences remain
legacy-only until an explicit, reviewed migration exists.

That future migration must preserve protected state and account binding. It
must not stop a live legacy shell without a **separate, default-No confirmation
bound to that exact generation**. Host setup or sharing consent is not restart
consent. If the operation cannot proceed safely, retain the live legacy
session and defer migration manually. Recipe metadata commands are not a
migration/restart command and never collect or consume such confirmation.
The metadata-preservation tests do not qualify an actual migration dialog or
live-shell stop; that evidence remains deferred under #681.

`HostBrowser` is a request/state seam, not a worker: opening/rendering local
rows and selecting an account produces no request or online status. Only
explicit Find hosts, single-host inspection and Refresh produce scoped intent;
there is no timer, fan-out or availability cache. Request identity and account/
host epochs fence cancellation and stale completions.

Wiring requires a reviewed encrypted/pinned endpoint, enrolled-key verified
principal and account binding, bounded authorized inventory and authoritative
absence/atomic ensure, exact-generation attach, idempotent reconciliation and
cancellation. Always-create must allocate a fresh logical target per explicit
Connect; resume-or-create must never interpret timeout/denial/revocation as
absence. No Connect control or fallback to legacy is provided by this slice.
See ADR 0084 and `HOST-01`/`HOST-02`; native/manual evidence remains required.

### Existing profiles and workspace

On `feat/powershell`, experimental native PSRP profiles use `kind =
"power_shell"` (distinct from a local `pwsh` process). They store metadata
only; the optional credential reference names a password in native secure
storage:

```toml
[[profiles]]
kind = "power_shell"
id = "remote-ps"
host = "win.example.test"
port = 5986
username = "alice"
domain = "CONTOSO"
configuration_name = "Microsoft.PowerShell"
trusted_ca_file = "/path/to/lab-ca.pem"
local_bind_policy = "ask"
```

`domain` and `trusted_ca_file` are optional. Omitting `local_bind_policy`
selects Automatic; a fixed source uses `[profiles.local_bind_policy]` with
`address = "127.0.0.1"`. `credential_id` is an optional opaque UUID, never a
password. An optional workspace tab uses `kind = "power_shell_session"` and
`profile_id = "remote-ps"`; it restores disconnected setup only. Neither
commands, results, runspace state nor a previously prompted source address
are persisted. Configuration names select registered endpoints exactly;
choosing `PowerShell.7` does not install or authorize that endpoint.

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

[[profiles]]
kind = "ssh"
id = "release-files"
profile_kind = "sftp"
host = "files.example"
port = 22
username = "alice"
sftp_gui_mode = false

[[accounts]]
id = "github-alice"
provider = "github"
account_id = 4242
login = "alice"
credential_id = "550e8400-e29b-41d4-a716-446655440001"

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
show_session_details = false
confirm_session_close = false
prefer_powershell = true
customize_local_shell = false
restore_workspace = true
terminal_font = "jet-brains-mono"
terminal_ligatures = true
emoji_presentation = "color"
scrollback_limit = "64-mib"
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

Saved SFTP destinations reuse the SSH transport shape with
`profile_kind = "sftp"`. They appear as reusable SFTP profiles rather than
shell profiles. `sftp_gui_mode` defaults to `true`; set it to `false` to open
the terminal SFTP command surface instead. SFTP profiles cannot configure
durable shell sessions or SSH port forwards.

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
| `local_session` | `id`, `profile_id`, optional `alias` | A saved frontend tab launched from an existing `local` profile, including native sessiond, tmux and Screen profiles. Its alias survives recreation of the backend. |
| `ssh_session` | `id`, `profile_id`, optional `alias` | An authentication-required shell surface for an existing SSH profile, including tmux and Screen. |
| `sftp_session` | `id`, `profile_id`, optional `alias` | An authentication-required terminal SFTP surface for an existing SSH or SFTP profile. |
| `sftp_file_manager` | `id`, `profile_id` | An authentication-required graphical SFTP surface for an existing SSH or SFTP profile. |
| `serial_session` | `id`, `profile_id`, optional `alias` | A serial terminal recreated from an existing serial profile. |

`profile_id` must name an existing profile, and the tab kind must match that
profile's kind. Tab IDs must be unique. If `focused_tab_id` is present, it
must name one saved tab; when it is omitted, restoration deterministically
focuses the first tab in order. A workspace cannot be empty. The application
must replace a final closed tab with a Launcher tab before taking a later
snapshot, consistent with ADR 0014.

This model stores only tab identities, tab order, optional focus, surface
kind, profile references, and explicit instance display aliases. It does not store terminal screen content,
scrollback, local processes, SSH channels, remote process memory, transport
attempts, window integration state, authentication, keys, host trust, or
credentials. Ad-hoc sessions and mutable launch definitions have no schema
representation yet: they are omitted rather than serialized. Unknown tab
kinds and fields are rejected.

### Session display aliases

`alias` belongs to the saved logical frontend tab, not its backend's physical
session, profile definition or launch fields. It survives backend recreation
for all restorable terminal kinds, including local/SSH tmux and Screen.
Newly opened attachments do not inherit another view's alias merely because
they use the same profile or provider target. It is omitted for defaults.
**Use default name** removes the current view's override rather than saving
today's default as a fixed name. Serialized aliases must be nonempty,
trimmed, single-line, control/bidi-direction-character-free and at most 200
Unicode scalar values (at most 800 UTF-8 bytes); invalid metadata is rejected, not repaired on
load. User edits are sanitized to those bounds before display and save.
Empty or sanitized-empty edits cancel without changing the prior name.
A nonempty rejected edit produces a bounded, content-free app notice and
leaves the prior name, configuration and native seed unchanged; it does not
attempt a save. The existing rejection of secret-bearing values is unchanged.

The optional top-level `[[durable_session_aliases]]` array is independent of
`workspace_enabled` and **Restore workspace**. It is a seed for newly opened
native `festerm-sessiond` views only, not a backend-global naming authority over
existing tabs. An entry contains `alias` and an `identity`
table with `provider = "festerm_sessiond"`, `name`, nonzero `pid`,
`created_at_unix_ms` (canonical positive decimal string preserving the daemon's
u128 value), and `endpoint`. The endpoint must be bounded to 4096 bytes and end
in that exact PID/generation (optionally `.sock`). It is a local provider
namespace/key, never an initial directory, executable or attach instruction.
Legacy reusable-name-only endpoints cannot safely retain durable aliases.

Matching compares the whole identity from the actual successful connection.
It does not inspect terminal output, infer identity from profile/display names,
or change provider attachment semantics. A fresh native Running Sessions view
may use this seed after its existing attach-only checks succeed. A new generation
does not inherit the old seed. A saved or already-open logical view keeps its own
alias across reconnect/replacement; native registry updates never override it.
Explicit native edits update the exact verified seed; the latest explicit edit
wins for future attachments only. Reset clears the current view and its associated
native seed, including a known prior seed after reconnect. Other current views'
aliases remain theirs. A saved workspace alias, including no override, is
authoritative for that saved tab rather than being replaced by the native seed.

At most 1024 distinct durable aliases are retained. Reset removes an entry.
Transient discovery absence, provider downtime, and missing workspace tabs
never prune it. At capacity, new metadata is refused with visible save-failure
feedback instead of evicting another alias. Old entries can be explicitly
removed in the configuration file followed by restart; there is no new
organization or provider-management UI.

Old documents have no aliases and retain their previous defaults. Empty alias
arrays/fields are omitted; schema version remains 1. New builds read legacy
documents, but an older strict build may reject these additive fields on
downgrade. Naming metadata is local and follows the existing no-secrets and
atomic-save boundaries; it is unrelated to terminal-content persistence.

Workspace persistence remains opt-in. With **Restore workspace** off, ordinary
and multiplexer rename/reset is live-only and gives a visible notice; it neither
enables restoration nor rewrites a disabled workspace snapshot. Ad-hoc sessions
without an existing restorable profile descriptor also remain live-only unless
their verified native generation can provide a reattachment seed. There is no
ordinary Saved Sessions inventory or arbitrary tmux/Screen alias lookup.
No remote Python, control file, pre-exec wrapper, kernel/platform minimum,
process FFI dependency or provider protocol/option change is required.
See [the rename contract](gui-design.md#session-chip-context-menu-and-rename).

## Interface settings

The additive `[[settings.keyboard_bindings]]` array stores typed application
action overrides and chord strings. An empty chord explicitly unbinds an
action; an absent entry inherits the host-platform default. Invalid/duplicate
actions, unsupported/reserved chords and overlapping effective mappings reject
the document through the existing strict load path; the original file remains
untouched. Settings applies and autosaves valid edits through the ordinary
interface-settings transaction, preserving profiles, workspace and unrelated
preferences. See [keyboard syntax, defaults and recovery](keyboard-shortcuts.md).
The Session Inspector's opt-in input recorder is RAM-only and has no
configuration representation.

`terminal_font` remains the desired family across upgrades. JetBrains Mono is
embedded; Iosevka Term, JuliaMono, and Maple Mono are verified downloads from
the matching fesTerm release, automatically reconciled at startup without
reselection. Missing/corrupt faces temporarily use JetBrains, with visible
progress/failure and Settings > Terminal typography > Retry font download.
Neither fallback nor acquisition failure changes the saved family, ligature,
or emoji preference. Verified cache entries work offline. The native
application data directory contains `fonts/v1/festerm-font-<sha256>.ttf`;
unchanged hashes are reused across versions. Close fesTerm before removing
that directory to reclaim historical fonts; a subsequent non-default startup
downloads the needed faces again. See [ADR 0049](adr/0049-downloadable-terminal-fonts.md)
for bounds, request metadata, and the release-asset prerequisite.
All windows in one process share the effective installed family and atlas
generation: a pending or failed sibling cannot replace a family another window
installed successfully. This also relies on successful configuration writes
being broadcast synchronously to every sibling before its next frame; delayed
or out-of-process preference propagation is outside that invariant. About
embeds complete notices for bundled JetBrains Mono and Noto Emoji as well as
all three optional families, including in packaged/offline applications.

The optional `[settings]` table includes these interface preferences:
`mcp_enabled` and `javascript_enabled` (independent booleans, both default
`false`). They record explicit feature opt-in, not caller/session permissions:
pairing and exact native grants are still required, and an unqualified platform
must refuse execution even if its preference was enabled. Resetting interface
settings disables both. Neither approval nor a reusable bearer token is stored
in configuration. See [the automation contract](automation-design.md).

Desktop windows share this pair for fesTerm's in-process saves and broadcasts,
so a stale sibling save cannot restore a successfully saved disable. If saving
an automation preference fails or its outcome is uncertain, MCP and JavaScript
stop for the current process and Settings warns that an enabled value may
remain on disk. After a pre-attempt failure, retry is offered only while the
shared save gate is healthy; a successful retry saves the requested pair but
keeps automation stopped until restart. A busy gate suppresses retry while its
outcome is pending; an indeterminate attempted save or poisoned gate blocks
retry and requires manual reconciliation and full restart. No durable off
latch is written on failure, so restart can read an enabled value still on disk,
although prior grants are not restored. This does not resolve stale
non-automation documents, filesystem namespace safety, durable restart
reconciliation, or external editors.

Other interface preferences are
`chip_layout` (`"wrap"` or `"single-row-scroll"`, default
`"single-row-scroll"`), `status_bar_visible` (default `true`),
`show_session_details` (default `false`), `confirm_session_close` (default
`false`), `prefer_powershell` (default `true`), `extend_dev_tunnel_lifetime`
(default `false`), `show_running_dev_tunnels` (default `false`), `dev_tunnel_extension_days` (default `30`, clamped to
1-365), `customize_local_shell` (default `false`), `restore_workspace` (default `true`), `terminal_font`
(`"jet-brains-mono"`, `"iosevka-term"`, `"julia-mono"`, or `"maple-mono"`;
default `"jet-brains-mono"`), `terminal_ligatures` (default `true`),
`emoji_presentation` (`"color"` or `"monochrome"`; default `"color"`),
`scrollback_limit` (`"disabled"`, `"16-mib"`, `"64-mib"`, or `"256-mib"`;
default `"64-mib"`),
`image_memory_budget` (`"64-mib"`, `"128-mib"`, `"256-mib"`, `"512-mib"`,
`"1024-mib"`, or `"2048-mib"`; default `"512-mib"`),
`scroll_speed` (`"very-slow"`, `"slow"`, `"normal"`, `"fast"`, or
`"very-fast"`; default `"normal"`), `quick_switch_overlay` (default `true`),
`compact_launcher_grid` (default `true`), and `show_resumable_sessions`
(default `true`). They
mirror the current Settings controls for chip layout, chip details, the
status bar, live-session close confirmation, workspace restoration, compact
Launcher layout, resumable local-session
surfacing, terminal-only typography, keyboard quick-switch overlays, and
scrollback scroll speed, plus the bounded Markdown image-memory budget. Unread
background output always uses a static ring-and-dot marker, independent of
focus, renderer, and reduced motion. Tab
activation clears it. The retired `pulse_new_output_dot` boolean is accepted
when loading older files, ignored, and omitted from subsequent saves.
Software rendering adapters automatically select reduced motion for the
current process without changing the saved preference. They also use a
solid-color fast path for opaque default terminal backgrounds on supported
eight-bit gamma framebuffers. No saved setting, output throttling, or
resolution reduction is involved. Running Sessions
discovery continues in the background without redrawing unchanged inventory.

fesTerm automatically uses Direct2D only on
Windows x64 with a DX12 CPU adapter and an eight-bit gamma terminal target
(`Bgra8Unorm` or `Rgba8Unorm`). Hardware GPUs, other backends/formats, secondary
windows, translucent/transformed painters, unsupported content, and same-frame
native rejections retain ordinary painting. Initialization failure or a logged
native error disables the optional painter for the rest of that process while
preserving the current frame's ordinary shapes. The experiment does not alter
terminal semantics or output/frame scheduling. See
[ADR 0039](adr/0039-opt-in-direct2d-terminal-composition.md) and
[the investigation](../validation/direct2d/README.md); native qualification
remains open under CP-18 and issue #244.

Host-copy and retained-prefix composition are also automatic on Windows x64
DX12 CPU/BGRA gamma with compatible opaque-root targets. There is no UI, TOML or
environment on/off setting for this pipeline. The retired
`FESTERM_EXPERIMENTAL_DIRECT2D`, `FESTERM_EXPERIMENTAL_HOST_COPY` and
`FESTERM_EXPERIMENTAL_RETAINED_COMPOSITION` variables are ignored, even if set
to `0`. Eligibility, target/lifecycle checks and ordinary same-frame fallback
remain correctness safeguards, not toggles. The owner approved this bounded
rollout based on repeated substantial CPU savings despite shared-host noise;
precise benchmark, resources and native/latency follow-ups remain documented
in #282/CP-18. See ADRs 0040/0041 for ownership and cache limits.

By default, choosing **Local Shell** starts the
platform shell immediately in the user's home directory.
`customize_local_shell = true` instead opens the executable, arguments, and
working-directory form before launch. On Windows, `prefer_powershell` makes new default
local sessions use `pwsh.exe` when it exists: first the per-user app-execution alias
`%LOCALAPPDATA%\Microsoft\WindowsApps\pwsh.exe`, then `pwsh.exe` on `PATH`, then
`%ProgramFiles%\PowerShell\7` or `7-preview`. It never embeds a username or
versioned package path, and falls back to the absolute `%COMSPEC%` executable
when the alias is absent or the preference is disabled. Font choice never
changes application chrome.
The scrollback limit is a binary-byte payload budget applied when a session is
created; changing it does not silently evict history from sessions that are
already open. `disabled` gives subsequent sessions no retained history.
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
the active tab, or an explicit instance alias changes - there is no manual "Save workspace" action. Each
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

The profile editor accepts an initial password or OpenSSH private key (with
optional passphrase) while creating either an SSH or SFTP profile. Profile
metadata is persisted first; only after that succeeds is the secret written on
a background worker and the generated opaque reference plus
`credential_kind` atomically saved into the profile. Existing profiles expose
separate password/private-key replacement actions. If reference persistence
fails, fesTerm removes the newly created native secret where possible and
leaves the old profile reference unchanged. Saved credentials can authenticate
SSH, terminal SFTP, and GUI SFTP; GUI SFTP still requires persisted host trust.
One-off SSH/SFTP connections remain transient. Restored workspace surfaces
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
existing target, the prior target is moved aside and restoration is attempted
if the final rename fails. Temporary files are cleaned up on failures where
possible. Errors can occur after installation, and filesystem namespace and
alias races remain unresolved; the app treats every error returned by
`save_to_path` as indeterminate rather than inferring disk state from it.

`ConfigurationFileError` reports stable, content-free categories for missing
files, reads, parse/validation, serialization, temporary-file writes, and
replacement steps. It deliberately retains neither TOML content nor
caller-supplied paths; parse and validation details are available separately
as the existing content-free `ConfigError`.

Session naming edits across windows are gathered by the composition root and
saved as one complete replacement, preserving profiles, trust, credentials,
settings and update bookkeeping. Verified native reattachment seeds save even
with workspace restoration off; every restorable tab kind saves its own alias
only with opted-in workspace metadata.
New secondary windows start on a Launcher, while restored secondary windows
restore only their own saved tabs. With workspace restore enabled, both retain
the complete document supplied by the primary window, including the workspace,
when preparing later saves. With restore disabled, stale workspace metadata is
discarded. This does not resolve stale cross-window saves or ambiguous
filesystem outcomes.
A rename/reset save failure leaves the in-memory committed configuration and
sibling documents unchanged while the local display edit remains usable. If
`save_to_path` returned an error, the disk outcome is unknown: every open window
receives a bounded reconciliation notice and subsequent saves are blocked.
Missing paths, directory-creation failures, session-name validation failures,
and busy refusals occur before that call and do not themselves latch the gate.
Validation errors returned by `save_to_path` still latch. A later
rename/reset request refused by a previously latched gate did not install its
own candidate; it does not clear the earlier uncertainty or retry the write.
After a failed reset, Use default name remains available in the live session
until removal actually commits. An admitted naming transaction also includes
other still-live unsaved native seed edits; failures do not cause
frame-by-frame write retries. An admitted workspace save likewise includes
those pending seed edits, so a failed reset cannot appear committed while
leaving its old native seed behind. Pending in-memory edits are not guaranteed
to survive the restart required after an unresolved outcome.
Successful document broadcasts do not rename other current logical views.

`extend_dev_tunnel_lifetime = true` makes each native Dev Tunnel sign-in or
connect ask the service to move the tunnel's expiry `dev_tunnel_extension_days`
days forward. Only the tunnel's owner can do so; a refused or failed extension
is logged and never blocks the connection.

`show_running_dev_tunnels = true` lists the first signed-in account's running
bridge tunnels that have no saved profile in New Session. The list is cached
for a minute (the Running Sessions refresh button re-lists). Choosing one
connects once with that account's stored sign-in; no profile is saved. The
New Session **Dev Tunnel** card opens a compact form that connects the same way
and saves a profile only when **Save as profile** is checked.

### HOST session-control state (ADR 0084 stage 2)

The local session-control API adds no portable TOML setting or network listener.
Its `Recipe` is an argument vector plus optional shell/cwd, not an administrative
command string. Original intent and the resolved executable/cwd fingerprint are
journaled separately. A conflicting running recipe refuses; an explicit Connect
after authoritative absence may use an edited future recipe and fresh generation.
Reusing an operation ID with changed intent always refuses.

`Persistent {}` survives bridge/client/UI/installer exit, not reboot.
`Temporary { reconnect_grace_seconds: 60 }` requires an owning attachment/lease.
The actual daemon enforces its protected deadline; the bridge must renew an
adopted owning lease more frequently than 60 seconds. Intentional disconnect
ends only that exact lease; unexpected loss starts grace once, and bridge crash
expires the last renewal without requiring another bridge process. Restart does
not reset grace. Takeover fences the previous lease epoch only when recovery
adoption and daemon writer replacement commit. Ordinary persistent shared shells
never inherit this policy, and legacy clients cannot bypass the temporary lease
handshake. Native mobile grace qualification remains required.

Private version-2 `host-sessions.json`, bounded launch journals and generation-scoped
`host-daemon-*.json` files are host-local runtime authority, not synchronized
profiles, credentials or import/export metadata. Never copy or delete them to
retry an uncertain spawn. Ambiguous unbound reservations remain Unknown; local
ownership investigation or protected exact-generation `retire_local` abandonment
is required, not an automatic fresh shell. The entire operation ledger is capped
at 128 compact entries: durable issuance rotation makes old captured requests
Stale without historical archive files. Never refresh an uncertain request's
captured scope automatically. The 64 logical records use fixed private native
slots; dead generation artifacts are reclaimed only after sealed supervision
and positive native absence proof. Ambiguous evidence remains Unknown; contention
is Busy. Reclamation takes only one requesting-owner known-Absent record, never
another owner's or a revoked fence. A new explicit operation may reuse the same
logical ID with a fresh generation. At capacity, authorized inventory/inspection
may first reconcile owned dead Running/Ending records before retrying Busy with
the original unchanged request. Advertised issuance comes from the current
protected journal reservation, reauthorized after rotation, not cached enrollment
consent or a client-provided epoch.

Revoked live shells retain resource accounting and exact local observation/end.
Their fence requires known absence and explicit new local consent before removal.
Missing/unsupported journal state fails closed; prerelease version-1 records are
not automatically migrated or reset. Do not copy/delete authority files or treat
setup/sharing consent as permission to stop a live generation. The separate
default-No exact-generation migration stop confirmation remains mandatory.
