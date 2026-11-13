#!/usr/bin/env python3
"""Capture sanitized mobile widgets headlessly; never native iOS acceptance."""

import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import subprocess
import uuid

ROOT = Path(__file__).resolve().parents[1]

SURFACES = [
    ("profiles profiles-empty", "shell.rs", "Profiles tab; empty state has no saved rows. Exit: open a profile, New, Sessions or Settings.", "MOB-01"),
    ("sessions sessions-sheet", "shell.rs", "Sessions tab, or terminal All chip → all-sessions sheet. Exit: select a session, Profiles or dismiss the sheet.", "MOB-01"),
    ("terminal terminal-devtunnel disconnected taken-over", "lib.rs, terminal_chrome.rs", "Open an SSH/Dev Tunnel profile; loss/takeover changes its banner. Exit: Back, Close tab, or explicit reconnect. Fixture never connects.", "MOB-03, MOB-06"),
    ("terminal-menu terminal-tab-menu", "lib.rs, tabs.rs", "Terminal More, or touch-and-hold a session chip. Exit: dismiss, choose a command, or explicitly Close tab.", "MOB-01"),
    ("terminal-close terminal-close-plain terminal-close-host", "lib.rs, terminal_chrome.rs", "Session menu → Close (or swipe a Sessions row) → close confirmation. Durable SSH detaches, plain SSH may end its shell, HOST Close is not End. Exit: Cancel or explicit Close tab; no transport exists in this fixture.", "MOB-06, HOST-02, HOST-03"),
    ("keyboard-hold keyboard-shell keyboard-symbols keyboard-preview keyboard-hinge", "terminal_keyboard.rs, key_press.rs", "Terminal More → keyboard choice; globe toggles built-in/custom; symbols key changes layer; hold q shows preview. Wide/hinge layouts split automatically. Exit: release, hide or switch keyboard.", "MOB-02"),
    ("keys-panel keys-fkeys keys-symbols", "terminal_chrome.rs", "Terminal More → Extended keys; choose navigation, F-keys or symbols section. Exit: Done/dismiss.", "MOB-02"),
    ("arrow-helper", "terminal_gesture.rs", "Hold terminal background, then drag up; synthetic touch remains held for capture. Exit: lift finger.", "MOB-02"),
    ("editor-new editor-edit editor-delete editor-error", "forms.rs", "Profiles + creates a profile; row edit affordance edits one; Delete requires confirmation; validation errors stay in the form. Exit: Cancel, save or explicit delete.", "MOB-06"),
    ("editor-field-menu", "widgets.rs, forms.rs", "Touch-and-hold profile Username; release without drag → edit menu; Select All exposes Cut/Copy. Exit: tap elsewhere or choose an action. Capture selects only synthetic text and never copies, cuts, pastes or reads the clipboard.", "MOB-06"),
    ("editor-devtunnel tunnel-rename tunnel-delete", "forms.rs, tunnel_shell.rs", "New/edit profile → Dev Tunnel; listed host Rename or Delete exposes its inline review state. Exit: cancel edit/review or explicitly apply; capture never invokes a service operation.", "MOB-03, MOB-06"),
    ("editor-sftp", "forms.rs", "Profiles + → SFTP; configure remote start folder and authentication. Exit: Cancel or Save.", "SFTPG-01"),
    ("auth-key auth-password auth-key-passphrase", "forms.rs", "Profile Authentication → password or key sheet; encrypted key import asks for passphrase. Exit: Cancel or explicit credential action; all fields empty/synthetic.", "MOB-06"),
    ("host-trust changed-host authentication", "forms.rs, live_ssh.rs", "Explicit SSH connect → first/changed fingerprint review → password request. Exit: Cancel or explicit Trust/authentication decision. Synthetic channel, no SSH worker.", "MOB-06"),
    ("settings settings-bottom settings-lock settings-lock-bottom", "shell.rs, app_lock.rs", "Settings tab; scroll for trusted hosts, lock, diagnostics and About. Lock variants inject an in-memory FakeLocker. Exit: Profiles/Sessions or Back.", "MOB-01, MOB-09"),
    ("recording-settings", "shell.rs", "Settings → Recording settings. Edit naming, pacing, appearance or folder defaults; no picker is invoked. Exit: Back to Settings, Save or discard/reload.", "REC-01"),
    ("recording-consent recording-consent-bottom recording-consent-sessions recording-consent-sessions-bottom recording-message recording-library-empty recording-library-error recording-library-options recording-library-confirm recording-library-complete recording-library-incomplete recording-library-source-confirm recording-library-retry recording-export", "recording_ui.rs, recording_library_ui.rs", "Settings → enable recording; terminal More → Record terminal sessions → privacy/session/export choices. Session-selection variants inject a synthetic two-session roster, not connections. Stop → Export recording. Settings → recording library → retained source, incomplete prefix, unused options, retry or delete confirmation. Exit: Close/Keep/Cancel or explicit confirm. Test cfg renders actual native-owner widgets; bounded sources contain only synthetic terminal output in a fresh repository target directory; native encoding/publication always refuses.", "REC-01, REC-02, REC-05"),
    ("files files-selected files-filter files-options files-error files-connecting files-password files-host-trust files-transfers files-conflict", "files.rs, files_ui.rs, files_shell.rs", "Open an SFTP profile or SSH More → Files. Select rows, Filter, pane Sort and view options, Transfers; conflicting transfer opens conflict decision. Connecting/trust/password/failure share the same Files owner. Exit: dismiss sheet, explicit conflict choice, Back or Close. In-memory listings/transfers only.", "SFTPG-01, SFTPG-04, SFTPG-06"),
    ("unlock unlock-waiting unlock-failure unlock-unavailable unlock-passcode", "app_lock.rs", "Enabled app lock gates content on launch/resume/Lock now. Synthetic FakeLocker supplies pending, unrecognized or unavailable outcomes; passcode variant waits for fake fallback. Exit: successful authentication; unavailable state offers recovery guidance.", "MOB-09"),
    ("signin signin-loading signin-error", "tunnel_link.rs, tunnel_shell.rs", "Dev Tunnel profile → Sign in with GitHub; code, preparing and error variants. Exit: Cancel, or explicit browser/code action (not invoked here).", "MOB-06"),
    ("host-setup host-setup-linux host-setup-windows host-script host-script-linux host-script-windows", "host_setup.rs", "Settings → Set up a host (also available from Dev Tunnel editor); choose macOS/Linux/Windows, View script. Exit: Back to command or Done. Never copies or runs the installer.", "MOB-06"),
    ("hosts-before-discovery hosts-found hosts-sessions hosts-unavailable", "host_clients.rs, host_clients_view.rs", "Profiles → Dev Tunnel hosts (host-clients feature) → Find hosts for the signed-in account → select a host; its sessions list opens beneath it. Unavailable shows a retained host with a timeout refusal. Exit: Back/close the Dev Tunnel window or choose another host. Fixture uses in-memory host/session metadata; no service or native vault is contacted.", "HOST-01, HOST-03"),
    ("hosts-new hosts-profile", "host_clients_view.rs", "Selected host → New session (name a new session) or Save as profile (profile name, session name, create if missing). Exit: Cancel returns to the selected host; nothing is created or connected by capture.", "HOST-02"),
    ("recording-preparing", "recording_ui.rs", "Terminal More → Record terminal sessions → Start recording → preparation sheet/status. Inert display flag, no PrepareTask or worker. Exit: Cancel preparation, Close or completed preparation.", "REC-01"),
    ("recording-active recording-finalizing", "recording_ui.rs", "Explicit Start recording → active status → Stop recording → finalizing status. Inert display flags, no session recording or worker. Exit: Stop recording or completed finalization to export review.", "REC-01"),
    ("recording-export-calculating recording-export-calculated", "recording_ui.rs", "Stop recording / library retry → Export recording → Calculate duration → calculating/estimated-duration review. Inert display values, no PreviewTask or forged ExportPreview. Exit: Keep, dismiss, recalculate or explicitly export in a real session.", "REC-02"),
    ("recording-export-preparing recording-export-progress recording-export-error recording-export-cancel-late", "recording_ui.rs", "Calculated export → Export recording → preparation/progress or retained-source failure/late cancellation status. Inert public display values, no ExportTask/encoder/publication. Exit: Cancel export, completion or Dismiss recording message; late cancel does not undo finalization.", "REC-02"),
    ("recording-library-scanning", "recording_library_ui.rs", "Settings → Recording library → initial scan / Refresh recording library. Inert busy flag, no LibraryTask or worker. Exit: Close or scan completion.", "REC-05"),
    ("recording-library-staging recording-library-staging-confirm", "recording_library_ui.rs", "Settings → Recording library → interrupted private video staging → Delete private staging. Actual bounded empty private staging is created under repository target, with no videos or native encoder. Exit: Close or Cancel/Confirm deletion; gallery dispatch is disabled.", "REC-05"),
    ("recording-library-journal recording-library-journal-published recording-library-journal-confirm", "recording_library_ui.rs", "Android Settings → Recording library → interrupted/unacknowledged export notice → Acknowledge export notice. Actual Android row/confirmation renderer receives public synthetic display values, no ProviderJournal, provider grant or publication receipt. Exit: Close or Cancel/Confirm acknowledgment; gallery dispatch is disabled.", "REC-05"),
    ("recording-library-retry-recovered", "recording_library_ui.rs", "Settings → Recording library → incomplete source with validated prefix → Export recovered prefix. Test-only draft invokes the actual recovered-prefix retry sheet; no native task is created. Exit: Cancel or explicit export in a real session.", "REC-05"),
]

