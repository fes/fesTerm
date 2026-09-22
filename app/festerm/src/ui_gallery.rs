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
    time::{Duration, SystemTime},
};

use eframe::egui;
use egui_kittest::{kittest::Queryable, Harness};
use sha2::{Digest, Sha256};

use festerm_config::{
    Configuration, EmojiPresentationPreference, KeyboardBindings, Profile, ScrollSpeedPreference,
    ScrollbackLimitPreference, SerialDataBits, SerialFlowControl, SerialParity, SerialStopBits,
    SftpPaneOrderPreference, TerminalFontPreference,
};
use festerm_core::{Dimensions, Terminal};
use festerm_markdown::{RemoteMarkdownSource, RemoteSourceOwner};
use festerm_sessiond::UnattachedSession;
use festerm_ssh::{
    SftpDirectoryItem, SftpDirectorySnapshot, SftpEntryType, SftpLocation, SftpPath,
};
use festerm_ui_egui::{
    chrome::{self, ChipId, ChipLayout, ChipStatus, ChipViewModel},
    EncodedInputSink, TerminalView,
};

use crate::{
    inspector::{self, InspectorContent, PersistentSessionFacts, TransportFacts},
    markdown_viewer::MarkdownViewerTab,
    multiplexer_sessions::MultiplexerSession,
    screens::{self, SettingsViewModel},
    sftp_file_manager::SftpFileManagerTab,
    tabs::{AppCommand, AppState, NewProfileKind},
    text_editor::TextEditorTab,
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
            title: "SSH connect form",
            caption: "The default SSH launch surface: a Connection section led by the \
                      squashed user@host:port field and repeating it as Username, Host and \
                      Port, an Authentication section for the credential method, and the \
                      durable-remote-session toggle, with Advanced settings collapsed.",
            capture: capture_ssh_connect_collapsed,
        },
        Scenario {
            id: "ssh-connect-advanced",
            section: "connection-forms",
            title: "SSH connect form with Advanced settings expanded",
            caption: "Expanding 'Advanced settings' reveals the port-forwarding controls \
                      beneath the always-visible connection, authentication and \
                      durable-session sections.",
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
            caption: "'Record shortcut' arms live chord capture; the editor waits for a \
                      chord instead of dispatching whatever is pressed next.",
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
            caption: "Every saved local, SSH, SFTP, and serial profile in a single \
                      searchable table, each row carrying an overflow menu for connecting, \
                      editing, duplicating, and deleting.",
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
        // -- terminal-sessions ----------------------------------------------
        Scenario {
            id: "terminal-ssh-session",
            section: "terminal-sessions",
            title: "A connected SSH terminal session",
            caption: "A remote shell mid-use: service status, a log tail, and the resting \
                      prompt, rendered by the same grid the terminal view paints for a real \
                      PTY.",
            capture: capture_terminal_ssh_session,
        },
        Scenario {
            id: "terminal-sftp-cli",
            section: "terminal-sessions",
            title: "The sftp command-line interface",
            caption: "The terminal-driven `sftp` client, distinct from the graphical file \
                      manager: directory listing, a `get`, and its transfer-progress line.",
            capture: capture_terminal_sftp_cli,
        },
        // -- sftp-workspace ---------------------------------------------------
        Scenario {
            id: "sftp-workspace-browser",
            section: "sftp-workspace",
            title: "The SFTP graphical file-manager workspace",
            caption: "Local and remote panes browsed side by side; the local pane lists an \
                      invented project directory, never the real filesystem.",
            capture: capture_sftp_workspace_browser,
        },
        // -- markdown ---------------------------------------------------------
        Scenario {
            id: "markdown-preview",
            section: "markdown",
            title: "Markdown workspace, rendered preview",
            caption: "A fictional project's Markdown fetched over an already-authenticated \
                      SFTP session, shown in rendered preview mode with headings, a list, \
                      and a code block.",
            capture: capture_markdown_preview,
        },
        Scenario {
            id: "markdown-source",
            section: "markdown",
            title: "Markdown workspace, source mode",
            caption: "The same document toggled to raw source, for readers who want to see \
                      the Markdown itself rather than its rendering.",
            capture: capture_markdown_source,
        },
        Scenario {
            id: "markdown-outline",
            section: "markdown",
            title: "Markdown workspace with the outline open",
            caption: "The heading outline docked alongside the document, letting a reader \
                      jump straight to a section of a longer file.",
            capture: capture_markdown_outline,
        },
        // -- editor -----------------------------------------------------------
        Scenario {
            id: "text-editor-saved",
            section: "editor",
            title: "The native editor, everything saved",
            caption: "A fictional project's notes open for editing, with the origin, the \
                      commands, and the status band the editor reports through.",
            capture: capture_text_editor_saved,
        },
        Scenario {
            id: "text-editor-unsaved",
            section: "editor",
            title: "The native editor holding unsaved changes",
            caption: "The same document after typing: the banner, the chip state, and the \
                      status band all report one unsaved document rather than disagreeing.",
            capture: capture_text_editor_unsaved,
        },
        Scenario {
            id: "text-editor-compare",
            section: "editor",
            title: "Compare, after the file changed underneath the editor",
            caption: "The conflict banner stays pinned above a read-only, line-oriented \
                      comparison of the unsaved version against the one the file now holds, \
                      so the choice is made with both versions in sight.",
            capture: capture_text_editor_compare,
        },
        Scenario {
            id: "text-editor-dirty-close",
            section: "editor",
            title: "Closing the last view of a document with unsaved changes",
            caption: "Closing the only remaining view of a typed-in document asks before \
                      anything is lost, names the file and where it lives, and makes Save \
                      the action the keyboard already has hold of.",
            capture: capture_text_editor_dirty_close,
        },
        Scenario {
            id: "text-editor-conflict-chip",
            section: "editor",
            title: "A document's three states side by side on the chips",
            caption: "Saved, edited, and changed-underneath are three different \
                      shapes before they are three different colours, so the row \
                      still reads with the colour taken away.",
            capture: capture_text_editor_conflict_chip,
        },
        Scenario {
            id: "text-editor-find",
            section: "editor",
            title: "Find and Replace over the open document",
            caption: "One regular-expression dialect for the toolbar and the \
                      command area alike, saying which match of how many is \
                      current before anything is replaced.",
            capture: capture_text_editor_find,
        },
        Scenario {
            id: "text-editor-save-as",
            section: "editor",
            title: "Choosing where a document is written",
            caption: "One destination browser for both origins, stating an overwrite \
                      in words before the fact rather than asking a second time \
                      after the Save is already pressed.",
            capture: capture_text_editor_save_as,
        },
        Scenario {
            id: "text-editor-options",
            section: "editor",
            title: "The per-view editor options",
            caption: "Line numbers, a fixed column count and vi keys belong to \
                      this view alone: another window on the same file keeps its \
                      own, and none of them touches the text.",
            capture: capture_text_editor_options,
        },
        Scenario {
            id: "text-editor-vi-mode",
            section: "editor",
            title: "A view with vi compatibility switched on",
            caption: "The view says which mode it is in, in words, above the text \
                      as well as in the status bar, so a letter behaving as a \
                      command is never a surprise.",
            capture: capture_text_editor_vi_mode,
        },
        Scenario {
            id: "text-editor-vi-command",
            section: "editor",
            title: "The vi command area over the open document",
            caption: "One line above the persistent status bar, never in place of \
                      it: what the document is and where the caret sits stay \
                      readable while a command is being typed.",
            capture: capture_text_editor_vi_command,
        },
        Scenario {
            id: "text-editor-vi-search",
            section: "editor",
            title: "A vi search matching as it is typed",
            caption: "The same regular-expression dialect the Find bar uses, \
                      counting what Enter is about to accept rather than what was \
                      last run.",
            capture: capture_text_editor_vi_search,
        },
        Scenario {
            id: "text-editor-syntax",
            section: "editor",
            title: "Source coloured by what it means",
            caption: "Keywords, strings, numbers, types and comments are told apart by \
                      role rather than by language, from the same palette the \
                      Markdown preview's fenced code uses.",
            capture: capture_text_editor_syntax,
        },
        Scenario {
            id: "text-editor-preview",
            section: "editor",
            title: "A Markdown file as it opens",
            caption: "A Markdown document lands in Preview, reading as the finished \
                      thing; the mode control above it is the way back to the source.",
            capture: capture_text_editor_preview,
        },
        Scenario {
            id: "text-editor-split",
            section: "editor",
            title: "The editor split with its live preview",
            caption: "One view, two panes: the text on the left and the same document \
                      rendered on the right, so a Markdown change can be read as it is \
                      typed.",
            capture: capture_text_editor_split,
        },
        Scenario {
            id: "text-editor-outline",
            section: "editor",
            title: "The editor with the Markdown outline beside it",
            caption: "The same rail the Markdown viewer has, offered while the file \
                      renders as Markdown: the headings stay in reach without \
                      scrolling to find out where you are.",
            capture: capture_text_editor_outline,
        },
        // -- diagnostics --------------------------------------------------
        Scenario {
            id: "diagnostics-ssh-session",
            section: "diagnostics",
            title: "Session Inspector over an SSH session",
            caption: "The inspector overlay reporting connection facts and a pending \
                      host-key fingerprint for a plain SSH shell, without covering the \
                      terminal it describes.",
            capture: capture_diagnostics_ssh_session,
        },
        Scenario {
            id: "diagnostics-durable-session",
            section: "diagnostics",
            title: "Session Inspector over a durable tmux session",
            caption: "The same overlay for a session attached through fesTerm's durable \
                      persistence provider, showing the extra 'Durable session' facts and \
                      'Resume' (rather than 'Reconnect') action.",
            capture: capture_diagnostics_durable_session,
        },
        // -- chips ----------------------------------------------------------
        Scenario {
            id: "chips-verbose",
            section: "chips",
            title: "Session chips with details shown",
            caption: "The same five sessions with 'Show session details in chips' enabled: \
                      each chip carries a secondary line under its title.",
            capture: capture_chips_verbose,
        },
        Scenario {
            id: "chips-compact",
            section: "chips",
            title: "Session chips in compact mode",
            caption: "The identical five sessions with session details turned off: chips \
                      shrink to a single line, fitting more of them in the same row.",
            capture: capture_chips_compact,
        },
        Scenario {
            id: "chips-update-badge",
            section: "chips",
            title: "Update-available badge on the overflow control",
            caption: "After a background check finds a newer release, the 'More actions' \
                      control carries a single accent dot until the user opens About; its \
                      menu gains an 'Update to fesTerm 0.3.0…' entry above the usual items.",
            capture: capture_chips_update_badge,
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
fn finish<State>(harness: &mut Harness<'_, State>) -> image::RgbaImage {
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
    harness.get_by_label("Advanced settings").click();
    harness.run();
}

/// How a connect form should be prefilled before its screenshot, mirroring
/// what a user would already have typed. Every form is shown mid-use with a
/// representative, synthetic identity and (per the no-PII/no-credential rule)
/// an always-empty password.
enum Prefill {
    /// Fills the advanced form's separate Username/Host fields.
    UsernameAndHost {
        username: &'static str,
        host: &'static str,
    },
    /// Fills the single combined `user@host:port` shorthand field.
    QuickConnect(&'static str),
    /// Fills the serial form's "Device" field, which (unlike the SSH/SFTP
    /// fields) has no accessible label to query directly, so it is reached
    /// by clicking just below its plain text label instead.
    SerialDevice(&'static str),
}

impl Prefill {
    fn apply(self, harness: &mut Harness<'static, ()>) {
        match self {
            Prefill::UsernameAndHost { username, host } => {
                // Every destination pane now opens on the shorthand, so the
                // separate fields have to be asked for before they exist.
                harness.get_by_label("Use separate fields").click();
                harness.run();
                enter_text(harness, "Username", username);
                enter_text(harness, "Host", host);
            }
            Prefill::QuickConnect(value) => {
                enter_text(harness, "Quick connect", value);
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
    render_launcher_form(
        860.0,
        900.0,
        "SSH — Connect to a remote host over SSH",
        false,
        Prefill::QuickConnect("devuser@web-1.staging.example.com"),
    )
}

fn capture_ssh_connect_advanced() -> image::RgbaImage {
    render_launcher_form(
        860.0,
        1000.0,
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
        customize_local_shell: false,
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
        show_durable_session_in_status_bar: false,
        automatic_update_checks: true,
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
    harness.get_by_label("Record shortcut").click();
    harness.run();
    finish(&mut harness)
}

fn capture_keyboard_editor_filtered() -> image::RgbaImage {
    let mut harness = keyboard_editor_harness(960.0, 1300.0, KeyboardBindings::default());
    harness.run();
    harness.get_by_label("Search actions…").click();
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

// -- terminal-sessions -------------------------------------------------------

/// Discards every byte instead of retaining it: these scenarios never send
/// real keystrokes, but `TerminalView::show` requires a sink to route to.
#[derive(Default)]
struct GallerySink;

impl EncodedInputSink for GallerySink {
    fn record_encoded_input(&mut self, _bytes: &[u8]) {}
}

struct TerminalSessionState {
    view: TerminalView,
    terminal: Terminal,
    sink: GallerySink,
}

/// A remote shell mid-use: a status check, a log tail, and the resting
/// prompt. Every host, IP, and PID below is invented -- this is never bytes
/// captured from a real shell.
const SSH_TRANSCRIPT: &str = "\x1b[32mdevuser@web-1\x1b[0m:\x1b[34m~\x1b[0m$ uptime\r\n \
     14:32:07 up 21 days,  4:12,  1 user,  load average: 0.08, 0.05, 0.01\r\n\
     \x1b[32mdevuser@web-1\x1b[0m:\x1b[34m~\x1b[0m$ systemctl status festerm-agent --no-pager\r\n\
     \x1b[32m\u{25cf}\x1b[0m festerm-agent.service - fesTerm background agent\r\n\
     \x20\x20\x20\x20Loaded: loaded (/etc/systemd/system/festerm-agent.service; enabled)\r\n\
     \x20\x20\x20\x20Active: \x1b[32mactive (running)\x1b[0m since Mon 2024-09-01 03:11:02 UTC; 2 weeks 0 days ago\r\n\
     \x20\x20\x20Main PID: 4821 (festerm-agent)\r\n\
     \x20\x20\x20\x20\x20Tasks: 6 (limit: 4915)\r\n\
     \x20\x20\x20\x20Memory: 38.2M\r\n\
     \x1b[32mdevuser@web-1\x1b[0m:\x1b[34m~\x1b[0m$ tail -n 4 /var/log/festerm-agent.log\r\n\
     2024-09-15T14:31:58Z INFO  accepted connection from 198.51.100.42:53214\r\n\
     2024-09-15T14:31:58Z INFO  session negotiated: user=devuser\r\n\
     2024-09-15T14:32:03Z INFO  heartbeat ok\r\n\
     2024-09-15T14:32:07Z INFO  heartbeat ok\r\n\
     \x1b[32mdevuser@web-1\x1b[0m:\x1b[34m~\x1b[0m$ ";

/// A terminal-driven `sftp` client transcript, distinct from the graphical
/// file manager: a directory listing and a `get`. Invented, not captured.
const SFTP_CLI_TRANSCRIPT: &str = "Connected to web-1.staging.example.com.\r\n\
     sftp> cd /srv/releases\r\n\
     sftp> ls -l\r\n\
     -rw-r--r--   1 devuser  devuser   4831201 Sep 14 09:02 release-2024.09.tar.gz\r\n\
     -rw-r--r--   1 devuser  devuser   4790112 Aug 30 22:47 release-2024.08.tar.gz\r\n\
     drwxr-xr-x   2 devuser  devuser      4096 Sep 01 03:10 logs\r\n\
     sftp> get release-2024.09.tar.gz\r\n\
     Fetching /srv/releases/release-2024.09.tar.gz to release-2024.09.tar.gz\r\n\
     /srv/releases/release-2024.09.tar.gz                          100% 4718KB   6.1MB/s   00:00\r\n\
     sftp> ";

fn render_terminal_session(
    width: f32,
    height: f32,
    cols: usize,
    rows: usize,
    transcript: &str,
) -> image::RgbaImage {
    let mut terminal =
        Terminal::new(Dimensions::new(cols, rows).expect("valid gallery terminal size"))
            .expect("gallery terminal allocation");
    terminal.ingest(transcript.as_bytes());
    let state = TerminalSessionState {
        view: TerminalView::default(),
        terminal,
        sink: GallerySink,
    };
    let mut harness = Harness::builder()
        .with_size(egui::vec2(width, height))
        .build_ui_state(
            |ui, state: &mut TerminalSessionState| {
                state.view.show(ui, &mut state.terminal, &mut state.sink);
            },
            state,
        );
    // The view's first frame or two may only install the terminal font
    // family and request a repaint rather than paint the grid; settle
    // before rendering so the transcript is actually on screen.
    harness.run();
    harness.run();
    finish(&mut harness)
}

fn capture_terminal_ssh_session() -> image::RgbaImage {
    // The transcript fills ~15 rows; size the grid to that plus a few
    // blank rows below the prompt rather than the full 28-row grid a real
    // window might use, so the figure isn't half empty background.
    render_terminal_session(980.0, 400.0, 100, 18, SSH_TRANSCRIPT)
}

fn capture_terminal_sftp_cli() -> image::RgbaImage {
    // Same reasoning as above: the sftp transcript is ~10 rows, so a
    // 13-row grid leaves a few trailing blank rows without wasting most
    // of the figure on empty background.
    render_terminal_session(980.0, 312.0, 100, 13, SFTP_CLI_TRANSCRIPT)
}

// -- sftp-workspace -----------------------------------------------------------

/// A fixed synthetic instant used for every "modified"/"loaded" timestamp
/// in this file's SFTP fixtures. Using `SystemTime::now()` here would make
/// the rendered "Modified" column (and therefore the PNG bytes and their
/// recorded sha256) drift every time the gallery is regenerated -- exactly
/// the kind of non-determinism this generator is supposed to avoid.
/// 2024-09-15T12:00:00Z, chosen to line up with the synthetic release dates
/// used elsewhere in these fixtures.
fn synthetic_timestamp() -> SystemTime {
    std::time::UNIX_EPOCH + Duration::from_secs(1_726_401_600)
}

fn synthetic_directory_item(
    parent: &SftpPath,
    name: &str,
    file_type: SftpEntryType,
    size: Option<u64>,
) -> SftpDirectoryItem {
    SftpDirectoryItem {
        name: name.to_owned(),
        path: parent.join_child(name),
        file_type,
        size,
        modified_at: Some(synthetic_timestamp()),
        permissions: None,
    }
}

/// An invented local project directory -- never the real filesystem the
/// test process happens to run in.
fn synthetic_local_project_snapshot() -> SftpDirectorySnapshot {
    let path = SftpPath::local("/home/devuser/projects/nimbus-relay");
    let entries = vec![
        synthetic_directory_item(&path, "src", SftpEntryType::Directory, None),
        synthetic_directory_item(&path, "target", SftpEntryType::Directory, None),
        synthetic_directory_item(&path, "Cargo.lock", SftpEntryType::File, Some(89_213)),
        synthetic_directory_item(&path, "Cargo.toml", SftpEntryType::File, Some(612)),
        synthetic_directory_item(&path, "deploy.sh", SftpEntryType::File, Some(1_875)),
        synthetic_directory_item(&path, "README.md", SftpEntryType::File, Some(4_204)),
    ];
    SftpDirectorySnapshot {
        location: SftpLocation::Local,
        path,
        loaded_at: synthetic_timestamp(),
        entries,
    }
}

/// An invented remote releases directory on the same synthetic staging host
/// used elsewhere in this gallery.
fn synthetic_remote_releases_snapshot() -> SftpDirectorySnapshot {
    let path = SftpPath::remote("/srv/releases");
    let entries = vec![
        synthetic_directory_item(&path, "logs", SftpEntryType::Directory, None),
        synthetic_directory_item(&path, "config.yaml", SftpEntryType::File, Some(512)),
        synthetic_directory_item(
            &path,
            "release-2024.08.tar.gz",
            SftpEntryType::File,
            Some(4_790_112),
        ),
        synthetic_directory_item(
            &path,
            "release-2024.09.tar.gz",
            SftpEntryType::File,
            Some(4_831_201),
        ),
    ];
    SftpDirectorySnapshot {
        location: SftpLocation::Remote,
        path,
        loaded_at: synthetic_timestamp(),
        entries,
    }
}

fn capture_sftp_workspace_browser() -> image::RgbaImage {
    let ctx = egui::Context::default();
    let tab = SftpFileManagerTab::for_gallery(
        "devuser@web-1.staging.example.com".to_owned(),
        "devuser".to_owned(),
        "web-1.staging.example.com".to_owned(),
        22,
        synthetic_local_project_snapshot(),
        synthetic_remote_releases_snapshot(),
        Some("deploy.sh"),
        Some("release-2024.09.tar.gz"),
        SftpPaneOrderPreference::LocalLeft,
        &ctx,
    );
    let tab_id = AppState::for_test().active();
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1180.0, 760.0))
        .build_ui_state(
            move |ui, tab: &mut SftpFileManagerTab| {
                let _ = tab.show(ui, tab_id);
            },
            tab,
        );
    harness.run();
    finish(&mut harness)
}

// -- markdown -----------------------------------------------------------------

/// Invented prose about a fictional project, never a real repository
/// document: this is what the reader should see, not what fesTerm's own
/// documentation says.
fn synthetic_markdown_prose() -> String {
    "# Nimbus Relay — Project Notes\n\n\
     Nimbus Relay is a small message-relay service. This file exists only to \
     exercise the Markdown viewer with representative, synthetic content.\n\n\
     ## Overview\n\n\
     The relay accepts webhook events from `ci.example.net` and republishes \
     them onto an internal queue consumed by worker nodes such as \
     `web-1.staging.example.com`.\n\n\
     ## Configuration\n\n\
     - `relay.toml` — top-level service configuration\n\
     - `queues/*.toml` — one file per queue definition\n\
     - `RELAY_LOG_LEVEL` — overrides the default `info` log level\n\n\
     ```toml\n\
     [relay]\n\
     listen = \"0.0.0.0:8443\"\n\
     queue_backend = \"memory\"\n\
     ```\n\n\
     ## Operational Notes\n\n\
     1. Restart the service with `systemctl restart nimbus-relay`.\n\
     2. Queue depth is exposed on `/metrics` for the `operator` role to check.\n\
     3. See `docs/runbooks/relay-failover.md` for the failover procedure.\n\n\
     ## Known Issues\n\n\
     - Queue draining is slow under heavy backpressure.\n\
     - The `/health` endpoint does not yet report per-queue status.\n"
        .to_owned()
}

// -- editor -------------------------------------------------------------------

/// The editor reads a real file, so the gallery writes one into a temporary
/// directory of its own. The contents are the same invented project notes the
/// Markdown scenarios use, so nothing here comes from the machine it runs on.
fn render_text_editor(typed: Option<&str>) -> image::RgbaImage {
    render_text_editor_in(typed, crate::text_editor::EditorMode::Edit, None)
}

/// Arranges what happened *outside* fesTerm before the frame is drawn — a file
/// changed underneath an open editor, say — with the document open and
/// anything typed already in it.
type PrepareEditor<'a> =
    &'a dyn Fn(&crate::documents::SharedDocuments, &mut TextEditorTab, &std::path::Path);

/// A small, synthetic Rust file with one of everything the palette names.
fn synthetic_rust_source() -> &'static str {
    r#"//! Nimbus Relay: a small message-relay service.

use std::collections::HashMap;

/// How many events one flush may carry.
const BATCH_LIMIT: usize = 256;

#[derive(Debug, Default)]
pub struct Relay {
    queue: Vec<Event>,
    seen: HashMap<String, u64>,
    draining: bool,
}

impl Relay {
    pub fn new() -> Self {
        Self::default()
    }

    /// Accepts one webhook event, refusing anything past the batch limit.
    pub fn accept(&mut self, event: Event) -> Result<(), RelayError> {
        if self.queue.len() >= BATCH_LIMIT {
            return Err(RelayError::Full { limit: BATCH_LIMIT });
        }
        // A repeated delivery is not an error: the sender is retrying.
        let count = self.seen.entry(event.id.clone()).or_insert(0);
        *count += 1;
        self.queue.push(event);
        Ok(())
    }

    pub fn drain(&mut self) -> Vec<Event> {
        self.draining = true;
        std::mem::take(&mut self.queue)
    }
}
"#
}

fn render_text_editor_in(
    typed: Option<&str>,
    mode: crate::text_editor::EditorMode,
    prepare: Option<PrepareEditor<'_>>,
) -> image::RgbaImage {
    render_editor_over(
        "NOTES.md",
        &synthetic_markdown_prose(),
        typed,
        mode,
        prepare,
    )
}

/// The same editor, over a named fixture, so a source file can be shown being
/// coloured rather than only prose.
fn render_editor_over(
    file_name: &str,
    contents: &str,
    typed: Option<&str>,
    mode: crate::text_editor::EditorMode,
    prepare: Option<PrepareEditor<'_>>,
) -> image::RgbaImage {
    // The editor names the file it is editing, so the fixture's own path ends
    // up in the capture. A per-process temporary directory would put a
    // different machine-specific path in every run, rewriting these images
    // whether or not the UI changed and publishing the host's temporary
    // directory layout along with them. One fixed, synthetic-looking
    // directory keeps a capture a function of the UI alone; the scenarios run
    // one after another, each cleaning up after itself.
    let directory = if cfg!(unix) {
        std::path::PathBuf::from("/tmp/festerm-ui-gallery")
    } else {
        std::env::temp_dir().join("festerm-ui-gallery")
    };
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).expect("the gallery can write a temporary directory");
    let path = directory.join(file_name);
    std::fs::write(&path, contents).expect("the gallery can write its fixture");

    let documents = crate::documents::DocumentRegistry::shared();
    let id = documents
        .borrow_mut()
        .open_local(&path)
        .expect("the gallery fixture is an editable file");
    let mut editor = TextEditorTab::new(id, &documents);
    editor.set_mode_for_gallery(mode);
    if let Some(typed) = typed {
        editor.type_for_gallery(&documents, typed);
    }
    if let Some(prepare) = prepare {
        prepare(&documents, &mut editor, &path);
    }
    let tab_id = AppState::for_test().active();
    let mut harness = Harness::builder()
        .with_size(egui::vec2(900.0, 820.0))
        .build_ui_state(
            move |ui, state: &mut (crate::documents::SharedDocuments, TextEditorTab)| {
                let _ = state.1.show(ui, tab_id, &state.0);
            },
            (documents, editor),
        );
    harness.run();
    // A blinking caret makes a capture depend on when it was taken, which
    // would rewrite these files on every run and bury real changes in churn.
    harness
        .ctx
        .all_styles_mut(|style| style.visuals.text_cursor.blink = false);
    harness.run();
    let image = finish(&mut harness);
    let _ = std::fs::remove_dir_all(&directory);
    image
}

fn capture_text_editor_saved() -> image::RgbaImage {
    render_text_editor(None)
}

fn capture_text_editor_vi_mode() -> image::RgbaImage {
    render_text_editor_in(
        None,
        crate::text_editor::EditorMode::Edit,
        Some(&|_documents, editor, _path| {
            editor.enable_vi_for_gallery();
        }),
    )
}

fn capture_text_editor_vi_command() -> image::RgbaImage {
    render_text_editor_in(
        None,
        crate::text_editor::EditorMode::Edit,
        Some(&|_documents, editor, _path| {
            editor.open_command_area_for_gallery(crate::vi_command::CommandPrompt::Ex, "wq");
        }),
    )
}

fn capture_text_editor_vi_search() -> image::RgbaImage {
    render_text_editor_in(
        None,
        crate::text_editor::EditorMode::Edit,
        Some(&|_documents, editor, _path| {
            editor.open_command_area_for_gallery(
                crate::vi_command::CommandPrompt::SearchForward,
                "queue(s)?",
            );
        }),
    )
}

fn capture_text_editor_find() -> image::RgbaImage {
    render_text_editor_in(
        None,
        crate::text_editor::EditorMode::Edit,
        Some(&|_documents, editor, _path| {
            editor.open_find_for_gallery("relay", Some("service"));
        }),
    )
}

fn capture_text_editor_options() -> image::RgbaImage {
    render_text_editor_in(
        None,
        crate::text_editor::EditorMode::Edit,
        Some(&|_documents, editor, _path| {
            editor.open_options_for_gallery(Some(72));
        }),
    )
}

fn capture_text_editor_syntax() -> image::RgbaImage {
    render_editor_over(
        "relay.rs",
        synthetic_rust_source(),
        None,
        crate::text_editor::EditorMode::Edit,
        None,
    )
}

fn capture_text_editor_preview() -> image::RgbaImage {
    render_text_editor_in(None, crate::text_editor::EditorMode::Preview, None)
}

fn capture_text_editor_split() -> image::RgbaImage {
    render_text_editor_in(None, crate::text_editor::EditorMode::Split, None)
}

fn capture_text_editor_outline() -> image::RgbaImage {
    render_text_editor_in(
        None,
        crate::text_editor::EditorMode::Edit,
        Some(&|_documents, editor, _path| {
            editor.set_outline_for_gallery(true);
        }),
    )
}

/// A real conflict, arranged the way one actually happens: the document is
/// typed into, the file is then changed underneath it, and the refresh that
/// notices refuses to overwrite either version.
fn capture_text_editor_compare() -> image::RgbaImage {
    render_text_editor_in(
        Some("\n## Known Issues\n\n- The relay drops duplicate webhook deliveries silently.\n"),
        crate::text_editor::EditorMode::Edit,
        Some(&|documents, editor, path| {
            let changed = synthetic_markdown_prose()
                .replace(
                    "Nimbus Relay is a small message-relay",
                    "Nimbus Relay is a compact message-relay",
                )
                .replace(
                    "overrides the default `info` log level",
                    "overrides the default `debug` log level",
                );
            // The rewrite changes the file's length as well as its contents,
            // so the generation differs whatever the filesystem's clock
            // granularity is.
            std::fs::write(path, changed).expect("the gallery can rewrite its fixture");
            let id = editor.document();
            documents.borrow_mut().refresh(id);
            editor.open_compare_for_gallery(documents);
        }),
    )
}

/// The dirty-close prompt, arranged the way it actually arises: the whole
/// application, one editor tab, real typing, and a real close request.
fn capture_text_editor_dirty_close() -> image::RgbaImage {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);

    let directory = std::env::temp_dir().join(format!(
        "festerm-ui-gallery-dirty-close-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&directory).expect("the gallery can write a temporary directory");
    let path = directory.join("NOTES.md");
    fs::write(&path, synthetic_markdown_prose()).expect("the gallery can write its fixture");

    let context = egui::Context::default();
    let mut app = crate::app::FesTermApp::for_test_with_configuration(Configuration::empty());
    app.dispatch_for_gallery(
        crate::tabs::AppCommand::OpenTextEditor { path: path.clone() },
        &context,
    );

    let mut harness = Harness::builder()
        .with_size(egui::vec2(980.0, 700.0))
        .build_ui_state(
            |ui, app: &mut crate::app::FesTermApp| app.ui_content(ui),
            app,
        );
    harness.run();
    press_edit_for_gallery(&mut harness);
    let body = harness.get_by_role(egui::accesskit::Role::MultilineTextInput);
    body.focus();
    body.type_text(
        "\n## Known Issues\n\n- The relay drops duplicate webhook deliveries silently.\n",
    );
    settle_markdown_for_gallery(&mut harness);

    harness
        .state_mut()
        .request_active_tab_close_for_gallery(&context);
    harness.run();

    let image = finish(&mut harness);
    let _ = fs::remove_dir_all(&directory);
    image
}

fn capture_text_editor_conflict_chip() -> image::RgbaImage {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);

    let directory = std::env::temp_dir().join(format!(
        "festerm-ui-gallery-conflict-chip-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&directory).expect("the gallery can write a temporary directory");
    // Three files so the row shows all three document states at once, which is
    // the only way to review whether the shapes are told apart at the size
    // they are actually drawn (ADR 0034 §8).
    let saved = directory.join("README.md");
    let edited = directory.join("NOTES.md");
    let conflicted = directory.join("CHANGELOG.md");
    for path in [&saved, &edited, &conflicted] {
        fs::write(path, synthetic_markdown_prose()).expect("the gallery can write its fixture");
    }

    let context = egui::Context::default();
    let mut app = crate::app::FesTermApp::for_test_with_configuration(Configuration::empty());
    app.dispatch_for_gallery(
        crate::tabs::AppCommand::OpenTextEditor {
            path: saved.clone(),
        },
        &context,
    );
    app.dispatch_for_gallery(
        crate::tabs::AppCommand::OpenTextEditor {
            path: conflicted.clone(),
        },
        &context,
    );
    let conflicted_tab = app.active_tab_for_gallery();
    app.dispatch_for_gallery(
        crate::tabs::AppCommand::OpenTextEditor {
            path: edited.clone(),
        },
        &context,
    );

    let mut harness = Harness::builder()
        .with_size(egui::vec2(980.0, 700.0))
        .build_ui_state(
            |ui, app: &mut crate::app::FesTermApp| app.ui_content(ui),
            app,
        );
    harness.run();

    // Both documents are typed into, because a file changed underneath a view
    // that has no unsaved edits of its own is simply adopted -- it is the
    // collision between the two versions that is a conflict.
    press_edit_for_gallery(&mut harness);
    let body = harness.get_by_role(egui::accesskit::Role::MultilineTextInput);
    body.focus();
    body.type_text("\n## Known Issues\n\n- The relay drops duplicate deliveries.\n");
    settle_markdown_for_gallery(&mut harness);

    harness.state_mut().dispatch_for_gallery(
        crate::tabs::AppCommand::ActivateTab(conflicted_tab),
        &context,
    );
    harness.run();
    press_edit_for_gallery(&mut harness);
    let body = harness.get_by_role(egui::accesskit::Role::MultilineTextInput);
    body.focus();
    body.type_text("\n- Mine.\n");
    settle_markdown_for_gallery(&mut harness);

    fs::write(&conflicted, "Rewritten by somebody else.\n")
        .expect("the gallery can change a fixture underneath the editor");
    harness.state_mut().refresh_documents_for_gallery();
    harness.run();

    let image = finish(&mut harness);
    let _ = fs::remove_dir_all(&directory);
    image
}

/// The Save As sheet over a real editor, so the destination browser can be
/// reviewed against the same chrome it actually sits on.
fn capture_text_editor_save_as() -> image::RgbaImage {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);

    let directory = std::env::temp_dir().join(format!(
        "festerm-ui-gallery-save-as-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&directory).expect("the gallery can write a temporary directory");
    fs::create_dir_all(directory.join("docs")).expect("the gallery can write a subdirectory");
    fs::create_dir_all(directory.join("scripts")).expect("the gallery can write a subdirectory");
    let path = directory.join("NOTES.md");
    fs::write(&path, synthetic_markdown_prose()).expect("the gallery can write its fixture");
    fs::write(directory.join("README.md"), synthetic_markdown_prose())
        .expect("the gallery can write its fixture");
    fs::write(
        directory.join("relay.toml"),
        "[relay]\nlisten = \"0.0.0.0:8443\"\n",
    )
    .expect("the gallery can write its fixture");

    let context = egui::Context::default();
    let mut app = crate::app::FesTermApp::for_test_with_configuration(Configuration::empty());
    app.dispatch_for_gallery(crate::tabs::AppCommand::OpenTextEditor { path }, &context);

    let mut harness = Harness::builder()
        .with_size(egui::vec2(1080.0, 760.0))
        .build_ui_state(
            |ui, app: &mut crate::app::FesTermApp| app.ui_content(ui),
            app,
        );
    harness.run();

    harness.get_by_label("Save As").click();
    harness.run();
    // The listing arrives on a worker thread, so the sheet is empty for the
    // first frames after it opens.
    for _ in 0..40 {
        harness.run();
        if harness.query_by_label_contains("README.md").is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(15));
    }

    let image = finish(&mut harness);
    let _ = fs::remove_dir_all(&directory);
    image
}

fn capture_text_editor_unsaved() -> image::RgbaImage {
    render_text_editor(Some(
        "\n## Known Issues\n\n- The relay drops duplicate webhook deliveries silently.\n",
    ))
}

fn synthetic_markdown_source() -> RemoteMarkdownSource {
    RemoteMarkdownSource::new(
        "web-1.staging.example.com",
        22,
        RemoteSourceOwner::username("devuser").expect("synthetic username is non-empty"),
        "SHA256:EXAMPLE0000000000000000000000000000000000",
        "/home/devuser/projects/nimbus-relay/NOTES.md",
        1,
    )
    .expect("synthetic remote markdown source is valid")
}

fn render_markdown(
    width: f32,
    height: f32,
    adjust: impl FnOnce(&mut MarkdownViewerTab),
    interact: impl FnOnce(&mut Harness<'_, MarkdownViewerTab>),
) -> image::RgbaImage {
    let mut tab = MarkdownViewerTab::open_remote(
        synthetic_markdown_source(),
        "~/projects/nimbus-relay/NOTES.md".to_owned(),
        synthetic_markdown_prose().into_bytes(),
    );
    adjust(&mut tab);
    let tab_id = AppState::for_test().active();
    let mut harness = Harness::builder()
        .with_size(egui::vec2(width, height))
        .build_ui_state(
            move |ui, tab: &mut MarkdownViewerTab| {
                let _ = tab.show(ui, tab_id);
            },
            tab,
        );
    harness.run();
    interact(&mut harness);
    harness.run();
    finish(&mut harness)
}

fn capture_markdown_preview() -> image::RgbaImage {
    render_markdown(900.0, 820.0, |_| {}, |_| {})
}

fn capture_markdown_source() -> image::RgbaImage {
    render_markdown(900.0, 820.0, |tab| tab.toggle_mode(), |_| {})
}

fn capture_markdown_outline() -> image::RgbaImage {
    // The outline is open by default, so a plain preview render can't be
    // told apart from this scenario. Click a later heading so the outline's
    // navigation behaviour (highlighting the selected heading and scrolling
    // the document to it) is actually visible in the picture.
    render_markdown(
        900.0,
        820.0,
        |_| {},
        |harness| {
            harness
                .get_by_role_and_label(
                    egui::accesskit::Role::Button,
                    "Heading level 2: Known Issues",
                )
                .click();
        },
    )
}

// -- diagnostics ----------------------------------------------------------

struct DiagnosticsSessionState {
    view: TerminalView,
    terminal: Terminal,
    sink: GallerySink,
}

fn render_inspector_over_terminal(
    width: f32,
    height: f32,
    cols: usize,
    rows: usize,
    transcript: &str,
    content: InspectorContent<'static>,
) -> image::RgbaImage {
    let mut terminal =
        Terminal::new(Dimensions::new(cols, rows).expect("valid gallery terminal size"))
            .expect("gallery terminal allocation");
    terminal.ingest(transcript.as_bytes());
    let state = DiagnosticsSessionState {
        view: TerminalView::default(),
        terminal,
        sink: GallerySink,
    };
    let mut harness = Harness::builder()
        .with_size(egui::vec2(width, height))
        .build_ui_state(
            move |ui, state: &mut DiagnosticsSessionState| {
                let content_rect = ui.max_rect();
                state.view.show(ui, &mut state.terminal, &mut state.sink);
                let _ = inspector::show(ui.ctx(), content_rect, content.clone(), false);
            },
            state,
        );
    harness.run();
    harness.run();
    finish(&mut harness)
}

fn capture_diagnostics_ssh_session() -> image::RgbaImage {
    render_inspector_over_terminal(
        980.0,
        900.0,
        100,
        28,
        SSH_TRANSCRIPT,
        InspectorContent {
            subject_id: 1,
            identity: "Staging web-1",
            type_label: "SSH session",
            state: "Connected",
            state_message: None,
            state_color: festerm_ui_egui::theme::STATUS_RUNNING,
            grid: Some("100 × 28"),
            terminal_title: Some("devuser@web-1: ~"),
            profile: Some("Staging web-1"),
            transport: TransportFacts::Ssh {
                username: "devuser",
                host: "web-1.staging.example.com",
                port: 22,
            },
            trust_fingerprint: Some("SHA256:EXAMPLE0000000000000000000000000000000000"),
            diagnostics: "queue_depth=0 last_outcome=Delivered",
            input_recording: false,
            input_report: "No input has been recorded for this session.",
            reconnect_available: true,
            open_sftp_available: true,
            persistent_session: None,
        },
    )
}

fn capture_diagnostics_durable_session() -> image::RgbaImage {
    render_inspector_over_terminal(
        980.0,
        900.0,
        100,
        28,
        SSH_TRANSCRIPT,
        InspectorContent {
            subject_id: 2,
            identity: "deploy-watch",
            type_label: "tmux session",
            state: "Attached",
            state_message: None,
            state_color: festerm_ui_egui::theme::STATUS_RUNNING,
            grid: Some("100 × 28"),
            terminal_title: Some("deploy-watch"),
            profile: None,
            transport: TransportFacts::Local,
            trust_fingerprint: None,
            diagnostics: "queue_depth=0 last_outcome=Delivered",
            input_recording: false,
            input_report: "No input has been recorded for this session.",
            reconnect_available: true,
            open_sftp_available: false,
            persistent_session: Some(PersistentSessionFacts {
                provider_label: "tmux",
                session_name: "deploy-watch",
            }),
        },
    )
}

// -- chips ------------------------------------------------------------------

fn synthetic_chip_view_models() -> Vec<ChipViewModel> {
    vec![
        ChipViewModel {
            id: ChipId(1),
            primary: "New Session".to_owned(),
            secondary: None,
            status: ChipStatus::Neutral,
            closable: false,
            renamable: false,
            movable_across_windows: false,
            quick_switch_number: Some(1),
            pulse_new_output: false,
        },
        ChipViewModel {
            id: ChipId(2),
            primary: "Staging web-1".to_owned(),
            secondary: Some("web-1.staging.example.com".to_owned()),
            status: ChipStatus::Connected,
            closable: true,
            renamable: true,
            movable_across_windows: true,
            quick_switch_number: Some(2),
            pulse_new_output: false,
        },
        ChipViewModel {
            id: ChipId(3),
            primary: "deploy-watch".to_owned(),
            secondary: Some("tmux session".to_owned()),
            status: ChipStatus::Connected,
            closable: true,
            renamable: true,
            movable_across_windows: true,
            quick_switch_number: Some(3),
            pulse_new_output: true,
        },
        ChipViewModel {
            id: ChipId(4),
            primary: "Bastion host".to_owned(),
            secondary: Some("Reconnecting…".to_owned()),
            status: ChipStatus::Reconnecting,
            closable: true,
            renamable: true,
            movable_across_windows: true,
            quick_switch_number: Some(4),
            pulse_new_output: false,
        },
        ChipViewModel {
            id: ChipId(5),
            primary: "NOTES.md".to_owned(),
            secondary: Some("Markdown".to_owned()),
            status: ChipStatus::Neutral,
            closable: true,
            renamable: false,
            movable_across_windows: true,
            quick_switch_number: Some(5),
            pulse_new_output: false,
        },
    ]
}

fn render_chips(
    show_session_details: bool,
    available_update: Option<&'static str>,
) -> image::RgbaImage {
    let chips = synthetic_chip_view_models();
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1000.0, 160.0))
        .build_ui(move |ui| {
            let _ = chrome::show(
                ui,
                &chips,
                ChipId(2),
                false,
                true,
                ChipLayout::Wrap,
                show_session_details,
                false,
                available_update,
            );
        });
    // One chip deliberately demonstrates the "new output" pulse cue
    // (`pulse_new_output: true`), which keeps requesting a repaint forever
    // by design. `finish`'s `Harness::run()` would treat that as a hang and
    // panic, so settle with a fixed number of steps instead of waiting for
    // repaints to stop.
    harness.remove_cursor();
    harness.run_steps(2);
    let image = harness.render().expect("headless render must succeed");
    trim_to_content(&image, GALLERY_MARGIN)
}

fn capture_chips_verbose() -> image::RgbaImage {
    render_chips(true, None)
}

fn capture_chips_compact() -> image::RgbaImage {
    render_chips(false, None)
}

fn capture_chips_update_badge() -> image::RgbaImage {
    render_chips(true, Some("0.3.0"))
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

/// Markdown opens in Preview (ADR 0034 §4); a capture that needs the body
/// presses Edit first, the way a reader does.
/// Lets the Markdown parse behind the outline and preview settle before the
/// frame is captured. Typing restarts a debounce that is deliberately not
/// instant, and a capture taken inside it would show a stale outline.
fn settle_markdown_for_gallery(harness: &mut Harness<'_, crate::app::FesTermApp>) {
    let slack = crate::markdown_viewer::PREVIEW_DEBOUNCE + std::time::Duration::from_millis(25);
    // Once past the debounce the parse happens on the next frame, and the
    // frame after that draws it. `run_ok` rather than `run` because the pane
    // asks for the repaint it is waiting on, which is not a runaway.
    for _ in 0..2 {
        std::thread::sleep(slack);
        let _ = harness.run_ok();
    }
}

fn press_edit_for_gallery(harness: &mut Harness<'_, crate::app::FesTermApp>) {
    if harness
        .query_all_by_role(egui::accesskit::Role::MultilineTextInput)
        .next()
        .is_some()
    {
        return;
    }
    harness
        .query_all_by_label("Edit")
        .next()
        .expect("the Edit segment of the mode control")
        .click();
    harness.run();
}
