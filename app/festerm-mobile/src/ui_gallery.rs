//! Review images of the actual mobile UI, never native iOS acceptance.
//! Only offline fixtures, navigation and synthetic trust/auth prompts are used: no sockets,
//! credentials, clipboard reads or personal filesystem resources.

use super::*;
use crate::files_fixture::{fixture_entry, transfer_rows, FixtureFs, FixtureTransfers};
use crate::model::{
    Capabilities, ChromeModel, ConnectionLine, DevTunnelChoices, ProfileKind, Reattachable,
    StaticSessions, SurfaceKind, SurfaceState, SurfaceSummary,
};
use egui_kittest::Harness;
use festerm_mobile_profiles::{RemoteSession, SshAuth, SshProfile};
use sha2::{Digest, Sha256};
use std::{fs, path::PathBuf, process::Command, sync::Arc, time::Duration};

fn git_fact(arguments: &[&str]) -> String {
    let output = Command::new("git")
        .args(arguments)
        .current_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .output()
        .expect("git source attribution");
    assert!(output.status.success(), "git source attribution failed");
    String::from_utf8(output.stdout)
        .expect("UTF-8 git attribution")
        .trim()
        .to_owned()
}

const STATES: [&str; 36] = [
    "profiles",
    "profiles-empty",
    "terminal",
    "terminal-devtunnel",
    "keyboard-hold",
    "keyboard-shell",
    "keyboard-symbols",
    "keyboard-preview",
    "sessions-sheet",
    "keys-panel",
    "keys-fkeys",
    "keys-symbols",
    "arrow-helper",
    "editor-new",
    "editor-devtunnel",
    "host-trust",
    "changed-host",
    "authentication",
    "disconnected",
    "editor-edit",
    "auth-key",
    "settings",
    "files",
    "files-transfers",
    "files-conflict",
    "unlock",
    "unlock-waiting",
    "unlock-failure",
    "unlock-unavailable",
    "unlock-passcode",
    "settings-lock",
    "signin",
    "taken-over",
    "host-setup",
    "host-setup-linux",
    "host-script",
];

fn fixture_rows() -> Vec<model::Profile> {
    [
        ("prod-web", ProfileKind::Ssh, "web.example.net", "deploy"),
        ("build-box", ProfileKind::DevTunnel, "build-tunnel", "dev"),
        ("staging-db", ProfileKind::Ssh, "db.example.net", "ops"),
        (
            "release-files",
            ProfileKind::Sftp,
            "files.example.net",
            "ops",
        ),
    ]
    .into_iter()
    .enumerate()
    .map(|(index, (name, kind, host, user))| model::Profile {
        id: format!("fixture-{index}"),
        name: name.to_owned(),
        kind,
        detail: model::profile_detail(kind, host, user),
        last_used: None,
    })
    .collect()
}

fn fixture_ssh(auth: SshAuth, remote_session: RemoteSession) -> SshProfile {
    SshProfile {
        host: "web.example.net".to_owned(),
        port: 22,
        username: "deploy".to_owned(),
        auth,
        remote_session,
        recovery: Default::default(),
    }
}

fn fixture_spec() -> profiles::SessionSpec {
    profiles::SessionSpec {
        profile: None,
        name: "prod-web".to_owned(),
        ssh: fixture_ssh(
            SshAuth::Ask,
            RemoteSession::Tmux {
                name: "work".to_owned(),
            },
        ),
    }
}

fn fixture_sessions(current: u64) -> StaticSessions {
    let surface = |id, title: &str, kind, detail: &str, status: &str, state| SurfaceSummary {
        id,
        title: title.to_owned(),
        kind,
        detail: detail.to_owned(),
        state,
        status: status.to_owned(),
        current: id == current,
        unread: id == 2 && current != 2,
    };
    StaticSessions {
        surfaces: vec![
            surface(
                1,
                "prod-web",
                SurfaceKind::Ssh,
                "SSH · deploy@web.example.net",
                "Connected · tmux / work",
                SurfaceState::Connected,
            ),
            surface(
                2,
                "build-box",
                SurfaceKind::DevTunnel,
                "Dev Tunnel · build-tunnel",
                "Connected · tmux / build",
                SurfaceState::Connected,
            ),
            surface(
                3,
                "release-files",
                SurfaceKind::Files,
                "SFTP · ops@files.example.net",
                "Disconnected · history available",
                SurfaceState::Disconnected,
            ),
        ],
        reattachable: vec![Reattachable {
            title: "tmux / deploy".to_owned(),
            detail: "3 windows · last active 2h ago".to_owned(),
            host: "prod-web".to_owned(),
        }],
    }
}