DOCUMENT_SURFACES = [
    ("launcher profiles sessions", "Documents from terminal More (launcher), or Profiles/Sessions tabs with a retained document. Exit: choose a document/session or another tab.", "EDIT-02, MOB-01"),
    ("source dirty read-only custom-source unavailable document-error", "Documents → open text/Markdown; Edit shows source, a duplicated dirty view shares edits, custom-source shows the terminal keyboard; revoked access/read-only/error preserve honest status. Exit: Preview, Back or Close document view.", "EDIT-02, EDIT-09, EDIT-12"),
    ("conflict", "External change while edited → Compare / resolve → read-only external version with Keep edits/Reload. Exit: dismiss or explicit decision.", "EDIT-11"),
    ("dirty-close", "Close the last dirty document view → Unsaved document. Exit: Cancel, Save or Discard changes.", "EDIT-09"),
    ("preview contents wide-preview preview-menu preview-selected", "Document Preview; Contents opens heading navigation on narrow layouts; wide preview has a rail. Secondary click opens the production preview selection menu; Select All paints a selection. Exit: dismiss/menu action, choose heading or Edit.", "MD-01, MD-02"),
    ("tab-menu document-menu", "Touch-and-hold a document chip, or document More. Exit: dismiss or explicit New view / Refresh source / Close document view.", "EDIT-09"),
    ("dictation-listening dictation-finishing dictation-review dictation-long dictation-native dictation-failed terminal-dictation", "Document source Mic (or terminal Mic) → speech draft; synthetic phases cover listening, finishing, review, long draft and denied permission; native variant uses the accessory row only. Exit: Cancel or Insert. No microphone, speech service or OS consent is invoked.", "MOB-07, MOB-08"),
]

