# Application Command Model

**Status:** Architectural rule

This document defines how user-visible application actions are represented and dispatched in fesTerm. It complements `ARCHITECTURE.md` and `docs/gui-design.md`.

## Principle

A user-visible application action should have one semantic command regardless of where the user invokes it.

Launcher items, keyboard shortcuts, application chrome, overflow menus, command-palette entries, settings links, and future automation surfaces must not each implement their own version of the same operation.

The intended flow is:

```text
invocation surface
    -> application command
    -> application coordinator/state owner
    -> state/session/window effect
    -> UI observes resulting state
```

UI components may translate gestures or controls into commands, but they should not contain independent session-management or workspace policy.

### Desktop-local Remote Access

`LocalRemoteAccess(Action)` is a composition-root command, not an `AppState`
metadata side effect. Settings renders immutable observations/drafts and emits
typed review/confirm/cancel actions. Only explicit confirmation admits a bounded
background operation delegating to the shared setup/controller/native factory.
The default `local-host-settings` feature enables those legitimate local
operations separately from client fingerprint/recipe functionality.

Each confirmation retains the complete protected installation snapshot and
draft and separately observed native policy revision. Shared lifecycle/policy CAS rejects stale epochs, same-epoch permission revocation, changed settings and
foreign origins; editing cancels review without granting anything. One process-
wide primary permit and one independent revoke permit remain owned by the
actual worker, including cancellation or blocked native work. Late completions
cannot publish access or readiness. Closing the coordinator cancels/drains
through the same policy. No widget runs an installer, native command, browser
or credential fallback.

Share/unshare review captures the actual attached native persistent identity,
tab, profile and lifecycle generation; dispatch rechecks that exact selection.
Unknown, remote or unsupported old-helper sources return an actionable
capability refusal without session effects.
No portable profile save/import, ordinary launch, PID or display metadata can
produce a HOST grant.

The separate desktop child consumes the qualified immutable
`private_session_sharing` adapter, not the legacy migration source authority.
Bind remains private; Switch requires separate explicit
confirmation. Resume reads the original protected operation without switching;
retrying Bind retains that same operation. Release and irreversible rollback
reopen the exact protected receipt, including after the original tab closes.
Neither action stops or acquires its frontend/PTY.

Reviewed recovery and retained-status controls are distinct typed intents.
Reviewed Resume/Release/Rollback use that exact current review and the explicitly
typed original operation, refusing missing, malformed or stale selections.
They never silently reuse another status target/operation or recover by Bind.
Status-group Resume/Release/Rollback instead retain the displayed target and
its original operation, even when another session and operation are reviewed.
An unknown operation remains uncertain; it never creates or switches a binding.

The connected-local provenance accessor is distinct from display identity and
default runtime discovery. The application never constructs its capability,
legacy receipts or a policy verifier. A window-local central-command epoch
invalidates reviewed and pending sharing before state mutation; refreshing the
installation/owner context also discards the old sharing review. This is only a
negative fence, never authorization. Cancellation retains real worker admission
and sends independent exact-operation release, not generic host Disable, so a
reply queued just before cancellation cannot escape reduction. Native/source
admission remains entirely in the shared protected adapter.

## Command Responsibilities

Application commands represent product-level intent, for example:

```text
NewLauncher
StartLocalSession
StartConfiguredLocalProfile(profile_id)
StartSshSession(profile, authentication, options)
StartConfiguredSshProfile(profile_id)
StartSerialSession { settings }
StartConfiguredSerialProfile { profile_id }
OpenSettings
ToggleSessionInspector
SwitchSession(session)
ReconnectSession(session)
CloseSession(session)
OpenWorkspace(workspace)
```

These names are illustrative rather than a required Rust API, but the current `AppCommand` vocabulary follows this shape closely. The implementation may use enums, typed command objects, or another explicit representation provided that the following properties hold.

## Required Properties

