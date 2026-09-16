//! "State of the UI" screenshot gallery: a deterministic, headless capture
//! pipeline for a reviewer-facing markdown document.
//!
//! This module exists to be *regenerated repeatedly*. A reviewer critiques
//! the UI, changes get made, and the whole gallery is rebuilt from scratch --
//! so capture must be a script, not an agent driving the app by hand. Every
//! scenario below renders the real product UI (`crate::screens`,
//! `crate::keyboard`) against `egui_kittest`'s headless harness, using only
//! fixture data owned by this module: no real config, shell, clipboard, or
//! filesystem state is ever touched.
//!
//! # No PII
//! Every profile name, hostname, username, and path below is synthetic:
//! hosts live under `example.com`/`example.net` or use RFC 5737
//! documentation-reserved IP ranges, usernames are generic role names
//! (`devuser`, `builder`, `operator`), and no real key material, credential,
//! clipboard content, or shell history ever appears.
//!
//! # Contract
//! `capture_ui_state_gallery` (below) writes one PNG per [`Scenario`] plus a
//! `manifest.json` describing them, into `FESTERM_UI_GALLERY_OUT` (default
//! `docs/images/ui-state`). See `scripts/capture-ui-state.sh`.
//!
//! # Regression vs. generation
//! Screenshots are obtained via `Harness::render()`, which returns the
//! rendered frame directly as an `image::RgbaImage`. This module never calls
//! `Harness::snapshot`/`snapshot_options`: those compare against a stored
//! baseline and *fail the test* on any pixel difference, which is exactly
//! backwards for a generator whose entire job is to reflect a changed UI.
//! A changed UI here simply produces a changed PNG; the test never fails
//! because of it.

use std::{
    fs,
    path::{Path, PathBuf},
};

use eframe::egui;
use egui_kittest::{kittest::Queryable, Harness};
use sha2::{Digest, Sha256};

use festerm_config::{
    Configuration, EmojiPresentationPreference, KeyboardBindings, Profile, ScrollSpeedPreference,
    ScrollbackLimitPreference, SerialDataBits, SerialFlowControl, SerialParity, SerialStopBits,
    SftpPaneOrderPreference, TerminalFontPreference,
};
use festerm_sessiond::UnattachedSession;
use festerm_ui_egui::chrome::ChipLayout;

use crate::{
    multiplexer_sessions::MultiplexerSession,
    screens::{self, SettingsViewModel},
    tabs::{AppCommand, AppState, NewProfileKind},
};

/// One screenshot scenario: what it renders, and the caption a reader of
/// the generated markdown should see next to it. The caption lives next to
/// the fixture on purpose, so it stays correct when the scenario changes.
struct Scenario {
    id: &'static str,
    section: &'static str,
    title: &'static str,
    caption: &'static str,
    capture: fn() -> image::RgbaImage,
}