def source_fingerprint(sources):
    return hashlib.sha256(
        json.dumps(sources, sort_keys=True, separators=(",", ":")).encode()
    ).hexdigest()


def source_snapshot():
    """Include dirty/untracked rendering inputs; exclude generated review pictures."""
    paths = subprocess.check_output(
        ["git", "ls-files", "-z", "--cached", "--others", "--exclude-standard", "--",
         "Cargo.toml", "Cargo.lock", ".cargo", "app/festerm-mobile", "crates",
         "assets", "vendor", "scripts/capture-mobile-ui.py"],
        cwd=ROOT,
    ).decode().split("\0")
    sources = {}
    for name in sorted(set(filter(None, paths))):
        path = ROOT / name
        if not path.is_file():
            continue
        if (name.startswith(("crates/", "vendor/"))
                and path.suffix not in {".rs", ".toml", ".wgsl", ".glsl", ".ttf", ".otf"}):
            continue
        if name.startswith("app/festerm-mobile/") and path.suffix not in {".rs", ".toml"}:
            continue
        sources[name] = hashlib.sha256(path.read_bytes()).hexdigest()
    return sources


def equivalence_reason(members):
    """Classify byte equality only where the actual fixture/layout explains it."""
    states = {member["state"] for member in members}
    sizes = {tuple(member["size"]) for member in members}
    kinds = {member["kind"] for member in members}
    no_native_occlusion = {
        "launcher", "read-only", "custom-source", "dictation-listening",
        "dictation-finishing", "dictation-review", "dictation-long",
        "dictation-failed", "terminal-dictation",
    }
    if kinds == {"documents"} and len(states) == 1 and states <= no_native_occlusion:
        if sizes == {(390, 844)}:
            return ("Same 390 × 844 geometry and zero effective native-keyboard occlusion. "
                    "`documents_tests.rs::capture_document_ui` explicitly clears occlusion "
                    "for launcher/read-only/custom-keyboard states. The phone-keyboard family "
                    "does not force a native keyboard. This is one presentation, not separate "
                    "native-keyboard evidence.")
    if kinds == {"documents"} and states == {"preview", "contents"} and sizes == {(1194, 834)}:
        return ("Wide preview already displays the Contents rail. "
                "`documents.rs::show_contents` clears a requested Contents sheet when "
                "`wide_preview` is true; the toolbar also omits that narrow-layout action. "
                "The tablet-contents request therefore normalizes to the same rail presentation; "
                "phone/narrow Contents sheets are captured separately.")
    if kinds == {"mobile"} and states == {"settings", "settings-lock"} and sizes == {(390, 844)}:
        return ("The phone's initial Settings viewport ends before App lock. Injecting FakeLocker "
                "changes only the lower section, not these visible pixels; the repeated split-gallery "
                "phone Settings control has identical geometry. Actual lock controls are visible in "
                "`tablet-settings-lock-bottom.png`, with phone timeout/Lock now controls in "
                "`phone-settings-lock-bottom.png`. These top views are not distinct lock screens.")
    if kinds == {"mobile"} and states == {"settings", "settings-lock"} and sizes == {(834, 1194)}:
        return ("The portrait tablet's initial Settings viewport now ends at Dev Tunnel hosts, "
                "before App lock. Injecting FakeLocker changes only the lower section, not these "
                "visible pixels. Actual lock controls are visible in "
                "`tablet-settings-lock-bottom.png`; this top view is not a distinct lock screen.")
    if kinds == {"mobile"} and states <= {"keyboard-shell", "keyboard-hinge"}:
        if sizes == {(390, 844)}:
            return ("Same Shell row keyboard at phone geometry, including the repeated gallery control. "
                    "`terminal_keyboard.rs::split_halves` rejects the hinge split because each half "
                    "would be narrower than MIN_HALF (240 points); Auto is also unsplit here. "
                    "The hinge-labelled fixture is a geometry request, not a different visible keyboard.")
        if states == {"keyboard-shell", "keyboard-hinge"} and sizes <= {(1194, 834), (1114, 720)}:
            return ("Auto landscape split and the centred synthetic hinge both cap each half at "
                    "MAX_HALF (420 points) in `terminal_keyboard.rs::split_halves`. The existing "
                    "centre gap already exceeds hinge plus clearance, so the resulting rectangles "
                    "are identical. This is equivalent visible geometry, not independent evidence "
                    "of a different hinge layout or a real device.")
    if kinds == {"mobile"} and len(states) == 1 and states <= {"keyboard-hold", "keyboard-symbols"}:
        if sizes == {(390, 844)}:
            return ("The shell gallery and split-keyboard gallery intentionally repeat the same "
                    "phone keyboard control: identical geometry, keyboard kind/layer and zero "
                    "native-keyboard occlusion. It counts once as visible content.")
    if kinds == {"mobile"} and states == {"unlock-waiting", "unlock-passcode"}:
        return ("Both fake authentication methods are deferred in LockState::Unlocking. "
                "`app_lock.rs::LockHost::gate` renders the same shared waiting screen; the "
                "biometric-versus-passcode distinction belongs to the native authentication "
                "prompt, which is not pictured. Two requests do not establish two native prompts.")
    raise ValueError(f"Unclassified identical presentations: {sorted(member['relative'] for member in members)}")


