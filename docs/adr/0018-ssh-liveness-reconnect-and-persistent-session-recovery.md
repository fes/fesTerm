# ADR 0018: SSH Liveness, Reconnect, and Persistent Session Recovery

- **Status:** Accepted
- **Date:** 2026-08-20
- **Supersedes:** ADR-0013 (reconnect-default policy only; `russh` transport
  selection, bounded-backoff mechanism, and host-trust decisions in ADR-0013
  remain in force)

## Context

fesTerm's native SSH implementation can currently create a fresh transport,
authenticate, allocate a remote PTY, start a shell, and perform bounded
reconnect attempts. The first implementation exposed reconnect as a per-session
choice and briefly made that choice enabled by default.

That conflates three different concerns:

1. **Liveness detection** — determining whether an established SSH transport is
   still usable after events such as laptop sleep, network loss, or a Wi-Fi
   transition.
2. **Transport reconnect** — creating a new TCP/SSH connection and authenticating
   again after the old transport is dead.
3. **Remote session recovery** — returning the user to durable remote shell
   state, which plain SSH cannot provide by itself after the remote PTY and shell
   associated with the old transport have been lost.

TCP does not guarantee prompt failure detection when a sleeping client simply
disappears from the network. Detection may occur only when later traffic is
sent, when TCP keepalive eventually fires, or when an SSH-level liveness
mechanism is used. fesTerm should detect stale connections promptly, especially
when the operating system reports resume-from-sleep or a material network
change, without treating detection as permission to create a new remote shell.

Plain SSH reconnect is inherently lossy from the user's perspective: a new
transport and PTY create a new remote shell, not a continuation of the old one.
By contrast, a remote persistence mechanism such as `tmux` or `screen` can make
a new SSH transport useful for recovering an existing durable remote session.
That capability must be explicit rather than inferred heuristically.

This decision therefore separates connection health, reconnect policy, and
remote session persistence.

## Decision

### Connection verification does not imply reconnect

fesTerm should actively verify an established SSH connection when there is a
reasonable indication that its network assumptions may have changed.

Candidate triggers include, when exposed reliably by the platform:

- resume from system sleep;
- network interface or route change;
- Wi-Fi disconnect/reconnect;
- transport read/write failure; and
- the ordinary SSH liveness/keepalive cadence configured by fesTerm.

A trigger causes a **liveness probe**, not an automatic reconnect by itself.
The implementation may use a supported SSH-level keepalive/global request or
other benign protocol traffic whose only purpose is to determine whether the
existing transport still responds. The exact packet mechanism is an
implementation detail of `festerm-ssh` and may evolve with `russh`.

If the probe succeeds, the existing session continues unchanged.

If the probe fails or the transport has already reported failure, the session
moves to a disconnected/recovery-eligible state. fesTerm must surface that
state promptly rather than waiting indefinitely for the next user keystroke.

### Plain SSH sessions default to explicit recovery

A plain SSH session has no durable remote-session provider. For such sessions:

- automatic reconnect is **off by default**;
- transport loss never silently creates a replacement remote shell;
- the UI offers an explicit user action to reconnect;
- explicit reconnect creates a fresh SSH transport, re-authenticates,
  re-verifies host trust, allocates a new PTY, and starts a new shell; and
- fesTerm must not describe that new shell as restoration or resumption of the
  prior remote process state.

User-initiated reconnect remains useful for ordinary SSH even though it is not
session persistence. The important distinction is that the user explicitly
chooses to create the replacement shell.

Explicit close/disconnect, authentication failure, host-key rejection or
changed-host-key policy failure, invalid configuration, or unavailable required
credentials must not trigger automatic reconnect.

### Session strategy and recovery policy are separate concepts

SSH profiles and live SSH sessions should model two orthogonal concepts.
Names below are illustrative rather than a required Rust API.

```text
SessionStrategy
  PlainShell
  Persistent(provider configuration)

RecoveryPolicy
  Manual
  Automatic(bounded policy)
```

`SessionStrategy` answers **what remote session fesTerm creates or attaches
to**.

