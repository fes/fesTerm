# ADR 0064: Dev Tunnel New Session Connect-Once Form

- **Status:** Accepted
- **Date:** 2026-11-01
- **Supersedes:** None
- **Related:** ADRs 0053, 0062; `docs/dev-tunnels-experiment.md`

## Context

The New Session **Dev Tunnel** card opened the full profile editor, and a
**My running Dev Tunnels** row saved a profile as part of connecting. SSH, SFTP
and Serial open a compact inline form where connecting is ephemeral and saving
is optional. Dev Tunnel was inconsistent and forced users to create a profile
for a one-off attach. The editor's "Connect once" needs a saved profile, and
the client always asked the host to displace a holder.

## Decision

- The card opens `LauncherForm::DevTunnel`: account picker with **Add GitHub
  account**, the account's running tunnels (the existing running-tunnels data)
  plus a manual tunnel name/id field, **Allow takeover of an attached session**
  (default on), **Save as profile** (default off; a name field appears only
  when checked) and **Connect**. Enter submits; Esc and Back cancel.
- **Connect** dispatches `AppCommand::ConnectDevTunnelForm`. Without `save_as`
  the native workflow runs with an in-memory profile (`ephemeral`): it reuses
  the account's stored sign-in (or a discarded one-off device sign-in), asks
  for host-fingerprint approval every time, and writes no profile, credential,
  host pin or recent. With `save_as` the profile is saved and connected through
  the existing flow.
- `AppCommand::ConnectRunningDevTunnel` replaces
  `SaveAndConnectRunningDevTunnel`: running-tunnel rows connect once.
- The takeover choice is carried by `Launch::with_takeover` and
  `transport::start_client_with_takeover` into the existing `Hello.takeover`
  field; `start_client` keeps its previous always-true behavior. The host's
  own takeover setting still applies.
- Saved profiles, Manage Profiles and New Profile → Dev Tunnel are unchanged.
  Ad-hoc SSH connects are not recorded as recents (recents are profile
  last-used times), so ephemeral tunnel connects are not either.

## Alternatives considered

- Save a hidden profile and delete it afterwards. Rejected: it writes
  configuration and credentials the user did not ask for.
- A session picker. Rejected: a host exports exactly one session.

## Consequences

Ephemeral connects confirm the host fingerprint each time because no pin is
stored. The tunnel list in the form follows `show_running_dev_tunnels`; with it
off only the manual field is offered. No wire change: `Hello.takeover` already
exists and defaults to true.

## Validation impact

- **Invariants introduced or changed:** Connecting from New Session saves no
  profile unless **Save as profile** is checked; takeover is sent as chosen.
- **GUI/action edges affected:** `TUNNEL-12`, `TUNNEL-14`.
- **Automated tests required:**
  `the_dev_tunnel_card_opens_the_inline_form_not_the_profile_editor`,
  `dev_tunnel_form_connect_is_ephemeral_with_takeover_on_by_default`,
  `dev_tunnel_form_can_decline_takeover_and_save_a_named_profile`,
  `dev_tunnel_form_offers_running_tunnels_and_submits_with_enter`,
  `a_running_dev_tunnel_row_connects_once_by_locator`,
  `form_connect_without_save_as_is_ephemeral_and_keeps_the_choice_of_takeover`.
- **Native/manual evidence required:** Live attach with takeover off against a
  held session is unverified (live acceptance gate #429).
- **Coverage superseded:**
  `a_running_dev_tunnel_row_saves_and_connects_by_locator`.