def write_catalog(output, destination):
    """Fail closed if a capture lacks a source/route or its image is missing."""
    catalog = {}
    for states, source, route, edges in SURFACES:
        for state in states.split():
            catalog[("mobile", state)] = (source, route, edges)
    for states, route, edges in DOCUMENT_SURFACES:
        for state in states.split():
            catalog[("documents", state)] = ("documents.rs, documents_tests.rs", route, edges)
    rows = []
    manifests = []
    presentations = {}
    for manifest_path in sorted(output.rglob("manifest.json")):
        manifest = json.loads(manifest_path.read_text())
        if source_fingerprint(manifest["source_files_sha256"]) != manifest["source_fingerprint_sha256"]:
            raise ValueError(f"Source fingerprint mismatch: {manifest_path}")
        manifests.append(manifest)
        kind = "documents" if manifest_path.parent.name == "documents" else "mobile"
        for image in manifest["images"]:
            state = image["state"]
            source, route, edges = catalog[(kind, state)]
            entry, separator, exit_route = route.partition("Exit:")
            if not separator or not entry.strip() or not exit_route.strip():
                raise ValueError(f"Missing explicit entry/exit route: {state}")
            path = manifest_path.parent / image["image"]
            if hashlib.sha256(path.read_bytes()).hexdigest() != image["sha256"]:
                raise ValueError(f"Image hash mismatch: {path}")
            relative = Path(os.path.relpath(path, destination.parent)).as_posix()
            source_links = ", ".join(
                f"[{name}](../app/festerm-mobile/src/{name})" for name in source.split(", ")
            )
            size = " × ".join(str(int(v)) for v in image["logical_size"])
            row = f"| [{image['image']}]({relative}) | {size} | {state} | {source_links} | {entry.strip()} | {exit_route.strip()} | {edges}"
            member = {
                "relative": path.relative_to(output).as_posix(), "kind": kind,
                "state": state, "size": image["logical_size"],
            }
            rows.append((row, member["relative"]))
            presentations.setdefault(image["sha256"], []).append(member)
    duplicates = sorted(
        (members for members in presentations.values() if len(members) > 1),
        key=lambda members: min(member["relative"] for member in members),
    )
    equivalents = {}
    equivalence_rows = []
    for number, members in enumerate(duplicates, 1):
        identifier = f"EQ-{number:02}"
        reason = equivalence_reason(members)
        paths = sorted(member["relative"] for member in members)
        for path in paths:
            equivalents[path] = identifier
        equivalence_rows.append(
            f"| {identifier} | " + "; ".join(f"`{path}`" for path in paths)
            + f" | {reason} |"
        )
    rendered_rows = [
        row + f" | {equivalents.get(path, '—')} |" for row, path in rows
    ]
    commits = sorted({manifest["source_commit"] for manifest in manifests})
    fingerprints = sorted({manifest["source_fingerprint_sha256"] for manifest in manifests})
    if len(fingerprints) != 1:
        raise ValueError("Mixed source fingerprints cannot form one unchanged-source capture set")
    text = f"""# State of mobile UI

Fresh synthetic review set: **{len(rows)} PNGs**, source HEAD `{', '.join(commits)}`.
The working tree contained the test-only gallery additions at capture time;
each manifest records `source_dirty`. This is a finite production-surface catalog,
not every possible content, scroll offset, connection outcome or gesture combination.

Unchanged-source fingerprint (SHA-256 of the canonical sorted per-file SHA-256 map):
`{', '.join(fingerprints)}`. The capture script hashes working-tree mobile sources,
workspace crate/vendor code, manifests/lockfile and rendering assets **before and
after all captures**, refusing attribution if any input changes. Thus dirty files
are identified by their actual contents, not merely the base Git SHA. Each of the
four manifests retains that complete input map plus every PNG's dimensions/hash.

## What these pictures establish

Actual shared `MobileApp` egui widgets rendered by `egui_kittest` headless wgpu
on macOS, with dark presentation and repository-owned synthetic state.
Phone: 390 × 844; narrow phone: 320 × 568; short/landscape: 844 × 390;
tablet: 834 × 1194 or 1194 × 834; dual screen: 1114 × 720 with a simulated hinge.
Sizes are logical points; manifests additionally record PNG pixel dimensions,
hashes and simulated keyboard occlusion. System keyboards are **not** rendered.
All image routes below describe user navigation, not actions executed during capture.
Stable IDs refer to [the GUI action graph](gui-action-graph.md); they are workflow
associations, not new acceptance evidence.

No personal configuration, credentials, actual connections, clipboard reads,
native Files grants, installed app or external service was used. HOST End review
uses only fixed synthetic public target/operation IDs, never an approved context
or End intent; its checkbox is unchecked and no End is sent.
HOST receipt/original/Forget pictures use the same
production presentation helpers with public display data only: no Receipt,
ReviewedForget, native owner key, protected checkpoint or grant is manufactured;
all their action dispatch is disabled. Recording task-only pictures use inert
display flags/progress/duration, never task handles or native workers. Android
export-notice pictures exercise the actual Android presentation helper without
constructing a provider journal or publication receipt. Recording fixtures use
an unused repository-local desktop destination placeholder, not manufactured
iOS bookmark bytes or Android provider grants; it is never exported to.

## iOS / Android distinction and explicit gaps

These are shared mobile widget screenshots, **not native iOS or Android captures**.
Desktop compilation selects the iOS wording fallback (`platform_text`); it does
not prove UIKit appearance. Android-specific wording, IME composition, system
bars/cutouts, permissions, file import/export and biometric sheets need Android
runtime captures. Portrait/tablet/hinge geometry is simulated, not device evidence.

Implemented surfaces not capturable by this desktop headless path are listed
explicitly rather than replaced with invented mockups:

| Surface | User route / exit | Exact prerequisite, source function and GUI IDs |
| --- | --- | --- |
| Native file picker, iOS document-provider grants, Android Open/import and SAF folder selection | Documents → Browse in Files / Open or import; Settings recording folder / export folder → system picker; exit Cancel/choose | Requires a foreground native window and genuine user-selected OS provider. `documents.rs::browse`, `festerm-ios-window/src/document_ios.rs::DocumentPicker::open`, `recording_folder_ios.rs::RecordingFolderPicker::open`; Android `native.rs::pick_document` / `pick_recording_folder`. EDIT-02, REC-01. No picker/grant is invoked or invented. |
| Native biometric/passcode prompt and app-switcher privacy cover | Settings → App lock → enable/Lock now or background/resume; exit authenticate/cancel | Requires a native window and device authentication/passcode capability. `app_lock.rs::LockHost::gate`; `festerm-ios-lock/src/ios.rs::LaContextLocker::authenticate`, `LockBridge::sync`; Android `native.rs::authenticate` / `privacy`. MOB-09. Surrounding shared unlock/settings widgets are pictured; fake lock outcomes do not prove OS authentication. |
| Microphone/speech consent, system speech capture, native keyboard and hardware IME | Mic / focus text field; exit Cancel/Insert/hide keyboard | Requires native speech permission/service and native keyboard/IME input. `dictation.rs::begin_dictation` / `show_dictation`, `festerm-ios-window/src/speech.rs`; Android `native.rs::speech_command` / `keyboard_mode`. MOB-02/07/08. Draft phases are pictured; no microphone or permission is requested. |
| Native recording encoder/folder picker/provider publication and genuine native-task execution (shared preparation/active/finalizing/calculation/progress/error/scanning/staging/journal widgets **are pictured**) | Enable in Settings → Record terminal sessions → Start → Stop → Calculate / Export; exit Cancel/Keep or finish | Requires native `RecordingFolderPicker`, Apple/Android encoder and a genuine user-selected destination. `recording_ui.rs::prepare_recording`, `start_recording_preview`, `start_confirmed_recording_export`; `recording_tasks.rs::encoder_factory`, `preflight`, `publish`. REC-01/02/05. Inert display evidence is not native task/lifecycle acceptance. |
| Genuine protected HOST receipt/original recovery and Forget dispatch (not pictured; the redesigned host window renders these only from genuine protected state) | Hosts → Prepare/load native owner → approve exact host → reopen originals → review/Forget; exit Cancel or explicit authorized confirmation | Legal memory-vault setup is blocked specifically by `/Volumes/Storage` being mode `0775`: `ReceiptIndex::in_app_sandbox` → `approval_storage::trusted_directory` → `festerm_unix_security::trusted_private_storage_ancestor` rejects `mode & 0o022 != 0`, mapped to `Failure::OriginalStorage`. A trusted existing sandbox whose every ancestor passes owner/mode/ACL validation is required for `originals::testing::memory_fixture`; genuine native evidence additionally requires the native vault and approved HOST. The gallery does not change mount/OS permissions, substitute another path or bypass validation. `host_clients.rs::host_action`, `show_host_recovery`; `host_clients_view.rs::show_hosts_in_bounds`; HOST-01/02/03. |
| Native touch selection handles, system text edit menu, share/export/provider error UI, accessibility overlays and device rotation/keyboard animation | Long press selection / invoke OS action; dismiss or return | Requires actual native touch/IME/accessibility/provider and window events. `terminal_gesture.rs`, `widgets.rs`, `documents.rs` and `festerm-ios-window` adapters; MOB-02, EDIT-09, MD-02. Shared field/preview menus are pictured; synthetic pointer input is not evidence of native OS handles/overlays. |

No mobile serial, local PTY launcher, enterprise/PowerShell screen, desktop
inspector, arbitrary untitled-file editor, scripting or plugin UI is implemented
by `MobileApp`; desktop screenshots must not be relabeled as mobile.
Experimental status and native acceptance gates remain in
[manual validation](manual-validation.md) and [mobile host README](../app/festerm-mobile/README.md).

## Reproduce

From the repository root, use a **new** directory (existing paths are refused):

```sh
CARGO_BUILD_JOBS=2 python3 scripts/capture-mobile-ui.py --output docs/images/ui-state-mobile-fresh --index-output docs/state-of-mobile-ui-fresh.md
```

The script runs the four ignored actual-widget generators with `--features
host-clients`, verifies all PNG hashes, and requires a catalog route for each
image. Captures do not qualify native iOS/Android behavior.

## Identical presentations and fixture requests

The **{len(rows)} PNG files contain {len(presentations)} distinct byte presentations**.
There are **{len(duplicates)} duplicate groups**; their repeated requests are retained
for stable viewport/state routes, not counted as additional visible surfaces.
The State column names the requested fixture state, which may normalize to an
already-visible production layout. Equal pixels do not imply equal model state,
native authentication method or independent native-platform evidence.

The generator groups verified PNG hashes and requires an explicit layout/fixture
explanation for every duplicate group; an unclassified duplicate fails catalog
generation. This reconciliation found no duplicate hiding a missing implemented
surface. Source/geometry explanations and distinct companion views are below.

| Equivalence | Identical files (relative to gallery root) | Why equivalent / where the distinct surface appears |
| --- | --- | --- |
""" + "\n".join(equivalence_rows) + f"""

## Screenshots and navigation

Each PNG is linked exactly once, including keyboard-occluded, narrow and landscape
document variants. Lower-region pictures intentionally show a different region
of the same production surface; calculated/calculating export captures scroll to
the estimate. HOST fixtures initialize the actual resizable egui Window near the
viewport's full size, equivalent to enlarging it before review, so headings and
confirmations remain visible. Menus are opened by synthetic pointer input.

| Screenshot | Viewport (points) | State / fixture request | Production source | Entry | Exit | GUI IDs | Equivalence |
| --- | --- | --- | --- | --- | --- | --- | --- |
""" + "\n".join(rendered_rows) + "\n"
    destination.parent.mkdir(parents=True, exist_ok=True)
    destination.write_text(text)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, help="new output directory; existing paths are refused")
    parser.add_argument("--index-output", type=Path, help="write the source/navigation catalog after verifying captures")
    args = parser.parse_args(argv)
    output = (args.output or ROOT / "target/mobile-ui-captures" / uuid.uuid4().hex).resolve()
    if output.exists():
        parser.error("output already exists; retain previous captures and use a fresh directory")
    output.parent.mkdir(parents=True, exist_ok=True)
    scratch = ROOT / "target/mobile-ui-scratch"
    scratch.mkdir(parents=True, exist_ok=True)
    sources = source_snapshot()
    # The Rust generator only constructs controlled in-memory MobileApp fixtures.
    # It never constructs the native host or connects a session.
    command = ["cargo", "test", "--locked", "-p", "festerm-mobile",
               "--features", "host-clients", "--lib"]
    for test, variable, destination in [
        ("ui_gallery::capture_mobile_ui_gallery", "FESTERM_MOBILE_UI_OUT", output),
        ("documents::tests::capture_document_ui", "FESTERM_DOCUMENT_UI_OUT", output / "documents"),
        ("ui_gallery::capture_split_keyboard_ui_gallery", "FESTERM_MOBILE_UI_OUT", output / "split-keyboard"),
        ("ui_gallery::capture_close_ui_gallery", "FESTERM_MOBILE_UI_OUT", output / "close-session"),
        ("ui_gallery::capture_host_client_ui_gallery", "FESTERM_MOBILE_UI_OUT", output / "hosts"),
    ]:
        subprocess.run(
            command + [test, "--", "--ignored", "--exact"],
            cwd=ROOT, env=dict(
                os.environ, TMPDIR=str(scratch), TMP=str(scratch), TEMP=str(scratch),
                **{variable: str(destination)},
            ),
            check=True, timeout=600,
        )
    if source_snapshot() != sources:
        raise RuntimeError("Capture inputs changed during rendering; retain this attempt and rerun from unchanged sources")
    for path in sorted(output.rglob("manifest.json")):
        manifest = json.loads(path.read_text())
        manifest.update({
            "captured_at_utc": datetime.now(timezone.utc).isoformat(),
            "native_android": False,
            "features": ["host-clients"],
            "source_files_sha256": sources,
            "source_fingerprint_sha256": source_fingerprint(sources),
        })
        path.write_text(json.dumps(manifest, indent=2) + "\n")
    if args.index_output:
        write_catalog(output, args.index_output.resolve())
    print(output)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