fn scenarios() -> Vec<Scenario> {
    vec![
        // -- new-session ------------------------------------------------
        Scenario {
            id: "launcher-populated",
            section: "new-session",
            title: "New Session with saved profiles and running sessions",
            caption: "Launch cards sit above saved profiles of every kind plus resumable \
                      local, tmux, and screen sessions, at a normal desktop width.",
            capture: capture_launcher_populated,
        },
        Scenario {
            id: "launcher-compact",
            section: "new-session",
            title: "New Session in the compact launcher-grid layout",
            caption: "The 'Compact New Session layout' preference shrinks the launch cards \
                      so saved profiles and running sessions start higher up the window.",
            capture: capture_launcher_compact,
        },
        Scenario {
            id: "launcher-narrow",
            section: "new-session",
            title: "New Session at a narrow, responsive width",
            caption: "The same populated launcher reflowing at a narrow window: cards \
                      and panels stack instead of sitting side by side.",
            capture: capture_launcher_narrow,
        },
        Scenario {
            id: "launcher-empty",
            section: "new-session",
            title: "New Session on first run, before any profile exists",
            caption: "The empty/first-run state: only the fixed launch cards, with no saved \
                      profiles or resumable sessions panels to show yet.",
            capture: capture_launcher_empty,
        },
        // -- connection-forms --------------------------------------------
        Scenario {
            id: "ssh-connect-collapsed",
            section: "connection-forms",
            title: "SSH connect form, Quick Connect",
            caption: "The default SSH launch surface: host/username/password only, with \
                      advanced settings collapsed.",
            capture: capture_ssh_connect_collapsed,
        },
        Scenario {
            id: "ssh-connect-advanced",
            section: "connection-forms",
            title: "SSH connect form with advanced settings shown",
            caption: "Revealing 'Show advanced settings' exposes durable-session, port \
                      forwarding, and authentication-method controls.",
            capture: capture_ssh_connect_advanced,
        },
        Scenario {
            id: "sftp-connect-form",
            section: "connection-forms",
            title: "SFTP connect form",
            caption: "The SFTP launch surface defaults to opening the graphical two-pane \
                      file manager once connected.",
            capture: capture_sftp_connect_form,
        },
        Scenario {
            id: "serial-connect-form",
            section: "connection-forms",
            title: "Serial connect form",
            caption: "Serial launches ask only for a device path and line settings; there is \
                      no host/credential concept for a local serial device.",
            capture: capture_serial_connect_form,
        },
        // -- settings ------------------------------------------------------
        Scenario {
            id: "settings-interface-scrolling",
            section: "settings",
            title: "Settings: Interface and Scrolling",
            caption: "The top of Settings: chip layout, session-detail, and workspace-restore \
                      toggles, followed by scrollback limit and scroll-speed controls.",
            capture: capture_settings_interface_scrolling,
        },
        Scenario {
            id: "settings-terminal-typography",
            section: "settings",
            title: "Settings: Terminal typography",
            caption: "Font family, programming ligatures, and emoji presentation, all scoped \
                      to terminal cell rendering only.",
            capture: capture_settings_terminal_typography,
        },
        Scenario {
            id: "settings-quick-switch",
            section: "settings",
            title: "Settings: Quick switch",
            caption: "The single toggle controlling whether held quick-switch modifiers \
                      overlay chip numbers.",
            capture: capture_settings_quick_switch,
        },
        Scenario {
            id: "settings-sftp-card",
            section: "settings",
            title: "Settings: SFTP",
            caption: "Pane order and the default local directory used when opening new SFTP \
                      tabs.",
            capture: capture_settings_sftp_card,
        },
        // -- keyboard --------------------------------------------------
        Scenario {
            id: "keyboard-editor-collapsed",
            section: "keyboard",
            title: "Keyboard bindings editor, collapsed",
            caption: "Actions grouped by scope, each showing its current chord as keycaps; \
                      one customized action carries a 'Customized' badge.",
            capture: capture_keyboard_editor_collapsed,
        },
        Scenario {
            id: "keyboard-editor-selected",
            section: "keyboard",
            title: "Keyboard bindings editor with an action selected",
            caption: "Selecting an action expands its inline editor directly under its own \
                      row, showing scope, default, and current chord as read-only context.",
            capture: capture_keyboard_editor_selected,
        },
        Scenario {
            id: "keyboard-editor-press-keys",
            section: "keyboard",
            title: "Keyboard bindings editor capturing a chord",
            caption: "'Press keys' arms live chord capture; the editor waits for a chord \
                      instead of dispatching whatever is pressed next.",
            capture: capture_keyboard_editor_press_keys,
        },
        Scenario {
            id: "keyboard-editor-filtered",
            section: "keyboard",
            title: "Keyboard bindings editor filtered by search",
            caption: "Typing into Search narrows the list to matching actions and hides scope \
                      groups with no match.",
            capture: capture_keyboard_editor_filtered,
        },
        // -- profiles ------------------------------------------------------
        Scenario {
            id: "profiles-list",
            section: "profiles",
            title: "Profiles list",
            caption: "Every saved local, SSH, SFTP, and serial profile in one reorderable \
                      list.",
            capture: capture_profiles_list,
        },
        Scenario {
            id: "profiles-ssh-editor",
            section: "profiles",
            title: "SSH profile editor",
            caption: "Editing an existing SSH profile's connection metadata and durable- \
                      session settings.",
            capture: capture_profiles_ssh_editor,
        },
        Scenario {
            id: "profiles-sftp-editor",
            section: "profiles",
            title: "SFTP profile editor",
            caption: "The same SSH-family editor in SFTP mode, offering the graphical \
                      file-manager toggle instead of a terminal type.",
            capture: capture_profiles_sftp_editor,
        },
        Scenario {
            id: "profiles-serial-editor",
            section: "profiles",
            title: "Serial profile editor",
            caption: "Device path and line settings (baud, data bits, parity, stop bits, \
                      flow control) for a saved serial profile.",
            capture: capture_profiles_serial_editor,
        },
    ]
}