fn fixture_files(rows: Vec<files::TransferRow>, ctx: &egui::Context) -> files::FilesSession {
    const DAY: i64 = 86_400;
    let base = 1_782_000_000;
    let remote = FixtureFs {
        kind: festerm_sftp::PaneKind::Remote,
        home: "/srv/app/releases",
        entries: vec![
            fixture_entry("current", true, 0, base),
            fixture_entry("previous", true, 0, base - DAY),
            fixture_entry("app-2.4.1.tar.gz", false, 38_400_000, base - 2 * DAY),
            fixture_entry("README.md", false, 4_200, base - 3 * DAY),
            fixture_entry("deploy.log", false, 182_000, base - 4 * DAY),
            fixture_entry("config.yaml", false, 2_100, base - 5 * DAY),
        ],
    };
    let local = FixtureFs {
        kind: festerm_sftp::PaneKind::Local,
        home: "",
        entries: vec![
            fixture_entry("Archive", true, 0, base),
            fixture_entry("release-notes.md", false, 6_100, base - DAY),
            fixture_entry("app-2.4.2.tar.gz", false, 38_900_000, base - 2 * DAY),
            fixture_entry("screenshot.png", false, 912_000, base - 6 * DAY),
        ],
    };
    let mut session = files::FilesSession::ready(
        "release-files".to_owned(),
        "SFTP · ops@files.example.net".to_owned(),
        files::Ready {
            remote: Arc::new(remote),
            local: Arc::new(local),
            local_root: PathBuf::from("/nonexistent-festerm-gallery"),
            transfers: Arc::new(FixtureTransfers::new(rows)),
            start_dir: None,
        },
        ctx,
    );
    // Listings load on a worker; wait so the capture is deterministic.
    for _ in 0..200 {
        session.poll(ctx);
        let loading = [session.remote.as_ref(), session.local.as_ref()]
            .into_iter()
            .flatten()
            .any(|pane| pane.loading());
        if !loading {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    session
}

fn fixture_terminal_text(lines: &[&str]) -> Terminal {
    let mut terminal = Terminal::with_scrollback_limit(
        Dimensions::new(48, 24).expect("fixed valid dimensions"),
        SCROLLBACK_LIMIT,
    )
    .expect("bounded fixture grid");
    for line in lines {
        terminal.ingest(line.as_bytes());
        terminal.ingest(b"\r\n");
    }
    terminal
}

fn configure(app: &mut MobileApp, state: &str) {
    app.caps = Capabilities::all();
    app.profiles = Some(profiles::testing::profiles());
    app.rows_override = Some(fixture_rows());
    let devtunnel = matches!(state, "terminal-devtunnel");
    app.sessions_override = Some(fixture_sessions(if devtunnel { 2 } else { 1 }));
    app.surface = Surface::Terminal;
    app.chrome_override = Some(if devtunnel {
        ChromeModel {
            title: "build-box".to_owned(),
            subtitle: "Dev Tunnel · host session".to_owned(),
            line: ConnectionLine {
                state: SurfaceState::Connected,
                label: "Connected".to_owned(),
                context: Some("tmux · build".to_owned()),
            },
        }
    } else {
        ChromeModel {
            title: "prod-web".to_owned(),
            subtitle: "SSH · deploy@web.example.net".to_owned(),
            line: ConnectionLine {
                state: SurfaceState::Connected,
                label: "Connected".to_owned(),
                context: Some("tmux · work".to_owned()),
            },
        }
    });
    app.terminal = fixture_terminal_text(&[
        "\x1b[32mdeploy@prod-web\x1b[0m:\x1b[34m~/app\x1b[0m$ git status",
        "On branch main",
        "Your branch is up to date with 'origin/main'.",
        "",
        "nothing to commit, working tree clean",
        "\x1b[32mdeploy@prod-web\x1b[0m:\x1b[34m~/app\x1b[0m$ systemctl status web",
        "\x1b[32m●\x1b[0m web.service - Web frontend",
        "     Active: \x1b[32mactive (running)\x1b[0m since Mon 09:14",
        "   Main PID: 2210 (node)",
        "\x1b[32mdeploy@prod-web\x1b[0m:\x1b[34m~/app\x1b[0m$ ",
    ]);
    match state {
        "keyboard-hold" | "keyboard-shell" | "keyboard-symbols" | "keyboard-preview"
        | "keyboard-hinge" => {
            if state == "keyboard-hinge" {
                // Surface Duo-style spanned window: two 540 pt screens, 34 pt hinge.
                app.terminal_keyboard.hinge = Some(terminal_keyboard::Hinge {
                    left: 540.0 / 1114.0,
                    right: 574.0 / 1114.0,
                });
            }
            app.select_keyboard(if matches!(state, "keyboard-shell" | "keyboard-hinge") {
                terminal_keyboard::KeyboardKind::ShellRow
            } else {
                terminal_keyboard::KeyboardKind::HoldSymbols
            });
            app.terminal_keyboard.symbols = state == "keyboard-symbols";
        }
        "profiles" => {
            app.surface = Surface::Tabs;
        }
        "profiles-empty" => {
            app.surface = Surface::Tabs;
            app.caps = Capabilities::default();
            app.rows_override = None;
            app.sessions_override = Some(StaticSessions::default());
        }
        "settings" => {
            app.surface = Surface::Tabs;
            app.shell.tab = shell::Tab::Settings;
        }
        "unlock" | "unlock-waiting" | "unlock-failure" | "unlock-unavailable"
        | "unlock-passcode" | "settings-lock" => {
            use festerm_ios_lock::{
                AppLock, AuthFailure, Availability, BiometryKind, FakeLocker, LockSettings,
                ManualClock,
            };
            let locker = FakeLocker::new(Availability::Available(BiometryKind::FaceId));
            let lock = AppLock::new(None, locker.clone(), Arc::new(ManualClock::new()), || {});
            lock.apply_settings(LockSettings {
                enabled: true,
                ..LockSettings::default()
            })
            .expect("fake locker can authenticate");
            if state.starts_with("unlock") {
                lock.lock_now();
                match state {
                    "unlock-waiting" | "unlock-passcode" => locker.set_deferred(true),
                    "unlock-failure" => locker.push_outcome(Err(AuthFailure::NotRecognized)),
                    "unlock-unavailable" => locker.push_outcome(Err(AuthFailure::Unavailable)),
                    _ => {}
                }
                if state == "unlock-passcode" {
                    lock.begin_passcode_unlock();
                }
            } else {
                app.surface = Surface::Tabs;
                app.shell.tab = shell::Tab::Settings;
            }
            app.lock = Some(app_lock::LockHost::new(lock, None));
        }
        "sessions-sheet" => app.shell.sessions_sheet = true,
        "keys-panel" => app.keys_panel.open = true,
        "keys-fkeys" => {
            app.keys_panel.open = true;
            app.keys_panel.tab = 1;
        }
        "keys-symbols" => {
            app.keys_panel.open = true;
            app.keys_panel.tab = 2;
        }
        "editor-new" => app.editor.open_new(),
        "editor-devtunnel" => {
            app.editor.open_new();
            app.editor.kind_index = 1;
            app.editor.name = "build-box".to_owned();
            app.tunnel.preview_editor = true;
            app.editor.tunnel_name = "build-tunnel".to_owned();
            app.editor.take_over = true;
            app.editor.auto_reconnect = true;
            app.editor.tunnel = Some(DevTunnelChoices {
                account_login: Some("octo".to_owned()),
                tunnels: vec![
                    super::model::DevTunnelChoice {
                        locator: "build-tunnel.usa".to_owned(),
                        name: Some("build-tunnel".to_owned()),
                        description: Some("Build box".to_owned()),
                    },
                    super::model::DevTunnelChoice {
                        locator: "docs-tunnel.usa".to_owned(),
                        name: Some("docs-tunnel".to_owned()),
                        description: Some("Docs server".to_owned()),
                    },
                ],
                ..DevTunnelChoices::default()
            });
        }
        "host-trust" | "changed-host" | "authentication" => {
            *app.sink.live.connection.borrow_mut() = Some(live_ssh::prompt_fixture(
                state == "changed-host",
                state == "authentication",
            ));
            app.chrome_override = None;
            app.terminal = fixture_terminal_text(&["Connecting…"]);
        }
        "disconnected" => {
            app.fixture_ssh_tab(fixture_spec());
            *app.sink.live.connection.borrow_mut() = Some(live_ssh::disconnected_fixture());
            app.chrome_override = Some(ChromeModel {
                title: "prod-web".to_owned(),
                subtitle: "SSH · deploy@web.example.net".to_owned(),
                line: ConnectionLine {
                    state: SurfaceState::Disconnected,
                    label: "Connection lost".to_owned(),
                    context: Some("tmux · work".to_owned()),
                },
            });
        }
        "signin" => {
            app.editor.open_new();
            app.editor.kind_index = 1;
            app.editor.name = "build-box".to_owned();
            app.editor.take_over = true;
            app.editor.auto_reconnect = true;
            app.tunnel.preview_prompt = Some(festerm_mobile_tunnels::SignInPrompt {
                user_code: "ABCD-1234".to_owned(),
                verification_url: "https://github.com/login/device".to_owned(),
                expires_in: Duration::from_secs(900),
            });
        }
        "taken-over" => {
            app.fixture_ssh_tab(fixture_spec());
            if let Some(tab) = app.tabs.last_mut() {
                tab.tunnel = Some(profiles::TunnelSpec {
                    profile: festerm_mobile_profiles::ProfileId::generate(),
                    name: "build-box".to_owned(),
                    account: None,
                    tunnel: "build-tunnel".to_owned(),
                    session: None,
                    take_over: true,
                    auto_reconnect: true,
                });
                tab.spec = None;
            }
            *app.sink.live.connection.borrow_mut() = Some(live_ssh::taken_over_fixture());
            app.chrome_override = Some(ChromeModel {
                title: "build-box".to_owned(),
                subtitle: "Dev Tunnel · host session".to_owned(),
                line: ConnectionLine {
                    state: SurfaceState::Disconnected,
                    label: "Taken over by another device".to_owned(),
                    context: Some("host session".to_owned()),
                },
            });
        }
        "host-setup" | "host-setup-linux" | "host-script" => {
            app.editor.open_new();
            app.editor.kind_index = 1;
            app.editor.name = "build-box".to_owned();
            app.host_setup.open();
            app.host_setup.platform_index = if state == "host-setup" { 0 } else { 1 };
            app.host_setup.install_dependencies = state == "host-setup-linux";
            app.host_setup.takeover = state != "host-setup-linux";
            app.host_setup.viewing = state == "host-script";
        }
        "editor-edit" => {
            let id = app
                .profiles
                .as_mut()
                .expect("fixture profiles")
                .save_ssh(
                    None,
                    "prod-web",
                    fixture_ssh(
                        SshAuth::Ask,
                        RemoteSession::Tmux {
                            name: "work".to_owned(),
                        },
                    ),
                )
                .expect("save fixture profile");
            app.edit_profile(&id);
        }
        "files" | "files-transfers" | "files-conflict" => {
            app.fixture_ssh_tab(fixture_spec());
            let rows = if state == "files" {
                Vec::new()
            } else {
                transfer_rows(state == "files-conflict")
            };
            let ctx = egui::Context::default();
            let mut session = fixture_files(rows, &ctx);
            session.transfers_sheet = state == "files-transfers";
            app.files = Some(Box::new(session));
            app.workspace = workspaces::Workspace::Files;
        }
        "auth-key" => {
            app.editor.open_new();
            app.editor.host = "web.example.net".to_owned();
            app.editor.username = "deploy".to_owned();
            app.editor.auth_mode = forms::AuthMode::Key;
            app.editor.auth_sheet = true;
            app.editor.durable_credentials = true;
        }
        _ => {}
    }
}

#[test]
#[ignore = "explicit screenshot generation requires a working headless wgpu adapter"]
fn capture_mobile_ui_gallery() {
    capture_gallery(
        &STATES,
        &[
            ("phone", 390.0, 844.0, 0.35),
            ("tablet", 834.0, 1194.0, 0.30),
        ],
    );
}

#[test]
#[ignore = "explicit screenshot generation requires a working headless wgpu adapter"]
fn capture_split_keyboard_ui_gallery() {
    capture_gallery(
        &[
            "keyboard-hold",
            "keyboard-shell",
            "keyboard-symbols",
            "keyboard-hinge",
            "settings",
        ],
        &[
            ("tablet-landscape", 1194.0, 834.0, 0.0),
            ("dual-screen", 1114.0, 720.0, 0.0),
            ("phone", 390.0, 844.0, 0.0),
        ],
    );
}

#[test]
#[ignore = "explicit screenshot generation requires a working headless wgpu adapter"]
fn capture_unlock_ui_gallery() {
    capture_gallery(
        &[
            "unlock",
            "unlock-waiting",
            "unlock-failure",
            "unlock-unavailable",
            "unlock-passcode",
        ],
        &[
            ("phone", 390.0, 844.0, 0.0),
            ("phone-narrow", 320.0, 568.0, 0.0),
            ("phone-short", 844.0, 390.0, 0.0),
            ("tablet", 834.0, 1194.0, 0.0),
        ],
    );
}

fn capture_gallery(states: &[&str], viewports: &[(&str, f32, f32, f32)]) {
    let output = PathBuf::from(
        std::env::var_os("FESTERM_MOBILE_UI_OUT")
            .expect("use scripts/capture-mobile-ui.py for fresh attributed output"),
    );
    // Never overwrite an earlier attempt, including a failed one.
    fs::create_dir(&output).expect("capture output must be new, with an existing parent");
    let source = git_fact(&["rev-parse", "HEAD"]);
    let dirty = !git_fact(&["status", "--porcelain", "--untracked-files=normal"]).is_empty();
    let mut images = Vec::new();
    for &(family, width, height, occlusion) in viewports {
        for &state in states {
            let state_occlusion = if state.starts_with("unlock")
                || matches!(
                    state,
                    "disconnected"
                        | "profiles"
                        | "profiles-empty"
                        | "settings"
                        | "sessions-sheet"
                        | "editor-new"
                        | "editor-devtunnel"
                        | "host-trust"
                        | "changed-host"
                        | "editor-edit"
                        | "auth-key"
                        | "files"
                        | "files-transfers"
                        | "files-conflict"
                        | "unlock"
                        | "settings-lock"
                        | "host-setup"
                        | "host-setup-linux"
                        | "host-script"
                        | "keyboard-hold"
                        | "keyboard-shell"
                        | "keyboard-symbols"
                        | "keyboard-preview"
                        | "keyboard-hinge"
                ) {
                0.0
            } else {
                occlusion
            };
            let lifecycle = Rc::new(Cell::new(Lifecycle {
                active: true,
                ..Default::default()
            }));
            let mut app = MobileApp::new(lifecycle);
            configure(&mut app, state);
            let mut harness = Harness::builder()
                .with_size(egui::vec2(width, height))
                .build_ui_state(
                    move |ui, app: &mut MobileApp| app.show_with_occlusion(ui, state_occlusion),
                    app,
                );
            // Pin the presentation preference, not the native platform theme.
            harness.ctx.set_theme(egui::ThemePreference::Dark);
            harness
                .ctx
                .all_styles_mut(|style| style.visuals.text_cursor.blink = false);
            harness.remove_cursor();
            // Live transfer rows request repaints for their progress clock.
            if state.starts_with("files-") || state.starts_with("host-setup") {
                harness.run_steps(3);
            } else {
                harness.run();
            }
            if state == "keyboard-preview" {
                use egui_kittest::kittest::Queryable;
                let position = harness.get_by_label("q").rect().center();
                harness.event(egui::Event::PointerMoved(position));
                harness.event(egui::Event::PointerButton {
                    pos: position,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                });
                harness.step();
                assert_eq!(harness.state().sink.diagnostics.byte_count, 0);
            } else if state == "arrow-helper" {
                let origin = harness
                    .state()
                    .view
                    .diagnostics()
                    .grid_rect
                    .unwrap()
                    .center();
                let time = harness.ctx.input(|input| input.time);
                for (elapsed, phase, position) in [
                    (0.01, egui::TouchPhase::Start, origin),
                    (0.51, egui::TouchPhase::Move, origin),
                    (
                        0.52,
                        egui::TouchPhase::Move,
                        origin + egui::vec2(0.0, -90.0),
                    ),
                ] {
                    harness.input_mut().time = Some(time + elapsed);
                    harness.event(egui::Event::Touch {
                        device_id: egui::TouchDeviceId(1),
                        id: egui::TouchId(1),
                        phase,
                        pos: position,
                        force: None,
                    });
                    harness.step();
                }
                assert_eq!(
                    harness.state().gestures.helper(),
                    Some(Some(terminal_gesture::Direction::Up))
                );
                // Keep the contact held for the image. Running to idle would
                // continue the real repeat scheduler rather than settle.
                harness.remove_cursor();
                harness.step();
            } else {
                harness.remove_cursor();
                if state.starts_with("files-") {
                    harness.run_steps(3);
                } else {
                    harness.run();
                }
            }
            let image = harness.render().expect("headless mobile rendering");
            let filename = format!("{family}-{state}.png");
            let path = output.join(&filename);
            image.save(&path).expect("save mobile review PNG");
            let hash: String = Sha256::digest(fs::read(&path).expect("read PNG"))
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            images.push(serde_json::json!({
                "image": filename,
                "state": state,
                "viewport_class": family,
                "logical_size": [width, height],
                "pixel_size": [image.width(), image.height()],
                "simulated_keyboard_occlusion_fraction": state_occlusion,
                "native_keyboard_rendered": false,
                "sha256": hash,
            }));
        }
    }
    fs::write(
        output.join("manifest.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "schema_version": 1,
            "capture_method": "egui_kittest headless wgpu; actual MobileApp widgets",
            "source_commit": source,
            "source_dirty": dirty,
            "host_os": std::env::consts::OS,
            "native_ios": false,
            "native_validation": "not-run",
            "fixture": "repository-owned offline shell screens, terminal chrome, editors, synthetic hold/drag, retired-session and trust/auth prompts; no SSH worker",
            "presentation": "Dark preference; nonblinking cursor; no system keyboard pixels",
            "images": images,
        }))
        .expect("serialize mobile capture manifest"),
    )
    .expect("save mobile capture manifest");
}