`RecoveryPolicy` answers **whether fesTerm may create a new transport without an
explicit user action after an unintentional transport loss**.

Automatic recovery is only valid when the configured session strategy declares
that it can safely recover durable remote state. A plain shell strategy does not
have that capability.

### Persistent-session providers are explicit

fesTerm may support remote persistence through providers such as `tmux` and
`screen`.

A provider is responsible for a small, explicit contract:

- determine whether its remote executable/capability is available;
- validate provider-specific profile configuration;
- construct the command that creates or attaches to the durable remote
  session;
- define what recovery means after a new SSH transport is established; and
- report unsupported or failed recovery honestly.

The reconnect coordinator should depend on provider capabilities rather than
hard-coding `tmux` or `screen` behavior into transport logic.

The initial implementation does not require a public plugin system or general
provider framework. A small internal enum/trait boundary is sufficient until
multiple real implementations justify more abstraction.

### Provider discovery is lazy and explicit

fesTerm should not continuously probe remote hosts for `tmux`, `screen`, or
other persistence software.

When the user asks to enable persistent sessions for a profile, the relevant
provider may run a simple remote capability probe such as:

```sh
command -v tmux
```

or the provider-specific equivalent.

This is the preferred initial approach because it is direct, portable across
ordinary POSIX shells, and easy to explain. The result may be cached as
advisory profile/host metadata, but stale cached capability must never be
treated as authoritative proof that a later recovery command will succeed.

Future implementations may add richer discovery or server-side integration,
but reconnect policy must not depend on heuristics such as guessing from shell
output, process names, terminal titles, or previously observed commands.

### Enabling persistence changes future session creation, not the live shell

fesTerm must not claim that it can retroactively convert an arbitrary existing
plain remote shell into a durable `tmux`/`screen` session.

If the user chooses "Enable persistent sessions" while using a plain SSH
profile, fesTerm may:

1. explain that persistence changes how future sessions for that profile are
   launched;
2. probe the selected provider on the remote host;
3. update the profile's session strategy after explicit confirmation; and
4. use that strategy on the next connection or on an explicitly requested new
   persistent session.

The current live plain shell remains what it is. Any attempt to migrate or
capture a live shell would require a separate explicit design and is out of
scope for this ADR.

### Automatic recovery is opt-in even for persistent providers

A persistent-session strategy makes automatic recovery *safe enough to offer*;
it does not automatically enable it.

The user must explicitly choose an automatic recovery policy for the profile or
session. Once enabled, an unintentional transport loss may initiate bounded
reconnect attempts using the existing SSH trust/authentication boundaries.

Each automatic recovery attempt must:

- establish a fresh transport;
- re-verify host identity according to current trust policy;
- re-authenticate using only credentials already available under the current
  credential policy;
- create a new PTY/channel as required by the provider; and
- ask the provider to attach to or recreate the configured durable session.

Automatic recovery stops and becomes user-action-required when host trust,
authentication, provider availability, or required credential access cannot be
satisfied without user input.

Retries remain bounded with cancellation and backoff. A recovering session must
be visibly distinguishable from running, disconnected, authentication-required,
and failed states.

### Wake/network events should optimize detection, not change policy

Operating-system sleep/wake and network-change signals are useful because they
can trigger an immediate liveness probe rather than waiting for a failed user
write or a long TCP timeout.

Those signals do not change the configured recovery policy. In particular:

- a wake event may prove that a plain SSH transport is dead, but it does not
  grant permission to open a new plain shell;
- a persistent session with manual recovery remains manual after wake; and
- a persistent session with explicitly enabled automatic recovery may begin its
  bounded recovery flow once the old transport is confirmed dead.

Platforms that cannot expose reliable wake/network events still use transport
errors and ordinary SSH liveness checks. Correctness must not depend on every
platform providing the same notification APIs.

## Alternatives considered

### Automatically reconnect every dropped SSH session

