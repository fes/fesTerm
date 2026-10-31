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

const STATES: [&str; 25] = [
    "profiles",
    "profiles-empty",
    "terminal",
    "terminal-devtunnel",
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
    "settings-lock",
    "signin",
    "taken-over",
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
            subtitle: "Dev Tunnel · build-tunnel".to_owned(),
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
        "unlock" | "settings-lock" => {
            use festerm_ios_lock::{
                AppLock, Availability, BiometryKind, FakeLocker, LockSettings, ManualClock,
            };
            let locker = FakeLocker::new(Availability::Available(BiometryKind::FaceId));
            let lock = AppLock::new(None, locker, Arc::new(ManualClock::new()), || {});
            lock.apply_settings(LockSettings {
                enabled: true,
                ..LockSettings::default()
            })
            .expect("fake locker can authenticate");
            if state == "unlock" {
                lock.lock_now();
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
            app.editor.tunnel = Some(DevTunnelChoices {
                account_login: Some("octo".to_owned()),
                tunnels: vec!["build-tunnel".to_owned(), "docs-tunnel".to_owned()],
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
                });
                tab.spec = None;
            }
            *app.sink.live.connection.borrow_mut() = Some(live_ssh::taken_over_fixture());
            app.chrome_override = Some(ChromeModel {
                title: "build-box".to_owned(),
                subtitle: "Dev Tunnel · build-tunnel".to_owned(),
                line: ConnectionLine {
                    state: SurfaceState::Disconnected,
                    label: "Taken over by another device".to_owned(),
                    context: Some("host session".to_owned()),
                },
            });
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
    let output = PathBuf::from(
        std::env::var_os("FESTERM_MOBILE_UI_OUT")
            .expect("use scripts/capture-mobile-ui.py for fresh attributed output"),
    );
    // Never overwrite an earlier attempt, including a failed one.
    fs::create_dir(&output).expect("capture output must be new, with an existing parent");
    let source = git_fact(&["rev-parse", "HEAD"]);
    let dirty = !git_fact(&["status", "--porcelain", "--untracked-files=normal"]).is_empty();
    let mut images = Vec::new();
    for (family, width, height, occlusion) in [
        ("phone", 390.0, 844.0, 0.35),
        ("tablet", 834.0, 1194.0, 0.30),
    ] {
        for state in STATES {
            let state_occlusion = if matches!(
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
            if state.starts_with("files-") {
                harness.run_steps(3);
            } else {
                harness.run();
            }
            if state == "arrow-helper" {
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