1. **Single semantic implementation.** The same operation invoked from two UI surfaces must converge on the same command handling path.
2. **Typed targets.** Session, profile, workspace, and window targets should use stable typed identifiers rather than presentation strings.
3. **Central policy.** Confirmation, lifecycle, restoration, and failure policy belongs in the application layer, not in individual widgets.
4. **Testability.** Command handling should be exercisable without synthesizing every GUI interaction that can invoke it.
5. **Observable results.** Failures and state transitions should become application state or structured outcomes that the UI can present consistently.
6. **No terminal-protocol leakage.** Application commands may direct a session or terminal, but terminal protocol semantics remain in `festerm-core`.
7. **No backend leakage into UI.** UI code should request operations such as reconnect or resize through application/session abstractions rather than reaching into PTY or SSH implementations.
8. **Singleton application surfaces.** Commands that open Launcher or Settings
   focus the existing surface when present rather than creating duplicates.

ADR 0084's client-local foundation uses
`AppCommand::HostRecipe(MetadataCommand)` on desktop and
`ProfileStore::apply_host_recipe_command` on mobile, backed by the same
`festerm-client-recipes::RecipeBook::apply` policy. Save/edit/duplicate/import/
export/delete are metadata-only, distinct from any future Connect command.
Persistence publishes the replacement/outcome only on success; export does
not write. No command in this vocabulary can sign in, create/end a remote
shell, grant host access or install a host. `HostBrowser` independently plans
explicit find/single-host/refresh requests with stale-result fencing; it is
not a network executor. The experimental desktop Profiles recipe manager uses
`HostRecipeFromProfiles { tab_id, request, command }` as an identified invocation
of that same handler. The coordinator refuses obsolete editors before any
effect and reports the transactional result only to the submitting editor.
Preview uses the shared portable parser; import confirmation only saves metadata.
Export returns JSON to the view for explicit clipboard copy and never saves.
Desktop selected-host review and explicit creation are described below.
The opt-in shared mobile `host-clients` route uses `ShellAction::OpenHosts` and
the mobile application's typed foreground HOST actions, not legacy
`SaveAndConnectDevTunnel`. See the [configuration contract](configuration.md#public-host-recipes-adr-0084-foundation).

The shared `festerm-tunnel-bridge::host_client` executor performs actual
selected-host inventory and existing-generation attachment only after its
composition root supplies protected foreground approval. It cannot manufacture
that context from a public recipe. Its bounded worker reserves before vault and
network work; each completion carries the original request, binding and lifecycle
generation. The composition root must cancel on account change, supersession,
editor closure or mobile app-lock. Snapshot delivery is not Attached: the exact
frontend must install recovery with `Adoption::adopt_into`, then wait for the
host's activation receipt. Keep the worker alive with its live terminal, drive
the shared `HostSession` through the existing presentation owner, and retain
correlated detach receipts. The typed `OpenApprovedHostFromProfiles` route accepts
only an `ApprovedAttachment`, checks the originating editor request and current
account before starting, installs its real `SessionTab` before acknowledging
recovery, and cancels on account removal/credential replacement. Its
`ApplicationSession::Host` branch translates recovery sync to the existing desktop
controller format; it grants no files, port forwarding or automation.

The reachable recipe button emits `ReviewHostFromProfiles { tab_id, request,
recipe }`, never an `ApprovedAttachment`. Merely opening review performs no vault
or network work. The foreground composition root checks the original saved
recipe, editor and exact account credential. First connection requires the full
host-local `owner_approval_prerequisite.binding`, independently confirmed host
fingerprint and a distinct explicit native-key preparation action. The private
local receipt journal reserves its opaque reference before writing the native
key, resumes only that same pending reference, and never regenerates a missing
ready key. Authentication only reopens existing exact keys. Neither host-local
setup metadata nor imported recipes are client receipts.

The public owner key and its `ED25519-SHA256:` fingerprint are displayed for
separate host-local `approve-owner`; preparing a client key does not enroll it.
After reviewing that full protected binding, the user explicitly supplies the
selected locator and requests authorized sessions. Only the exact signed-in
account is confirmed; expired credentials require foreground account sign-in,
not implicit refresh. HOST-only resolution refuses legacy bridge ports. The
bounded inventory chooser creates the privileged attachment command only for
an allowed observed target: exact attach preserves logical/generation identity,
resume only selects the recipe's logical session, and AlwaysCreate never
substitutes attachment to an existing shell. Busy/unknown/refused observations never become
absence or permission to create. Receipt/vault actions have a 15-second visible
deadline and resolution a 45-second bound; blocked native calls retain admission
until actual completion and late results cannot replace the originating UI.

The separate foreground `Action::Create` is coordinated by the same application
owner after exact launch/lifetime confirmation. Cached inventory shows capability
and creation/temporary permission, but the worker checks freshly authenticated
capabilities again. ResumeOrCreate issues HOST `Ensure` for the saved logical ID;
AlwaysCreate and explicit creation from ChooseSession freeze a fresh logical ID
once. Neither a lookup nor an unknown/denied/offline/Busy reply chooses creation.
`CreationIntent` binds the protected owner/reference, endpoint, launch, logical ID,
reserved attachment and original sealed `IssuedControlOperation`. Explicit retry
retains that whole action even when a fresh handshake advertises a newer epoch.
Only one flight per action is admitted; no automatic retry/reconnect or input
replay is added.

The bounded application registry retains sixteen unresolved actions across review
closure. Changed recipe/credential cannot replace one. A confirmed wire reply is
not presentation: a cancelled/lost completion or failed terminal admission keeps
the original action. After the application admits its authoritative target for
presentation, a later fresh review may create a new action. Confirmed Created,
Resumed and Recreated descriptions remain distinct; all use the existing
`OpenApprovedHostFromProfiles` origin/account checks and installed-frontend
snapshot/adoption path. This desktop registry remains process-local; the opt-in
mobile protected checkpoint path below does not reconstruct older lost actions.

The mobile composition root owns the same protected receipt/context, bounded
worker, exact authorized session selection and original-action registry. Its
native callback cancellation token is registered **before** worker/session
creation, closing the lock/suspension admission race. Result installation checks
the complete original account record, recipe, request and native lifetime epoch;
account removal/credential replacement also cancels active and parked HOST
transports. Imported metadata is never an owner binding or creation permission.
The native app-owned storage root alone selects the private receipt index
(canonicalized once for OS sandbox aliases); receipt traversal remains no-follow
and signing material stays in the native vault.

`LiveSsh::from_host` starts non-running, wraps the actual `HostSession`, and
adopts its pending snapshot into the already installed mobile terminal. Only
the subsequent correlated host activation admits input. Protected mobile
created-generation records remain until an exact authoritative Ended settles
them; starting a presentation worker alone never retires an original. Native lock
and suspension cancel without a UI frame and never forge detach receipts or
end the daemon-owned shell; no automatic reconnect or replay is added. Default
offline and legacy live-SSH/bridge behavior remain separate. Physical/native
mobile acceptance remains open under HOST-01–03/MOB-01–06.

Mobile inventory and `EndHostSurface(tab)` converge on one application-owned,
default-unconfirmed exact End review. Confirmation revalidates the full account,
protected owner/reference/endpoint, native privacy epoch, request generation and,
for an adopted tab, its original Session identity. `EndIntent` retains one
operation and logical/native generation across explicit retries; it does not
borrow creation permission or a creation issuance epoch (the existing End wire
contract forbids that field). Fresh authenticated End capability and protected
host admission remain mandatory. Native namespace/PID resolution stays in the
host's exact-generation journal, not profile authority or an invented UI PID.
Busy/unknown/denied/offline/Stale/cancellation/lost reply never mean absence.
The current native backend returns OutcomeUncertain after durable stop intent;
only an exactly correlated Ended reply is presented as confirmed End.

Close-tab and `DisconnectSurface(tab)` retire the exact old HOST Session through
the existing correlated `Session::shutdown` helper on a bounded background job,
never a blocking UI call or ExplicitEnd. A late receipt only completes that old
attachment; it cannot mutate the replacement frontend. Persistent daemons
survive; an owning temporary lease follows the host's intentional-detach policy.
Lock/suspension still directly cancel transport without a UI frame or a false
detach; unexpected-loss heartbeat/grace remains host-owned. End originals and
retirements are bounded to sixteen each. Exactly confirmed End originals retire
even if their completed UI event was discarded; unresolved ones do not.

Mobile Create/Ensure and End require explicit **Reopen protected originals**
under the current independently approved owner, full account and app lifetime.
This bounded native-vault job performs no network retry and grants no launch or
End consent. Its private receipt reserves the native payload reference before
initialization; a pending update nonce is published/synced before native writes.
Only the matching native payload can settle an ambiguous update. Missing,
corrupt, locked, mismatched or uncommitted pending data cannot become an empty
registry or new operation. Neither profiles nor workspaces store its authority.

An admitted mutation captures its exact original before effectful wire bytes;
durability failure refuses that send. Explicit cold-start review restores the
same creation UUID, old issuance epoch and whole body through the client-bound
protected checkpoint factory after fresh authenticated capability checks, never
`issue_operation` on restored work. End restores its distinct operation/target
without a creation epoch. Fresh launch/destructive consent is still required.
Multiple originals require exact selection; stale selection never mints another
action. Each purpose remains bounded to sixteen (64KiB total). Only correlated
Ended removes an End and its exact confirmed-created generation, not
StopRequested/OutcomeUncertain or frontend closure. The unprotected desktop path,
default offline fixture and opt-in legacy live-SSH path are unchanged.

No implicit discovery, background sign-in or input replay is implemented by this
executor. Shared framing, protected-file/memory-vault, headless application tests
and owned native SSH/PTY fixtures are not native-vault, development-wrapper GUI
or physical-mobile acceptance.

Destructive confirmation remains composition-owned even when the final effect
is an `AppCommand`. Every close invocation first passes through the same
application policy: non-live surfaces close immediately; an owned
starting/running transport produces a confirmation bound to its typed tab and
lifecycle generation while **Confirm before closing live sessions** is on,
and dispatches `CloseTab` immediately when that preference is off. Widgets and
individual invocation routes never interpret the preference themselves. Only
a revalidated confirmation dispatches `CloseTab` in the confirming mode.

Native Running Sessions termination follows the same composition-owned
confirmation rule, independently of the live-tab close preference.
`TerminateUnattachedSession` carries the discovered name/PID/creation
time/endpoint; the bounded helper revalidates that exact, still-unattached
generation under the registry lock before destructive work. Widgets only
dispatch intent; cancellation does not start a helper or remove a profile.

Installed Local-profile hosting uses `SaveLocalProfile` for explicit
enable/disable intent and its identified persistence result. The composition
root prepares private consent, saves config and commits the request before
background work; revocation precedes any disabling/deleting write and is not
rolled back on failure. `LocalTunnelHost` and `LocalTunnelCleanup` converge on
the same bounded coordinator and protected removal operation, including cleanup
after the profile is deleted. Widgets never launch the CLI, start a daemon or
delete cloud resources. Separate composition-owned confirmations distinguish a
fresh shell from recreation of a definitively missing cloud tunnel (ADR 0072).

Explicit terminal clipboard reads carry request identity, originating tab,
transport generation and input-ownership epoch; cancellation/supersession
cannot retarget a late callback. Already supplied native-key payloads are used
without a second read. View-owned context/middle-click gestures return read
intent to the composition root rather than issuing unowned Paste callbacks.
Widget Paste events alone never authorize terminal input.

Identified reads reserve one metadata-only position in the existing bounded
session write queue. Later keyboard/paste writes wait at that position;
successful delivery fills it, while cancellation/failure rejects the waiting
suffix with explicit content-free feedback. Ready callbacks precede later
keyboard dispatch. The composition root continues to own confirmation and
target validation; SessionController owns only bounded bytes and ordering.
Core replies and focus/mouse protocol reports remain independent of this
keyboard barrier, preserving existing query and gesture behavior.

Likewise, clipboard delivery remains terminal input, but risky-paste policy is
owned by the composition root so stable session identity/generation and UI
focus can be enforced before one ordered paste is returned to the terminal
encoder.

An SSH/SFTP profile's **Ask on first connection** source-address choice is
also application-owned preconnection policy. All launch surfaces must reach
the same decision before opening a transport; stored credentials and provider
probes cannot bypass it. Confirmation belongs to the original tab/request,
not whichever tab happens to be active later. The resolved source then belongs
to the live session and its related SFTP/retry connections, not to mutable
profile metadata. Cancelling leaves the connection unopened. Deferred
remember-password completion verifies the originating tab, ownership epoch,
and profile snapshot; a changed setup context cancels the launch rather than
activating an old tab or using the current tab. See
[local source address selection](gui-design.md#local-source-address-selection).

## Invocation Surfaces

Terminal OSC 8 activation enters `OpenTerminalHyperlink` with the originating
tab and lifecycle generation plus a redacted target. The application preserves
the web-only `OpenExternalLink` OS sink. Absolute local file URIs instead
require the same local source generation and enter the no-follow internal
document-opening path; SSH/SFTP and serial cannot grant client-local access.
Presentation eligibility and decoded previews are not filesystem authority.

Native PowerShell uses this same command boundary without adopting the
terminal byte-stream contract. Connect, Run, Stop and Close target the owning
structured-session tab. Its worker owns blocking protocol and secret/trust
loading, reports bounded state/results and wakes the application; GUI
callbacks never wait for a network operation. Closing an owner prevents late
results from being installed into another tab and initiates background cleanup.
Workspace restore opens disconnected metadata only. See
[native PowerShell sessions](gui-design.md#native-powershell-sessions).

The following surfaces should reuse the command model:

- session launcher;
- independent session chips;
- keyboard shortcuts;
- command palette and session switcher;
- application overflow menu;
- session inspector actions;
- settings links or actions;
- workspace restoration and retry controls; and
- future plugin or scripting entry points if those capabilities are later accepted.

A new invocation surface is not justification for a duplicate implementation of an existing action.

Desktop document activation is another invocation surface: command-line paths,
forwarded local-IPC requests and macOS open-document events enter one bounded
application-owned queue. The application resolves an existing document's window
or the last-active window, defers while dialogs block activation, and dispatches
`OpenLocalMarkdownFile { replacing: None }`. Native adapters never write terminal
input, upload files, or mutate document buffers. Opening an existing document
focuses its view without replacing unsaved edits.

`LoadMarkdownLocalImage` routes to either the active standalone viewer or its
text editor Preview; both use the same bounded image policy. Only a typed saved
local origin authorizes Preview reads. `SetImageMemoryBudget` changes the shared
live allowance through the composition root and its normal settings persistence
path; widgets do not own separate limits, loaders or configuration writes.

## UI Events vs. Application Commands

Low-level terminal interaction remains separate from application commands.

Examples that remain terminal/UI input rather than application commands include:

- printable text;
- terminal key encoding;
- paste contents;
- terminal mouse reports;
- selection movement; and
- focus reports required by terminal modes.

Examples that are application commands include:

- create or close a session;
- switch the active session;
- reconnect a transport;
- open Settings;
- toggle the session inspector;
- reorder or rename a session chip;
- restore a workspace; and
- open the launcher.

This distinction prevents the application command system from becoming a second terminal-input protocol.

Keyboard-binding replacements and Session Inspector input-recording
start/stop/clear/report-copy are typed application commands. The bounded,
off-by-default recorder observes existing core and controller outcomes; it
does not become an input owner, log terminal/clipboard contents, or invent
physical-event identities. Keyboard/menu/UI actions retain their ordinary
dispatch policy. See [the keyboard routing inventory](keyboard-shortcuts.md).

Terminal-local selection Copy and Paste contents remain UI/input operations.
Starting bounded rectangle copy converges on `BeginRectangleCopy(TabId)` from
its configurable binding and palette entry. The command admits the active
viewport, refuses an in-progress pointer gesture or oversized presentation,
and invalidates pending terminal paste authorization. Corner updates, frozen
cell extraction and Copy/cancel remain UI-local; live terminal/session ownership
and output processing do not change. Its whitespace preference is a typed
interface-settings toggle using the existing persistence/broadcast path.
Explicit OSC 8 activation emits an application `OpenExternalLink` intent so
URL parsing, scheme/host policy, safe rejection feedback, and OS launch remain
centralized and testable. Copy Link Address remains a local clipboard action.
In contrast, a chip context menu translates Rename, Use default name, Move left/right, and Close
into the same typed tab commands used by direct chrome gestures.
Opening a chip menu targets its stable tab identifier without activating it.
`RenameTab(TabId, String)` creates a sanitized explicit logical-view alias;
`UseDefaultSessionName(TabId)` removes that override. Neither command mutates
a reusable profile or durable attachment target. Empty/sanitized-empty rename
edits cancel. Nonempty validation failures remain typed, content-free outcomes
of the same command policy: an app notice reports refusal without echoing the
rejected text or changing the name, saved metadata, or transport.
The tab coordinator records
pending naming metadata; the composition root combines all windows' edits and
optional workspace snapshots into one transactional configuration save.
Save failure is visible, and successful documents alone are broadcast.
All restorable terminal kinds, including local/SSH tmux and Screen, retain
their own aliases in opted-in workspace metadata, even if their backend is
recreated. Native seeds use actual registry generations for newly opened views
only; broadcasts never rename independent current views. Reset clears the
current view and its associated native seed. Workspace opt-out or a missing
restorable descriptor produces a live-only notice unless an exact native seed
is available; no guessed identity or arbitrary mux alias lookup is added.

## Command Palette Rule

The command palette is a discoverability and invocation surface, not a separate control plane. Palette entries must dispatch the same application commands used by shortcuts and visible chrome.

Searchable session switching should resolve a stable session identity and dispatch the ordinary session-switch command.

The palette-only **Redraw Terminal** entry dispatches
`AppCommand::RedrawTerminal(TabId)`. The presentation owner invalidates only
that view's row/glyph caches and requests a full local paint. An optional
native painter receives a one-frame presentation hint to bypass retained
pixels; no terminal state or backend control crosses the renderer boundary.
There is deliberately no keyboard action/default chord for this command.

## State Ownership

Smart terminal selection (`TERM-14`) remains a low-level presentation operation,
like ordinary drag selection, not a session-management command. Context-menu
and fixed keyboard targeting gestures converge on
`TerminalView::request_smart_selection`; all inferred text is read through the
same `Selection`/`selection_text` path used by application Copy. No clipboard
read, transport write or backend ownership is added. Copy remains a separate
existing semantic operation, including native Edit and palette Copy.

Command execution should follow the ownership boundaries in `ARCHITECTURE.md`:

- application state and workspace/session policy belong to the application layer;
- terminal state has one logical writer;
- session backends own transport/process details but not terminal state;
- `festerm-ui-egui` owns presentation and gesture translation but not product policy.

## Evolution

Experimental Dev Tunnel profiles emit `CreateProfile`, `SaveProfile`,
`SaveDevTunnelProfile`, `SaveAndConnectDevTunnel`, `StoreDevTunnelConnectToken`
and `StartConfiguredDevTunnelProfile`; CLI launch converges through
`ConnectDevTunnel`. The profile editor's Save emits `SaveDevTunnelProfile`,
whose composition-root handler persists through the configuration reloader and
reports the committed/failed outcome to the exact source tab and pending save
request, rejecting stale results for replaced drafts, so the form closes to
the profile list only on a successful write and otherwise keeps the exact draft
with an actionable error (it never connects). **Save and Connect** keeps the
explicit save/authenticate/connect flow through `SaveAndConnectDevTunnel`. The
composition root, not widgets, owns cancellable auth,
profile/account compare-before-publication, native credential rotation,
Cancel-default host approval and bounded pre-admission discovery.
The composition root owns native-store transactions; AppState captures each
immutable launch binding. Detected paths and `OpenTerminalHyperlink` converge
on the same source-bound filesystem dispatch and remote snapshot commands.
Widgets never select another account/transport or perform filesystem reads.
See [ADR 0053](adr/0053-separate-dev-tunnels-session-bridge.md).

Do not design a generalized plugin command API before plugin or scripting work is accepted. The initial command model should solve first-party application actions cleanly while preserving enough typing and separation to expose selected commands later if justified.

A material change that bypasses these rules or moves product policy into individual GUI widgets requires architectural review and, during the 0.1 architecture-stability period, an ADR.