Rejected as the default. It is convenient after Wi-Fi loss or sleep, but for a
plain SSH session it silently creates a new remote shell and can mislead the
user into believing remote process state survived. It also causes new network
activity without an explicit persistence/recovery decision.

### Never reconnect automatically

Not selected as the long-term rule. Once the user has deliberately configured a
durable remote-session provider, automatic transport recovery can restore the
intended durable session cleanly and is a valuable workstation behavior.

### Infer persistence from remote behavior

Rejected. Detecting `tmux` processes, shell commands, terminal titles, or other
incidental signals is ambiguous and creates surprising recovery behavior.
Persistence is a profile/session strategy, not a heuristic.

### Automatically wrap every SSH shell in tmux

Rejected. It changes server-side behavior, requires software that may not be
installed or desired, complicates shell startup semantics, and violates
fesTerm's simple-by-default posture.

### Build a generalized remote-session plugin framework now

Rejected as premature. The architecture should preserve an internal provider
seam without creating a public ABI or plugin system before multiple concrete
providers demonstrate the need.

## Consequences

- The current SSH launcher/default must return to **manual reconnect** for plain
  SSH sessions.
- Existing wording that treats reconnect as generic session persistence must be
  corrected; plain reconnect always means a fresh shell.
- Liveness probing becomes a first-class SSH/session responsibility and may be
  triggered by platform lifecycle/network notifications when available.
- `tmux` and `screen` support can be added incrementally behind a shared
  persistence-provider capability boundary.
- Profiles gain a durable-session strategy separately from recovery policy.
- Automatic recovery becomes an explicit user choice and is only offered when a
  persistent-session strategy supports it.
- Workspace restoration continues to recreate sessions from metadata; it does
  not serialize live SSH transports or terminal process state.
- Secret handling, host-key verification, and authentication policy remain
  governed by the existing SSH and native-secret-store boundaries.
- The persistence UI and Inspector explain the difference between **Reconnect**
  (new transport/new plain shell) and **Resume** (new transport plus provider
  reattachment to durable remote state).

## Validation impact

- **Invariants introduced or changed:** Plain SSH defaults to manual reconnect;
  liveness detection does not imply reconnect; automatic recovery requires an
  explicitly configured persistent-session strategy plus explicit user opt-in;
  reconnect always re-verifies host trust and never claims transport/process
  continuity.
- **GUI/action edges affected:** Existing SSH Launcher connect/reconnect controls,
  disconnected-session recovery actions, and future profile persistence controls.
- **Automated tests required:** Add state-machine coverage proving liveness-probe
  failure only marks a plain session disconnected; explicit reconnect creates a
  fresh plain shell; explicit close/auth/host-key failures never auto-reconnect;
  persistent-provider automatic recovery is bounded/cancellable and stops when
  trust/auth/provider prerequisites require user action; provider probes are
  lazy and do not silently mutate profiles.
- **Native/manual evidence required:** Validate resume-from-sleep and network
  transition behavior on macOS, Windows, and Linux where lifecycle/network
  notifications are available; confirm UI state is clear when probes succeed,
  fail, or recovery requires user action. Persistent-provider acceptance should
  include a real remote `tmux` or `screen` environment once those providers are
  implemented.
- **Coverage superseded:** Existing tests or documentation that assumed the plain
  SSH Launcher defaulted reconnect to enabled have been updated. See
  `validation/traceability.json` (`launcher`, `ssh-lifecycle`) for the current
  automated-test references.

## Implementation status

The `SessionStrategy`/`RecoveryPolicy` split, the removal of the automatic-
reconnect checkbox from the plain SSH connect form, and the decoupling of
manual (user-initiated) reconnect from automatic policy are implemented in
`festerm-ssh` and `app/festerm`. `SessionStrategy::PlainShell` with
`RecoveryPolicy::Manual` remains the only combination `app/festerm` currently
constructs; `RecoveryPolicy::Automatic` is still rejected at the API boundary
for any strategy that reports `supports_automatic_recovery() == false`,
matching the strict reading of this ADR's decision that automatic recovery is
not merely off by default but not valid for a strategy that cannot safely
recover durable remote state.