// --------------------------------------------------------------------
// Fixtures. Every host, username and path below is synthetic: no real
// config, credential, or filesystem state is ever read to produce them.
// --------------------------------------------------------------------

/// A representative, populated mix of profile kinds: several SSH hosts (one
/// addressed by a documentation-reserved IP), a local shell, an SFTP
/// endpoint, and a serial device.
fn synthetic_profiles() -> Vec<Profile> {
    vec![
        Profile::local("Local dev shell", "/bin/zsh", Vec::new(), None)
            .expect("gallery profile is valid"),
        Profile::local(
            "Local build shell",
            "/bin/bash",
            vec!["-l".to_owned()],
            Some("/home/devuser/projects/example-app".to_owned()),
        )
        .expect("gallery profile is valid"),
        Profile::ssh(
            "Staging web-1",
            "web-1.staging.example.com",
            22,
            "devuser",
            "xterm-256color",
            120,
            40,
        )
        .expect("gallery profile is valid"),
        Profile::ssh(
            "Staging web-2",
            "web-2.staging.example.com",
            22,
            "devuser",
            "xterm-256color",
            120,
            40,
        )
        .expect("gallery profile is valid"),
        Profile::ssh(
            "Bastion host",
            "198.51.100.42",
            22,
            "operator",
            "xterm-256color",
            100,
            32,
        )
        .expect("gallery profile is valid"),
        Profile::sftp(
            "Build artifacts",
            "artifacts.example.net",
            22,
            "builder",
            true,
        )
        .expect("gallery profile is valid"),
        Profile::serial(
            "Bench serial adapter",
            "/dev/tty.usbserial-EXAMPLE1",
            115_200,
            SerialDataBits::Eight,
            SerialParity::None,
            SerialStopBits::One,
            SerialFlowControl::None,
        )
        .expect("gallery profile is valid"),
    ]
}

fn synthetic_configuration() -> Configuration {
    Configuration::new(synthetic_profiles()).expect("gallery configuration is valid")
}

fn synthetic_resumable_sessions() -> Vec<UnattachedSession> {
    [
        ("background-build", "/home/devuser/projects/example-app"),
        ("log-tail", "/home/devuser/projects/example-app/logs"),
    ]
    .into_iter()
    .map(|(name, working_directory)| UnattachedSession {
        pid: 4242,
        endpoint: String::new(),
        name: name.to_owned(),
        shell: "/bin/zsh".to_owned(),
        arguments: Vec::new(),
        working_directory: Some(working_directory.to_owned()),
        created_at_unix_ms: u128::from(
            screens::unix_now_seconds()
                .expect("gallery clock is after the Unix epoch")
                .saturating_sub(2 * 60 * 60),
        ) * 1000,
    })
    .collect()
}

fn synthetic_tmux_sessions() -> Vec<MultiplexerSession> {
    vec![MultiplexerSession {
        name: "deploy-watch".to_owned(),
        match_key: "deploy-watch".to_owned(),
        attached: false,
        started_at_unix_seconds: None,
    }]
}