#[test]
fn dev_tunnel_host_session_field_keeps_the_keyboard_up() {
    use egui_kittest::kittest::{By, Queryable};
    let lifecycle = Rc::new(Cell::new(Lifecycle {
        active: true,
        ..Default::default()
    }));
    let mut app = MobileApp::new(lifecycle);
    configure(&mut app, "editor-devtunnel");
    let occlusion = Rc::new(Cell::new(0.0_f32));
    let shared = Rc::clone(&occlusion);
    let mut harness = Harness::builder()
        .with_size(egui::vec2(390.0, 844.0))
        .build_ui_state(
            move |ui, app: &mut MobileApp| app.show_with_occlusion(ui, shared.get()),
            app,
        );
    harness.run_steps(3);
    harness
        .get(By::new().predicate(|node| node.placeholder() == Some("Host session")))
        .click();
    let mut log = Vec::new();
    for step in 0..12 {
        harness.step();
        let ime = harness.output().platform_output.ime;
        log.push((
            step,
            occlusion.get(),
            ime.map(|ime| ime.should_interrupt_composition),
            harness.ctx.memory(|memory| memory.focused()),
        ));
        // UIKit reports the docked keyboard once the IME is allowed.
        occlusion.set(if ime.is_some() { 0.36 } else { 0.0 });
    }
    let focused: Vec<_> = log.iter().map(|entry| entry.3).collect();
    assert!(
        log.iter().skip(1).all(|entry| entry.2 == Some(false))
            && focused.windows(2).all(|pair| pair[0] == pair[1]),
        "keyboard must stay up: {log:#?}"
    );
    let field = harness
        .get(By::new().predicate(|node| node.placeholder() == Some("Host session")))
        .rect();
    assert!(
        field.bottom() <= 844.0 * (1.0 - 0.36),
        "the focused field scrolls above the keyboard: {field:?}"
    );
}