Liveness probing (wake/network-triggered active verification) is
implemented, and platform-specific wake hooks now call it proactively:
`AppState::request_liveness_check_on_all_sessions()` fans out to every open
SSH session's `try_check_liveness()` (a benign no-op for local sessions),
triggered from a `WakeMonitor` per platform — macOS via
`NSWorkspaceDidWakeNotification`, Windows via a message-only window
listening for `WM_POWERBROADCAST`/`PBT_APMRESUME*`, Linux via
systemd-logind's `PrepareForSleep(false)` D-Bus signal — wired into
`app/festerm`'s `FesTermApp` so a resume-from-sleep event runs one liveness
pass across all open sessions on the next frame. Network-interface/route-
change detection remains out of scope on all three platforms and is tracked
as a further #48 follow-up. The macOS hook is verified locally (build +
clippy, this being a macOS development host); the Windows and Linux hooks
were written against each platform's documented API without local
cross-compilation and are verified via CI's per-OS `windows-latest`/
`ubuntu-latest` jobs. Native/manual evidence of an actual resume-from-sleep
event driving a real reconnect on hardware/VMs for all three platforms is
still outstanding, per the "Validation impact" section above.

`festerm-ssh` now implements the persistent-session-provider layer for issue
#49: a `PersistenceProvider` enum (`Tmux`, `Screen`) supplies a lazy,
explicit remote capability-probe command (e.g. `command -v tmux`, only ever
run when a user opts a profile into persistence — never speculatively or in
the background) and an idempotent attach-or-create command. Provider sessions are deliberately bare: tmux's
status bar is disabled for the selected session, and GNU Screen starts without
the user's screenrc chrome. The tmux command also seeds a new session with the
actual fesTerm PTY dimensions and makes the most recently attached client
authoritative for later window sizing. A validated
`PersistentSessionName` restricts session names to a conservative,
shell-metacharacter-free character set by construction, since the name is
interpolated directly into a remote exec string and there is no reliable,
portable way to shell-quote it for an arbitrary remote login shell.
`SessionStrategy::Persistent { provider, session_name }` is a real,
constructible variant now, and `supports_automatic_recovery()` returns `true`
for it — `RecoveryPolicy::Automatic` is valid (still opt-in) once a session
uses a persistent strategy. `establish_connection` execs the provider's
attach-or-create command in place of an interactive shell whenever the
strategy is `Persistent`; because that same command is idempotent, every
reconnect attempt (manual or automatic) naturally reattaches to the same
durable remote session rather than creating a new one, with no separate
recovery code path required.

`app/festerm` now exposes this strategy through one shared durable-session
editor used by Quick Connect, Advanced Connect, and saved SSH profiles. Users
can leave persistence off, or select `tmux`/GNU screen and a validated session
name; each launch can separately opt into bounded automatic recovery. Saved
profiles persist only provider/name, so automatic recovery remains a deliberate
per-launch choice. The session Inspector uses **Resume** rather than
**Reconnect** when the active SSH session has durable-provider metadata.
Provider capability probing and controlled real-provider interoperability
remain tracked by issue #49.

`festerm-ssh` now implements the SSH-level liveness probe itself
(`SshSession::try_check_liveness`, backed by an ordinary `keepalive`/`ping`
global request via `russh`), on an automatic cadence
(`LIVENESS_PROBE_INTERVAL`) plus the on-demand, nonblocking trigger used by
the implemented platform wake hooks. A probe failure is routed through
the exact same `ConnectionFailure::Transport` path as any other unintentional
transport loss, so it inherits the existing guarantee that a plain-shell
session only moves to `Disconnected` and never auto-reconnects by itself.
The platform-specific resume-from-sleep hooks (macOS/Windows/Linux) described
above already call this trigger proactively; only network-interface/route-
change detection remains unimplemented on all three platforms, so detection
of that specific case still relies on the automatic probe cadence and
ordinary transport read/write failures. See issue #48 for the remaining
network-change-detection scope.