fn synthetic_screen_sessions() -> Vec<MultiplexerSession> {
    vec![MultiplexerSession {
        name: "monitor".to_owned(),
        match_key: "55201.monitor".to_owned(),
        attached: true,
        started_at_unix_seconds: None,
    }]
}

// --------------------------------------------------------------------
// Rendering helpers.
// --------------------------------------------------------------------

/// Crops a rendered frame to the vertical band `[top, bottom)`, in points
/// (== pixels at the harness's default `pixels_per_point` of 1.0).
fn crop_vertical(image: &image::RgbaImage, top: f32, bottom: f32) -> image::RgbaImage {
    let top = top.max(0.0).round() as u32;
    let bottom = (bottom.round() as u32).min(image.height()).max(top);
    image::imageops::crop_imm(image, 0, top, image.width(), bottom - top).to_image()
}

/// The fixed breathing room re-added around content after trimming dead
/// background, so screenshots never sit flush against their own edge.
const GALLERY_MARGIN: u32 = 8;

/// Trims uniform background rows/columns from every edge inward, stopping
/// the instant a row or column is no longer perfectly uniform -- so a row
/// that merely *starts* with background pixels (because real content sits
/// further right, say) is never mistaken for a blank one -- then re-adds
/// `margin` pixels of background on each side so content is not flush
/// against the final edge.
///
/// The background colour is taken from the top-left pixel, which every
/// scenario here paints as the window/page background before any content.
fn trim_to_content(image: &image::RgbaImage, margin: u32) -> image::RgbaImage {
    let width = image.width();
    let height = image.height();
    if width == 0 || height == 0 {
        return image.clone();
    }
    let background = *image.get_pixel(0, 0);
    let row_is_background = |y: u32| (0..width).all(|x| *image.get_pixel(x, y) == background);
    let column_is_background = |x: u32| (0..height).all(|y| *image.get_pixel(x, y) == background);

    let mut top = 0;
    while top < height && row_is_background(top) {
        top += 1;
    }
    let mut bottom = height;
    while bottom > top && row_is_background(bottom - 1) {
        bottom -= 1;
    }
    let mut left = 0;
    while left < width && column_is_background(left) {
        left += 1;
    }
    let mut right = width;
    while right > left && column_is_background(right - 1) {
        right -= 1;
    }

    // Entirely background: nothing to trim to, so return the frame as-is
    // rather than produce a degenerate zero-sized image.
    if top >= bottom || left >= right {
        return image.clone();
    }

    let top = top.saturating_sub(margin);
    let left = left.saturating_sub(margin);
    let bottom = (bottom + margin).min(height);
    let right = (right + margin).min(width);
    image::imageops::crop_imm(image, left, top, right - left, bottom - top).to_image()
}

/// Synthesizes a primary-button click at an arbitrary point, for the rare
/// field (the serial form's "Device" text edit) that has no accessible
/// label to query directly.
fn click_at(harness: &Harness<'_, ()>, pos: egui::Pos2) {
    harness.event(egui::Event::PointerMoved(pos));
    for pressed in [true, false] {
        harness.event(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        });
    }
}

/// Clicks a labelled field and types `text` into it -- the same two-step
/// gesture a real user performs, so focus/selection state ends up exactly
/// as it would in the app.
fn enter_text(harness: &mut Harness<'static, ()>, label: &str, text: &str) {
    harness.get_by_label(label).click();
    harness.run();
    harness.get_by_label(label).type_text(text);
    harness.run();
}

/// Settles a harness into a resting state -- no leftover simulated pointer,
/// no accidental hover -- then renders and trims it. This is the single
/// path every scenario's capture goes through, so a screenshot never shows
/// the cursor artifact or dead background left over from reaching its
/// target state.
fn finish(harness: &mut Harness<'_, ()>) -> image::RgbaImage {
    harness.remove_cursor();
    harness.run();
    let image = harness.render().expect("headless render must succeed");
    trim_to_content(&image, GALLERY_MARGIN)
}