fn placeholder(text: &'static str) -> egui_kittest::kittest::By<'static> {
    egui_kittest::kittest::By::new().predicate(move |node| node.placeholder() == Some(text))
}

struct FieldTouch {
    harness: Harness<'static, MobileApp>,
    occlusion: Rc<Cell<f32>>,
    time: f64,
    username_at: egui::Pos2,
}

impl FieldTouch {
    fn editor() -> Self {
        let lifecycle = Rc::new(Cell::new(Lifecycle {
            active: true,
            ..Default::default()
        }));
        let mut app = MobileApp::new(lifecycle);
        configure(&mut app, "editor-new");
        let occlusion = Rc::new(Cell::new(0.0_f32));
        let shared = Rc::clone(&occlusion);
        let mut harness = Harness::builder()
            .with_size(egui::vec2(390.0, 844.0))
            .build_ui_state(
                move |ui, app: &mut MobileApp| app.show_with_occlusion(ui, shared.get()),
                app,
            );
        harness.run_steps(3);
        let time = harness.ctx.input(|input| input.time);
        let username_at = {
            use egui_kittest::kittest::Queryable;
            harness.get(placeholder("dev")).rect().center()
        };
        Self {
            harness,
            occlusion,
            time,
            username_at,
        }
    }

    fn frame(&mut self, events: Vec<egui::Event>, dt: f64) {
        self.time += dt;
        self.harness.input_mut().time = Some(self.time);
        for event in events {
            self.harness.event(event);
        }
        self.harness.step();
        let ime = self.harness.output().platform_output.ime.is_some();
        self.occlusion.set(if ime { 0.36 } else { 0.0 });
    }

    fn center(&self, by: egui_kittest::kittest::By<'_>) -> egui::Pos2 {
        use egui_kittest::kittest::Queryable;
        self.harness.get(by).rect().center()
    }

    fn touch(&mut self, pos: egui::Pos2, hold: f64) {
        self.touch_path(&[pos], hold);
    }

    /// Presses at `path[0]`, holds, slides through the rest, then lifts.
    fn touch_path(&mut self, path: &[egui::Pos2], hold: f64) {
        let touch = |phase, pos| egui::Event::Touch {
            device_id: egui::TouchDeviceId(1),
            id: egui::TouchId(1),
            phase,
            pos,
            force: None,
        };
        let press = |pressed, pos| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        let start = path[0];
        self.frame(
            vec![
                touch(egui::TouchPhase::Start, start),
                egui::Event::PointerMoved(start),
                press(true, start),
            ],
            0.02,
        );
        let mut held = 0.0;
        while held < hold {
            self.frame(vec![], 0.1);
            held += 0.1;
        }
        for pos in &path[1..] {
            self.frame(
                vec![
                    touch(egui::TouchPhase::Move, *pos),
                    egui::Event::PointerMoved(*pos),
                ],
                0.03,
            );
        }
        let end = *path.last().expect("a touch has a position");
        self.frame(
            vec![
                touch(egui::TouchPhase::End, end),
                press(false, end),
                egui::Event::PointerGone,
            ],
            0.02,
        );
        for _ in 0..3 {
            self.frame(vec![], 0.02);
        }
    }

    fn caret(&self) -> Option<std::ops::Range<usize>> {
        let field = self.harness.ctx.memory(|memory| memory.focused())?;
        let range = egui::TextEdit::load_state(&self.harness.ctx, field)?
            .cursor
            .char_range()?
            .as_sorted_char_range();
        Some(range.start.0..range.end.0)
    }

    fn long_press_username(&mut self) {
        self.touch(self.username_at, 0.7);
    }

    fn tap_menu(&mut self, label: &'static str) {
        let item = self.center(egui_kittest::kittest::By::new().label(label));
        self.touch(item, 0.0);
    }

    fn has_menu(&self, label: &str) -> bool {
        use egui_kittest::kittest::{By, Queryable};
        self.harness.query(By::new().label(label)).is_some()
    }

    fn keyboard_up(&self) -> bool {
        self.harness.output().platform_output.ime.is_some()
            && self.harness.ctx.memory(|memory| memory.focused()).is_some()
    }

    fn username(&self) -> &str {
        &self.harness.state().editor.username
    }
}

#[test]
fn long_pressing_a_form_field_offers_paste_and_keeps_the_keyboard() {
    let mut rig = FieldTouch::editor();
    assert!(!rig.has_menu("Paste"));
    rig.long_press_username();
    assert!(rig.has_menu("Paste"), "a held field offers its edit menu");
    assert!(
        !rig.has_menu("Select All"),
        "nothing to select in an empty field"
    );
    assert!(
        rig.keyboard_up(),
        "the held field takes focus and its keyboard"
    );

    super::set_test_clipboard(Some("fes\nlabs"));
    rig.tap_menu("Paste");
    assert_eq!(
        rig.username(),
        "fes labs",
        "single-line paste flattens newlines"
    );
    assert!(!rig.has_menu("Paste"), "choosing an item closes the menu");
    assert!(
        rig.keyboard_up(),
        "tapping the menu must not drop the keyboard"
    );

    rig.long_press_username();
    assert!(rig.has_menu("Select All"));
    rig.tap_menu("Select All");
    assert!(rig.has_menu("Copy"), "a selection offers Copy");
    assert!(rig.has_menu("Cut"));
    super::set_test_clipboard(Some("root"));
    rig.tap_menu("Paste");
    assert_eq!(
        rig.username(),
        "root",
        "pasting over a selection replaces it"
    );
    super::set_test_clipboard(None);
}

#[test]
fn a_quick_tap_or_drag_on_a_form_field_does_not_open_the_edit_menu() {
    let mut rig = FieldTouch::editor();
    let field = rig.username_at;
    rig.touch(field, 0.2);
    assert!(!rig.has_menu("Paste"), "a tap only focuses the field");
    assert!(rig.keyboard_up());

    rig.long_press_username();
    assert!(rig.has_menu("Paste"));
    rig.touch(field + egui::vec2(0.0, 120.0), 0.0);
    assert!(
        !rig.has_menu("Paste"),
        "pressing outside dismisses the menu"
    );
}

#[test]
fn holding_then_sliding_on_a_form_field_moves_its_caret_without_selecting() {
    let mut rig = FieldTouch::editor();
    let field = {
        use egui_kittest::kittest::Queryable;
        rig.harness.get(placeholder("dev")).rect()
    };
    rig.harness.state_mut().editor.username = "festerm".to_owned();
    let end_of_text = rig.username_at;
    rig.touch(end_of_text, 0.2);
    assert_eq!(
        rig.caret(),
        Some(7..7),
        "a tap past the text puts the caret at its end"
    );

    let start_of_text = egui::pos2(field.left() + 14.0, field.center().y);
    let path: Vec<egui::Pos2> = (0..=6)
        .map(|step| end_of_text.lerp(start_of_text, step as f32 / 6.0))
        // A finger drifting off the row still steers the caret sideways.
        .chain([start_of_text + egui::vec2(0.0, 60.0)])
        .collect();
    rig.touch_path(&path, 0.7);
    assert_eq!(
        rig.caret(),
        Some(0..0),
        "the held finger carries a caret, not a selection"
    );
    assert!(
        !rig.has_menu("Paste"),
        "a moved hold does not open the menu"
    );
    assert!(rig.keyboard_up());
    assert_eq!(rig.username(), "festerm");
}

#[test]
fn native_keyboard_edits_reach_the_focused_form_field_in_order() {
    use festerm_ios_window::{FieldDocument, FieldInput};
    type Field = (String, Vec<FieldInput>, Option<FieldDocument>);
    let mut harness = Harness::builder()
        .with_size(egui::vec2(390.0, 200.0))
        .build_ui_state(
            |ui, (text, inputs, document): &mut Field| {
                super::apply_field_input(ui.ctx(), std::mem::take(inputs));
                let response = crate::widgets::form_field(
                    ui,
                    [ui.available_width(), 48.0],
                    "field",
                    egui::TextEdit::singleline(text),
                    false,
                );
                if ui.ctx().memory(|memory| memory.focused()).is_none() {
                    response.request_focus();
                }
                *document = crate::widgets::take_field_document(ui.ctx());
            },
            ("abc".to_owned(), Vec::new(), None),
        );
    harness.run_steps(2);
    let document = harness
        .state()
        .2
        .clone()
        .expect("the focused field is published");
    assert_eq!(document.text, "abc");
    assert_eq!(document.selection, 3..3);
    assert_eq!(document.carets.len(), 4);
    assert!(document.carets.windows(2).all(|pair| pair[0] < pair[1]));
    assert!(!document.secure);

    let mut send = |inputs: Vec<FieldInput>| {
        harness.state_mut().1 = inputs;
        harness.run_steps(1);
        let (text, _, document) = harness.state();
        (
            text.clone(),
            document.as_ref().map(|document| document.selection.clone()),
        )
    };
    assert_eq!(
        send(vec![
            FieldInput::Select(1..1),
            FieldInput::Insert("X".into())
        ]),
        ("aXbc".to_owned(), Some(2..2)),
        "a trackpad move lands before the typed text"
    );
    assert_eq!(
        send(vec![FieldInput::DeleteBackward]),
        ("abc".to_owned(), Some(1..1))
    );
    assert_eq!(
        send(vec![FieldInput::Preedit {
            text: "k".into(),
            selected: 1..1,
        }])
        .0,
        "akbc"
    );
    assert_eq!(
        send(vec![FieldInput::Commit("か".into())]),
        ("aかbc".to_owned(), Some(2..2)),
        "a commit replaces the marked text"
    );
    assert_eq!(send(vec![FieldInput::Select(0..4)]).1, Some(0..4));
}

#[test]
fn a_password_field_menu_pastes_but_never_offers_copy_or_cut() {
    let mut harness = Harness::builder()
        .with_size(egui::vec2(390.0, 400.0))
        .build_ui_state(
            |ui, password: &mut String| {
                super::apply_field_edit(ui.ctx());
                ui.add_space(120.0);
                crate::widgets::form_field(
                    ui,
                    [ui.available_width(), 48.0],
                    "password",
                    egui::TextEdit::singleline(password)
                        .password(true)
                        .sensitive(true)
                        .hint_text("Password"),
                    true,
                );
            },
            "hunter2".to_owned(),
        );
    harness.run_steps(2);
    let mut time = harness.ctx.input(|input| input.time);
    let mut touch = |harness: &mut Harness<'_, String>, pos: egui::Pos2, hold: f64| {
        let finger = |phase| egui::Event::Touch {
            device_id: egui::TouchDeviceId(1),
            id: egui::TouchId(1),
            phase,
            pos,
            force: None,
        };
        let button = |pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        let mut step = |harness: &mut Harness<'_, String>, events: Vec<egui::Event>, dt: f64| {
            time += dt;
            harness.input_mut().time = Some(time);
            for event in events {
                harness.event(event);
            }
            harness.step();
        };
        step(
            harness,
            vec![
                finger(egui::TouchPhase::Start),
                egui::Event::PointerMoved(pos),
                button(true),
            ],
            0.02,
        );
        let mut held = 0.0;
        while held < hold {
            step(harness, vec![], 0.1);
            held += 0.1;
        }
        step(
            harness,
            vec![
                finger(egui::TouchPhase::End),
                button(false),
                egui::Event::PointerGone,
            ],
            0.02,
        );
        for _ in 0..3 {
            step(harness, vec![], 0.02);
        }
    };
    let at = |harness: &Harness<'_, String>, by: egui_kittest::kittest::By<'_>| {
        use egui_kittest::kittest::Queryable;
        harness.get(by).rect().center()
    };
    let has = |harness: &Harness<'_, String>, label: &str| {
        use egui_kittest::kittest::{By, Queryable};
        harness.query(By::new().label(label)).is_some()
    };
    let field = at(
        &harness,
        egui_kittest::kittest::By::new().predicate(|node| {
            matches!(
                node.role(),
                egui::accesskit::Role::PasswordInput | egui::accesskit::Role::TextInput
            )
        }),
    );
    touch(&mut harness, field, 0.7);
    let select_all = at(
        &harness,
        egui_kittest::kittest::By::new().label("Select All"),
    );
    touch(&mut harness, select_all, 0.0);
    assert!(has(&harness, "Paste"));
    assert!(
        !has(&harness, "Copy") && !has(&harness, "Cut"),
        "secrets stay put"
    );
    super::set_test_clipboard(Some("s3cret"));
    let paste = at(&harness, egui_kittest::kittest::By::new().label("Paste"));
    touch(&mut harness, paste, 0.0);
    super::set_test_clipboard(None);
    assert_eq!(harness.state(), "s3cret");
}
