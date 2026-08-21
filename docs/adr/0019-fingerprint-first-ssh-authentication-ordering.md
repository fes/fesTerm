# ADR 0019: Fingerprint-first SSH authentication ordering

- **Status:** Accepted
- **Date:** 2026-08-21
- **Supersedes:** None (refines the Quick Connect/password-prompt UX
  introduced alongside ADR-0013; ADR-0013's host-trust decisions and
  `russh` transport selection remain in force)

## Context

The Quick Connect and Advanced SSH forms let a user submit a destination with
no password up front. Before this decision, that path opened a separate
pre-connection prompt tab (`SshPasswordPromptTab`/
`AppCommand::OpenSshPasswordPrompt`) that collected a typed password *before*
any transport existed, then started the real SSH connection once a password
was entered.

That ordering is backwards relative to `ssh` itself, and to essentially every
other interactive SSH client: real `ssh` connects first, verifies the host
key (the user must accept or reject the fingerprint), and only asks for a
password once authentication actually begins. Collecting a password blind,
before host-key verification, also meant a user could type a password against
a host whose fingerprint they had not yet had a chance to reject.

fesTerm already has the machinery to verify a host key on an already-connected
worker (`HostKeyDecisionGate`/`Resolver`/`Waiter`, ADR-0013). The gap was
purely in UI/worker sequencing: `SshAuthentication` had no way to start a
connection with no credential attached at all.

## Decision