// -- new-session ----------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn render_launcher(
    width: f32,
    height: f32,
    compact: bool,
    configuration: Configuration,
    resumable_sessions: Vec<UnattachedSession>,
    tmux_sessions: Vec<MultiplexerSession>,
    screen_sessions: Vec<MultiplexerSession>,
) -> image::RgbaImage {
    let tab_id = AppState::for_test().active();
    let mut harness = Harness::builder()
        .with_size(egui::vec2(width, height))
        .build_ui(move |ui| {
            let _ = screens::show_launcher(
                ui,
                tab_id,
                &configuration,
                true,
                None,
                compact,
                &resumable_sessions,
                &tmux_sessions,
                &screen_sessions,
            );
        });
    harness.run();
    finish(&mut harness)
}

fn capture_launcher_populated() -> image::RgbaImage {
    render_launcher(
        1240.0,
        880.0,
        false,
        synthetic_configuration(),
        synthetic_resumable_sessions(),
        synthetic_tmux_sessions(),
        synthetic_screen_sessions(),
    )
}

fn capture_launcher_compact() -> image::RgbaImage {
    render_launcher(
        1240.0,
        880.0,
        true,
        synthetic_configuration(),
        synthetic_resumable_sessions(),
        synthetic_tmux_sessions(),
        synthetic_screen_sessions(),
    )
}

fn capture_launcher_narrow() -> image::RgbaImage {
    render_launcher(
        560.0,
        920.0,
        false,
        synthetic_configuration(),
        synthetic_resumable_sessions(),
        synthetic_tmux_sessions(),
        synthetic_screen_sessions(),
    )
}

