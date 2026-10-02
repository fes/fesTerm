//! "State of the UI" screenshot gallery: a deterministic, headless capture
//! pipeline for a reviewer-facing markdown document.
//!
//! This module exists to be *regenerated repeatedly*. A reviewer critiques
//! the UI, changes get made, and the whole gallery is rebuilt from scratch --
//! so capture must be a script, not an agent driving the app by hand. Every
//! scenario below renders the real product UI (`crate::screens`,
//! `crate::keyboard`) against `egui_kittest`'s headless harness, using only
//! fixture data owned by this module: no personal config, shell or clipboard
//! is read. File-backed examples perform real I/O on owned synthetic files.
//!
//! # Synthetic resources and publication review
//! Every profile name, hostname, username, and path below is synthetic:
//! hosts live under `example.com`/`example.net` or use RFC 5737
//! documentation-reserved IP ranges, usernames are generic role names
//! (`devuser`, `builder`, `operator`), and no real key material, credential,
//! clipboard content, or shell history ever appears.
//! Actual local document/picker labels can include this checkout's physical
//! location and username. Review them before publication; owned fixture files
//! do not by themselves provide canonical, publication-safe display identities.
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
    EncodedInputSink, TerminalContextMenuAction, TerminalView, TerminalViewOptions,
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
    let mut scenarios = vec![
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
        Scenario {
            id: "terminal-copy-paste-menu",
            section: "terminal-sessions",
            title: "Copy and Paste in the terminal context menu",
            caption: "A selection remains visible while the terminal's context menu offers \
                      Copy and Paste alongside Find, without sending the secondary click to \
                      the remote program.",
            capture: capture_terminal_copy_paste_menu,
        },
        Scenario {
            id: "terminal-path-menu",
            section: "terminal-sessions",
            title: "Actions for a detected terminal path",
            caption: "Secondary-clicking a filesystem path freezes the detected target in \
                      the menu, where it can be opened in the editor or copied exactly as \
                      a path.",
            capture: capture_terminal_path_menu,
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
        Scenario {
            id: "palette-command-menu",
            section: "surface-probes",
            title: "Production command palette over Launcher",
            caption: "The real application-owned command list and its initial search focus, \
                      over synthetic Launcher metadata. This gallery-only fixture is not \
                      included in the bounded performance batch.",
            capture: capture_palette_normal,
        },
        Scenario {
            id: "palette-command-menu-narrow",
            section: "surface-probes",
            title: "Production command palette at a narrow root",
            caption: "The same real command palette at 360 × 516 logical pixels, preserving \
                      its production sizing rather than drawing a fitted substitute. \
                      Geometry/native qualification remains pending.",
            capture: capture_palette_narrow,
        },
    ];
    scenarios.extend(bounded_surface_scenes().into_iter().map(|scene| Scenario {
        id: scene.id,
        section: "surface-probes",
        title: scene.title,
        caption: scene.caption,
        capture: scene.capture,
    }));
    scenarios.extend(style_review_scenarios());
    scenarios
}

fn selected_gallery_scenarios(selection: Option<&str>) -> Result<Vec<Scenario>, String> {
    let scenes = scenarios();
    let Some(selection) = selection else {
        return Ok(scenes);
    };
    let selection = selection.trim();
    if selection == "style-review" {
        return Ok(scenes
            .into_iter()
            .filter(|scene| scene.id.starts_with("style-"))
            .collect());
    }
    let mut requested = std::collections::BTreeSet::new();
    for id in selection.split(',').map(str::trim) {
        if id.is_empty() {
            return Err("gallery selection must contain nonempty exact scene IDs".into());
        }
        if !requested.insert(id) {
            return Err(format!("duplicate gallery scene selection: {id}"));
        }
        if !scenes.iter().any(|scene| scene.id == id) {
            return Err(format!("unknown gallery scene selection: {id}"));
        }
    }
    Ok(scenes
        .into_iter()
        .filter(|scene| requested.contains(scene.id))
        .collect())
}

