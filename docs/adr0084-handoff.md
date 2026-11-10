# ADR 0084 VM-lab handoff

## Status and stop boundary

Implementation is paused for transfer to a stronger laptop and the native VM
testing lab. This is a source/evidence checkpoint, not acceptance of ADR 0084.
There is not yet a usable end-to-end multi-session host.

Tracking issue: <https://github.com/fes/fesTerm/issues/681>.
Design: PR #679, ADR 0084 (still Proposed). Preserve upstream ADR 0085.
The verified upstream integration baseline is
`8ac8070dacc959200127fa8dd0ce9ce7f6d60991`; check current main before resuming.

All implementation/checkpoint branches below are on `origin`. No main merge was
performed by the coordinator. This coordinator branch is a handoff/documentation
branch, **not an integrated product tree**; do not merge it wholesale into main.

## Branch and qualification inventory

Branch names below have the prefix `feature/adr0084-`.

| Area | Branch suffix | Frozen commit | PR / qualification at freeze |
|---|---|---|---|
| Artifacts | `authenticated-artifacts` | `8efe3846e90c3ce7990812bb375ea4ea704c9c9d` | #682 merged as `b4405d74`; reviewed tree identical |
| Contracts | `host-contracts` | `7b61af7b5bbd6f1e9da12650c40f0af2888736ec` | #683 review-ready; all required CI green |
| Lifecycle | `headless-setup` | `b0e5bff3874b8b7021a154b6f6344b3aecb8ea57` | #684 review-ready; all required CI green |
| Recipe foundation | `client-recipes` | `19e20226b46292f8a88d7d6c7b64479ac922dbbd` | #685 review-ready; all required CI green; contracts first |
| Enrollment | `host-enrollment` | `e893cdb1c7d9026e34a3fd08d4a63ae831f9eef4` | #686 draft; new-head CI must finish |
| Session corrections | `session-control` | `4fca3fc98a04de51a361580da2e8cf0430123cf9` | #687 draft checkpoint; corrective review/integration/cross-platform gates pending |
| Original routing | `host-routing` | `0cd6acb386855ecfdce8d5fae988965510ceafb3` | #688 draft; old Windows CI red, not waived |
| Packaging | `headless-packaging` | `585ea70b2ec513de8c4061d2c8739fed26f50c79` | #689 review-ready; all required CI green; lifecycle first |
| Signed channel proof | `ssh-channel-binding` | `6c795246b8c8e4ce842a0e0382b7d423453dd61e` | #690 draft; source antagonists clear, exact-head CI/dependencies pending |
| Desktop metadata UI | `host-recipe-ui` | `f03f0cc44725e93c0e706ddfedcc956cfbeb47e4` | #691 draft; local tests/review clear, exact-head CI pending |
| Cloud management | `host-cloud-management` | `243dc3342dd3cc6e0d8e724b04e8b95ad00ebdb5` | No PR; integrated source locally tested, narrow merge review/publication pending |
| Native services | `native-host-services` | `a6c2a367fa00967dbd8a94676a7da0deac2a6906` | No PR; source checkpoint, Rust/native/review gates pending |
| Terminal integration | `terminal-routing` | `b545392db7cd13fd5879dd4a48d346ba9dd23d52` | No PR; source-only freeze, requires session corrections and native qualification |

The four review-ready PRs each completed 19 checks: 18 successful and the
explicitly optional Windows sampler skipped. Re-query GitHub against the exact
head before merging anything. Green package builds are not native service,
vault, reboot, device or live-cloud qualification.

`session-control`, `native-host-services` and the old `host-routing` branch do
not yet contain the main `8ac8070d` merge. Terminal integration already contains
that main baseline and signed-channel source, but not the final session
correction. Use normal merges and preserve both sides' behavior; do not use
ancestry/ours shortcuts for real upstream changes.

## Exact evidence and unresolved findings

### Sessions and routing

Session checkpoint `4fca3fc9` is atop reviewed `744d05b4`. It corrects the
closed pending-client EPIPE that could kill the old writer/shell, pre-adoption
starvation after 64 tiny output chunks, and startup contention caused by
snapshot readers holding the publication lock for 635-812 ms against an
unchanged 500 ms mutation deadline. Private atomic snapshot reads no longer
hold that mutation lock; legacy fallback and mutation fences remain.

Actual focused results: host control 25 passed, registry 6, candidate 2, native
daemon 17 passed / 7 existing ignored, lease 3, strict package all-targets
clippy, fmt, trace and diff passed. The native followup completed before STOP;
no test began after STOP. READY fixtures synchronize actual child output and
obtain a fresh snapshot; exact assertions, IDs, deadlines and ignores remain.
RED proofs and the original failures are retained, not relabeled as cache noise.
Independent corrective security/reliability/scope reviews are still required.

Do not expose the terminal adapter/listener before actual protected native
admission and the sealed channel qualify together. Preserve opaque authorities,
detach receipts, original issued operations and partial inventory semantics.
The old #688 Windows failure was 18 protected setup/enrollment/staging fixture
`PermissionDenied` failures, not a clean run:
<https://github.com/fes/fesTerm/actions/runs/42757630576/job/129635593565>.
Integrate the reviewed private-fixture fixes; do not weaken ACL checks.

### Owner consent, issuer and channel

The permission-template source `91cf2880` is shared by #686/#690. Strict public
wire validation and verifier new/replace remain strict. Enrollment permits
static creation consent but never persists a runtime issuer. Unscoped templates
cannot advertise or authorize Ensure/Create. Only the protected backend's
actual current reservation may supply the issuer.