Add `SshAuthentication::Interactive` (and a matching
`WorkerAuthentication::Interactive`): no credential is supplied upfront, so
the worker connects immediately, and `establish_connection` verifies the host
key exactly as it already did for every other authentication method — this
requires no change to host-key handling itself. Only once host-key
verification has resolved and authentication is actually attempted does the
worker request a password, via a new `PasswordDecisionGate`/
`PasswordDecisionResolver`/`PasswordDecisionWaiter` family that is an exact
structural mirror of the existing host-key gate (`SshSession::try_shutdown`/
`Drop` reject both gates' pending requests identically). The worker retries
a rejected password in-connection up to `MAX_INTERACTIVE_PASSWORD_ATTEMPTS`
(3, matching `ssh`'s own `NumberOfPasswordPrompts` default) before letting the
connection fail with `SessionErrorKind::Authentication`, exactly as a plain
upfront `SshAuthentication::Password` failure already did.

Quick Connect and the Advanced form's empty-password submit path now dispatch
`SshAuthentication::interactive()` directly instead of opening a separate
prompt tab. Both prompts (`HostKeyPrompt`/`PasswordPrompt`) render pty-styled
(bundled terminal font and background) in place of the terminal view while
pending, rather than as a chrome dialog:

- The host-key prompt shows `ssh`'s own confirmation text with a blinking
  terminal-style cursor and captures `y`/`n`/Escape directly from the
  keyboard — no Reject/Accept Once buttons.
- The password prompt shows `user@host's password:` with no textbox and no
  Connect button; typed characters are never echoed (not even as masked
  dots), matching real `ssh`. Enter submits. A rejected attempt appends
  "Permission denied, please try again." plus a fresh prompt line below,
  rather than replacing the line in place, so prior attempts remain visible
  like a real terminal transcript.

The pre-existing outer full-reconnect retry loop
(`SshPasswordRetryState`/`MAX_SSH_PASSWORD_PROMPT_ATTEMPTS`/
`reprompt_rejected_ssh_passwords`) is kept for the one remaining case that
still supplies a credential upfront — a plain typed password entered in the
Advanced form — but it now restarts the rejected connection as
`SshAuthentication::interactive()` rather than reopening the old blind
pre-connection prompt tab. Because an `Interactive` session never populates
`ssh_password_retry` (only `SshAuthentication::Password` does), this handoff
happens at most once per connection lineage: after that, the `Interactive`
session's own in-connection retry loop owns further rejections.

`Password`/`StoredPassword`/`PublicKey` worker-authentication paths are
unchanged; only the new `Interactive` variant has the in-connection retry
loop.

## Alternatives considered

- **Keep the separate pre-connection prompt tab, just reorder its presentation
  after a synthetic host-key check.** Rejected: there is no way to verify a
  real host key without an actual transport, so this would have meant either
  a second, throwaway connection just to check the key, or continuing to
  fake the ordering. Connecting once and gating on the real worker state is
  simpler and matches `ssh`'s actual behavior instead of imitating it.
- **Reuse the host-key gate's type for the password gate (generic
  `DecisionGate<T>`).** Rejected for this change: the two gates carry
  different decision types and resolution error shapes
  (`HostTrustDecision`/`HostKeyTrustResolutionError` vs. a plain `String`
  password/`PasswordDecisionResolutionError`), and only `SshSession`'s
  shutdown path needs both together. A shared generic would have added
  indirection without removing any duplication that matters at this crate's
  size; revisit if a third gate of this shape appears.
- **Render the password prompt as an ordinary masked `TextEdit` (as the
  removed pre-connection prompt tab did).** Rejected per explicit user
  feedback: real `ssh` never echoes typed characters at all, and a persistent
  textbox/button breaks the "feel" of being inside the terminal that this
  change is trying to achieve.

## Consequences

- `SshSession` now owns two independent decision gates with parallel
  shutdown/`Drop` handling; a future third interactive decision (e.g. the
  keyboard-interactive/2FA follow-up, issue #60) should extend this same
  gate shape rather than inventing a new one.
- The password prompt captures raw keyboard `Text`/`Backspace`/`Enter`
  events directly from `egui::Context` instead of using a focused widget,
  exactly like the host-key `[y/N]` prompt already did — this only works
  because the terminal view is not rendered at all while either prompt is
  pending, so there is no competing widget for keyboard focus. A future
  change that renders both the terminal and a prompt in the same frame would
  need a different focus strategy.
- `festerm-ui-egui` now exports `terminal_font`, `DEFAULT_TERMINAL_FONT_SIZE`,
  and `terminal_fonts_installed` so application chrome can visually match the
  terminal before any live `TerminalView` exists for a tab (both prompts
  render before a session has produced its first frame of real output).
- This does not implement server-driven keyboard-interactive authentication
  (multi-round OTP/2FA prompts); that remains tracked separately (issue #60),
  which should be updated to authentication-mirror this ordering rather than
  the removed pre-connection prompt tab.

## Validation impact

- **Invariants introduced or changed:** An SSH connection started with no
  credential (`SshAuthentication::Interactive`) always verifies the host key
  before requesting a password; a rejected in-connection password reprompts
  up to `MAX_INTERACTIVE_PASSWORD_ATTEMPTS` times before failing; typed
  password characters are never echoed anywhere in the UI.
- **GUI/action edges affected:** `AUTH-*` (Quick Connect/Advanced-form
  empty-password submission now starts an Interactive session instead of
  opening a prompt tab), `TRUST-*` (host-key prompt keyboard/`[y/N]`
  presentation).
- **Automated tests required:**
  `festerm_ssh::tests::password_gate_rejects_missing_timeout_cancel_and_invalid_resolutions`,
  `festerm_ssh::tests::stale_password_decision_cannot_resolve_a_later_prompt`,
  `festerm_ssh::tests::password_prompt_is_rejected_when_the_bounded_event_queue_is_full`,
  `festerm::tabs::tests::interactive_ssh_command_replaces_the_active_launcher_in_place`,
  `festerm::tabs::tests::a_plain_typed_password_session_retains_full_reconnect_retry_state`,
  `festerm::screens::tests::ssh_live_password_prompt_shows_the_ssh_style_prompt_line`,
  `festerm::screens::tests::ssh_live_password_prompt_submits_a_typed_password_on_enter`,
  `festerm::session_controller::tests::password_prompt_crosses_the_session_boundary_and_clears_once_resolved`,
  `festerm::session_controller::tests::a_pending_host_key_prompt_and_a_pending_password_prompt_can_coexist_on_the_controller`,
  and the new live-fixture
  `festerm_ssh::tests::controlled_openssh_interactive_password_interoperability`
  (`crates/festerm-ssh/tests/openssh_interop.rs`, requires the OpenSSH Docker
  fixture) covering a rejected-then-corrected interactive password round
  end to end.
- **Native/manual evidence required:** Manual confirmation that the
  keyboard-driven `[y/N]` prompt and the unechoed password prompt feel
  responsive and correctly styled in a real window (performed this session;
  see checkpoint history).
- **Coverage superseded:** Prior tests exercising the removed
  `SshPasswordPromptTab`/`AppCommand::OpenSshPasswordPrompt` mechanism
  (`open_ssh_password_prompt_replaces_the_active_launcher_in_place`,
  `submitting_from_a_password_prompt_carries_its_attempt_count_forward`,
  `ssh_password_prompt_shows_the_ssh_style_prompt_line`,
  `ssh_password_prompt_submits_a_typed_password_on_enter`) were replaced by
  the tests listed above.