#[test]
fn surface_gallery_selection_preserves_default_and_limits_curated_captures() {
    assert_eq!(selected_gallery_scenarios(None).unwrap().len(), 125);
    let curated = selected_gallery_scenarios(Some("style-review")).unwrap();
    assert_eq!(curated.len(), 23);
    assert_eq!(
        curated
            .iter()
            .filter(|scene| scene.id.ends_with("-short"))
            .count(),
        5
    );
    assert!(curated.iter().all(|scene| scene.id.starts_with("style-")));
    let exact = selected_gallery_scenarios(Some(
        "style-about-ready-licenses-short, style-palette-long-identity-narrow",
    ))
    .unwrap();
    let identifiers: std::collections::BTreeSet<_> = exact.iter().map(|scene| scene.id).collect();
    assert_eq!(
        identifiers,
        std::collections::BTreeSet::from([
            "style-about-ready-licenses-short",
            "style-palette-long-identity-narrow",
        ])
    );
    for invalid in [
        "",
        " ",
        "unknown-scene",
        "style-about-ready-licenses-short,",
        "style-about-ready-licenses-short,style-about-ready-licenses-short",
    ] {
        assert!(selected_gallery_scenarios(Some(invalid)).is_err());
    }
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

impl SurfaceDriver for SurfaceProbe {
    fn step(&mut self) {
        let events = std::mem::take(&mut *self.events.lock());
        if events.is_empty() {
            self.frame().textures_delta.clear();
        } else {
            for event in events {
                self.input.events.push(event);
                self.frame().textures_delta.clear();
            }
        }
    }
    fn fixture(&self) -> &SurfaceFixture {
        &self.fixture
    }
    fn has_label(&self, label: &str) -> bool {
        self.node().query_all_by_label(label).next().is_some()
    }
    fn has_label_containing(&self, label: &str) -> bool {
        self.node().query_by_label_contains(label).is_some()
    }
    fn focused(&self, label: &str) -> bool {
        self.node()
            .query_all_by_label(label)
            .any(|node| node.is_focused())
    }
    fn click(&self, label: &str, secondary: bool) {
        if secondary {
            self.node().get_by_label(label).click_secondary();
        } else {
            self.node().get_by_label(label).click();
        }
    }
    fn event(&self, event: egui::Event) {
        self.events.lock().push(event);
    }
}

fn surface_fixture_root() -> PathBuf {
    let root = gallery_fixture_directory("surface-batch");
    match std::env::var("FESTERM_UI_SURFACE_FIXTURE_RUN") {
        Ok(tag) => {
            assert!(
                !tag.is_empty()
                    && tag
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
                "fixture run tag must be a simple owned-directory name"
            );
            root.join(tag)
        }
        Err(_) => root,
    }
}

fn synthetic_configuration() -> Configuration {
    Configuration::new(synthetic_profiles()).expect("gallery configuration is valid")
}

#[test]
fn surface_catalog_matches_reconciled_matrix_without_dropping_controls() {
    let matrix: serde_json::Value = serde_json::from_str(include_str!(
        "../../../validation/windows-warp/surface-matrix.json"
    ))
    .unwrap();
    let mut declared = std::collections::BTreeSet::new();
    for state in matrix["bounded_batch"]["states"].as_array().unwrap() {
        let id = state["scene"].as_str().unwrap();
        assert!(declared.insert(id.to_owned()));
        assert!(declared.insert(format!("{id}-narrow")));
    }
    let scenes = bounded_surface_scenes();
    let actual: std::collections::BTreeSet<_> =
        scenes.iter().map(|scene| scene.id.to_owned()).collect();
    assert_eq!(actual, declared);
    assert_eq!(scenes.len(), 52);
    let gallery = scenarios();
    let ids: std::collections::BTreeSet<_> = gallery.iter().map(|scene| scene.id).collect();
    assert_eq!(ids.len(), gallery.len());
    assert_eq!(gallery.len(), 48 + 52 + 2 + 23);
    let gallery_only: std::collections::BTreeSet<_> = matrix["gallery_only_states"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|state| state["scenes"].as_array().unwrap())
        .map(|id| id.as_str().unwrap())
        .collect();
    let style_ids: std::collections::BTreeSet<_> = style_review_scenarios()
        .iter()
        .map(|scene| scene.id)
        .chain(["palette-command-menu", "palette-command-menu-narrow"])
        .collect();
    assert_eq!(gallery_only, style_ids);
    assert_eq!(
        matrix["original_controls"]["warp"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
    assert_eq!(
        matrix["original_controls"]["construction"]
            .as_array()
            .unwrap()
            .len(),
        12
    );
}

#[test]
fn surface_menu_actions_keep_typed_targets_and_never_send_terminal_bytes() {
    for (kind, label) in [
        (SurfaceKind::TerminalPath, "Open in editor"),
        (SurfaceKind::TerminalDisabledPath, "Open in editor"),
        (SurfaceKind::TerminalLink, "Open link"),
        (
            SurfaceKind::TerminalHistory,
            "Open Terminal History in Editor",
        ),
        (
            SurfaceKind::TerminalReadOnlyHistory,
            "Save Terminal History As…",
        ),
        (SurfaceKind::TerminalSelection, "Find in Terminal"),
    ] {
        let scene = bounded_surface_scenes()
            .into_iter()
            .find(|scene| scene.kind == kind && !scene.narrow)
            .unwrap();
        let directory = unique_surface_fixture_directory();
        let mut probe = SurfaceProbe::new(scene, &directory, 1.0);
        probe.prepare(kind, |delta| delta.clear());
        probe.click(label, false);
        probe.step();
        let SurfaceBody::Terminal(state) = &mut probe.fixture.body else {
            unreachable!()
        };
        match kind {
            SurfaceKind::TerminalPath => {
                assert!(state.view.take_context_action_request());
                assert_eq!(
                    state
                        .options
                        .context_menu_action
                        .as_ref()
                        .unwrap()
                        .copy
                        .as_ref()
                        .unwrap()
                        .1,
                    "/srv/releases/NOTES.md"
                );
            }
            SurfaceKind::TerminalDisabledPath => assert!(!state.view.take_context_action_request()),
            SurfaceKind::TerminalLink => assert_eq!(
                state
                    .view
                    .take_link_requests()
                    .iter()
                    .map(|link| link.as_ref())
                    .collect::<Vec<_>>(),
                ["https://fixture.example.com/release"]
            ),
            SurfaceKind::TerminalHistory => assert_eq!(
                state.view.take_history_actions(),
                [festerm_ui_egui::TerminalHistoryAction::OpenInEditor]
            ),
            SurfaceKind::TerminalReadOnlyHistory => assert_eq!(
                state.view.take_history_actions(),
                [festerm_ui_egui::TerminalHistoryAction::SaveAs]
            ),
            SurfaceKind::TerminalSelection => assert!(state.view.take_find_request()),
            _ => unreachable!(),
        }
        probe.fixture.assert_no_transport_input();
        drop(probe);
        fs::remove_dir_all(directory).unwrap();
    }
}

#[test]
fn surface_safety_cancellation_preserves_targets_and_unsaved_source() {
    for kind in [
        SurfaceKind::LiveClose,
        SurfaceKind::Paste,
        SurfaceKind::DirtyClose,
    ] {
        let scene = bounded_surface_scenes()
            .into_iter()
            .find(|scene| scene.kind == kind && !scene.narrow)
            .unwrap();
        let directory = unique_surface_fixture_directory();
        let mut probe = SurfaceProbe::new(scene, &directory, 1.0);
        probe.prepare(kind, |delta| delta.clear());
        probe.click("Cancel", false);
        probe.step();
        probe.step();
        probe.fixture.assert_no_transport_input();
        assert!(!probe.has_label_containing("This is the last view of this document."));
        if kind == SurfaceKind::DirtyClose {
            let SurfaceBody::App(app) = &probe.fixture.body else {
                unreachable!()
            };
            assert!(app.active_document_dirty_for_gallery());
            assert_eq!(
                fs::read_to_string(directory.join("SOURCE.md")).unwrap(),
                "# Synthetic document\n"
            );
        }
        drop(probe);
        fs::remove_dir_all(directory).unwrap();
    }
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

fn gallery_harness<'a, State>(
    size: egui::Vec2,
    show: impl FnMut(&mut egui::Ui, &mut State) + 'a,
    state: State,
) -> Harness<'a, State> {
    let mut builder = Harness::builder().with_size(size);
    if OWNED_STYLE_SCOPE.with(|slot| slot.borrow().is_some()) {
        let metadata = std::sync::Arc::new(std::sync::Mutex::new(serde_json::Value::Null));
        STYLE_CAPTURE_ADAPTER.with(|slot| *slot.borrow_mut() = Some(metadata.clone()));
        let mut setup = egui_kittest::wgpu::default_wgpu_setup();
        let eframe::egui_wgpu::WgpuSetup::CreateNew(options) = &mut setup else {
            unreachable!("default kittest setup")
        };
        let select = options
            .native_adapter_selector
            .take()
            .expect("default software-preferred selector");
        options.native_adapter_selector = Some(std::sync::Arc::new(move |adapters, surface| {
            let selected = select(adapters, surface)?;
            let info = selected.get_info();
            *metadata.lock().unwrap() = serde_json::json!({
                "name": info.name, "backend": format!("{:?}", info.backend),
                "device_type": format!("{:?}", info.device_type),
                "driver": info.driver, "driver_info": info.driver_info,
                "vendor": info.vendor, "device": info.device,
                "policy": "unchanged kittest default software-preferred selection; predictable renderer options",
            });
            Ok(selected)
        }));
        builder = builder.wgpu_setup(setup);
    }
    let mut harness = builder.build_ui_state(show, state);
    // The test app constructor bypasses production context setup. Apply it to
    // the harness's actual context, not a separate context used by fixtures.
    harness.ctx.set_theme(egui::ThemePreference::Dark);
    harness
        .ctx
        .set_visuals(festerm_ui_egui::theme::default_visuals());
    // set_visuals replaces kittest's non-blinking cursor setting.
    harness
        .ctx
        .all_styles_mut(|style| style.visuals.text_cursor.blink = false);
    harness.step();
    harness
}

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
    let mut harness = gallery_harness(
        egui::vec2(width, height),
        move |ui, ()| {
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
        },
        (),
    );
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
    let mut harness = gallery_harness(
        egui::vec2(width, height),
        move |ui, ()| {
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
        },
        (),
    );
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

#[test]
#[ignore = "optional full-resolution Windows WARP UI replay, not a native latency measurement"]
fn replay_warp_ui_surfaces() {
    use egui_kittest::wgpu::{create_render_state, default_wgpu_setup, WgpuTestRenderer};
    use egui_kittest::TestRenderer;
    use std::time::Instant;

    assert_eq!(
        std::env::var("FESTERM_RUN_OPTIONAL_VALIDATION").as_deref(),
        Ok("1"),
        "set FESTERM_RUN_OPTIONAL_VALIDATION=1",
    );
    let output =
        PathBuf::from(std::env::var_os("FESTERM_WARP_UI_OUT").expect("set FESTERM_WARP_UI_OUT"));
    assert!(
        !output.exists(),
        "use a fresh WARP evidence directory; retain failed attempts"
    );
    fs::create_dir_all(&output).expect("create replay output directory");
    let provenance = surface_probe_provenance();
    for surface in ["launcher", "settings", "profiles", "terminal"] {
        let scene_output = output.join(surface);
        fs::create_dir(&scene_output).unwrap();
        fs::write(scene_output.join("status.json"), r#"{"status":"running"}"#).unwrap();
        let preparation_started = Instant::now();
        let state = create_render_state(
            default_wgpu_setup(),
            eframe::egui_wgpu::RendererOptions::default(),
        );
        let info = state.adapter.get_info();
        assert!(
            cfg!(windows)
                && info.backend == eframe::wgpu::Backend::Dx12
                && info.device_type == eframe::wgpu::DeviceType::Cpu,
            "replay requires Windows DX12 WARP, got {info:?}",
        );
        let mut renderer = WgpuTestRenderer::from_render_state(state.clone());
        let mut app =
            crate::app::FesTermApp::for_test_with_configuration(synthetic_configuration());
        let mut terminal = Terminal::new(Dimensions::new(160, 48).unwrap()).unwrap();
        terminal.ingest(SSH_TRANSCRIPT.as_bytes());
        let mut view = TerminalView::default();
        let mut sink = GallerySink::default();
        let context = egui::Context::default();
        context.set_theme(egui::ThemePreference::Dark);
        context.set_visuals(festerm_ui_egui::theme::default_visuals());
        context.all_styles_mut(|style| {
            style.visuals.text_cursor.blink = false;
            style.animation_time = 0.0;
        });
        crate::software_background::install(&context, &state);
        crate::direct2d::install_from_environment(&context, Some(&state));
        match surface {
            "settings" => app.dispatch_for_test(AppCommand::OpenSettings, &context),
            "profiles" => app.dispatch_for_test(AppCommand::OpenProfiles, &context),
            _ => {}
        }
        let mut input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1774.0, 1075.0),
            )),
            ..Default::default()
        };
        input
            .viewports
            .get_mut(&egui::ViewportId::ROOT)
            .unwrap()
            .native_pixels_per_point = Some(2.0);
        let preparation_ms = preparation_started.elapsed().as_secs_f64() * 1000.0;
        // Use eframe's root-UI entry point. Harness::build_ui adds another large
        // filled frame around the app, which would dominate this measurement.
        let mut step = || {
            context.run_ui(input.clone(), |ui| {
                if surface == "terminal" {
                    view.show(ui, &mut terminal, &mut sink);
                } else {
                    app.ui_content(ui);
                }
            })
        };
        let started = Instant::now();
        let mut first = step();
        let first_ui_ms = started.elapsed().as_secs_f64() * 1000.0;
        renderer.handle_delta(&mut first.textures_delta);
        let started = Instant::now();
        std::hint::black_box(context.tessellate(first.shapes, first.pixels_per_point));
        let first_tessellation_ms = started.elapsed().as_secs_f64() * 1000.0;
        let mut warmup_ui_ms = Vec::new();
        let mut warmup_tessellation_ms = Vec::new();
        for _ in 0..8 {
            let started = Instant::now();
            let mut frame = step();
            renderer.handle_delta(&mut frame.textures_delta);
            warmup_ui_ms.push(started.elapsed().as_secs_f64() * 1000.0);
            let started = Instant::now();
            std::hint::black_box(context.tessellate(frame.shapes, frame.pixels_per_point));
            warmup_tessellation_ms.push(started.elapsed().as_secs_f64() * 1000.0);
        }
        let mut frame = step();
        renderer.handle_delta(&mut frame.textures_delta);
        let started = Instant::now();
        let image = renderer
            .render(&context, &frame)
            .expect("surface replay warmup");
        let first_ready_draw_readback_ms = started.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(image.dimensions(), (3548, 2150));
        image
            .save(output.join(format!("{surface}.png")))
            .expect("save surface replay pixels");
        if let Some(reference) = std::env::var_os("FESTERM_WARP_UI_REFERENCE") {
            let reference = image::open(PathBuf::from(reference).join(format!("{surface}.png")))
                .expect("open reference replay image")
                .into_rgba8();
            assert_eq!(image.dimensions(), reference.dimensions());
            let mismatches = image
                .pixels()
                .zip(reference.pixels())
                .filter(|(a, b)| a != b)
                .count();
            assert_eq!(mismatches, 0, "{surface} framebuffer changed");
        }
        let mut ui_times = Vec::new();
        for _ in 0..20 {
            let started = Instant::now();
            frame = step();
            renderer.handle_delta(&mut frame.textures_delta);
            ui_times.push(started.elapsed().as_secs_f64() * 1000.0);
        }
        let mut tessellation_times = Vec::new();
        for _ in 0..20 {
            let started = Instant::now();
            std::hint::black_box(
                context.tessellate(frame.shapes.clone(), context.pixels_per_point()),
            );
            tessellation_times.push(started.elapsed().as_secs_f64() * 1000.0);
        }
        let started = Instant::now();
        renderer
            .render(&context, &frame)
            .expect("settled surface replay");
        let warmup_draw_readback_ms = started.elapsed().as_secs_f64() * 1000.0;
        let mut draw_times = Vec::new();
        for _ in 0..5 {
            let started = Instant::now();
            renderer
                .render(&context, &frame)
                .expect("completed surface replay frame");
            draw_times.push(started.elapsed().as_secs_f64() * 1000.0);
        }
        let report = serde_json::json!({
            "schema": "festerm-warp-ui-replay-v2",
            "scene": surface, "viewport_points": [1774, 1075],
            "physical_pixels": [3548, 2150], "pixels_per_point": 2,
            "adapter": format!("{info:?}"), "provenance": provenance,
            "fixture_state_verified": false,
            "preparation_ms": preparation_ms,
            "preparation_scope": "renderer/adapter setup, app/terminal/context construction and installation of existing painting paths",
            "first_ui_ms": first_ui_ms, "first_tessellation_ms": first_tessellation_ms,
            "warmup_ui_ms": warmup_ui_ms, "warmup_tessellation_ms": warmup_tessellation_ms,
            "first_ready_draw_readback_ms": first_ready_draw_readback_ms,
            "warmup_draw_readback_ms": [warmup_draw_readback_ms],
            "steady_ui": crate::surface_performance::timing_distribution(&ui_times),
            "steady_tessellation": crate::surface_performance::timing_distribution(&tessellation_times),
            "steady_completed_draw_readback": crate::surface_performance::timing_distribution(&draw_times),
            "ui_scope": "raw Context::run_ui; steady and warmup include texture-delta handling, first_ui excludes it",
            "draw_scope": "completed drawing, tessellation, submission, synchronization and CPU image readback; NOT native input-to-display or presentation latency",
            "cold_process_start_ms": serde_json::Value::Null,
            "cold_process_start_status": "not measured inside a running test process",
        });
        fs::write(
            scene_output.join("report.json"),
            serde_json::to_string_pretty(&report).unwrap(),
        )
        .unwrap();
        fs::write(scene_output.join("status.json"), r#"{"status":"complete"}"#).unwrap();
        eprintln!("{report}");
    }
    for scene in bounded_surface_scenes() {
        let scene_output = output.join(scene.id);
        fs::create_dir(&scene_output).unwrap();
        fs::write(scene_output.join("status.json"), r#"{"status":"running"}"#).unwrap();
        let initialization = Instant::now();
        let state = create_render_state(
            default_wgpu_setup(),
            eframe::egui_wgpu::RendererOptions::default(),
        );
        let info = state.adapter.get_info();
        assert!(
            cfg!(windows)
                && info.backend == eframe::wgpu::Backend::Dx12
                && info.device_type == eframe::wgpu::DeviceType::Cpu,
            "replay requires Windows DX12 WARP, got {info:?}",
        );
        let mut renderer = WgpuTestRenderer::from_render_state(state.clone());
        let renderer_initialization_ms = initialization.elapsed().as_secs_f64() * 1000.0;
        let fixture_directory = surface_fixture_directory(scene.id);
        let mut probe = SurfaceProbe::with_setup(scene, &fixture_directory, 2.0, |context| {
            crate::software_background::install(context, &state);
            crate::direct2d::install_from_environment(context, Some(&state));
        });
        let started = Instant::now();
        std::hint::black_box(probe.context.tessellate(
            probe.first_output.shapes.clone(),
            probe.first_output.pixels_per_point,
        ));
        let first_tessellation_ms = started.elapsed().as_secs_f64() * 1000.0;
        probe.prepare(scene.kind, |delta| renderer.handle_delta(delta));
        let mut warmup_ui_ms = Vec::new();
        let mut warmup_tessellation_ms = Vec::new();
        for _ in 0..8 {
            let mut frame = probe.frame();
            warmup_ui_ms.push(probe.last_ui_ms);
            renderer.handle_delta(&mut frame.textures_delta);
            let started = Instant::now();
            std::hint::black_box(
                probe
                    .context
                    .tessellate(frame.shapes, frame.pixels_per_point),
            );
            warmup_tessellation_ms.push(started.elapsed().as_secs_f64() * 1000.0);
        }
        let mut frame = probe.frame();
        probe.assert_state(scene.kind);
        renderer.handle_delta(&mut frame.textures_delta);
        let started = Instant::now();
        let image = renderer
            .render(&probe.context, &frame)
            .expect("ready surface completed render");
        let first_ready_draw_readback_ms = started.elapsed().as_secs_f64() * 1000.0;
        let dimensions = ((scene.size().x * 2.0) as u32, (scene.size().y * 2.0) as u32);
        assert_eq!(image.dimensions(), dimensions);
        let background = image.get_pixel(0, 0);
        assert!(
            image.pixels().filter(|pixel| *pixel != background).count() > 128,
            "{} must produce a nonblank real framebuffer",
            scene.id,
        );
        image
            .save(output.join(format!("{}.png", scene.id)))
            .unwrap();
        if let Some(reference) = std::env::var_os("FESTERM_WARP_UI_REFERENCE") {
            let reference = image::open(PathBuf::from(reference).join(format!("{}.png", scene.id)))
                .expect("matching expanded scene reference")
                .into_rgba8();
            assert_eq!(image.dimensions(), reference.dimensions());
            assert_eq!(
                image.as_raw(),
                reference.as_raw(),
                "{} framebuffer changed",
                scene.id
            );
        }
        let mut ui_times = Vec::new();
        let mut tessellation_times = Vec::new();
        for _ in 0..20 {
            frame = probe.frame();
            ui_times.push(probe.last_ui_ms);
            renderer.handle_delta(&mut frame.textures_delta);
            let started = Instant::now();
            std::hint::black_box(
                probe
                    .context
                    .tessellate(frame.shapes.clone(), frame.pixels_per_point),
            );
            tessellation_times.push(started.elapsed().as_secs_f64() * 1000.0);
        }
        let mut draw_times = Vec::new();
        for _ in 0..5 {
            let started = Instant::now();
            renderer
                .render(&probe.context, &frame)
                .expect("completed drawing, synchronization and readback");
            draw_times.push(started.elapsed().as_secs_f64() * 1000.0);
        }
        let report = serde_json::json!({
            "schema": "festerm-warp-ui-replay-v2",
            "scene": scene.id, "viewport_points": [scene.size().x, scene.size().y],
            "physical_pixels": [dimensions.0, dimensions.1], "pixels_per_point": 2,
            "adapter": format!("{info:?}"), "provenance": provenance,
            "renderer_initialization_ms": renderer_initialization_ms,
            "fixture_state_verified": true,
            "preparation_ms": probe.preparation_ms, "readiness_ms": probe.readiness_ms,
            "preparation_ui_ms": probe.preparation_ui_ms,
            "first_ui_ms": probe.first_ui_ms, "first_tessellation_ms": first_tessellation_ms,
            "warmup_ui_ms": warmup_ui_ms, "warmup_tessellation_ms": warmup_tessellation_ms,
            "first_ready_draw_readback_ms": first_ready_draw_readback_ms,
            "steady_ui": crate::surface_performance::timing_distribution(&ui_times),
            "steady_tessellation": crate::surface_performance::timing_distribution(&tessellation_times),
            "steady_completed_draw_readback": crate::surface_performance::timing_distribution(&draw_times),
            "ui_scope": "raw production Dark Context::run_ui, AccessKit enabled; excludes query-tree updates and texture uploads",
            "preparation_scope": "owned synthetic fixture creation and actual model construction; readiness separately includes interaction frames and actual picker worker completion",
            "draw_scope": "completed drawing, tessellation, submission, synchronization and CPU readback; NOT native input-to-display or presentation latency",
            "cold_process_start_ms": serde_json::Value::Null,
            "cold_process_start_status": "not measured inside a running test process; caches may be warm from earlier scenes",
        });
        fs::write(
            scene_output.join("report.json"),
            serde_json::to_string_pretty(&report).unwrap(),
        )
        .unwrap();
        fs::write(scene_output.join("status.json"), r#"{"status":"complete"}"#).unwrap();
        eprintln!("{report}");
        drop(probe);
        fs::remove_dir_all(fixture_directory).unwrap();
    }
}

/// Renders the whole Settings page tall enough for every card to be laid
/// out (so the cropped band below is always available), then crops to the
/// vertical band from `top_label` (or the very top when `None`) up to just
/// above `bottom_label`.
fn render_settings_card(top_label: Option<&str>, bottom_label: &str) -> image::RgbaImage {
    let model = synthetic_settings_view_model();
    let mut harness = gallery_harness(
        egui::vec2(900.0, 3200.0),
        move |ui, ()| {
            let _ = screens::show_settings(ui, model.clone());
        },
        (),
    );
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
    gallery_harness(
        egui::vec2(width, height),
        move |ui, ()| {
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
        },
        (),
    )
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
    let mut harness = gallery_harness(
        egui::vec2(width, height),
        move |ui, ()| {
            let _ = screens::show_profiles(
                ui,
                tab_id,
                &configuration,
                pending_edit.clone(),
                None::<NewProfileKind>,
                festerm_config::PersistenceProviderKind::FestermSessiond,
            );
        },
        (),
    );
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
struct GallerySink {
    input: Vec<u8>,
}

impl EncodedInputSink for GallerySink {
    fn record_encoded_input(&mut self, bytes: &[u8]) {
        self.input.extend_from_slice(bytes);
    }
}

struct TerminalSessionState {
    view: TerminalView,
    terminal: Terminal,
    sink: GallerySink,
}

struct TerminalMenuState {
    view: TerminalView,
    terminal: Terminal,
    sink: GallerySink,
    options: TerminalViewOptions,
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
        sink: GallerySink::default(),
    };
    let mut harness = gallery_harness(
        egui::vec2(width, height),
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

fn terminal_cell_center(
    harness: &Harness<'_, TerminalMenuState>,
    column: usize,
    row: usize,
) -> egui::Pos2 {
    let diagnostics = harness.state().view.diagnostics();
    let grid = diagnostics.grid_rect.expect("terminal grid is rendered");
    let dimensions = diagnostics
        .calculated_dimensions
        .expect("terminal dimensions are calculated");
    let cell_width = grid.width() / dimensions.columns() as f32;
    let cell_height = grid.height() / dimensions.rows() as f32;
    egui::pos2(
        grid.left() + (column as f32 + 0.5) * cell_width,
        grid.top() + (row as f32 + 0.5) * cell_height,
    )
}

fn drag_terminal_selection(
    harness: &mut Harness<'_, TerminalMenuState>,
    start: (usize, usize),
    end: (usize, usize),
) {
    let start = terminal_cell_center(harness, start.0, start.1);
    let end = terminal_cell_center(harness, end.0, end.1);
    harness.event(egui::Event::PointerMoved(start));
    harness.event(egui::Event::PointerButton {
        pos: start,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    });
    harness.event(egui::Event::PointerMoved(end));
    harness.event(egui::Event::PointerButton {
        pos: end,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    harness.run();
}

fn open_terminal_context_menu(
    harness: &mut Harness<'_, TerminalMenuState>,
    column: usize,
    row: usize,
) {
    let position = terminal_cell_center(harness, column, row);
    harness.event(egui::Event::PointerMoved(position));
    for pressed in [true, false] {
        harness.event(egui::Event::PointerButton {
            pos: position,
            button: egui::PointerButton::Secondary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
    }
    harness.run();
}

fn terminal_menu_harness(
    transcript: &str,
    options: TerminalViewOptions,
) -> Harness<'static, TerminalMenuState> {
    let mut terminal = Terminal::new(Dimensions::new(92, 12).expect("valid gallery terminal size"))
        .expect("gallery terminal allocation");
    terminal.ingest(transcript.as_bytes());
    let state = TerminalMenuState {
        view: TerminalView::default(),
        terminal,
        sink: GallerySink::default(),
        options,
    };
    let mut harness = gallery_harness(
        egui::vec2(940.0, 320.0),
        |ui, state: &mut TerminalMenuState| {
            let options = state.options.clone();
            state
                .view
                .show_with_options(ui, &mut state.terminal, &mut state.sink, options);
        },
        state,
    );
    harness.run();
    harness.run();
    harness
}

fn capture_terminal_copy_paste_menu() -> image::RgbaImage {
    let mut harness = terminal_menu_harness(
        "Deployment completed successfully.\r\n\
         12 services healthy; 0 pending restarts.\r\n\
         devuser@web-1:~$ ",
        TerminalViewOptions::default(),
    );
    drag_terminal_selection(&mut harness, (0, 0), (19, 0));
    open_terminal_context_menu(&mut harness, 19, 0);
    assert!(harness.query_by_label("Copy").is_some());
    assert!(harness.query_by_label("Paste").is_some());
    finish(&mut harness)
}

fn capture_terminal_path_menu() -> image::RgbaImage {
    const PATH: &str = "/srv/releases/NOTES.md";
    let mut harness = terminal_menu_harness(
        "devuser@web-1:~$ cat /srv/releases/NOTES.md\r\n\
         # Release checklist\r\n\
         devuser@web-1:~$ ",
        TerminalViewOptions {
            context_menu_action: Some(TerminalContextMenuAction {
                label: "Open in editor".to_owned(),
                preview: format!("devuser@web-1 · {PATH}"),
                enabled: true,
                disabled_reason: None,
                copy: Some(("Copy path".to_owned(), PATH.to_owned())),
            }),
            ..TerminalViewOptions::default()
        },
    );
    // The path begins at column 22; open over its NOTES.md component.
    open_terminal_context_menu(&mut harness, 38, 0);
    assert!(harness.query_by_label("Open in editor").is_some());
    assert!(harness.query_by_label("Copy path").is_some());
    finish(&mut harness)
}

#[test]
fn gallery_frames_use_production_theme_for_app_and_popup() {
    fn has_fill_at(shape: &egui::Shape, position: egui::Pos2, expected: egui::Color32) -> bool {
        match shape {
            egui::Shape::Rect(rect) => rect.fill == expected && rect.rect.contains(position),
            egui::Shape::Vec(shapes) => shapes
                .iter()
                .any(|shape| has_fill_at(shape, position, expected)),
            _ => false,
        }
    }

    fn assert_fill_at<State>(
        harness: &Harness<'_, State>,
        position: egui::Pos2,
        expected: egui::Color32,
    ) {
        assert!(
            harness.output().shapes.iter().any(|shape| has_fill_at(
                &shape.shape,
                position,
                expected
            )),
            "production fill {expected:?} must be painted at {position:?}",
        );
    }

    let mut menu = terminal_menu_harness("Synthetic menu fixture.\r\n", Default::default());
    open_terminal_context_menu(&mut menu, 4, 0);
    menu.remove_cursor();
    menu.run();
    assert_fill_at(
        &menu,
        menu.get_by_label("Paste").rect().center(),
        festerm_ui_egui::theme::SURFACE_OVERLAY,
    );
    assert_eq!(menu.ctx.theme(), egui::Theme::Dark);

    let app = crate::app::FesTermApp::for_test_with_configuration(Configuration::empty());
    let mut app = gallery_harness(
        egui::vec2(980.0, 700.0),
        |ui, app: &mut crate::app::FesTermApp| app.ui_content(ui),
        app,
    );
    let context = app.ctx.clone();
    app.state_mut()
        .dispatch_for_gallery(AppCommand::OpenSettings, &context);
    app.remove_cursor();
    app.run();
    assert!(app.query_by_label("INTERFACE").is_some());
    assert_fill_at(
        &app,
        egui::pos2(9.0, 9.0),
        festerm_ui_egui::theme::SURFACE_WINDOW,
    );
    assert_eq!(app.ctx.theme(), egui::Theme::Dark);
    assert!(
        !app.ctx
            .style_of(egui::Theme::Dark)
            .visuals
            .text_cursor
            .blink
    );
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

fn gallery_fixture_directory(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("gallery package belongs to the workspace")
        .join("target")
        .join("ui-gallery-fixtures")
        .join(name)
}

fn reset_gallery_fixture_directory(name: &str) -> PathBuf {
    let directory = gallery_fixture_directory(name);
    let _ = fs::remove_dir_all(&directory);
    fs::create_dir_all(&directory).expect("the gallery can write its isolated fixture directory");
    directory
}

fn set_synthetic_modified_time(path: &Path) {
    #[cfg(windows)]
    let file = {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_WRITE_ATTRIBUTES: u32 = 0x0100;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        // Directory handles need backup semantics; timestamps need attribute
        // write access for both files and directories.
        fs::OpenOptions::new()
            .access_mode(FILE_WRITE_ATTRIBUTES)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)
    };
    #[cfg(not(windows))]
    let file = fs::File::open(path);
    file.and_then(|file| file.set_times(fs::FileTimes::new().set_modified(synthetic_timestamp())))
        .unwrap_or_else(|error| {
            panic!(
                "setting deterministic gallery timestamp for {} must succeed: {error}",
                path.display()
            )
        });
}

#[test]
fn gallery_fixture_timestamps_are_fixed_for_files_and_directories() {
    let directory = reset_gallery_fixture_directory("timestamp-regression");
    let file = directory.join("NOTES.md");
    fs::write(&file, "Synthetic timestamp fixture.\n").unwrap();
    for path in [&directory, &file] {
        set_synthetic_modified_time(path);
        assert_eq!(
            fs::metadata(path).unwrap().modified().unwrap(),
            synthetic_timestamp()
        );
    }
    fs::remove_dir_all(directory).unwrap();
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
    let mut harness = gallery_harness(
        egui::vec2(1180.0, 760.0),
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

/// The editor reads a real file, so the gallery writes one into an isolated
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
    // Keep fixtures in the worktree rather than a personal OS temp directory,
    // since the editor and picker display the fixture path.
    let directory = reset_gallery_fixture_directory("editor");
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
    let mut harness = gallery_harness(
        egui::vec2(900.0, 820.0),
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
    let directory = reset_gallery_fixture_directory("dirty-close");
    let path = directory.join("NOTES.md");
    fs::write(&path, synthetic_markdown_prose()).expect("the gallery can write its fixture");

    let context = egui::Context::default();
    let mut app = crate::app::FesTermApp::for_test_with_configuration(Configuration::empty());
    app.dispatch_for_gallery(
        crate::tabs::AppCommand::OpenTextEditor { path: path.clone() },
        &context,
    );

    let mut harness = gallery_harness(
        egui::vec2(980.0, 700.0),
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
    let directory = reset_gallery_fixture_directory("conflict-chip");
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

    let mut harness = gallery_harness(
        egui::vec2(980.0, 700.0),
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
    let directory = reset_gallery_fixture_directory("save-as");
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
    for entry in ["docs", "scripts", "NOTES.md", "README.md", "relay.toml"] {
        set_synthetic_modified_time(&directory.join(entry));
    }

    let context = egui::Context::default();
    let mut app = crate::app::FesTermApp::for_test_with_configuration(Configuration::empty());
    app.dispatch_for_gallery(crate::tabs::AppCommand::OpenTextEditor { path }, &context);

    let mut harness = gallery_harness(
        egui::vec2(1080.0, 760.0),
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
    assert!(
        harness.query_by_label_contains("README.md").is_some(),
        "Save As capture must reach the real task's ready listing"
    );

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
    let mut harness = gallery_harness(
        egui::vec2(width, height),
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
        sink: GallerySink::default(),
    };
    let mut harness = gallery_harness(
        egui::vec2(width, height),
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
    let mut harness = gallery_harness(
        egui::vec2(1000.0, 160.0),
        move |ui, ()| {
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
        },
        (),
    );
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

// These fixtures are shared by the existing gallery, construction profile and
// completed-render replay. They arrange product state, never draw substitutes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SurfaceKind {
    About,
    Licenses,
    UpdateIdle,
    UpdateReady,
    UpdateInstalled,
    ChromeMenu,
    ChipFirst,
    ChipMiddle,
    ChipLastReadOnly,
    TerminalSelection,
    TerminalReadOnlySelection,
    TerminalLink,
    TerminalPath,
    TerminalDisabledPath,
    TerminalHistory,
    TerminalReadOnlyHistory,
    LiveClose,
    Paste,
    DirtyClose,
    OpenSmall,
    OpenLarge,
    OpenError,
    SaveSmall,
    SaveLarge,
    SaveError,
    SaveOverwrite,
}

#[derive(Clone, Copy)]
pub(crate) struct SurfaceScene {
    pub(crate) id: &'static str,
    pub(crate) kind: SurfaceKind,
    pub(crate) narrow: bool,
    title: &'static str,
    caption: &'static str,
    capture: fn() -> image::RgbaImage,
}

impl SurfaceScene {
    pub(crate) fn size(self) -> egui::Vec2 {
        if self.narrow {
            egui::vec2(360.0, 516.0)
        } else {
            egui::vec2(1180.0, 760.0)
        }
    }

    pub(crate) fn items(self) -> usize {
        match self.kind {
            SurfaceKind::OpenSmall | SurfaceKind::SaveSmall | SurfaceKind::SaveOverwrite => 12,
            SurfaceKind::OpenLarge | SurfaceKind::SaveLarge => 5000,
            _ => 0,
        }
    }
}

pub(crate) fn bounded_surface_scenes() -> Vec<SurfaceScene> {
    macro_rules! pair {
        ($id:literal, $kind:ident, $title:literal, $caption:literal) => {{
            fn normal() -> image::RgbaImage {
                capture_surface_gallery(SurfaceKind::$kind, false)
            }
            fn narrow() -> image::RgbaImage {
                capture_surface_gallery(SurfaceKind::$kind, true)
            }
            [
                SurfaceScene {
                    id: $id,
                    kind: SurfaceKind::$kind,
                    narrow: false,
                    title: $title,
                    caption: $caption,
                    capture: normal,
                },
                SurfaceScene {
                    id: concat!($id, "-narrow"),
                    kind: SurfaceKind::$kind,
                    narrow: true,
                    title: concat!($title, " — narrow root"),
                    caption: concat!(
                        $caption,
                        " The root is 360 × 516 logical pixels; this is not native acceptance."
                    ),
                    capture: narrow,
                },
            ]
        }};
    }
    [
        pair!("about-unavailable", About, "About in a developer build", "The real About dialog honestly reports unavailable updating. No endpoint is contacted."),
        pair!("about-licenses", Licenses, "About with licenses expanded", "Bundled-font attribution is expanded in the real bounded license scroll area."),
        pair!("about-update-idle", UpdateIdle, "About with an idle synthetic updater", "The existing in-process no-update backend supplies the Check for Updates state; no network or installed package is used."),
        pair!("about-update-ready", UpdateReady, "About with a verified synthetic update", "The existing test controller supplies Ready to Install. This does not verify downloading, signatures or restart."),
        pair!("about-update-installed", UpdateInstalled, "About after a synthetic install", "The existing test controller supplies the Installed message without installing anything."),
        pair!("chrome-expanded-menu", ChromeMenu, "Expanded More actions", "The actual full-application overflow menu retains applicable controls and narrow-width collapse policy."),
        pair!("chip-context-first", ChipFirst, "First live session chip context menu", "The first chip omits Move left; no transport is running."),
        pair!("chip-context-middle-inactive", ChipMiddle, "Inactive middle chip context menu", "A real session chip is targeted without activating it; both move directions remain applicable."),
        pair!("chip-context-last-read-only", ChipLastReadOnly, "Last read-only session chip context menu", "An exited fake session is the last chip; Move right is absent and Close remains available."),
        pair!("terminal-context-selection", TerminalSelection, "Live terminal selection menu", "A real pointer drag selects repository-owned text; Copy, Paste and Find are shown without snapshot actions."),
        pair!("terminal-context-read-only-selection", TerminalReadOnlySelection, "Read-only terminal selection menu", "The real terminal view preserves selection and Copy/Find but omits Paste."),
        pair!("terminal-context-link", TerminalLink, "Explicit terminal link menu", "An OSC 8 documentation URL retains its exact Open/Copy target."),
        pair!("terminal-context-path", TerminalPath, "Detected-path action menu", "The production view displays an application-owned frozen synthetic path action and Copy path."),
        pair!("terminal-context-disabled-path", TerminalDisabledPath, "Unavailable detected-path action", "The same frozen target has a disabled Open action with an honest reason; Copy path remains available."),
        pair!("terminal-context-history", TerminalHistory, "Live terminal history menu", "Without selection, the real view offers Find and application-enabled history snapshot actions."),
        pair!("terminal-context-read-only-history", TerminalReadOnlyHistory, "Read-only terminal history menu", "History remains inspectable without Paste or accepted terminal keystrokes."),
        pair!("safety-live-close", LiveClose, "Live SSH close confirmation", "The real close request names the synthetic SSH target and generation; Cancel is the safe default."),
        pair!("safety-paste", Paste, "Bounded risky-paste preview", "Application paste policy produces a large multiline confirmation over a fake live session. No clipboard is read or input delivered."),
        pair!("safety-dirty-document", DirtyClose, "Final dirty-document close", "A real document edit and final-view close request produce the production modal, with Save rather than Cancel as its documented default."),
        pair!("open-file-small-ready", OpenSmall, "Open File with a ready small directory", "Twelve synthetic files arrive through the actual local-directory worker. Ready state is asserted before capture or steady timing."),
        pair!("open-file-large-ready", OpenLarge, "Open File with a ready large directory", "Five thousand synthetic files arrive through the actual worker; the real virtualized listing is used."),
        pair!("open-file-error", OpenError, "Open File directory error", "A nonexistent owned fixture path produces the real task error, which is asserted rather than substituting a loading frame."),
        pair!("save-as-small-ready", SaveSmall, "Save As with a ready small directory", "The real editor and destination picker browse twelve task-loaded synthetic files; the destination is a new name."),
        pair!("save-as-large-ready", SaveLarge, "Save As with a ready large directory", "The real destination picker browses five thousand task-loaded files, with a pinned name/action footer."),
        pair!("save-as-error", SaveError, "Save As directory error", "A nonexistent owned destination produces the actual directory-worker error; no save is performed."),
        pair!("save-as-overwrite", SaveOverwrite, "Save As existing-target notice", "A task-loaded existing file produces the production overwrite notice. The fixture never presses Save."),
    ]
    .into_iter()
    .flatten()
    .collect()
}

enum SurfaceBody {
    App(Box<crate::app::FesTermApp>),
    Terminal(Box<TerminalMenuState>),
}

pub(crate) struct SurfaceFixture {
    body: SurfaceBody,
    transport: Option<crate::session_controller::fake::FakeSshSession>,
    active_tab: Option<crate::tabs::TabId>,
}

impl SurfaceFixture {
    fn install_context_assets(&mut self, kind: SurfaceKind, context: &egui::Context) {
        if matches!(
            kind,
            SurfaceKind::About
                | SurfaceKind::Licenses
                | SurfaceKind::UpdateIdle
                | SurfaceKind::UpdateReady
                | SurfaceKind::UpdateInstalled
        ) {
            let SurfaceBody::App(app) = &mut self.body else {
                unreachable!()
            };
            app.install_about_icon_for_gallery(context);
        }
    }

    fn new(kind: SurfaceKind, directory: &Path, context: &egui::Context) -> Self {
        Self::with_inputs(kind, directory, context, false)
    }

    fn with_inputs(
        kind: SurfaceKind,
        directory: &Path,
        context: &egui::Context,
        reuse: bool,
    ) -> Self {
        use SurfaceKind as K;
        if reuse {
            assert!(directory.is_dir(), "retained owned scene directory");
        } else {
            fs::create_dir_all(directory).expect("create owned surface fixture");
        }
        if matches!(
            kind,
            K::TerminalSelection
                | K::TerminalReadOnlySelection
                | K::TerminalLink
                | K::TerminalPath
                | K::TerminalDisabledPath
                | K::TerminalHistory
                | K::TerminalReadOnlyHistory
        ) {
            let mut terminal = Terminal::new(Dimensions::new(92, 12).unwrap()).unwrap();
            let transcript = if kind == K::TerminalLink {
                "\x1b]8;;https://fixture.example.com/release\x1b\\Release documentation\x1b]8;;\x1b\\\r\nfixture$ "
            } else if matches!(kind, K::TerminalPath | K::TerminalDisabledPath) {
                "fixture$ cat /srv/releases/NOTES.md\r\nfixture$ "
            } else {
                "Synthetic terminal history.\r\nfixture$ "
            };
            terminal.ingest(transcript.as_bytes());
            let read_only = matches!(
                kind,
                K::TerminalReadOnlySelection | K::TerminalReadOnlyHistory
            );
            let path_action = matches!(kind, K::TerminalPath | K::TerminalDisabledPath);
            return Self {
                body: SurfaceBody::Terminal(Box::new(TerminalMenuState {
                    terminal,
                    view: TerminalView::default(),
                    sink: GallerySink::default(),
                    options: TerminalViewOptions {
                        paste_available: !read_only,
                        keyboard_input_enabled: !read_only,
                        defer_paste_to_application: true,
                        history_snapshot_actions: true,
                        context_menu_action: path_action.then(|| TerminalContextMenuAction {
                            label: "Open in editor".into(),
                            preview: "devuser@fixture.example.com · /srv/releases/NOTES.md".into(),
                            enabled: kind != K::TerminalDisabledPath,
                            disabled_reason: (kind == K::TerminalDisabledPath)
                                .then(|| "The synthetic session is unavailable.".into()),
                            copy: Some(("Copy path".into(), "/srv/releases/NOTES.md".into())),
                        }),
                        ..Default::default()
                    },
                })),
                transport: None,
                active_tab: None,
            };
        }
        let (mut app, tab, transport) = if matches!(
            kind,
            K::LiveClose | K::Paste | K::ChipFirst | K::ChipMiddle | K::ChipLastReadOnly
        ) {
            let (mut app, tab, transport) =
                crate::app::FesTermApp::for_test_with_fake_ssh_session([]);
            app.configure_session_for_gallery(kind == K::ChipLastReadOnly, context);
            (app, Some(tab), Some(transport))
        } else {
            (
                crate::app::FesTermApp::for_test_with_configuration(Configuration::empty()),
                None,
                None,
            )
        };
        match kind {
            K::About | K::Licenses | K::UpdateIdle | K::UpdateReady | K::UpdateInstalled => {
                let updates = match kind {
                    K::UpdateIdle => crate::updates::UpdateController::inert_for_test(),
                    K::UpdateReady => crate::updates::UpdateController::ready_to_install_for_test(),
                    K::UpdateInstalled => crate::updates::UpdateController::installed_for_test(),
                    _ => crate::updates::UpdateController::unavailable_for_test(),
                };
                app.open_about_for_gallery(kind == K::Licenses, updates, context);
            }
            K::LiveClose => app.request_active_tab_close_for_gallery(context),
            K::Paste => app.request_paste_for_gallery(
                (0..100)
                    .map(|n| format!("echo synthetic-line-{n:03}\n"))
                    .collect(),
                context,
            ),
            K::ChipMiddle => {
                app.dispatch_for_gallery(AppCommand::OpenLauncher, context);
                app.dispatch_for_gallery(AppCommand::OpenSettings, context);
                app.dispatch_for_gallery(AppCommand::MoveTabRight(tab.unwrap()), context);
            }
            K::ChipLastReadOnly => {
                app.dispatch_for_gallery(AppCommand::OpenLauncher, context);
                app.dispatch_for_gallery(AppCommand::MoveTabRight(tab.unwrap()), context);
            }
            K::DirtyClose
            | K::OpenSmall
            | K::OpenLarge
            | K::OpenError
            | K::SaveSmall
            | K::SaveLarge
            | K::SaveError
            | K::SaveOverwrite => {
                let listing = directory.join("listing");
                if reuse {
                    assert!(listing.is_dir());
                } else {
                    fs::create_dir(&listing).unwrap();
                }
                let count = if matches!(kind, K::OpenLarge | K::SaveLarge) {
                    5000
                } else {
                    12
                };
                for index in 0..count {
                    let file = listing.join(format!("entry-{index:06}.md"));
                    prepare_style_file(&file, b"# Synthetic fixture\n", reuse);
                }
                let source = directory.join("SOURCE.md");
                prepare_style_file(&source, b"# Synthetic document\n", reuse);
                if !reuse {
                    set_synthetic_modified_time(&listing);
                }
                let destination = if matches!(kind, K::OpenError | K::SaveError) {
                    directory.join("missing-fixture-directory")
                } else {
                    listing
                };
                if kind == K::DirtyClose {
                    app.dirty_document_close_for_gallery(source, context);
                } else if matches!(kind, K::OpenSmall | K::OpenLarge | K::OpenError) {
                    app.open_file_picker_for_gallery(destination, context);
                } else {
                    let name = if kind == K::SaveOverwrite {
                        "entry-000000.md"
                    } else {
                        "NEW.md"
                    };
                    app.open_save_as_for_gallery(source, destination, name.into(), context);
                }
            }
            _ => {}
        }
        let active_tab = tab.map(|_| app.active_tab_for_gallery());
        Self {
            body: SurfaceBody::App(Box::new(app)),
            transport,
            active_tab,
        }
    }

    pub(crate) fn show(&mut self, ui: &mut egui::Ui) {
        match &mut self.body {
            SurfaceBody::App(app) => app.ui_content(ui),
            SurfaceBody::Terminal(state) => {
                state.view.show_with_options(
                    ui,
                    &mut state.terminal,
                    &mut state.sink,
                    state.options.clone(),
                );
            }
        }
    }

    fn assert_no_transport_input(&self) {
        if let SurfaceBody::Terminal(state) = &self.body {
            assert!(
                state.sink.input.is_empty(),
                "local menu input must not reach the terminal sink"
            );
        }
        if let Some(transport) = &self.transport {
            assert!(
                transport.sent().is_empty(),
                "fixture arrangement must not deliver input"
            );
        }
        if let (SurfaceBody::App(app), Some(active)) = (&self.body, self.active_tab) {
            assert_eq!(
                app.active_tab_for_gallery(),
                active,
                "menu must not activate its target"
            );
        }
    }
}

trait SurfaceDriver {
    fn step(&mut self);
    fn fixture(&self) -> &SurfaceFixture;
    fn has_label(&self, label: &str) -> bool;
    fn has_label_containing(&self, label: &str) -> bool;
    fn focused(&self, label: &str) -> bool;
    fn click(&self, label: &str, secondary: bool);
    fn event(&self, event: egui::Event);
}

impl SurfaceDriver for Harness<'_, SurfaceFixture> {
    fn step(&mut self) {
        Harness::step(self);
    }
    fn fixture(&self) -> &SurfaceFixture {
        self.state()
    }
    fn has_label(&self, label: &str) -> bool {
        self.query_all_by_label(label).next().is_some()
    }
    fn has_label_containing(&self, label: &str) -> bool {
        self.query_by_label_contains(label).is_some()
    }
    fn focused(&self, label: &str) -> bool {
        self.query_all_by_label(label).any(|node| node.is_focused())
    }
    fn click(&self, label: &str, secondary: bool) {
        if secondary {
            self.get_by_label(label).click_secondary();
        } else {
            self.get_by_label(label).click();
        }
    }
    fn event(&self, event: egui::Event) {
        Harness::event(self, event);
    }
}

fn surface_cell(driver: &impl SurfaceDriver, column: usize) -> egui::Pos2 {
    let SurfaceBody::Terminal(state) = &driver.fixture().body else {
        panic!("terminal fixture");
    };
    let diagnostics = state.view.diagnostics();
    let grid = diagnostics.grid_rect.expect("real terminal grid is ready");
    let size = diagnostics.calculated_dimensions.unwrap();
    grid.left_top()
        + egui::vec2(
            (column as f32 + 0.5) * grid.width() / size.columns() as f32,
            0.5 * grid.height() / size.rows() as f32,
        )
}

fn prepare_surface(kind: SurfaceKind, driver: &mut impl SurfaceDriver) {
    use SurfaceKind as K;
    // Fixed settling is bounded even for a widget that requests animation.
    for _ in 0..3 {
        driver.step();
    }
    match kind {
        K::ChromeMenu => {
            driver.click("More actions", false);
            driver.step();
        }
        K::ChipFirst | K::ChipMiddle | K::ChipLastReadOnly => {
            driver.click("Synthetic SSH chip", true);
            driver.step();
        }
        K::TerminalSelection
        | K::TerminalReadOnlySelection
        | K::TerminalLink
        | K::TerminalPath
        | K::TerminalDisabledPath
        | K::TerminalHistory
        | K::TerminalReadOnlyHistory => {
            if matches!(kind, K::TerminalSelection | K::TerminalReadOnlySelection) {
                let start = surface_cell(driver, 0);
                let end = surface_cell(driver, 8);
                for event in [
                    egui::Event::PointerMoved(start),
                    egui::Event::PointerButton {
                        pos: start,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: egui::Modifiers::NONE,
                    },
                    egui::Event::PointerMoved(end),
                    egui::Event::PointerButton {
                        pos: end,
                        button: egui::PointerButton::Primary,
                        pressed: false,
                        modifiers: egui::Modifiers::NONE,
                    },
                ] {
                    driver.event(event);
                    driver.step();
                }
            }
            let pos = surface_cell(
                driver,
                if matches!(kind, K::TerminalPath | K::TerminalDisabledPath) {
                    27
                } else {
                    3
                },
            );
            driver.event(egui::Event::PointerMoved(pos));
            driver.step();
            for pressed in [true, false] {
                driver.event(egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Secondary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                });
                driver.step();
            }
        }
        K::OpenSmall
        | K::OpenLarge
        | K::OpenError
        | K::SaveSmall
        | K::SaveLarge
        | K::SaveError
        | K::SaveOverwrite => {
            let started = std::time::Instant::now();
            loop {
                driver.step();
                let ready = if matches!(kind, K::OpenError | K::SaveError) {
                    driver.has_label("Could not load the folder.")
                } else {
                    driver.has_label("entry-000000.md")
                };
                if ready {
                    break;
                }
                assert!(
                    started.elapsed() < Duration::from_secs(10),
                    "real picker task did not reach the required state: {kind:?}"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        _ => {}
    }
    for _ in 0..2 {
        driver.step();
    }
    assert_surface(kind, driver);
}

fn assert_surface(kind: SurfaceKind, driver: &impl SurfaceDriver) {
    use SurfaceKind as K;
    let required = match kind {
        K::About | K::Licenses => "About fesTerm",
        K::UpdateIdle => "Check for Updates",
        K::UpdateReady => "Install and Restart",
        K::UpdateInstalled => "Copy Version Information",
        K::ChromeMenu => "About fesTerm",
        K::ChipFirst | K::ChipMiddle | K::ChipLastReadOnly => "Close session",
        K::TerminalSelection | K::TerminalReadOnlySelection => "Copy",
        K::TerminalLink => "Copy link",
        K::TerminalPath | K::TerminalDisabledPath => "Copy path",
        K::TerminalHistory | K::TerminalReadOnlyHistory => "Open Terminal History in Editor",
        K::LiveClose => "Close Session",
        K::Paste => "Paste",
        K::DirtyClose => "Save",
        K::OpenError | K::SaveError => "Could not load the folder.",
        _ => "entry-000000.md",
    };
    assert!(driver.has_label(required), "{kind:?} must show {required}");
    if matches!(kind, K::LiveClose | K::Paste) {
        assert!(driver.focused("Cancel"));
    }
    if kind == K::DirtyClose {
        assert!(driver.focused("Save"));
    }
    if matches!(
        kind,
        K::TerminalReadOnlySelection | K::TerminalReadOnlyHistory
    ) {
        assert!(!driver.has_label("Paste"));
    }
    if matches!(kind, K::TerminalSelection | K::TerminalReadOnlySelection) {
        assert!(!driver.has_label("Open Terminal History in Editor"));
        let SurfaceBody::Terminal(state) = &driver.fixture().body else {
            unreachable!()
        };
        assert!(state.view.selection().range().is_some());
    }
    if kind == K::ChipFirst {
        assert!(!driver.has_label("Move left"));
    }
    if kind == K::ChipLastReadOnly {
        assert!(!driver.has_label("Move right"));
    }
    if kind == K::ChipMiddle {
        assert!(driver.has_label("Move left") && driver.has_label("Move right"));
    }
    if kind == K::OpenLarge {
        assert!(driver.has_label("5000 items"));
    }
    if kind == K::OpenSmall {
        assert!(driver.has_label("12 items"));
    }
    if kind == K::SaveOverwrite {
        assert!(driver.has_label_containing("Saving will replace it."));
    }
    if matches!(kind, K::SaveSmall | K::SaveLarge) {
        assert!(!driver.has_label_containing("Saving will replace it."));
    }
    if kind == K::Licenses {
        assert!(driver.has_label("Hide Licenses"));
    }
    if kind == K::ChromeMenu {
        assert!(driver.has_label("Open File…"));
        assert!(driver.has_label("Open Profiles"));
        assert!(driver.has_label("Open Settings"));
    }
    driver.fixture().assert_no_transport_input();
}

/// Raw production root context, with semantic queries only for arranging and
/// checking a fixture. No Harness outer Frame is submitted to a timed replay.
pub(crate) struct SurfaceProbe {
    pub(crate) context: egui::Context,
    fixture: SurfaceFixture,
    input: egui::RawInput,
    tree: Option<egui_kittest::kittest::State>,
    events: egui_kittest::EventQueue,
    pub(crate) first_output: egui::FullOutput,
    pub(crate) preparation_ms: f64,
    pub(crate) first_ui_ms: f64,
    pub(crate) readiness_ms: f64,
    pub(crate) preparation_ui_ms: Vec<f64>,
    pub(crate) last_ui_ms: f64,
}

impl SurfaceProbe {
    pub(crate) fn new(scene: SurfaceScene, directory: &Path, pixels_per_point: f32) -> Self {
        Self::with_setup(scene, directory, pixels_per_point, |_| {})
    }

    pub(crate) fn with_setup(
        scene: SurfaceScene,
        directory: &Path,
        pixels_per_point: f32,
        setup: impl FnOnce(&egui::Context),
    ) -> Self {
        let started = std::time::Instant::now();
        let context = egui::Context::default();
        context.set_theme(egui::ThemePreference::Dark);
        context.set_visuals(festerm_ui_egui::theme::default_visuals());
        context.enable_accesskit();
        context.all_styles_mut(|style| {
            style.visuals.text_cursor.blink = false;
            style.animation_time = 0.0;
        });
        setup(&context);
        let mut fixture = SurfaceFixture::new(scene.kind, directory, &context);
        fixture.install_context_assets(scene.kind, &context);
        let preparation_ms = started.elapsed().as_secs_f64() * 1000.0;
        let mut input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, scene.size())),
            ..Default::default()
        };
        input
            .viewports
            .get_mut(&egui::ViewportId::ROOT)
            .unwrap()
            .native_pixels_per_point = Some(pixels_per_point);
        let mut probe = Self {
            context,
            fixture,
            input,
            tree: None,
            events: Default::default(),
            first_output: Default::default(),
            preparation_ms,
            first_ui_ms: 0.0,
            readiness_ms: 0.0,
            preparation_ui_ms: Vec::new(),
            last_ui_ms: 0.0,
        };
        probe.first_output = probe.frame();
        probe.first_ui_ms = probe.last_ui_ms;
        probe
    }

    fn node(&self) -> egui_kittest::Node<'_> {
        egui_kittest::Node::new(
            self.tree.as_ref().expect("initial raw UI frame").root(),
            &self.events,
            self.context.pixels_per_point(),
        )
    }

    pub(crate) fn frame(&mut self) -> egui::FullOutput {
        let started = std::time::Instant::now();
        let mut output = self
            .context
            .run_ui(self.input.take(), |ui| self.fixture.show(ui));
        self.last_ui_ms = started.elapsed().as_secs_f64() * 1000.0;
        if let Some(update) = output.platform_output.accesskit_update.take() {
            if let Some(tree) = &mut self.tree {
                tree.update(update);
            } else {
                self.tree = Some(egui_kittest::kittest::State::new(update));
            }
        }
        output
    }

    pub(crate) fn assert_state(&self, kind: SurfaceKind) {
        assert_surface(kind, self);
    }

    pub(crate) fn prepare(
        &mut self,
        kind: SurfaceKind,
        mut textures: impl FnMut(&mut egui::TexturesDelta),
    ) {
        textures(&mut self.first_output.textures_delta);
        let started = std::time::Instant::now();
        // Preparation frames' texture deltas must not be discarded on GPU
        // paths: later output need not resend the fonts/atlas they installed.
        self.prepare_with(kind, &mut textures);
        self.readiness_ms = started.elapsed().as_secs_f64() * 1000.0;
    }

    fn prepare_with(
        &mut self,
        kind: SurfaceKind,
        textures: &mut impl FnMut(&mut egui::TexturesDelta),
    ) {
        struct Driver<'a, F> {
            probe: &'a mut SurfaceProbe,
            textures: &'a mut F,
        }
        impl<F: FnMut(&mut egui::TexturesDelta)> SurfaceDriver for Driver<'_, F> {
            fn step(&mut self) {
                let events = std::mem::take(&mut *self.probe.events.lock());
                if events.is_empty() {
                    let mut output = self.probe.frame();
                    self.probe.preparation_ui_ms.push(self.probe.last_ui_ms);
                    (self.textures)(&mut output.textures_delta);
                } else {
                    for event in events {
                        self.probe.input.events.push(event);
                        let mut output = self.probe.frame();
                        self.probe.preparation_ui_ms.push(self.probe.last_ui_ms);
                        (self.textures)(&mut output.textures_delta);
                    }
                }
            }
            fn fixture(&self) -> &SurfaceFixture {
                &self.probe.fixture
            }
            fn has_label(&self, label: &str) -> bool {
                self.probe.node().query_all_by_label(label).next().is_some()
            }
            fn has_label_containing(&self, label: &str) -> bool {
                self.probe.node().query_by_label_contains(label).is_some()
            }
            fn focused(&self, label: &str) -> bool {
                self.probe
                    .node()
                    .query_all_by_label(label)
                    .any(|node| node.is_focused())
            }
            fn click(&self, label: &str, secondary: bool) {
                if secondary {
                    self.probe.node().get_by_label(label).click_secondary();
                } else {
                    self.probe.node().get_by_label(label).click();
                }
            }
            fn event(&self, event: egui::Event) {
                self.probe.events.lock().push(event);
            }
        }
        prepare_surface(
            kind,
            &mut Driver {
                probe: self,
                textures,
            },
        );
    }
}

fn unique_surface_fixture_directory() -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let stamp = SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let directory = surface_fixture_root().join(format!(
        "{stamp}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed),
    ));
    fs::create_dir_all(&directory).unwrap();
    directory
}

fn surface_fixture_directory(id: &str) -> PathBuf {
    assert!(
        !id.is_empty()
            && id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')),
        "fixture id must be one simple owned path component"
    );
    let directory = surface_fixture_root().join(id);
    assert!(
        !directory.exists(),
        "retain/inspect the previous failed fixture at {}; choose a fresh FESTERM_UI_SURFACE_FIXTURE_RUN tag for a retry",
        directory.display()
    );
    fs::create_dir_all(&directory).unwrap();
    directory
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct OwnedStyleEntry {
    path: PathBuf,
    directory: bool,
    sha256: Option<String>,
    modified_unix_nanos: Option<u128>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct OwnedStyleRecord {
    signature: serde_json::Value,
    inventory: Vec<OwnedStyleEntry>,
}

struct OwnedStyleScope {
    root: PathBuf,
    output: PathBuf,
    run: String,
    phase: &'static str,
    planned: std::collections::BTreeSet<String>,
}

fn owned_style_no_aliases(path: &Path) -> Result<(), String> {
    let mut ancestor = PathBuf::new();
    for component in path.components() {
        ancestor.push(component.as_os_str());
        if matches!(component, std::path::Component::Prefix(_)) {
            continue;
        }
        let metadata = match fs::symlink_metadata(&ancestor) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => return Err(error.to_string()),
        };
        let alias = metadata.file_type().is_symlink();
        #[cfg(windows)]
        let alias = {
            use std::os::windows::fs::MetadataExt;
            const REPARSE_POINT: u32 = 0x400;
            alias || metadata.file_attributes() & REPARSE_POINT != 0
        };
        if alias {
            return Err(format!(
                "fixture aliases/reparse points are forbidden: {}",
                ancestor.display()
            ));
        }
    }
    Ok(())
}

fn owned_style_path_policy(root: &Path, run: &str) -> Result<(), String> {
    use std::path::Component;
    if run.is_empty()
        || run.len() > 64
        || !run
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        || !root.is_absolute()
        || root.file_name().and_then(|name| name.to_str()) != Some(run)
        || root
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        return Err("owned fixture root must be absolute, traversal-free and end in its explicit simple run identity".into());
    }
    #[cfg(windows)]
    if !matches!(root.components().next(), Some(Component::Prefix(prefix)) if matches!(prefix.kind(), std::path::Prefix::Disk(_)))
    {
        return Err(
            "owned fixture root must use a local ordinary drive path, not UNC/device syntax".into(),
        );
    }
    let control = root.parent().ok_or("missing fixture control directory")?;
    let target = control.parent().ok_or("missing fixture target directory")?;
    if control.file_name().and_then(|name| name.to_str()) != Some("ui-gallery-owned-comparisons")
        || target.file_name().and_then(|name| name.to_str()) != Some("target")
    {
        return Err("owned fixture root must be <controlled-workspace>/target/ui-gallery-owned-comparisons/<run>".into());
    }
    let text = root
        .to_str()
        .ok_or("fixture root must have a Unicode display identity")?;
    let normalized = text.replace('\\', "/").to_lowercase();
    for component in normalized.split('/') {
        if matches!(
            component,
            "." | ".."
                | "users"
                | "home"
                | "desktop"
                | "documents"
                | "downloads"
                | "appdata"
                | "onedrive"
                | "tmp"
                | "temp"
                | "windows"
                | "programdata"
                | "proc"
                | "sys"
                | "dev"
                | "etc"
        ) || component.starts_with("program files")
        {
            return Err("personal, system and temporary fixture roots are forbidden".into());
        }
    }
    for variable in [
        "HOME",
        "USERPROFILE",
        "LOCALAPPDATA",
        "APPDATA",
        "TEMP",
        "TMP",
    ] {
        if let Some(value) = std::env::var_os(variable) {
            let excluded = value
                .to_string_lossy()
                .replace('\\', "/")
                .trim_end_matches('/')
                .to_lowercase();
            if !excluded.is_empty()
                && (normalized == excluded || normalized.starts_with(&format!("{excluded}/")))
            {
                return Err(format!("fixture root is inside excluded {variable} scope"));
            }
        }
        for variable in ["USERNAME", "USER"] {
            if let Ok(username) = std::env::var(variable) {
                if !username.is_empty()
                    && normalized
                        .split('/')
                        .any(|component| component == username.to_lowercase())
                {
                    return Err("personal username components are forbidden in the controlled fixture scope".into());
                }
            }
        }
    }
    Ok(())
}

fn owned_style_bytes(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    use std::io::Read;
    owned_style_no_aliases(path)?;
    if !fs::symlink_metadata(path)
        .map_err(|error| error.to_string())?
        .is_file()
    {
        return Err(format!(
            "owned fixture metadata/input is not a regular file: {}",
            path.display()
        ));
    }
    let mut bytes = Vec::new();
    fs::File::open(path)
        .map_err(|error| error.to_string())?
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > limit {
        return Err(format!(
            "owned fixture file exceeds its bounded limit: {}",
            path.display()
        ));
    }
    Ok(bytes)
}

fn owned_style_read_json(path: &Path) -> Result<serde_json::Value, String> {
    serde_json::from_slice(&owned_style_bytes(path, 256 * 1024)?).map_err(|error| error.to_string())
}

fn owned_style_write_new(path: &Path, value: &serde_json::Value) -> Result<(), String> {
    use std::io::Write;
    owned_style_no_aliases(path)?;
    let bytes = serde_json::to_vec_pretty(value).map_err(|error| error.to_string())?;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| error.to_string())?;
    file.write_all(&bytes).map_err(|error| error.to_string())?;
    file.sync_all().map_err(|error| error.to_string())
}

fn owned_style_inventory(directory: &Path) -> Result<Vec<OwnedStyleEntry>, String> {
    fn visit(
        root: &Path,
        directory: &Path,
        entries: &mut Vec<OwnedStyleEntry>,
        bytes: &mut u64,
    ) -> Result<(), String> {
        owned_style_no_aliases(directory)?;
        for entry in fs::read_dir(directory).map_err(|error| error.to_string())? {
            let path = entry.map_err(|error| error.to_string())?.path();
            if path == root.join(".owner.json") {
                continue;
            }
            owned_style_no_aliases(&path)?;
            let metadata = fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
            if entries.len() >= 128 || (!metadata.is_file() && !metadata.is_dir()) {
                return Err("owned scene contains an unsupported entry or too many entries".into());
            }
            let relative = path
                .strip_prefix(root)
                .map_err(|error| error.to_string())?
                .to_owned();
            let (sha256, modified) = if metadata.is_file() {
                *bytes = bytes
                    .checked_add(metadata.len())
                    .ok_or("fixture size overflow")?;
                if *bytes > 1024 * 1024 {
                    return Err("owned style fixture exceeds one MiB".into());
                }
                let contents = owned_style_bytes(&path, 1024 * 1024)?;
                let modified = metadata
                    .modified()
                    .map_err(|error| error.to_string())?
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_err(|error| error.to_string())?
                    .as_nanos();
                (Some(sha256_hex(&contents)), Some(modified))
            } else {
                (None, None)
            };
            entries.push(OwnedStyleEntry {
                path: relative,
                directory: metadata.is_dir(),
                sha256,
                modified_unix_nanos: modified,
            });
            if metadata.is_dir() {
                visit(root, &path, entries, bytes)?;
            }
        }
        Ok(())
    }
    let mut entries = Vec::new();
    visit(directory, directory, &mut entries, &mut 0)?;
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(entries)
}

impl OwnedStyleScope {
    fn owner(&self) -> serde_json::Value {
        serde_json::json!({
            "schema": "festerm-owned-style-scope-v1",
            "physical_root": self.root, "run": self.run, "planned": self.planned,
        })
    }

    fn begin(
        root: PathBuf,
        run: String,
        phase: &str,
        output: PathBuf,
        planned: std::collections::BTreeSet<String>,
    ) -> Result<Self, String> {
        let phase = match phase {
            "baseline" => "baseline",
            "candidate" => "candidate",
            _ => return Err("owned fixture phase must be baseline or candidate".into()),
        };
        owned_style_path_policy(&root, &run)?;
        owned_style_no_aliases(&root)?;
        if planned.is_empty()
            || planned.iter().any(|id| {
                !style_review_scenarios()
                    .iter()
                    .any(|scene| scene.id == id.as_str())
            })
        {
            return Err(
                "owned physical identity supports only an explicit curated style scene selection"
                    .into(),
            );
        }
        owned_style_no_aliases(&output)?;
        let output = fs::canonicalize(output).map_err(|error| error.to_string())?;
        let control = root.parent().ok_or("missing control directory")?;
        let target = control.parent().ok_or("missing target directory")?;
        let workspace = target.parent().ok_or("missing controlled workspace")?;
        for marker in [workspace.join(".git"), workspace.join("Cargo.toml")] {
            owned_style_no_aliases(&marker)?;
            if !marker.exists() {
                return Err("owned fixture control must be inside an existing controlled Cargo/Git workspace".into());
            }
        }
        let canonical_workspace = fs::canonicalize(workspace).map_err(|error| error.to_string())?;
        let control_owner = serde_json::json!({
            "schema": "festerm-owned-style-control-v1", "workspace": canonical_workspace,
        });
        if !target.exists() {
            fs::create_dir(target).map_err(|error| error.to_string())?;
        }
        if control.exists() {
            if owned_style_read_json(&control.join(".owner.json"))? != control_owner {
                return Err(
                    "existing fixture control directory is unowned or belongs to another workspace"
                        .into(),
                );
            }
        } else {
            if phase != "baseline" {
                return Err("candidate has no completed owned baseline".into());
            }
            fs::create_dir(control).map_err(|error| error.to_string())?;
            owned_style_write_new(&control.join(".owner.json"), &control_owner)?;
        }
        let prospective_root = fs::canonicalize(control)
            .map_err(|error| error.to_string())?
            .join(&run);
        if output.starts_with(&prospective_root) || prospective_root.starts_with(&output) {
            return Err("evidence output and physical fixture scope must be disjoint".into());
        }
        let scope = Self {
            root,
            output,
            run,
            phase,
            planned,
        };
        if fs::read_dir(&scope.output)
            .map_err(|error| error.to_string())?
            .next()
            .is_some()
        {
            return Err("owned capture evidence output must be fresh and empty".into());
        }
        if phase == "baseline" {
            // Never adopt even an empty pre-existing run directory.
            if scope.root.exists() {
                return Err(
                    "baseline run root already exists; retain it and choose a new identity".into(),
                );
            }
            owned_style_write_new(
                &scope
                    .root
                    .parent()
                    .unwrap()
                    .join(format!(".claimed-run-{}.json", scope.run)),
                &scope.owner(),
            )?;
            fs::create_dir(&scope.root)
                .map_err(|error| format!("baseline needs a fresh unclaimed run root: {error}"))?;
            owned_style_write_new(&scope.root.join("scope.json"), &scope.owner())?;
            fs::create_dir(scope.root.join("scenes")).map_err(|error| error.to_string())?;
        } else {
            scope.validate()?;
            let completed = owned_style_read_json(&scope.root.join("baseline.complete.json"))?;
            if completed["owner"] != scope.owner() {
                return Err("candidate baseline ownership/selection differs".into());
            }
            let baseline_started =
                owned_style_read_json(&scope.root.join("baseline.started.json"))?;
            if baseline_started["owner"] != scope.owner() {
                return Err("baseline start ownership differs".into());
            }
            let baseline_output = PathBuf::from(
                baseline_started["output"]
                    .as_str()
                    .ok_or("missing baseline evidence output")?,
            );
            if scope.output.starts_with(&baseline_output)
                || baseline_output.starts_with(&scope.output)
            {
                return Err("baseline and candidate evidence directories must be disjoint".into());
            }
            if completed["manifest_sha256"]
                != serde_json::json!(sha256_hex(&owned_style_bytes(
                    &baseline_output.join("manifest.json"),
                    256 * 1024
                )?))
            {
                return Err("retained baseline manifest changed or is missing".into());
            }
            let baseline_manifest = owned_style_read_json(&baseline_output.join("manifest.json"))?;
            let baseline_scenes = baseline_manifest["scenarios"]
                .as_array()
                .ok_or("missing baseline scenes")?;
            let mut retained = std::collections::BTreeSet::new();
            for scene in baseline_scenes {
                let id = scene["id"].as_str().ok_or("invalid baseline scene")?;
                if !scope.planned.contains(id) || !retained.insert(id.to_owned()) {
                    return Err("retained baseline selection changed".into());
                }
                let name = format!("{id}.png");
                let digest = sha256_hex(&owned_style_bytes(
                    &baseline_output.join(&name),
                    4 * 1024 * 1024,
                )?);
                if scene["image"].as_str() != Some(name.as_str())
                    || scene["sha256"].as_str() != Some(digest.as_str())
                {
                    return Err("retained baseline image changed or is missing".into());
                }
            }
            if retained != scope.planned {
                return Err("retained baseline images are incomplete".into());
            }
            for id in &scope.planned {
                let digest = sha256_hex(&owned_style_bytes(
                    &scope.root.join("baseline").join(format!("{id}.json")),
                    256 * 1024,
                )?);
                if completed["record_sha256"][id.as_str()] != serde_json::json!(digest) {
                    return Err("retained baseline input proof changed or is missing".into());
                }
            }
            let retained: std::collections::BTreeSet<_> = fs::read_dir(scope.root.join("scenes"))
                .map_err(|error| error.to_string())?
                .map(|entry| {
                    entry.map_err(|error| error.to_string()).and_then(|entry| {
                        entry
                            .file_name()
                            .into_string()
                            .map_err(|_| "invalid retained scene".to_owned())
                    })
                })
                .collect::<Result<_, _>>()?;
            if retained != scope.planned || completed["inputs_retained"] != true {
                return Err("baseline retained input scope differs or is incomplete".into());
            }
            for id in &scope.planned {
                let record: OwnedStyleRecord = serde_json::from_value(owned_style_read_json(
                    &scope.root.join("baseline").join(format!("{id}.json")),
                )?)
                .map_err(|error| error.to_string())?;
                if owned_style_inventory(&scope.scene_directory(id))? != record.inventory {
                    return Err(
                        "retained baseline input contents/mtime changed before candidate".into(),
                    );
                }
            }
        }
        let mut provenance = surface_probe_provenance();
        provenance["synthetic_fixture_root"] = serde_json::json!(scope.root.join("scenes"));
        provenance["fixture_identity_mode"] = serde_json::json!("explicit-owned-physical");
        owned_style_write_new(
            &scope.root.join(format!("{phase}.started.json")),
            &serde_json::json!({
                "owner": scope.owner(), "output": scope.output,
                "pid": std::process::id(), "phase": phase,
                "provenance": provenance,
            }),
        )?;
        fs::create_dir(scope.root.join(phase)).map_err(|error| error.to_string())?;
        Ok(scope)
    }

    fn validate(&self) -> Result<(), String> {
        owned_style_path_policy(&self.root, &self.run)?;
        owned_style_no_aliases(&self.root)?;
        if owned_style_read_json(&self.root.join("scope.json"))? != self.owner() {
            return Err("physical fixture scope ownership/identity changed".into());
        }
        let control = self.root.parent().ok_or("missing fixture control")?;
        if owned_style_read_json(&control.join(format!(".claimed-run-{}.json", self.run)))?
            != self.owner()
        {
            return Err(
                "physical fixture run identity is unclaimed or belongs to another scope".into(),
            );
        }
        let workspace = control
            .parent()
            .and_then(Path::parent)
            .ok_or("missing controlled workspace")?;
        if owned_style_read_json(&control.join(".owner.json"))?
            != serde_json::json!({
                "schema": "festerm-owned-style-control-v1",
                "workspace": fs::canonicalize(workspace).map_err(|error| error.to_string())?,
            })
        {
            return Err("fixture control ownership changed".into());
        }
        for entry in fs::read_dir(&self.root).map_err(|error| error.to_string())? {
            let path = entry.map_err(|error| error.to_string())?.path();
            owned_style_no_aliases(&path)?;
            if !matches!(
                path.file_name().and_then(|name| name.to_str()),
                Some(
                    "scope.json"
                        | "scenes"
                        | "baseline"
                        | "candidate"
                        | "baseline.started.json"
                        | "candidate.started.json"
                        | "baseline.complete.json"
                        | "candidate.complete.json"
                )
            ) {
                return Err("fixture scope contains unowned control entries".into());
            }
        }
        Ok(())
    }

    fn scene_owner(&self, id: &str) -> serde_json::Value {
        serde_json::json!({"owner": self.owner(), "scene": id})
    }

    fn scene_directory(&self, id: &str) -> PathBuf {
        self.root.join("scenes").join(id)
    }

    fn start_scene(&self, id: &str) -> Result<PathBuf, String> {
        self.validate()?;
        if self
            .root
            .join(format!("{}.complete.json", self.phase))
            .exists()
        {
            return Err("completed physical fixture phase cannot be reused".into());
        }
        if !self.planned.contains(id) {
            return Err("scene is outside the explicitly owned selection".into());
        }
        let directory = self.scene_directory(id);
        owned_style_no_aliases(&directory)?;
        owned_style_write_new(
            &self
                .root
                .join(self.phase)
                .join(format!(".{id}.started.json")),
            &self.scene_owner(id),
        )?;
        if self.phase == "candidate" {
            if owned_style_read_json(&directory.join(".owner.json"))? != self.scene_owner(id) {
                return Err("retained candidate scene is unowned".into());
            }
        } else {
            fs::create_dir(&directory)
                .map_err(|error| format!("owned scene must be fresh: {error}"))?;
            owned_style_write_new(&directory.join(".owner.json"), &self.scene_owner(id))?;
        }
        Ok(directory)
    }

    fn record_scene(&self, id: &str, signature: serde_json::Value) -> Result<(), String> {
        self.validate()?;
        let directory = self.scene_directory(id);
        if owned_style_read_json(&directory.join(".owner.json"))? != self.scene_owner(id) {
            return Err("scene directory is not owned by this capture scope".into());
        }
        let record = OwnedStyleRecord {
            signature,
            inventory: owned_style_inventory(&directory)?,
        };
        let value = serde_json::to_value(record).map_err(|error| error.to_string())?;
        if self.phase == "candidate"
            && owned_style_read_json(&self.root.join("baseline").join(format!("{id}.json")))?
                != value
        {
            return Err("candidate physical scene signature, file contents or freshness identity differs from baseline".into());
        }
        owned_style_write_new(
            &self.root.join(self.phase).join(format!("{id}.json")),
            &value,
        )
    }

    fn complete(&self) -> Result<(), String> {
        self.validate()?;
        let manifest_path = self.output.join("manifest.json");
        let manifest = owned_style_read_json(&manifest_path)?;
        let scenarios = manifest["scenarios"]
            .as_array()
            .ok_or("missing completed capture manifest")?;
        let actual: std::collections::BTreeSet<_> = scenarios
            .iter()
            .map(|scene| {
                scene["id"]
                    .as_str()
                    .ok_or("invalid manifest scene ID")
                    .map(str::to_owned)
            })
            .collect::<Result<_, _>>()?;
        if actual != self.planned || scenarios.len() != self.planned.len() {
            return Err("completed evidence does not match the owned scene selection".into());
        }
        let scene_directories: std::collections::BTreeSet<_> =
            fs::read_dir(self.root.join("scenes"))
                .map_err(|error| error.to_string())?
                .map(|entry| {
                    entry.map_err(|error| error.to_string()).and_then(|entry| {
                        entry
                            .file_name()
                            .into_string()
                            .map_err(|_| "non-Unicode scene entry".to_owned())
                    })
                })
                .collect::<Result<_, _>>()?;
        if scene_directories != self.planned {
            return Err("fixture scope contains incomplete or unowned scene entries".into());
        }
        let proof_files: std::collections::BTreeSet<_> = fs::read_dir(self.root.join(self.phase))
            .map_err(|error| error.to_string())?
            .map(|entry| {
                entry.map_err(|error| error.to_string()).and_then(|entry| {
                    entry
                        .file_name()
                        .into_string()
                        .map_err(|_| "non-Unicode proof entry".to_owned())
                })
            })
            .collect::<Result<_, _>>()?;
        let expected_proofs = self
            .planned
            .iter()
            .flat_map(|id| [format!("{id}.json"), format!(".{id}.started.json")])
            .collect();
        if proof_files != expected_proofs {
            return Err("fixture phase contains unowned or missing input proofs".into());
        }
        let mut records = Vec::new();
        let mut record_digests = std::collections::BTreeMap::new();
        for scene in scenarios {
            let id = scene["id"].as_str().ok_or("invalid scene")?;
            let image_name = format!("{id}.png");
            if scene["image"].as_str() != Some(image_name.as_str())
                || scene["sha256"].as_str()
                    != Some(
                        sha256_hex(&owned_style_bytes(
                            &self.output.join(&image_name),
                            4 * 1024 * 1024,
                        )?)
                        .as_str(),
                    )
            {
                return Err(
                    "completed scene image is absent or differs from its manifest digest".into(),
                );
            }
            let directory = self.scene_directory(id);
            if owned_style_read_json(&directory.join(".owner.json"))? != self.scene_owner(id) {
                return Err("scene ownership changed before cleanup".into());
            }
            let record: OwnedStyleRecord = serde_json::from_value(owned_style_read_json(
                &self.root.join(self.phase).join(format!("{id}.json")),
            )?)
            .map_err(|error| error.to_string())?;
            if owned_style_inventory(&directory)? != record.inventory {
                return Err(
                    "scene has modified or untracked inputs; retaining it instead of cleaning"
                        .into(),
                );
            }
            record_digests.insert(
                id.to_owned(),
                sha256_hex(&owned_style_bytes(
                    &self.root.join(self.phase).join(format!("{id}.json")),
                    256 * 1024,
                )?),
            );
            records.push((directory, record));
        }
        // Cleanup happens only after every saved image/manifest and input was
        // checked. Remove tracked files/empty directories, never a whole tree.
        for (directory, record) in records.into_iter().filter(|_| self.phase == "candidate") {
            owned_style_no_aliases(&directory)?;
            for entry in record.inventory.iter().filter(|entry| !entry.directory) {
                let path = directory.join(&entry.path);
                owned_style_no_aliases(&path)?;
                fs::remove_file(path).map_err(|error| error.to_string())?;
            }
            let mut directories: Vec<_> = record
                .inventory
                .iter()
                .filter(|entry| entry.directory)
                .collect();
            directories.sort_by_key(|entry| std::cmp::Reverse(entry.path.components().count()));
            for entry in directories {
                fs::remove_dir(directory.join(&entry.path)).map_err(|error| error.to_string())?;
            }
            fs::remove_file(directory.join(".owner.json")).map_err(|error| error.to_string())?;
            fs::remove_dir(directory).map_err(|error| error.to_string())?;
        }
        owned_style_write_new(
            &self.root.join(format!("{}.complete.json", self.phase)),
            &serde_json::json!({
                "owner": self.owner(), "manifest_sha256": sha256_hex(&owned_style_bytes(&manifest_path, 256 * 1024)?),
                "output": self.output, "phase": self.phase, "record_sha256": record_digests,
                "inputs_retained": self.phase == "baseline",
            }),
        )
    }
}

std::thread_local! {
    static OWNED_STYLE_SCOPE: std::cell::RefCell<Option<OwnedStyleScope>> = const { std::cell::RefCell::new(None) };
}

fn begin_owned_style_scope(entries: &[Scenario], output: &Path) -> Result<(), String> {
    let root = std::env::var_os("FESTERM_UI_SURFACE_FIXTURE_ROOT");
    let phase = std::env::var("FESTERM_UI_SURFACE_FIXTURE_PHASE");
    let Some(root) = root else {
        if !matches!(phase, Err(std::env::VarError::NotPresent)) {
            return Err("physical fixture phase requires the explicit owned root override".into());
        }
        return Ok(());
    };
    let run = std::env::var("FESTERM_UI_SURFACE_FIXTURE_RUN").map_err(|error| error.to_string())?;
    let phase = phase.map_err(|error| error.to_string())?;
    if OWNED_STYLE_SCOPE.with(|slot| slot.borrow().is_some()) {
        return Err("a physical fixture phase cannot be restarted in this process".into());
    }
    let planned = entries.iter().map(|scene| scene.id.to_owned()).collect();
    let scope =
        OwnedStyleScope::begin(PathBuf::from(root), run, &phase, output.to_owned(), planned)?;
    OWNED_STYLE_SCOPE.with(|slot| {
        if slot.borrow().is_some() {
            return Err("a physical fixture phase cannot be restarted in this process".into());
        }
        *slot.borrow_mut() = Some(scope);
        Ok(())
    })
}

#[test]
fn surface_owned_physical_policy_rejects_unsafe_roots_and_mismatched_states() {
    let root = if cfg!(windows) {
        PathBuf::from(r"Q:\controlled\festerm\target\ui-gallery-owned-comparisons\pair-01")
    } else {
        PathBuf::from("/srv/controlled/festerm/target/ui-gallery-owned-comparisons/pair-01")
    };
    assert!(owned_style_path_policy(&root, "pair-01").is_ok());
    for (path, run) in [
        (PathBuf::from("relative-root"), "pair-01"),
        (root.clone(), ".."),
        (root.clone(), "different-run"),
        (
            root.parent().unwrap().parent().unwrap().to_owned(),
            "pair-01",
        ),
        (root.join("..").join("pair-01"), "pair-01"),
    ] {
        assert!(owned_style_path_policy(&path, run).is_err());
    }
    let personal = if cfg!(windows) {
        PathBuf::from(r"C:\Users\fixture\project\target\ui-gallery-owned-comparisons\pair-01")
    } else {
        PathBuf::from("/home/fixture/project/target/ui-gallery-owned-comparisons/pair-01")
    };
    assert!(owned_style_path_policy(&personal, "pair-01").is_err());
    let temporary = if cfg!(windows) {
        PathBuf::from(r"C:\Temp\project\target\ui-gallery-owned-comparisons\pair-01")
    } else {
        PathBuf::from("/tmp/project/target/ui-gallery-owned-comparisons/pair-01")
    };
    assert!(owned_style_path_policy(&temporary, "pair-01").is_err());
    assert!(StyleReviewKind::MaxPaste
        .owned_signature(egui::vec2(360.0, 240.0), "style-paste-max-preview-short",)
        .is_ok());
    assert!(StyleReviewKind::MaxPaste
        .owned_signature(egui::vec2(360.0, 240.0), "style-paste-max-preview-narrow",)
        .is_err());
    assert!(StyleReviewKind::MaxPaste
        .owned_signature(egui::vec2(360.0, 241.0), "style-paste-max-preview-short",)
        .is_err());
    assert!(StyleReviewKind::LongPalette
        .owned_signature(
            egui::vec2(752.0, 516.0),
            "style-about-ready-licenses-baseline",
        )
        .is_err());
}

#[cfg(windows)]
#[test]
fn surface_owned_physical_scope_preserves_paths_and_refuses_reuse_or_untracked_cleanup() {
    fn evidence(scope: &OwnedStyleScope, id: &str) {
        let path = scope.output.join(format!("{id}.png"));
        // Protocol-unit evidence only; this test does not render product UI.
        image::RgbaImage::new(1, 1).save(&path).unwrap();
        let digest = sha256_hex(&fs::read(&path).unwrap());
        fs::write(
            scope.output.join("manifest.json"),
            serde_json::to_vec(&serde_json::json!({
                "schema_version": 1,
                "scenarios": [{"id": id, "image": format!("{id}.png"), "sha256": digest}],
            }))
            .unwrap(),
        )
        .unwrap();
    }
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let workspace = PathBuf::from(
        workspace
            .to_string_lossy()
            .strip_prefix(r"\\?\")
            .unwrap_or(&workspace.to_string_lossy()),
    );
    let run = format!(
        "ownership-test-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let root = workspace
        .join("target")
        .join("ui-gallery-owned-comparisons")
        .join(&run);
    let output = workspace.join("target").join("evidence").join(&run);
    let before = output.join("before");
    let after = output.join("after");
    fs::create_dir_all(&before).unwrap();
    fs::create_dir(&after).unwrap();
    let id = "style-about-ready-licenses-baseline";
    let planned = std::collections::BTreeSet::from([id.to_owned()]);
    assert!(OwnedStyleScope::begin(
        root.clone(),
        run.clone(),
        "candidate",
        after.clone(),
        planned.clone()
    )
    .is_err());
    let baseline = OwnedStyleScope::begin(
        root.clone(),
        run.clone(),
        "baseline",
        before,
        planned.clone(),
    )
    .unwrap();
    assert!(OwnedStyleScope::begin(
        root.clone(),
        run.clone(),
        "baseline",
        after.clone(),
        planned.clone()
    )
    .is_err());
    assert!(OwnedStyleScope::begin(
        root.clone(),
        run.clone(),
        "candidate",
        after.clone(),
        planned.clone()
    )
    .is_err());
    let before_directory = baseline.start_scene(id).unwrap();
    let junction_target = output.join("junction-test-target");
    fs::create_dir(&junction_target).unwrap();
    let junction = before_directory.join("unsafe-junction");
    let command = format!(
        "New-Item -ItemType Junction -Path '{}' -Target '{}' -ErrorAction Stop | Out-Null",
        junction.display().to_string().replace('\'', "''"),
        junction_target.display().to_string().replace('\'', "''"),
    );
    let created = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &command])
        .output()
        .unwrap();
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );
    assert!(owned_style_no_aliases(&junction).is_err());
    assert!(owned_style_inventory(&before_directory).is_err());
    fs::remove_dir(&junction).unwrap();
    fs::remove_dir(junction_target).unwrap();
    let source = before_directory.join("SOURCE.md");
    fs::write(&source, "# Synthetic protocol-unit input\n").unwrap();
    set_synthetic_modified_time(&source);
    let signature = StyleReviewKind::AboutReadyLicenses
        .owned_signature(egui::vec2(752.0, 516.0), id)
        .unwrap();
    baseline.record_scene(id, signature.clone()).unwrap();
    evidence(&baseline, id);
    let untracked = before_directory.join("untracked-owned-test-injection.txt");
    fs::write(&untracked, "must not be silently cleaned").unwrap();
    assert!(baseline.complete().is_err());
    assert!(source.exists() && untracked.exists());
    fs::remove_file(untracked).unwrap();
    let changed_mtime = fs::FileTimes::new().set_modified(SystemTime::now());
    fs::File::options()
        .write(true)
        .open(&source)
        .unwrap()
        .set_times(changed_mtime)
        .unwrap();
    assert!(
        baseline.complete().is_err(),
        "changed real mtime must not qualify"
    );
    set_synthetic_modified_time(&source);
    baseline.complete().unwrap();
    assert!(before_directory.exists());
    let candidate = OwnedStyleScope::begin(
        root.clone(),
        run.clone(),
        "candidate",
        after,
        planned.clone(),
    )
    .unwrap();
    let after_directory = candidate.start_scene(id).unwrap();
    assert_eq!(before_directory, after_directory);
    let source = after_directory.join("SOURCE.md");
    fs::write(&source, "# Different input must not qualify\n").unwrap();
    set_synthetic_modified_time(&source);
    assert!(candidate.record_scene(id, signature.clone()).is_err());
    assert!(source.exists());
    fs::write(&source, "# Synthetic protocol-unit input\n").unwrap();
    set_synthetic_modified_time(&source);
    candidate.record_scene(id, signature).unwrap();
    evidence(&candidate, id);
    candidate.complete().unwrap();
    assert!(!after_directory.exists());
    assert!(root
        .parent()
        .unwrap()
        .join(format!(".claimed-run-{run}.json"))
        .exists());
    assert!(candidate.start_scene(id).is_err());
    assert!(OwnedStyleScope::begin(
        root.clone(),
        run.clone(),
        "candidate",
        candidate.output.clone(),
        planned.clone()
    )
    .is_err());
    let unowned_run = format!("{run}-unowned");
    let unowned_root = root.parent().unwrap().join(&unowned_run);
    fs::create_dir(&unowned_root).unwrap();
    let fresh_output = output.join("unowned-refusal");
    fs::create_dir(&fresh_output).unwrap();
    assert!(OwnedStyleScope::begin(
        unowned_root.clone(),
        unowned_run,
        "baseline",
        fresh_output,
        planned
    )
    .is_err());
    assert!(unowned_root.exists());
    fs::remove_dir(unowned_root).unwrap();
}

pub(crate) fn surface_probe_provenance() -> serde_json::Value {
    use std::io::Read;
    let revision = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned());
    let dirty = std::process::Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| !output.stdout.is_empty());
    let executable = std::env::current_exe().unwrap();
    let mut file = fs::File::open(executable).unwrap();
    let mut hasher = Sha256::new();
    let mut bytes = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut bytes).unwrap();
        if count == 0 {
            break;
        }
        hasher.update(&bytes[..count]);
    }
    let digest = hasher.finalize();
    let digest: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    serde_json::json!({
        "source_revision": revision, "source_dirty": dirty,
        "test_binary_sha256": digest, "package_version": env!("CARGO_PKG_VERSION"),
        "release": !cfg!(debug_assertions), "os": std::env::consts::OS,
        "architecture": std::env::consts::ARCH,
        "process_id": std::process::id(),
        "recorded_unix_seconds": SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs(),
        "execution": "exclusive-slot-required; artifact existence does not itself attest host isolation",
        "synthetic_fixture_root": surface_fixture_root(),
        "prerequisites": ["#286 CI/font-worker repair", "#287 production-gallery theme/timestamps"],
    })
}

fn palette_gallery_harness(narrow: bool) -> Harness<'static, crate::app::FesTermApp> {
    let mut app = crate::app::FesTermApp::for_test_with_configuration(synthetic_configuration());
    app.open_palette_for_gallery(&egui::Context::default());
    let size = if narrow {
        egui::vec2(360.0, 516.0)
    } else {
        egui::vec2(1180.0, 760.0)
    };
    let mut harness = gallery_harness(
        size,
        |ui, app: &mut crate::app::FesTermApp| app.ui_content(ui),
        app,
    );
    harness.run_steps(3);
    harness
}