fn capture_launcher_empty() -> image::RgbaImage {
    render_launcher(
        1240.0,
        880.0,
        false,
        Configuration::empty(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    )
}

// -- connection-forms -------------------------------------------------------

fn open_launcher_card(harness: &mut Harness<'static, ()>, label: &str) {
    harness.get_by_label(label).click();
    harness.run();
}

fn show_advanced_settings(harness: &mut Harness<'static, ()>) {
    harness.get_by_label("Show advanced settings").click();
    harness.run();
}

/// How a connect form should be prefilled before its screenshot, mirroring
/// what a user would already have typed. `None` is the genuinely-empty
/// first-use state, which is only ever used for the collapsed SSH form --
/// every other form is shown mid-use with a representative, synthetic
/// identity and (per the no-PII/no-credential rule) an always-empty
/// password.
enum Prefill {
    None,
    /// Fills the advanced form's separate Username/Host fields.
    UsernameAndHost {
        username: &'static str,
        host: &'static str,
    },
    /// Fills the single combined `user@host` Quick Connect field.
    QuickConnect(&'static str),
    /// Fills the serial form's "Device" field, which (unlike the SSH/SFTP
    /// fields) has no accessible label to query directly, so it is reached
    /// by clicking just below its plain text label instead.
    SerialDevice(&'static str),
}

impl Prefill {
    fn apply(self, harness: &mut Harness<'static, ()>) {
        match self {
            Prefill::None => {}
            Prefill::UsernameAndHost { username, host } => {
                enter_text(harness, "Username", username);
                enter_text(harness, "Host", host);
            }
            Prefill::QuickConnect(value) => {
                enter_text(harness, "user@host", value);
            }
            Prefill::SerialDevice(device) => {
                let label_rect = harness.get_by_label("Device").rect();
                let field_pos = egui::pos2(label_rect.left() + 40.0, label_rect.bottom() + 14.0);
                click_at(harness, field_pos);
                harness.run();
                harness.event(egui::Event::Text(device.to_owned()));
                harness.run();
            }
        }
    }
}

fn render_launcher_form(
    width: f32,
    height: f32,
    card_label: &str,
    reveal_advanced: bool,
    prefill: Prefill,
) -> image::RgbaImage {
    let configuration = synthetic_configuration();
    let tab_id = AppState::for_test().active();
    let mut harness = Harness::<()>::builder()
        .with_size(egui::vec2(width, height))
        .build_ui(move |ui| {
            let _ = screens::show_launcher(
                ui,
                tab_id,
                &configuration,
                true,
                None,
                false,
                &[],
                &[],
                &[],
            );
        });
    harness.run();
    open_launcher_card(&mut harness, card_label);
    if reveal_advanced {
        show_advanced_settings(&mut harness);
    }
    prefill.apply(&mut harness);
    finish(&mut harness)
}

fn capture_ssh_connect_collapsed() -> image::RgbaImage {
    // The genuinely empty first-use state: worth showing as-is.
    render_launcher_form(
        720.0,
        820.0,
        "SSH — Connect to a remote host over SSH",
        false,
        Prefill::None,
    )
}

fn capture_ssh_connect_advanced() -> image::RgbaImage {
    render_launcher_form(
        720.0,
        1100.0,
        "SSH — Connect to a remote host over SSH",
        true,
        Prefill::UsernameAndHost {
            username: "devuser",
            host: "web-1.staging.example.com",
        },
    )
}

fn capture_sftp_connect_form() -> image::RgbaImage {
    render_launcher_form(
        720.0,
        820.0,
        "SFTP — Browse and transfer files",
        false,
        Prefill::QuickConnect("builder@artifacts.example.net"),
    )
}

fn capture_serial_connect_form() -> image::RgbaImage {
    render_launcher_form(
        720.0,
        820.0,
        "Serial — Connect to a serial device",
        false,
        Prefill::SerialDevice("/dev/tty.usbserial-EXAMPLE1"),
    )
}

// -- settings ---------------------------------------------------------------

fn synthetic_settings_view_model() -> SettingsViewModel {
    SettingsViewModel {
        keyboard_bindings: KeyboardBindings::default(),
        chip_layout: ChipLayout::Wrap,
        status_bar_visible: true,
        show_session_details: true,
        confirm_session_close: true,
        prefer_powershell: true,
        restore_workspace: false,
        terminal_font: TerminalFontPreference::JetBrainsMono,
        terminal_ligatures: false,
        emoji_presentation: EmojiPresentationPreference::Color,
        scroll_speed: ScrollSpeedPreference::Normal,
        scrollback_limit: ScrollbackLimitPreference::MiB64,
        quick_switch_overlay: true,
        compact_launcher_grid: false,
        pulse_new_output_dot: true,
        show_resumable_sessions: true,
        default_sftp_local_directory: Some("/home/devuser/sftp/example-drop".to_owned()),
        sftp_pane_order: SftpPaneOrderPreference::LocalLeft,
    }
}

/// Renders the whole Settings page tall enough for every card to be laid
/// out (so the cropped band below is always available), then crops to the
/// vertical band from `top_label` (or the very top when `None`) up to just
/// above `bottom_label`.
fn render_settings_card(top_label: Option<&str>, bottom_label: &str) -> image::RgbaImage {
    let model = synthetic_settings_view_model();
    let mut harness = Harness::<()>::builder()
        .with_size(egui::vec2(900.0, 3200.0))
        .build_ui(move |ui| {
            let _ = screens::show_settings(ui, model.clone());
        });
    harness.run();
    let top = top_label
        .map(|label| harness.get_by_label(label).rect().top() - 22.0)
        .unwrap_or(0.0);
    // Cards are separated by a fixed 12px `add_space` gap between one
    // card's bottom border and the next card's top border, and each card's
    // heading sits ~16px inside its own frame's top border (the frame's
    // `inner_margin`). Landing the cut in the middle of that 12px gap --
    // 22px above the *next* card's heading -- keeps this card's own bottom
    // border in frame while never touching the next card's top border.
    let bottom = harness.get_by_label(bottom_label).rect().top() - 22.0;
    harness.remove_cursor();
    harness.run();
    let image = harness
        .render()
        .expect("headless settings render must succeed");
    let cropped = crop_vertical(&image, top, bottom);
    trim_to_content(&cropped, GALLERY_MARGIN)
}

fn capture_settings_interface_scrolling() -> image::RgbaImage {
    render_settings_card(None, "TERMINAL TYPOGRAPHY")
}

fn capture_settings_terminal_typography() -> image::RgbaImage {
    render_settings_card(Some("TERMINAL TYPOGRAPHY"), "QUICK SWITCH")
}

fn capture_settings_quick_switch() -> image::RgbaImage {
    render_settings_card(Some("QUICK SWITCH"), "SFTP")
}

fn capture_settings_sftp_card() -> image::RgbaImage {
    render_settings_card(Some("SFTP"), "KEYBOARD BINDINGS")
}

// -- keyboard -----------------------------------------------------------

/// Self-contained editor harness, mirroring the production dispatch path
/// (parking input while a chord is being captured) without depending on any
/// other module's private test helper.
fn keyboard_editor_harness(
    width: f32,
    height: f32,
    initial_bindings: KeyboardBindings,
) -> Harness<'static, ()> {
    let mut bindings = initial_bindings;
    Harness::builder()
        .with_size(egui::vec2(width, height))
        .build_ui(move |ui| {
            if crate::keyboard::recording(ui.ctx()) {
                let events = ui
                    .ctx()
                    .input_mut(|input| std::mem::take(&mut input.events));
                crate::keyboard::stash_recorded_events(ui.ctx(), events);
            }
            if let Some(AppCommand::SetKeyboardBindings(next)) =
                crate::keyboard::show_editor(ui, &bindings)
            {
                bindings = next;
            }
        })
}

fn capture_keyboard_editor_collapsed() -> image::RgbaImage {
    // Deliberately kept at this narrower width: it demonstrates the editor
    // still working comfortably at a Settings-card-sized viewport, while
    // the interactive scenarios below use a wider one to give the inline
    // editor's keycap columns room.
    let mut customized = KeyboardBindings::default();
    customized.set(
        festerm_config::KeyboardAction::ClearTerminal,
        Some("Ctrl+Shift+F9".into()),
    );
    let mut harness = keyboard_editor_harness(640.0, 1300.0, customized);
    harness.run();
    finish(&mut harness)
}

fn capture_keyboard_editor_selected() -> image::RgbaImage {
    let mut harness = keyboard_editor_harness(960.0, 1300.0, KeyboardBindings::default());
    harness.run();
    harness
        .get_by_role_and_label(accesskit::Role::Button, "New Session")
        .click();
    harness.run();
    finish(&mut harness)
}

fn capture_keyboard_editor_press_keys() -> image::RgbaImage {
    let mut harness = keyboard_editor_harness(960.0, 1300.0, KeyboardBindings::default());
    harness.run();
    harness
        .get_by_role_and_label(accesskit::Role::Button, "New Session")
        .click();
    harness.run();
    harness.get_by_label("Press keys").click();
    harness.run();
    finish(&mut harness)
}

fn capture_keyboard_editor_filtered() -> image::RgbaImage {
    let mut harness = keyboard_editor_harness(960.0, 1300.0, KeyboardBindings::default());
    harness.run();
    harness.get_by_label("Search").click();
    harness.event(egui::Event::Text("markdown".into()));
    harness.run();
    finish(&mut harness)
}

// -- profiles ---------------------------------------------------------------

fn render_profiles(width: f32, height: f32, pending_edit: Option<String>) -> image::RgbaImage {
    let configuration = synthetic_configuration();
    let tab_id = AppState::for_test().active();
    let mut harness = Harness::<()>::builder()
        .with_size(egui::vec2(width, height))
        .build_ui(move |ui| {
            let _ = screens::show_profiles(
                ui,
                tab_id,
                &configuration,
                pending_edit.clone(),
                None::<NewProfileKind>,
                festerm_config::PersistenceProviderKind::FestermSessiond,
            );
        });
    harness.run();
    finish(&mut harness)
}

fn capture_profiles_list() -> image::RgbaImage {
    render_profiles(760.0, 780.0, None)
}

fn capture_profiles_ssh_editor() -> image::RgbaImage {
    render_profiles(520.0, 820.0, Some("Staging web-1".to_owned()))
}

fn capture_profiles_sftp_editor() -> image::RgbaImage {
    render_profiles(520.0, 820.0, Some("Build artifacts".to_owned()))
}

fn capture_profiles_serial_editor() -> image::RgbaImage {
    render_profiles(520.0, 820.0, Some("Bench serial adapter".to_owned()))
}

// --------------------------------------------------------------------
// Manifest emission.
// --------------------------------------------------------------------

#[derive(serde::Serialize)]
struct ManifestScenario {
    id: &'static str,
    section: &'static str,
    title: &'static str,
    caption: &'static str,
    image: String,
    width: u32,
    height: u32,
    tier: &'static str,
    sha256: String,
}

#[derive(serde::Serialize)]
struct Manifest {
    schema_version: u32,
    scenarios: Vec<ManifestScenario>,
}

fn output_directory() -> PathBuf {
    // `CARGO_MANIFEST_DIR` is `app/festerm`; the workspace root is two levels up.
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    match std::env::var_os("FESTERM_UI_GALLERY_OUT") {
        // A relative override is resolved against the workspace root rather
        // than the process working directory, which `cargo test` sets to the
        // package directory. Otherwise `docs/images/ui-state` would silently
        // land in `app/festerm/docs/images/ui-state`.
        Some(value) => {
            let requested = PathBuf::from(value);
            if requested.is_absolute() {
                requested
            } else {
                workspace_root.join(requested)
            }
        }
        None => workspace_root.join("docs/images/ui-state"),
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[test]
#[ignore = "manual capture: generates docs/images/ui-state for the State of the UI document"]
fn capture_ui_state_gallery() {
    let output = output_directory();
    fs::create_dir_all(&output).expect("gallery output directory must be creatable");

    let mut entries = scenarios();
    entries.sort_by(|a, b| (a.section, a.id).cmp(&(b.section, b.id)));

    let mut manifest_scenarios = Vec::with_capacity(entries.len());
    let mut expected_files = std::collections::HashSet::new();
    expected_files.insert("manifest.json".to_owned());

    for scenario in &entries {
        let image = (scenario.capture)();
        let file_name = format!("{}.png", scenario.id);
        let path = output.join(&file_name);
        image
            .save(&path)
            .unwrap_or_else(|error| panic!("saving {file_name} must succeed: {error}"));
        let bytes = fs::read(&path)
            .unwrap_or_else(|error| panic!("reading back {file_name} must succeed: {error}"));

        manifest_scenarios.push(ManifestScenario {
            id: scenario.id,
            section: scenario.section,
            title: scenario.title,
            caption: scenario.caption,
            image: file_name.clone(),
            width: image.width(),
            height: image.height(),
            tier: "headless",
            sha256: sha256_hex(&bytes),
        });
        expected_files.insert(file_name);
    }

    let manifest = Manifest {
        schema_version: 1,
        scenarios: manifest_scenarios,
    };
    let mut json = serde_json::to_string_pretty(&manifest).expect("manifest must serialize");
    json.push('\n');
    fs::write(output.join("manifest.json"), json).expect("manifest.json must be writable");

    // Prune stale artifacts from earlier runs so the directory always
    // matches the manifest exactly, even after scenarios are renamed or
    // removed.
    for entry in fs::read_dir(&output).expect("gallery output directory must be readable") {
        let entry = entry.expect("directory entry must be readable");
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !expected_files.contains(name) {
            fs::remove_file(entry.path()).unwrap_or_else(|error| {
                panic!("removing stale gallery artifact {name} must succeed: {error}")
            });
        }
    }
}
