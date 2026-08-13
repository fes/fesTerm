# ADR 0014: Window, Workspace, Tab, and Session Ownership

- **Status:** Accepted
- **Date:** 2026-08-08

## Context

fesTerm now has independent tabs, an implemented M7 SSH transport, and an M8
metadata-only workspace foundation. Reconnect and persistence require stable
ownership rules. In particular, a terminal title,
transport attempt, and remote process are transient; none can define the
identity of a user-visible session.

ADR 0002 already separates reusable profiles from persisted workspaces. This
decision defines the in-memory ownership and lifetime model that makes that
separation implementable.

## Decision

The ownership hierarchy is:

```text
Application
  -> Window
    -> Workspace view
      -> ordered Tabs
        -> optional Session
          -> current transport attempt
          -> Terminal and SessionController
```

- **Application** owns global command dispatch, service construction, and
  cross-window policy. It does not make a terminal transport a global mutable
  singleton.
- **Window** owns one workspace view: its open tab order, focused tab, window
  presentation state, and window-scoped UI preferences. A workspace may be
  opened in more than one window in the future, but each view has independent
  focus and presentation state.
- **Workspace** is persisted metadata describing restorable tabs and their
  order/focus. It recreates session definitions; it never serializes a local
  process, SSH channel, remote process memory, or terminal screen contents.
- **Tab** has a stable application identity that outlives a particular
  transport attempt. Launcher and Settings are singleton application surfaces
  with no session. Session tabs retain their identity through reconnect and
  display a stable user/profile label; terminal-provided titles remain
  secondary, transient metadata.
- **Session** is a user-visible local, SSH, or future serial session represented
  by a session tab. It owns the current `Terminal`, `SessionController`, and a
  single transport attempt at a time.
- **Transport attempt** owns byte I/O and lifecycle only. A failed or
  reconnected SSH attempt is replaced beneath the same session tab identity.
  Reconnect allocates a new remote PTY and does not claim to restore an
  ordinary remote shell process.

Each session tab owns exactly one terminal viewport; split-pane ownership and
UI are not part of this model. Closing the final tab returns the window to a
Launcher tab. Closing a window
shuts down only the sessions in that window; it does not terminate sessions in
other windows. Profile and workspace data contain stable identifiers and
non-secret settings only. Secrets and host-key material remain in secure
storage/trust services, never ordinary workspace data.

## Consequences

- Reconnect, local relaunch, and serial reopen preserve tab/session identity
  while replacing only the transport attempt and terminal state. Once M9
  scrollback exists, completed generations may remain as bounded read-only
  history separated by non-terminal UI metadata; this is not process-state
  restoration and is not an implementation claim before M9.
- M8 restoration persists tab descriptors, tab order, active-tab identity, and
  window presentation metadata, then creates fresh sessions from profiles or
  ad hoc non-secret launch descriptors.
- `TabId` remains distinct from `festerm_session::SessionId`; future
  persisted identities require a serializable stable identifier rather than
  the current process-local counters.
- Tests must cover final-tab Launcher fallback, reconnect without changing
  session-tab identity, and metadata-only restoration of tab order/focus.