pub(crate) fn capture_palette_gallery(narrow: bool) -> image::RgbaImage {
    let mut harness = palette_gallery_harness(narrow);
    harness.get_by_label("Command Palette");
    harness.remove_cursor();
    harness.render().expect("render production command palette")
}

fn capture_palette_normal() -> image::RgbaImage {
    capture_palette_gallery(false)
}

fn capture_palette_narrow() -> image::RgbaImage {
    capture_palette_gallery(true)
}

#[test]
fn surface_palette_capture_uses_real_commands_and_initial_search_focus() {
    for narrow in [false, true] {
        let harness = palette_gallery_harness(narrow);
        harness.get_by_label("Command Palette");
        let window = harness
            .ctx
            .memory(|memory| memory.area_rect(egui::Id::new("festerm_command_palette")))
            .expect("real command-palette window");
        assert!(harness
            .query_all_by_role(egui::accesskit::Role::TextInput)
            .into_iter()
            .any(|input| input.is_focused() && window.contains_rect(input.rect())));
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StyleReviewKind {
    AboutReadyLicenses,
    LongPalette,
    DeepOpenReady,
    DeepOpenError,
    NotesOverwrite,
    LongLiveClose,
    MaxPaste,
    DirtyClose,
    ExpandedOverflow,
}

const STYLE_LONG_IDENTITY: &str =
    "Synthetic staging relay — deliberately verbose stable identity for bounded disclosure review";
const STYLE_LONG_SECONDARY: &str =
    "/srv/synthetic/release-review/documentation/architecture/decisions/long-secondary-path/NOTES.md";

impl StyleReviewKind {
    fn owned_signature(self, size: egui::Vec2, id: &str) -> Result<serde_json::Value, String> {
        use StyleReviewKind as K;
        let prefix = match self {
            K::AboutReadyLicenses => "style-about-ready-licenses",
            K::LongPalette => "style-palette-long-identity",
            K::DeepOpenReady => "style-open-file-deep-ready",
            K::DeepOpenError => "style-open-file-deep-error",
            K::NotesOverwrite => "style-save-as-notes-overwrite",
            K::LongLiveClose => "style-live-close-long-identity",
            K::MaxPaste => "style-paste-max-preview",
            K::DirtyClose => "style-dirty-document-close",
            K::ExpandedOverflow => "style-expanded-overflow",
        };
        let suffix = if size == egui::vec2(752.0, 516.0) {
            "baseline"
        } else if size == egui::vec2(360.0, 516.0) {
            "narrow"
        } else if size == egui::vec2(360.0, 240.0) {
            "short"
        } else {
            return Err("owned physical comparison requires a catalogued root size".into());
        };
        if id != format!("{prefix}-{suffix}") {
            return Err("owned scene ID must match its actual kind and root size".into());
        }
        Ok(serde_json::json!({
            "schema": "festerm-style-fixture-input-v1", "kind": format!("{self:?}"),
            "size": [size.x, size.y], "scene": id,
        }))
    }

    fn base_kind(self) -> SurfaceKind {
        use StyleReviewKind as K;
        match self {
            K::AboutReadyLicenses => SurfaceKind::UpdateReady,
            K::LongPalette | K::ExpandedOverflow => SurfaceKind::ChipFirst,
            K::DeepOpenReady => SurfaceKind::OpenSmall,
            K::DeepOpenError => SurfaceKind::OpenError,
            K::NotesOverwrite => SurfaceKind::SaveOverwrite,
            K::LongLiveClose => SurfaceKind::LiveClose,
            K::MaxPaste => SurfaceKind::Paste,
            K::DirtyClose => SurfaceKind::DirtyClose,
        }
    }
}

fn style_review_scenarios() -> Vec<Scenario> {
    let mut scenes = Vec::new();
    macro_rules! pair {
        ($id:literal, $kind:ident, $title:literal, $caption:literal) => {{
            fn baseline() -> image::RgbaImage {
                capture_style_review_image(
                    StyleReviewKind::$kind,
                    egui::vec2(752.0, 516.0),
                    concat!($id, "-baseline"),
                )
            }
            fn narrow() -> image::RgbaImage {
                capture_style_review_image(
                    StyleReviewKind::$kind,
                    egui::vec2(360.0, 516.0),
                    concat!($id, "-narrow"),
                )
            }
            scenes.extend([
                Scenario {
                    id: concat!($id, "-baseline"),
                    section: "surface-probes",
                    title: concat!($title, " — 752 × 516 root"),
                    caption: concat!($caption, " Gallery-only geometry evidence, not a timing or native acceptance result. Physical fixture paths, where present, belong to this checkout and require publication review."),
                    capture: baseline,
                },
                Scenario {
                    id: concat!($id, "-narrow"),
                    section: "surface-probes",
                    title: concat!($title, " — 360 × 516 root"),
                    caption: concat!($caption, " The same fixture at a narrow root; clipping or inaccessible actions are recorded, not treated as a pass. Owned physical checkout paths require publication review."),
                    capture: narrow,
                },
            ]);
        }};
    }
    macro_rules! short {
        ($id:literal, $kind:ident, $title:literal) => {{
            fn capture() -> image::RgbaImage {
                capture_style_review_image(
                    StyleReviewKind::$kind,
                    egui::vec2(360.0, 240.0),
                    concat!($id, "-short"),
                )
            }
            scenes.push(Scenario {
                id: concat!($id, "-short"),
                section: "surface-probes",
                title: concat!($title, " — 360 × 240 root"),
                caption: "Actual application content after its state was verified at 752 × 516 and the root resized to 360 × 240. This exposes disclosure scrolling and action reachability; the manifest records missing/off-root controls rather than certifying them. Owned physical checkout paths require publication review. No timing or native acceptance is claimed.",
                capture,
            });
        }};
    }
    pair!("style-about-ready-licenses", AboutReadyLicenses, "About with packaged update and licenses", "The existing synthetic ReadyToInstall controller is combined with expanded licenses in the real About dialog; no download or installation occurs.");
    pair!("style-palette-long-identity", LongPalette, "Filtered palette with long identity and secondary path", "An actual session rename, synthetic terminal-title protocol metadata and real palette filtering expose the full accessible identity, long secondary path and configured quick-tab shortcut.");
    pair!("style-open-file-deep-ready", DeepOpenReady, "Open File in a deep owned directory", "Twelve owned files are loaded by the actual directory task; the ready listing is asserted before resizing.");
    pair!("style-open-file-deep-error", DeepOpenError, "Open File missing deep directory", "A missing owned path produces the actual directory-task error before resizing; no loading fallback is substituted.");
    pair!("style-save-as-notes-overwrite", NotesOverwrite, "Save As existing NOTES.md", "The actual task loads an owned deep destination containing NOTES.md, producing the overwrite notice beside the disabled remote destination. Save is never pressed.");
    pair!("style-live-close-long-identity", LongLiveClose, "Live close with long target identity", "A real rename and generation-targeted close request preserve the long synthetic session identity and Cancel default.");
    pair!("style-paste-max-preview", MaxPaste, "Maximum bounded paste preview", "The actual paste-policy preview reaches eight lines and 800 characters, with an omission notice and Cancel default. No clipboard is read and no bytes are delivered.");
    pair!("style-dirty-document-close", DirtyClose, "Final dirty-document close", "An actual edit and final-view close request preserve the document origin, Save default, Discard changes and Cancel. The source remains unchanged.");
    pair!("style-expanded-overflow", ExpandedOverflow, "Expanded overflow over a live synthetic session", "Actual More actions exposes the applicable palette and inspector controls, including narrow-width collapse. Menu action bounds and row heights are recorded without inventing a menu budget.");
    short!(
        "style-about-ready-licenses",
        AboutReadyLicenses,
        "About with packaged update and licenses"
    );
    short!(
        "style-open-file-deep-ready",
        DeepOpenReady,
        "Open File in a deep owned directory"
    );
    short!(
        "style-save-as-notes-overwrite",
        NotesOverwrite,
        "Save As existing NOTES.md"
    );
    short!(
        "style-paste-max-preview",
        MaxPaste,
        "Maximum bounded paste preview"
    );
    short!(
        "style-dirty-document-close",
        DirtyClose,
        "Final dirty-document close"
    );
    scenes
}

fn prepare_style_file(path: &Path, contents: &[u8], reuse: bool) {
    if reuse {
        assert_eq!(
            fs::read(path).unwrap(),
            contents,
            "retained real fixture input"
        );
    } else {
        fs::write(path, contents).unwrap();
        set_synthetic_modified_time(path);
    }
}

fn style_review_fixture(
    kind: StyleReviewKind,
    directory: &Path,
    context: &egui::Context,
) -> SurfaceFixture {
    use StyleReviewKind as K;
    let reuse = OWNED_STYLE_SCOPE.with(|slot| {
        slot.borrow()
            .as_ref()
            .is_some_and(|scope| scope.phase == "candidate")
    });
    let mut fixture = SurfaceFixture::with_inputs(kind.base_kind(), directory, context, reuse);
    let SurfaceBody::App(app) = &mut fixture.body else {
        unreachable!()
    };
    match kind {
        K::AboutReadyLicenses => app.open_about_for_gallery(
            true,
            crate::updates::UpdateController::ready_to_install_for_test(),
            context,
        ),
        K::LongPalette | K::LongLiveClose => {
            let tab = app.active_tab_for_gallery();
            app.dispatch_for_gallery(
                AppCommand::RenameTab(tab, STYLE_LONG_IDENTITY.into()),
                context,
            );
            if kind == K::LongPalette {
                app.set_terminal_title_for_gallery(STYLE_LONG_SECONDARY);
                app.open_palette_for_gallery(context);
            } else {
                app.request_active_tab_close_for_gallery(context);
            }
        }
        K::DeepOpenReady | K::DeepOpenError | K::NotesOverwrite => {
            let deep = [
                "synthetic-project",
                "documentation",
                "architecture",
                "decisions",
                "release-review",
            ]
            .into_iter()
            .fold(directory.to_owned(), |path, component| path.join(component));
            // Owned I/O remains real; no injected snapshot discards request IDs.
            if reuse {
                assert!(deep.is_dir());
            } else {
                fs::create_dir_all(&deep).unwrap();
            }
            for index in 0..12 {
                let name = format!("entry-{index:06}.md");
                let destination = deep.join(&name);
                let source = directory.join("listing").join(name);
                if reuse {
                    assert_eq!(fs::read(&source).unwrap(), fs::read(&destination).unwrap());
                } else {
                    fs::copy(source, &destination).unwrap();
                    set_synthetic_modified_time(&destination);
                }
            }
            if !reuse {
                set_synthetic_modified_time(&deep);
            }
            if kind == K::NotesOverwrite {
                let notes = deep.join("NOTES.md");
                prepare_style_file(&notes, b"# Synthetic release notes\n", reuse);
                if !reuse {
                    set_synthetic_modified_time(&deep);
                }
                app.open_save_as_for_gallery(
                    directory.join("SOURCE.md"),
                    deep,
                    "NOTES.md".into(),
                    context,
                );
            } else {
                let destination = if kind == K::DeepOpenError {
                    deep.join("missing-fixture-directory")
                } else {
                    deep
                };
                app.open_file_picker_for_gallery(destination, context);
            }
        }
        K::MaxPaste => app.request_paste_for_gallery(
            (0..100)
                .map(|n| format!("echo {n:03} {}.\n", "synthetic".repeat(10)))
                .collect(),
            context,
        ),
        K::DirtyClose | K::ExpandedOverflow => {}
    }
    fixture
}

fn style_review_harness(
    kind: StyleReviewKind,
    directory: &Path,
    size: egui::Vec2,
) -> Harness<'static, SurfaceFixture> {
    use StyleReviewKind as K;
    let fixture = style_review_fixture(kind, directory, &egui::Context::default());
    let mut harness = gallery_harness(
        egui::vec2(752.0, 516.0),
        |ui, state: &mut SurfaceFixture| state.show(ui),
        fixture,
    );
    let context = harness.ctx.clone();
    harness
        .state_mut()
        .install_context_assets(kind.base_kind(), &context);
    if kind == K::LongPalette {
        harness.run_steps(3);
        harness
            .get_by_role(egui::accesskit::Role::TextInput)
            .type_text("Synthetic staging relay");
        harness.run_steps(3);
        let SurfaceBody::App(app) = &harness.state().body else {
            unreachable!()
        };
        let identity = app.palette_identity_for_gallery();
        assert!(identity.contains(STYLE_LONG_SECONDARY));
        harness.get_by_label(&identity);
        assert!(harness
            .get_by_role(egui::accesskit::Role::TextInput)
            .is_focused());
    } else if kind == K::ExpandedOverflow {
        harness.run_steps(3);
    } else {
        prepare_surface(kind.base_kind(), &mut harness);
        match kind {
            K::AboutReadyLicenses => {
                harness.get_by_label("Hide Licenses");
            }
            K::NotesOverwrite => {
                harness.get_by_label("Remote host — none connected");
                harness.get_by_label_contains("Saving will replace it.");
            }
            K::LongLiveClose => {
                harness.get_by_label(&format!("Close “{STYLE_LONG_IDENTITY}”?"));
            }
            K::MaxPaste => {
                let SurfaceBody::App(app) = &harness.state().body else {
                    unreachable!()
                };
                let (preview, lines, characters) = app.paste_preview_for_gallery();
                assert_eq!((lines, characters, preview.chars().count()), (8, 800, 800));
                harness.get_by_label_contains("Preview omits");
            }
            K::DirtyClose => {
                harness.get_by_label("Discard changes");
                harness.get_by_label("Cancel");
            }
            _ => {}
        }
    }
    // A short root can legitimately virtualize all listing rows away. Prove
    // the real task at a usable root, then resize without navigation/reload.
    harness.set_size(size);
    harness.run_steps(3);
    if kind == K::ExpandedOverflow {
        prepare_surface(SurfaceKind::ChromeMenu, &mut harness);
    }
    harness.state().assert_no_transport_input();
    harness
}

fn style_review_geometry(
    kind: StyleReviewKind,
    harness: &Harness<'_, SurfaceFixture>,
) -> serde_json::Value {
    use StyleReviewKind as K;
    let root = harness.ctx.content_rect();
    let rect = |value: egui::Rect| [value.min.x, value.min.y, value.max.x, value.max.y];
    let (area_id, actions): (Option<&str>, &[&str]) = match kind {
        K::AboutReadyLicenses => (
            Some("fesTerm about dialog"),
            &[
                "Copy Version Information",
                "Install and Restart",
                "Hide Licenses",
                "Close",
            ],
        ),
        K::LongPalette => (Some("festerm_command_palette"), &[]),
        K::DeepOpenReady | K::DeepOpenError => (Some("markdown_file_picker"), &["Cancel"]),
        K::NotesOverwrite => (
            Some("text_editor_save_as"),
            &["Save", "Cancel", "Remote host — none connected"],
        ),
        K::LongLiveClose => (
            Some("close_session_confirmation"),
            &["Cancel", "Close Session"],
        ),
        K::MaxPaste => (Some("paste_confirmation"), &["Cancel", "Paste"]),
        K::DirtyClose => (
            Some("document_close_confirmation"),
            &["Save", "Discard changes", "Cancel"],
        ),
        K::ExpandedOverflow => (
            None,
            &[
                "Open File…",
                "Open Profiles",
                "Open Settings",
                "Command palette",
                "Session inspector",
                "About fesTerm",
            ],
        ),
    };
    let area = area_id.and_then(|id| {
        harness
            .ctx
            .memory(|memory| memory.area_rect(egui::Id::new(id)))
    });
    let actions: Vec<_> = actions.iter().map(|label| {
        match harness.query_all_by_label(label).rev().find(|node| area.is_none_or(|area| area.intersects(node.rect()))) {
            Some(node) => serde_json::json!({
                "label": label, "rect": rect(node.rect()), "in_root": root.contains_rect(node.rect()),
                "focused": node.is_focused(), "height": node.rect().height(),
            }),
            None => serde_json::json!({"label": label, "rect": null, "in_root": null, "status": "not-in-accessibility-tree"}),
        }
    }).collect();
    let identity = if kind == K::LongPalette {
        let SurfaceBody::App(app) = &harness.state().body else {
            unreachable!()
        };
        let label = app.palette_identity_for_gallery();
        harness.query_by_label(&label).map(|node| {
            serde_json::json!({"label": label, "rect": rect(node.rect()), "in_root": root.contains_rect(node.rect())})
        })
    } else {
        None
    };
    serde_json::json!({
        "schema": "festerm-style-review-geometry-v1", "fixture": format!("{kind:?}"),
        "root_rect": rect(root), "area_rect": area.map(rect),
        "area_in_root": area.map(|area| root.contains_rect(area)),
        "readiness_asserted_at": [752, 516], "actions": actions, "accessible_identity": identity,
        "qualification": "observations-only; clipping/missing actions are not passes; not native acceptance",
        "default_focus": match kind {
            K::LongLiveClose | K::MaxPaste => Some("Cancel"),
            K::DirtyClose => Some("Save"),
            _ => None,
        },
        "physical_fixture_identity": "owned checkout paths; publication review required",
    })
}

std::thread_local! {
    static STYLE_REVIEW_GEOMETRY: std::cell::RefCell<Option<serde_json::Value>> = const { std::cell::RefCell::new(None) };
    static STYLE_CAPTURE_ADAPTER: std::cell::RefCell<Option<std::sync::Arc<std::sync::Mutex<serde_json::Value>>>> = const { std::cell::RefCell::new(None) };
}

pub(crate) fn capture_style_review_gallery(
    kind: StyleReviewKind,
    size: egui::Vec2,
    fixture_id: &str,
) -> (image::RgbaImage, serde_json::Value) {
    let owned = OWNED_STYLE_SCOPE.with(|slot| {
        slot.borrow().as_ref().map(|scope| {
            kind.owned_signature(size, fixture_id)
                .unwrap_or_else(|error| panic!("{error}"));
            scope
                .start_scene(fixture_id)
                .unwrap_or_else(|error| panic!("{error}"))
        })
    });
    assert!(
        std::env::var_os("FESTERM_UI_SURFACE_FIXTURE_ROOT").is_none() || owned.is_some(),
        "owned physical root mode must start through the curated gallery entry point"
    );
    let directory = owned
        .clone()
        .unwrap_or_else(|| surface_fixture_directory(fixture_id));
    let mut harness = style_review_harness(kind, &directory, size);
    harness.remove_cursor();
    harness.run_steps(2);
    OWNED_STYLE_SCOPE.with(|slot| {
        if let Some(scope) = slot.borrow().as_ref() {
            scope
                .record_scene(
                    fixture_id,
                    kind.owned_signature(size, fixture_id)
                        .unwrap_or_else(|error| panic!("{error}")),
                )
                .unwrap_or_else(|error| panic!("{error}"));
        }
    });
    let mut geometry = style_review_geometry(kind, &harness);
    geometry["pixels_per_point"] = serde_json::json!(harness.ctx.pixels_per_point());
    geometry["native_pixels_per_point"] = serde_json::json!(harness
        .ctx
        .input(|input| input.viewport().native_pixels_per_point));
    geometry["adapter"] = STYLE_CAPTURE_ADAPTER.with(|slot| {
        slot.borrow()
            .as_ref()
            .map(|metadata| metadata.lock().unwrap().clone())
            .unwrap_or(serde_json::Value::Null)
    });
    geometry["physical_fixture_directory"] = serde_json::json!(directory);
    geometry["physical_identity_mode"] = OWNED_STYLE_SCOPE.with(|slot| {
        match slot.borrow().as_ref() {
            Some(scope) => serde_json::json!({
                "mode": "explicit-owned-physical", "root": scope.root,
                "run": scope.run, "phase": scope.phase,
                "boundary": "actual I/O path; not synthetic metadata, anonymity or native acceptance",
            }),
            None => serde_json::json!({
                "mode": "compiled-worktree-root", "cross_worktree_identity": "unqualified",
            }),
        }
    });
    eprintln!("style-review-geometry {fixture_id}: {geometry}");
    let image = harness
        .render()
        .expect("render production style-review fixture");
    drop(harness);
    if owned.is_none() {
        fs::remove_dir_all(directory).unwrap();
    }
    (image, geometry)
}

fn capture_style_review_image(
    kind: StyleReviewKind,
    size: egui::Vec2,
    fixture_id: &str,
) -> image::RgbaImage {
    let (image, geometry) = capture_style_review_gallery(kind, size, fixture_id);
    STYLE_REVIEW_GEOMETRY.with(|slot| *slot.borrow_mut() = Some(geometry));
    image
}

#[test]
fn style_review_fixtures_verify_ready_tasks_and_semantics_before_short_resize() {
    for kind in [
        StyleReviewKind::AboutReadyLicenses,
        StyleReviewKind::LongPalette,
        StyleReviewKind::DeepOpenReady,
        StyleReviewKind::DeepOpenError,
        StyleReviewKind::NotesOverwrite,
        StyleReviewKind::LongLiveClose,
        StyleReviewKind::MaxPaste,
        StyleReviewKind::DirtyClose,
        StyleReviewKind::ExpandedOverflow,
    ] {
        let directory = unique_surface_fixture_directory();
        let harness = style_review_harness(kind, &directory, egui::vec2(360.0, 240.0));
        let geometry = style_review_geometry(kind, &harness);
        assert_eq!(
            geometry["root_rect"],
            serde_json::json!([0.0, 0.0, 360.0, 240.0])
        );
        assert_eq!(
            geometry["readiness_asserted_at"],
            serde_json::json!([752, 516])
        );
        if kind != StyleReviewKind::ExpandedOverflow {
            assert!(geometry["area_rect"].is_array());
        }
        harness.state().assert_no_transport_input();
        if kind == StyleReviewKind::DirtyClose {
            assert_eq!(
                fs::read_to_string(directory.join("SOURCE.md")).unwrap(),
                "# Synthetic document\n",
            );
        }
        drop(harness);
        fs::remove_dir_all(directory).unwrap();
    }
}

pub(crate) fn capture_surface_gallery(kind: SurfaceKind, narrow: bool) -> image::RgbaImage {
    let scene = bounded_surface_scenes()
        .into_iter()
        .find(|scene| scene.kind == kind && scene.narrow == narrow)
        .unwrap();
    let directory = surface_fixture_directory(scene.id);
    let state = SurfaceFixture::new(kind, &directory, &egui::Context::default());
    let mut harness = gallery_harness(
        scene.size(),
        |ui, state: &mut SurfaceFixture| state.show(ui),
        state,
    );
    let context = harness.ctx.clone();
    harness.state_mut().install_context_assets(kind, &context);
    prepare_surface(kind, &mut harness);
    harness.remove_cursor();
    harness.run_steps(2);
    assert_surface(kind, &harness);
    let image = harness.render().expect("render production-widget surface");
    drop(harness);
    fs::remove_dir_all(directory).unwrap();
    image
}

#[test]
fn bounded_surface_fixtures_preserve_ready_state_focus_selection_and_targets() {
    for scene in bounded_surface_scenes()
        .into_iter()
        .filter(|scene| !scene.narrow)
    {
        let directory = unique_surface_fixture_directory();
        let mut probe = SurfaceProbe::new(scene, &directory, 1.0);
        probe.prepare(scene.kind, |delta| delta.clear());
        probe.fixture.assert_no_transport_input();
        drop(probe);
        fs::remove_dir_all(directory).unwrap();
    }
}

// --------------------------------------------------------------------
// Manifest emission.
// --------------------------------------------------------------------

#[derive(serde::Serialize)]
struct ManifestScenario {
    id: &'static str,
    section: &'static str,
    title: &'static str,
    caption: String,
    image: String,
    width: u32,
    height: u32,
    tier: &'static str,
    sha256: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    geometry: Option<serde_json::Value>,
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
    let selection = match std::env::var("FESTERM_UI_GALLERY_SCENES") {
        Ok(value) => Some(value),
        Err(std::env::VarError::NotPresent) => None,
        Err(error) => panic!("invalid gallery scene selector: {error}"),
    };
    let mut entries =
        selected_gallery_scenarios(selection.as_deref()).unwrap_or_else(|error| panic!("{error}"));
    if selection.is_some() {
        assert!(
            std::env::var_os("FESTERM_UI_GALLERY_OUT").is_some(),
            "curated capture needs an explicit fresh evidence output, not the published gallery"
        );
        if output.exists() {
            assert!(
                fs::read_dir(&output)
                    .expect("inspect curated capture output")
                    .next()
                    .is_none(),
                "curated output must be fresh; retain previous attempts at {}",
                output.display()
            );
        }
    }
    fs::create_dir_all(&output).expect("gallery output directory must be creatable");
    begin_owned_style_scope(&entries, &output).unwrap_or_else(|error| panic!("{error}"));

    entries.sort_by(|a, b| (a.section, a.id).cmp(&(b.section, b.id)));

    let mut manifest_scenarios = Vec::with_capacity(entries.len());
    let mut expected_files = std::collections::HashSet::new();
    expected_files.insert("manifest.json".to_owned());

    for scenario in &entries {
        let _ = STYLE_REVIEW_GEOMETRY.with(|slot| slot.borrow_mut().take());
        let image = (scenario.capture)();
        let file_name = format!("{}.png", scenario.id);
        let path = output.join(&file_name);
        image
            .save(&path)
            .unwrap_or_else(|error| panic!("saving {file_name} must succeed: {error}"));
        let bytes = fs::read(&path)
            .unwrap_or_else(|error| panic!("reading back {file_name} must succeed: {error}"));

        let geometry = STYLE_REVIEW_GEOMETRY.with(|slot| slot.borrow_mut().take());
        let caption = match geometry.as_ref().and_then(|geometry| geometry["physical_fixture_directory"].as_str()) {
            Some(directory) => format!(
                "{} Actual physical scene directory: {directory}. This is real I/O identity, not anonymized or synthetic display metadata; only explicitly matched owned runs qualify that input identity.",
                scenario.caption,
            ),
            None => scenario.caption.to_owned(),
        };
        manifest_scenarios.push(ManifestScenario {
            id: scenario.id,
            section: scenario.section,
            title: scenario.title,
            caption,
            image: file_name.clone(),
            width: image.width(),
            height: image.height(),
            tier: "headless",
            sha256: sha256_hex(&bytes),
            geometry,
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
    OWNED_STYLE_SCOPE.with(|slot| {
        if let Some(scope) = slot.borrow_mut().take() {
            scope.complete().unwrap_or_else(|error| panic!("{error}"));
        }
    });
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