Projection is immutable: it retains only the receiver's watermark within one
chain. Fresh current-policy/enrollment reloads have no observed issuer watermark.
Durable backend fencing protects across reservations; never cache old consent
as current authority to retain an in-memory check.

Channel source binds the owner signature to the actual original signed SSH
session and role-ordered actual channel IDs, with a client-captured pinned key.
Failed authentication retains only an unauthenticated stream for bounded denial.
Source passed protocol 13, owner/channel 15 and enrollment 12 tests plus four
strict clippy configurations and independent Claude antagonists. Enrollment's
normal contract merge/backport also received a clear narrow merge review.
This does not implement first-connect trust, a listener or native endpoint.

### Cloud management

Cloud source remains the reviewed `0f51bce7`; merged source `a2da40e` passed
setup 49 / 1 existing ignored, client 20, strict package clippy and no-default
check using installed Rust 1.99. `243dc334` records that qualification.
One proven foreign protocol-metadata error was resolved by invalidating only
45 protocol dev artifacts (13.6 MiB); unchanged source then passed. This is not
an explanation for native startup failures.

Prepare-before-submit, retained uncertain requests, explicit foreground
acknowledgment and authoritative GET receipt pinning remain intact. Atomic
generation-conditional DELETE is not established by the official SDK contract;
arbitrary additional headers do not prove server-side If-Match enforcement.
Keep unsupported cleanup explicit and retain CleanupPending. Uninstall of an
acknowledged, unbound uncertain submission still needs read-only exact-resource
adoption; do not request new creation consent merely to uninstall.

### Native services

`a6c2a367` implements Linux MainPID/Result/clean-SIGTERM interpretation,
exact-owned reset-failed/crash cleanup, Windows durable registration reservation
recovery and bounded transitions. macOS enforces Apple Developer ID Application
chain, the release team and non-ad-hoc hardened runtime. Team provenance is the
authenticated release environment's `APPLE_SIGNING_IDENTITY`, consumed by
`.github/workflows/release.yml`; it is not taken from a candidate artifact.
Foreign Apple-binary rejection is not positive fesTerm native qualification.

Windows publication/start/registration/running-stop fail closed pending approved
`fesLabs-CodeSign/fesTermPublicTrust` publisher/certificate/rotation/revocation
policy and an authenticated graceful exact-host stop. Do not invent a pin,
accept any trusted publisher, disable revocation or enable whole-job killing
that terminates persistent children. The user already approved fixing all
previous findings. The corrective bundle still needs the same independent
review gates, Rust/clippy/Windows checking and live native qualification.
Only fmt/diff/trace, packaging and Python 35 / 10 platform skips are recorded.

### Desktop UI

#691 is public metadata management only, not Connect/Create/Trust. Six focused
tests actually passed: five headless interactions plus app persistence. Strict
feature package clippy and no-default check passed; the latter retains three
pre-existing warnings. Antagonist followup cleared CI selection, initial Cancel
focus, Escape/no-delete and stale-record deletion refusal. All five GUI names
match CI's existing `dev_tunnel` selector. No native visual acceptance is claimed.

## Resume safely in the lab

1. Fetch the named branches and verify the full commits above. Read repository
   governance, ADRs 0084/0085 and #681. Do not assume this coordinator checkout
   includes all implementation branches.
2. Re-query pending CI and review gates. Normally integrate main/auth/enrollment
   with the session correction, then consume that correction in terminal routing.
   Review frozen deltas before exposure; do not start a big-bang integration.
3. Use GPT-6.1-sol implementation and Claude Opus 5.5 antagonist review as chosen
   by the user. Reuse existing review context where available; new-machine agents
   cannot assume old background-agent IDs/session state are transferable.
4. Rebuild on the new machine. The old host used Rust 1.99 for desktop/cloud,
   one shared target, jobs=2, dev/test debug=0 and incremental=0. When sharing a
   target between worktrees, serialize whole Cargo/test windows: tests execute
   shared helper binaries after compilation. Separate VMs can use independent
   native targets. Do not copy caches or speculate that real failures are cache.
5. Use isolated fresh installations/accounts and exact-owned test resources for
   native service, vault, boot/reconnect and upgrade/cleanup qualification.
   Windows ARM Parallels is not x64 qualification; new Windows ARM CLI acquisition
   remains unsupported. Retain x64 Windows and Linux coverage plus GitHub CI.
6. Do not copy native-vault credentials, protected journals/enrollment grants or
   existing production application data. Fresh host identity and explicit owner
   enrollment are required. Obtain approved publisher policy/signed test inputs
   without copying private signing keys into the handoff.

Remaining implementation is substantial: actual authenticated listener/framing
and terminal stream composition, runnable setup/service health/update/cleanup,
desktop/mobile live host/session picker/connect/create/resume, foreground owner
trust and native service/boot/vault/physical-device evidence. Saving recipes,
provisioning a journal or green fixture/build CI is not product Ready.

## Old-host preservation and evidence

All implementation source is committed and pushed. Local validation logs were
moved out of source worktrees into session artifacts; they are supplied in the
separate handoff evidence archive. Local Cargo work is released, native test
helpers are reported stopped, and local CI watchers/keep-awake helper are stopped.
Remote GitHub CI may continue; it was not canceled to manufacture a green gate.

The original dirty checkout, unrelated `.DS_Store` changes, unowned protected
busy-test fixtures and existing shared stashes were preserved. Do not blanket
clean, prune worktrees, drop stashes, remove caches or kill helpers by name.
