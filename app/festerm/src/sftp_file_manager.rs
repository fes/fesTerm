use std::{
    cmp::Ordering,
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    sync::{
        mpsc::{self, Receiver, Sender},
        Arc,
    },
    thread,
    time::SystemTime,
};

use eframe::egui::{
    self, Align, Color32, FontFamily, FontId, Key, Layout, RichText, ScrollArea, Sense, TextEdit,
    Ui, WidgetInfo, WidgetType,
};
use festerm_config::{CredentialKind, SftpPaneOrderPreference};
use festerm_secret_store::{SecretReference, SecretStore};
use festerm_session::HostKeyPrompt;
use festerm_ssh::{
    connect_gui_sftp_session, GuiSftpSessionConnectError, GuiSftpSessionConnectOutcome,
    HostIdentity, HostKeyDecisionResolver, SftpCollision, SftpCollisionDecision,
    SftpCollisionResolution, SftpCollisionScope, SftpDirectoryItem, SftpDirectorySnapshot,
    SftpEntryType, SftpLocation, SftpPath, SftpPathMetadata, SftpTransferEvent, SftpTransferId,
    SftpTransferManager, SftpTransferRequest, SftpTransferState, SshAuthentication,
    SshConnectionProfile, SshPrivateKey,
};
use festerm_ui_egui::{
    icon::{self, Icon},
    theme,
};

const SFTP_SECTION_GAP: f32 = 8.0;
const SFTP_PANE_INNER_PADDING: i8 = 0;
const SFTP_PANE_HEADER_HEIGHT: f32 = 35.0;
const SFTP_PANE_TOOLBAR_HEIGHT: f32 = 39.0;
const SFTP_PANE_FILTER_ROW_HEIGHT: f32 = 37.0;
const SFTP_PANE_FOOTER_HEIGHT: f32 = 26.0;
const SFTP_TOOL_BUTTON_SIZE: f32 = 28.0;
const SFTP_BREADCRUMB_HEIGHT: f32 = 28.0;
const SFTP_FILTER_FIELD_HEIGHT: f32 = 26.0;
const SFTP_TABLE_HEADER_HEIGHT: f32 = 27.0;
const SFTP_TABLE_ROW_HEIGHT: f32 = 31.0;
const SFTP_TRANSFER_RAIL_WIDTH: f32 = 76.0;
const SFTP_TRANSFER_BUTTON_WIDTH: f32 = 54.0;
const SFTP_TRANSFER_BUTTON_HEIGHT: f32 = 57.0;
const SFTP_STATUS_DOT_SIZE: f32 = 7.0;

#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq)]
struct SftpVisualSpec {
    pane_header_height: f32,
    pane_toolbar_height: f32,
    pane_filter_row_height: f32,
    pane_footer_height: f32,
    toolbar_button_size: f32,
    breadcrumb_height: f32,
    filter_field_height: f32,
    table_header_height: f32,
    table_row_height: f32,
    transfer_rail_width: f32,
    transfer_button_width: f32,
    transfer_button_height: f32,
}

#[cfg_attr(not(test), allow(dead_code))]
const SFTP_VISUAL_SPEC: SftpVisualSpec = SftpVisualSpec {
    pane_header_height: SFTP_PANE_HEADER_HEIGHT,
    pane_toolbar_height: SFTP_PANE_TOOLBAR_HEIGHT,
    pane_filter_row_height: SFTP_PANE_FILTER_ROW_HEIGHT,
    pane_footer_height: SFTP_PANE_FOOTER_HEIGHT,
    toolbar_button_size: SFTP_TOOL_BUTTON_SIZE,
    breadcrumb_height: SFTP_BREADCRUMB_HEIGHT,
    filter_field_height: SFTP_FILTER_FIELD_HEIGHT,
    table_header_height: SFTP_TABLE_HEADER_HEIGHT,
    table_row_height: SFTP_TABLE_ROW_HEIGHT,
    transfer_rail_width: SFTP_TRANSFER_RAIL_WIDTH,
    transfer_button_width: SFTP_TRANSFER_BUTTON_WIDTH,
    transfer_button_height: SFTP_TRANSFER_BUTTON_HEIGHT,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SftpTextRole {
    PaneLabel,
    PaneMeta,
    Breadcrumb,
    Filter,
    TableHeader,
    TableBody,
    TableMetadata,
    Footer,
    TransferButton,
    TransferMeta,
    DialogTitle,
    DialogBody,
    DialogMeta,
}

fn font_for_text_role(role: SftpTextRole) -> FontId {
    match role {
        SftpTextRole::PaneLabel => FontId::new(11.0, FontFamily::Proportional),
        SftpTextRole::PaneMeta => FontId::new(10.0, FontFamily::Monospace),
        SftpTextRole::Breadcrumb => FontId::new(10.0, FontFamily::Monospace),
        SftpTextRole::Filter => FontId::new(10.0, FontFamily::Proportional),
        SftpTextRole::TableHeader => FontId::new(11.0, FontFamily::Proportional),
        SftpTextRole::TableBody => FontId::new(11.0, FontFamily::Proportional),
        SftpTextRole::TableMetadata => FontId::new(11.0, FontFamily::Monospace),
        SftpTextRole::Footer => FontId::new(10.0, FontFamily::Proportional),
        SftpTextRole::TransferButton => FontId::new(9.0, FontFamily::Proportional),
        SftpTextRole::TransferMeta => FontId::new(10.0, FontFamily::Proportional),
        SftpTextRole::DialogTitle => FontId::new(15.0, FontFamily::Proportional),
        SftpTextRole::DialogBody => FontId::new(12.0, FontFamily::Proportional),
        SftpTextRole::DialogMeta => FontId::new(10.0, FontFamily::Monospace),
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SftpFileManagerLaunchTarget {
    pub(crate) label: String,
    pub(crate) username: String,
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) profile_id: Option<String>,
    pub(crate) stored_credential_kind: Option<CredentialKind>,
    pub(crate) known_host_persisted: bool,
}

impl SftpFileManagerLaunchTarget {
    pub(crate) fn connection_profile(&self) -> Result<SshConnectionProfile, String> {
        let identity =
            HostIdentity::new(&self.host, self.port).map_err(|error| error.to_string())?;
        let size = festerm_session::TerminalSize::new(80, 24)
            .expect("default GUI SFTP terminal size is valid");
        SshConnectionProfile::new(
            identity,
            self.username.clone(),
            SshConnectionProfile::DEFAULT_TERMINAL_TYPE,
            size,
        )
        .map_err(|error| error.to_string())
    }
}

#[derive(Clone)]
pub(crate) enum SftpFileManagerAuthentication {
    Password(String),
    PrivateKey {
        key_text: String,
        passphrase: Option<String>,
    },
    StoredPassword {
        store: Arc<dyn SecretStore>,
        reference: Arc<SecretReference>,
    },
    StoredPrivateKey {
        store: Arc<dyn SecretStore>,
        reference: Arc<SecretReference>,
    },
}

impl std::fmt::Debug for SftpFileManagerAuthentication {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Password(_) => {
                formatter.write_str("SftpFileManagerAuthentication::Password([REDACTED])")
            }
            Self::PrivateKey { .. } => {
                formatter.write_str("SftpFileManagerAuthentication::PrivateKey([REDACTED])")
            }
            Self::StoredPassword { .. } => {
                formatter.write_str("SftpFileManagerAuthentication::StoredPassword([REDACTED])")
            }
            Self::StoredPrivateKey { .. } => {
                formatter.write_str("SftpFileManagerAuthentication::StoredPrivateKey([REDACTED])")
            }
        }
    }
}

#[derive(Clone, Copy, Default, Eq, PartialEq)]
enum AuthMode {
    #[default]
    Password,
    PrivateKey,
}

#[derive(Clone, Default)]
struct AuthenticationFormState {
    password: String,
    private_key: String,
    passphrase: String,
    mode: AuthMode,
    feedback: Option<String>,
}

pub(crate) fn show_authentication_required(
    ui: &mut Ui,
    tab_id: crate::tabs::TabId,
    target: &SftpFileManagerLaunchTarget,
) -> Option<crate::tabs::AppCommand> {
    let state_id = ui.id().with(("gui_sftp_auth_state", tab_id));
    let mut state = ui.data(|data| {
        data.get_temp::<AuthenticationFormState>(state_id)
            .unwrap_or_default()
    });
    let mut command = None;
    egui::Frame::new()
        .inner_margin(egui::Margin::same(16))
        .show(ui, |ui| {
            ui.vertical(|ui| {
                ui.add_space(24.0);
                ui.heading("Open GUI SFTP");
                ui.label(format!(
                    "Destination: {}@{}:{}",
                    target.username, target.host, target.port
                ));
                if !target.known_host_persisted {
                    ui.label(
                        "If this host is new or its key changed, fesTerm will pause for host-key verification before opening the file manager.",
                    );
                }
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    ui.radio_value(&mut state.mode, AuthMode::Password, "Password");
                    ui.radio_value(&mut state.mode, AuthMode::PrivateKey, "Private key");
                });
                ui.add_space(6.0);
                match state.mode {
                    AuthMode::Password => {
                        ui.label("Password");
                        ui.add(TextEdit::singleline(&mut state.password).password(true));
                    }
                    AuthMode::PrivateKey => {
                        ui.label("OpenSSH private key");
                        ui.add(TextEdit::multiline(&mut state.private_key).desired_rows(8));
                        ui.label("Passphrase (optional)");
                        ui.add(TextEdit::singleline(&mut state.passphrase).password(true));
                    }
                }
                if let Some(feedback) = &state.feedback {
                    ui.colored_label(theme::STATUS_ERROR, feedback);
                }
                ui.add_space(10.0);
                if let (Some(profile_id), Some(kind)) =
                    (&target.profile_id, target.stored_credential_kind)
                {
                    let label = match kind {
                        CredentialKind::Password => "Use stored password",
                        CredentialKind::PrivateKey => "Use stored private key",
                    };
                    if ui
                        .add(egui::Button::new(label))
                        .clicked()
                    {
                        command = Some(
                            crate::tabs::AppCommand::StartStoredSftpFileManagerProfile {
                                profile_id: profile_id.clone(),
                            },
                        );
                    }
                    ui.add_space(6.0);
                }
                let connect = ui.add(egui::Button::new("Open SFTP file manager"));
                if connect.clicked() {
                    let authentication = match state.mode {
                        AuthMode::Password if state.password.is_empty() => {
                            state.feedback = Some("Enter a password.".to_owned());
                            None
                        }
                        AuthMode::Password => Some(SftpFileManagerAuthentication::Password(
                            std::mem::take(&mut state.password),
                        )),
                        AuthMode::PrivateKey if state.private_key.trim().is_empty() => {
                            state.feedback = Some("Paste an OpenSSH private key.".to_owned());
                            None
                        }
                        AuthMode::PrivateKey => Some(SftpFileManagerAuthentication::PrivateKey {
                            key_text: std::mem::take(&mut state.private_key),
                            passphrase: (!state.passphrase.is_empty())
                                .then(|| std::mem::take(&mut state.passphrase)),
                        }),
                    };
                    if let Some(authentication) = authentication {
                        state.feedback = None;
                        command = Some(crate::tabs::AppCommand::StartSftpFileManager {
                            target: target.clone(),
                            authentication,
                        });
                    }
                }
            });
        });
    ui.data_mut(|data| data.insert_temp(state_id, state));
    command
}

#[derive(Clone, Debug)]
struct PendingHostKeyDecision {
    prompt: HostKeyPrompt,
    resolver: HostKeyDecisionResolver,
}

fn host_key_destination(target: &SftpFileManagerLaunchTarget) -> String {
    format!("{}@{}:{}", target.username, target.host, target.port)
}

fn host_key_host_port(prompt: &HostKeyPrompt) -> String {
    format!("{}:{}", prompt.host(), prompt.port())
}

fn show_unknown_host_key_prompt(
    ui: &mut Ui,
    tab_id: crate::tabs::TabId,
    target: &SftpFileManagerLaunchTarget,
    prompt: &HostKeyPrompt,
) -> Option<crate::tabs::AppCommand> {
    let mut command = None;
    egui::Frame::new()
        .inner_margin(egui::Margin::same(16))
        .show(ui, |ui| {
            ui.vertical(|ui| {
                ui.add_space(24.0);
                ui.heading("Open GUI SFTP");
                ui.label(format!("Destination: {}", host_key_destination(target)));
                ui.add_space(10.0);
                ui.label(format!(
                    "The authenticity of host '{}' can't be established.",
                    host_key_host_port(prompt)
                ));
                ui.label(format!(
                    "ED25519 key fingerprint is {}.",
                    prompt.sha256_fingerprint()
                ));
                ui.label("Are you sure you want to continue connecting?");
                ui.add_space(10.0);
                ui.horizontal_wrapped(|ui| {
                    if ui.button("Reject").clicked() {
                        command = Some(crate::tabs::AppCommand::ResolveHostKeyTrust {
                            tab: tab_id,
                            decision: crate::tabs::HostKeyTrustDecision::Reject,
                        });
                    }
                    if ui.button("Accept Once").clicked() {
                        command = Some(crate::tabs::AppCommand::ResolveHostKeyTrust {
                            tab: tab_id,
                            decision: crate::tabs::HostKeyTrustDecision::AcceptOnce,
                        });
                    }
                    if ui.button("Accept and Remember").clicked() {
                        command = Some(crate::tabs::AppCommand::ResolveHostKeyTrust {
                            tab: tab_id,
                            decision: crate::tabs::HostKeyTrustDecision::AcceptAndPersist,
                        });
                    }
                });
                ui.add_space(6.0);
                ui.label(RichText::new(
                    "Accept Once applies only to this GUI SFTP connection attempt. Accept and Remember saves the fingerprint for future SSH and SFTP connections.",
                ).small().color(theme::TEXT_MUTED));
            });
        });
    command
}

fn show_changed_host_key_prompt(
    ui: &mut Ui,
    tab_id: crate::tabs::TabId,
    target: &SftpFileManagerLaunchTarget,
    prompt: &HostKeyPrompt,
) -> Option<crate::tabs::AppCommand> {
    let state_id = ui.id().with(("gui_sftp_changed_host_key_state", tab_id));
    let mut typed: String = ui.data_mut(|data| data.get_temp(state_id).unwrap_or_default());
    let mut command = None;

    egui::Frame::new()
        .inner_margin(egui::Margin::same(16))
        .show(ui, |ui| {
            ui.vertical(|ui| {
                ui.add_space(24.0);
                ui.heading("Open GUI SFTP");
                ui.label(format!("Destination: {}", host_key_destination(target)));
                ui.add_space(10.0);
                ui.colored_label(
                    theme::STATUS_ERROR,
                    "@ WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED! @",
                );
                ui.label(format!(
                    "The key previously trusted for '{}' was {}.",
                    host_key_host_port(prompt),
                    prompt
                        .previously_trusted_fingerprint()
                        .unwrap_or_default()
                ));
                ui.label(format!(
                    "The server now presents a different key: {}.",
                    prompt.sha256_fingerprint()
                ));
                ui.label(
                    "This could mean someone is intercepting this connection, or the host's key was legitimately changed.",
                );
                ui.label(
                    "Type 'yes' and press Enter to replace the trusted key and continue, or press Escape to cancel.",
                );
                let response = ui.add(
                    TextEdit::singleline(&mut typed)
                        .hint_text("Type yes to continue")
                        .desired_width(220.0),
                );
                let submit = response.lost_focus() && ui.input(|input| input.key_pressed(Key::Enter));
                if submit && typed == "yes" {
                    command = Some(crate::tabs::AppCommand::ResolveHostKeyTrust {
                        tab: tab_id,
                        decision: crate::tabs::HostKeyTrustDecision::AcceptAndPersist,
                    });
                } else if submit {
                    command = Some(crate::tabs::AppCommand::ResolveHostKeyTrust {
                        tab: tab_id,
                        decision: crate::tabs::HostKeyTrustDecision::Reject,
                    });
                }
                if ui.button("Cancel").clicked()
                    || ui.input(|input| input.key_pressed(Key::Escape))
                {
                    command = Some(crate::tabs::AppCommand::ResolveHostKeyTrust {
                        tab: tab_id,
                        decision: crate::tabs::HostKeyTrustDecision::Reject,
                    });
                }
            });
        });
    ui.data_mut(|data| data.insert_temp(state_id, typed));
    command
}

fn show_host_key_prompt(
    ui: &mut Ui,
    tab_id: crate::tabs::TabId,
    target: &SftpFileManagerLaunchTarget,
    prompt: &HostKeyPrompt,
) -> Option<crate::tabs::AppCommand> {
    if prompt.is_key_change() {
        show_changed_host_key_prompt(ui, tab_id, target, prompt)
    } else {
        show_unknown_host_key_prompt(ui, tab_id, target, prompt)
    }
}

fn show_connection_status_banner(ui: &mut Ui, connection_state: &SftpConnectionState) {
    if let SftpConnectionState::Failed { summary, details }
    | SftpConnectionState::Disconnected { summary, details } = connection_state
    {
        ui.colored_label(theme::STATUS_ERROR, summary);
        if !details.is_empty() {
            ui.label(RichText::new(details).small().color(theme::TEXT_MUTED));
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SftpSortColumn {
    Name,
    Size,
    Modified,
    Type,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SftpSortState {
    pub(crate) column: SftpSortColumn,
    pub(crate) descending: bool,
}

impl Default for SftpSortState {
    fn default() -> Self {
        Self {
            column: SftpSortColumn::Name,
            descending: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum PaneFocus {
    Local,
    Remote,
}

impl PaneFocus {
    fn opposite(self) -> Self {
        match self {
            Self::Local => Self::Remote,
            Self::Remote => Self::Local,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Local => "Local",
            Self::Remote => "Remote",
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct SftpPaneState {
    pub(crate) current_path: SftpPath,
    pub(crate) previous_valid_path: SftpPath,
    pub(crate) snapshot: Option<SftpDirectorySnapshot>,
    pub(crate) directory_metadata: Option<SftpPathMetadata>,
    pub(crate) filter: String,
    pub(crate) sort: SftpSortState,
    pub(crate) selected_paths: BTreeSet<String>,
    pub(crate) selected_anchor: Option<String>,
    pub(crate) cursor_path: Option<String>,
    pub(crate) history: Vec<SftpPath>,
    pub(crate) history_index: usize,
    pub(crate) loading: bool,
    pub(crate) stale: bool,
    pub(crate) error: Option<String>,
    pub(crate) details: Option<String>,
    pub(crate) editing_path: bool,
    pub(crate) path_text: String,
    pub(crate) path_focus_requested: bool,
    pub(crate) filter_focus_requested: bool,
    pub(crate) pending_request_id: u64,
    entry_name_keys: Vec<String>,
    visible_entries_cache: Option<Vec<SftpDirectoryItem>>,
}

impl SftpPaneState {
    fn new(path: SftpPath) -> Self {
        let path_text = path.display();
        Self {
            current_path: path.clone(),
            previous_valid_path: path,
            snapshot: None,
            directory_metadata: None,
            filter: String::new(),
            sort: SftpSortState::default(),
            selected_paths: BTreeSet::new(),
            selected_anchor: None,
            cursor_path: None,
            history: Vec::new(),
            history_index: 0,
            loading: true,
            stale: false,
            error: None,
            details: None,
            editing_path: false,
            path_text,
            path_focus_requested: false,
            filter_focus_requested: false,
            pending_request_id: 0,
            entry_name_keys: Vec::new(),
            visible_entries_cache: None,
        }
    }

    fn selected_items(&mut self) -> Vec<SftpDirectoryItem> {
        let selected_paths = self.selected_paths.clone();
        self.visible_entries()
            .iter()
            .filter(|item| selected_paths.contains(&path_key(&item.path)))
            .cloned()
            .collect()
    }

    fn selected_count(&self) -> usize {
        self.selected_paths.len()
    }

    fn selected_total_size(&self) -> Option<u64> {
        let snapshot = self.snapshot.as_ref()?;
        let mut total = 0_u64;
        let mut found = false;
        for entry in &snapshot.entries {
            if self.selected_paths.contains(&path_key(&entry.path)) {
                if let Some(size) = entry.size {
                    total += size;
                }
                found = true;
            }
        }
        found.then_some(total)
    }

    fn item_count(&self) -> usize {
        self.snapshot
            .as_ref()
            .map(|snapshot| snapshot.entries.len())
            .unwrap_or_default()
    }

    fn visible_entries(&mut self) -> &[SftpDirectoryItem] {
        if self.visible_entries_cache.is_none() {
            let filter = self.filter.trim().to_ascii_lowercase();
            let mut indices = self
                .snapshot
                .as_ref()
                .map(|snapshot| {
                    snapshot
                        .entries
                        .iter()
                        .enumerate()
                        .filter(|(index, _)| {
                            filter.is_empty()
                                || self.entry_name_keys[*index].contains(filter.as_str())
                        })
                        .map(|(index, _)| index)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            if let Some(snapshot) = &self.snapshot {
                indices.sort_by(|left, right| {
                    compare_items_with_keys(
                        &snapshot.entries[*left],
                        &self.entry_name_keys[*left],
                        &snapshot.entries[*right],
                        &self.entry_name_keys[*right],
                        self.sort,
                    )
                });
                self.visible_entries_cache = Some(
                    indices
                        .into_iter()
                        .map(|index| snapshot.entries[index].clone())
                        .collect(),
                );
            } else {
                self.visible_entries_cache = Some(Vec::new());
            }
        }
        self.visible_entries_cache
            .as_deref()
            .expect("visible entry cache was just populated")
    }

    fn invalidate_visible_entries(&mut self) {
        self.visible_entries_cache = None;
    }

    fn set_filter(&mut self, filter: String) {
        if self.filter != filter {
            self.filter = filter;
            self.invalidate_visible_entries();
            self.retain_existing_selection();
        }
    }

    fn clear_filter(&mut self) {
        self.set_filter(String::new());
    }

    fn set_sort(&mut self, column: SftpSortColumn) {
        if self.sort.column == column {
            self.sort.descending = !self.sort.descending;
        } else {
            self.sort = SftpSortState {
                column,
                descending: false,
            };
        }
        self.invalidate_visible_entries();
    }

    fn select_single(&mut self, path: &SftpPath) {
        let key = path_key(path);
        self.selected_paths.clear();
        self.selected_paths.insert(key.clone());
        self.selected_anchor = Some(key.clone());
        self.cursor_path = Some(key);
    }

    fn clear_selection(&mut self) {
        self.selected_paths.clear();
        self.selected_anchor = None;
        self.cursor_path = None;
    }

    fn set_snapshot(
        &mut self,
        snapshot: SftpDirectorySnapshot,
        metadata: Option<SftpPathMetadata>,
    ) {
        self.current_path = snapshot.path.clone();
        self.previous_valid_path = snapshot.path.clone();
        self.path_text = snapshot.path.display();
        self.snapshot = Some(snapshot);
        self.directory_metadata = metadata;
        self.entry_name_keys = self
            .snapshot
            .as_ref()
            .map(|snapshot| {
                snapshot
                    .entries
                    .iter()
                    .map(|entry| entry.name.to_ascii_lowercase())
                    .collect()
            })
            .unwrap_or_default();
        self.invalidate_visible_entries();
        self.loading = false;
        self.stale = false;
        self.error = None;
        self.details = None;
        self.retain_existing_selection();
    }

    fn retain_existing_selection(&mut self) {
        let valid = self
            .snapshot
            .as_ref()
            .map(|snapshot| {
                snapshot
                    .entries
                    .iter()
                    .map(|entry| path_key(&entry.path))
                    .collect::<BTreeSet<_>>()
            })
            .unwrap_or_default();
        self.selected_paths.retain(|path| valid.contains(path));
        self.cursor_path = self
            .cursor_path
            .clone()
            .filter(|path| valid.contains(path))
            .or_else(|| self.selected_paths.iter().next().cloned())
            .or_else(|| valid.iter().next().cloned());
    }

    fn set_error(&mut self, summary: String, details: String) {
        self.loading = false;
        self.error = Some(summary);
        self.details = Some(details);
        self.current_path = self.previous_valid_path.clone();
        self.path_text = self.current_path.display();
    }

    fn visible_index_for_key(&mut self, key: &str) -> Option<usize> {
        self.visible_entries()
            .iter()
            .position(|entry| path_key(&entry.path) == key)
    }

    fn move_cursor(&mut self, delta: isize, extend: bool) -> Option<SftpDirectoryItem> {
        let entries = self.visible_entries().to_vec();
        if entries.is_empty() {
            return None;
        }
        let current = self
            .cursor_path
            .as_deref()
            .and_then(|key| {
                entries
                    .iter()
                    .position(|entry| path_key(&entry.path).as_str() == key)
            })
            .unwrap_or(0) as isize;
        let next = (current + delta).clamp(0, entries.len() as isize - 1) as usize;
        let key = path_key(&entries[next].path);
        let previous_cursor = self.cursor_path.clone();
        self.cursor_path = Some(key.clone());
        if extend {
            let anchor_key = self
                .selected_anchor
                .clone()
                .or(previous_cursor)
                .unwrap_or_else(|| key.clone());
            self.selected_anchor = Some(anchor_key.clone());
            let anchor_index = self.visible_index_for_key(&anchor_key).unwrap_or(next);
            let start = anchor_index.min(next);
            let end = anchor_index.max(next);
            self.selected_paths = entries[start..=end]
                .iter()
                .map(|entry| path_key(&entry.path))
                .collect();
        } else {
            self.select_single(&entries[next].path);
        }
        Some(entries[next].clone())
    }

    fn toggle_cursor_selection(&mut self) -> Option<SftpDirectoryItem> {
        let entries = self.visible_entries().to_vec();
        let current = self
            .cursor_path
            .as_deref()
            .and_then(|key| {
                entries
                    .iter()
                    .position(|entry| path_key(&entry.path).as_str() == key)
            })
            .unwrap_or(0);
        let item = entries.get(current)?.clone();
        let key = path_key(&item.path);
        if !self.selected_paths.remove(&key) {
            self.selected_paths.insert(key.clone());
        }
        self.selected_anchor = Some(key.clone());
        self.cursor_path = Some(key);
        Some(item)
    }

    fn activate_cursor(&mut self) -> Option<SftpDirectoryItem> {
        let entries = self.visible_entries().to_vec();
        let current = self
            .cursor_path
            .as_deref()
            .and_then(|key| {
                entries
                    .iter()
                    .position(|entry| path_key(&entry.path).as_str() == key)
            })
            .unwrap_or(0);
        let item = entries.get(current)?.clone();
        self.select_single(&item.path);
        Some(item)
    }

    fn is_writable(&self) -> bool {
        match (&self.current_path, &self.directory_metadata) {
            (SftpPath::Local(path), _) => fs::metadata(path)
                .map(|metadata| !metadata.permissions().readonly())
                .unwrap_or(false),
            (SftpPath::Remote(_), Some(metadata)) => metadata
                .permissions
                .map(|bits| bits & 0o222 != 0)
                .unwrap_or(true),
            (SftpPath::Remote(_), None) => !self.stale,
        }
    }

    fn push_history(&mut self, new_path: SftpPath) {
        if self.history.last() == Some(&new_path) {
            self.current_path = new_path.clone();
            self.path_text = new_path.display();
            self.history_index = self.history.len().saturating_sub(1);
            return;
        }
        if self.history_index + 1 < self.history.len() {
            self.history.truncate(self.history_index + 1);
        }
        self.history.push(new_path.clone());
        self.history_index = self.history.len().saturating_sub(1);
        self.current_path = new_path.clone();
        self.path_text = new_path.display();
    }
}

#[derive(Clone, Debug)]
pub(crate) struct TransferHistoryItem {
    pub(crate) transfer_id: SftpTransferId,
    pub(crate) request: SftpTransferRequest,
    pub(crate) state: SftpTransferState,
    pub(crate) destination: Option<SftpPath>,
    pub(crate) bytes_transferred: u64,
    pub(crate) total_bytes: Option<u64>,
    pub(crate) details: Option<String>,
    pub(crate) pending_collision: Option<SftpCollision>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct TransferDrawerState {
    pub(crate) items: Vec<TransferHistoryItem>,
}

impl TransferDrawerState {
    fn upsert(
        &mut self,
        transfer_id: SftpTransferId,
        request: SftpTransferRequest,
    ) -> &mut TransferHistoryItem {
        if let Some(index) = self
            .items
            .iter()
            .position(|item| item.transfer_id == transfer_id)
        {
            return &mut self.items[index];
        }
        self.items.push(TransferHistoryItem {
            transfer_id,
            request,
            state: SftpTransferState::Queued,
            destination: None,
            bytes_transferred: 0,
            total_bytes: None,
            details: None,
            pending_collision: None,
        });
        self.items
            .last_mut()
            .expect("transfer history item was just pushed")
    }

    fn has_work(&self) -> bool {
        !self.items.is_empty()
    }

    fn clear_finished(&mut self) {
        self.items.retain(|item| {
            !matches!(
                item.state,
                SftpTransferState::Completed
                    | SftpTransferState::Cancelled
                    | SftpTransferState::Skipped
            )
        });
    }

    fn active_transfer_ids(&self) -> Vec<SftpTransferId> {
        self.items
            .iter()
            .filter(|item| {
                matches!(
                    item.state,
                    SftpTransferState::Queued
                        | SftpTransferState::Planning
                        | SftpTransferState::Running
                        | SftpTransferState::AwaitingCollision(_)
                )
            })
            .map(|item| item.transfer_id)
            .collect()
    }

    fn summary(&self) -> Option<TransferDrawerSummary> {
        let current_item = self
            .items
            .iter()
            .find(|item| {
                matches!(
                    item.state,
                    SftpTransferState::Running
                        | SftpTransferState::AwaitingCollision(_)
                        | SftpTransferState::Planning
                        | SftpTransferState::Queued
                )
            })
            .or_else(|| self.items.last())?;
        let active = self.active_transfer_ids().len();
        let completed = self
            .items
            .iter()
            .filter(|item| matches!(item.state, SftpTransferState::Completed))
            .count();
        let failed = self
            .items
            .iter()
            .filter(|item| matches!(item.state, SftpTransferState::Failed { .. }))
            .count();
        let total_known_bytes = self
            .items
            .iter()
            .filter_map(|item| item.total_bytes)
            .sum::<u64>();
        let transferred_bytes = self
            .items
            .iter()
            .map(|item| item.bytes_transferred)
            .sum::<u64>();
        let progress = (total_known_bytes > 0)
            .then_some((transferred_bytes as f32 / total_known_bytes as f32).clamp(0.0, 1.0));
        let summary = if active > 0 {
            format!("{active} active · {}", format_size(Some(total_known_bytes)))
        } else if failed > 0 {
            if completed > 0 {
                format!("{failed} failed · {completed} completed")
            } else {
                format!("{failed} failed")
            }
        } else {
            format!("{completed} completed")
        };
        let header_action = if active > 0 {
            TransferDrawerHeaderAction::CancelActive
        } else if self
            .items
            .iter()
            .any(|item| matches!(item.state, SftpTransferState::Completed))
        {
            TransferDrawerHeaderAction::ClearFinished
        } else {
            TransferDrawerHeaderAction::ClearCompleted
        };
        Some(TransferDrawerSummary {
            current_state: current_item.state.clone(),
            summary,
            progress,
            header_action,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TransferDrawerHeaderAction {
    CancelActive,
    ClearFinished,
    ClearCompleted,
}

impl TransferDrawerHeaderAction {
    fn label(self) -> &'static str {
        match self {
            Self::CancelActive => "Cancel",
            Self::ClearFinished => "Clear finished",
            Self::ClearCompleted => "Clear completed",
        }
    }
}

#[derive(Debug)]
struct TransferDrawerSummary {
    current_state: SftpTransferState,
    summary: String,
    progress: Option<f32>,
    header_action: TransferDrawerHeaderAction,
}

#[derive(Clone, Debug)]
pub(crate) struct SftpCollisionDialogState {
    pub(crate) collision: SftpCollision,
    pub(crate) apply_to_all: bool,
}

#[derive(Clone, Debug)]
pub(crate) enum SftpConnectionState {
    Connecting,
    AwaitingHostKey,
    Ready,
    Failed { summary: String, details: String },
    Disconnected { summary: String, details: String },
}

pub(crate) struct SftpFileManagerTab {
    pub(crate) label: String,
    pub(crate) profile_identifier: Option<String>,
    pub(crate) launch_target: SftpFileManagerLaunchTarget,
    pub(crate) local_pane: SftpPaneState,
    pub(crate) remote_pane: SftpPaneState,
    pub(crate) pane_order: SftpPaneOrderPreference,
    pub(crate) focused_pane: PaneFocus,
    pub(crate) narrow_focus: PaneFocus,
    pub(crate) connection_state: SftpConnectionState,
    pub(crate) transfer_drawer: TransferDrawerState,
    pub(crate) collision_dialog: Option<SftpCollisionDialogState>,
    command_sender: tokio::sync::mpsc::UnboundedSender<WorkerCommand>,
    event_receiver: Receiver<WorkerEvent>,
    event_sender: Sender<WorkerEvent>,
    repaint: egui::Context,
    next_local_request_id: u64,
    pending_host_key: Option<PendingHostKeyDecision>,
}

impl SftpFileManagerTab {
    pub(crate) fn new(
        target: SftpFileManagerLaunchTarget,
        authentication: SftpFileManagerAuthentication,
        known_host_fingerprint: Option<String>,
        local_directory: PathBuf,
        pane_order: SftpPaneOrderPreference,
        context: &egui::Context,
    ) -> Self {
        let local_pane = SftpPaneState::new(SftpPath::local(local_directory));
        let remote_pane = SftpPaneState::new(SftpPath::remote("/"));
        let (command_sender, command_receiver) = tokio::sync::mpsc::unbounded_channel();
        let (event_sender, event_receiver) = mpsc::channel();
        let repaint = context.clone();
        let launch_target = target.clone();
        let worker_event_sender = event_sender.clone();
        thread::Builder::new()
            .name(format!("festerm-gui-sftp-{}", target.label))
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("could not build tokio runtime for GUI SFTP worker");
                runtime.block_on(async move {
                    run_worker(
                        launch_target,
                        authentication,
                        known_host_fingerprint,
                        command_receiver,
                        worker_event_sender,
                        repaint,
                    )
                    .await;
                });
            })
            .expect("could not spawn GUI SFTP worker thread");

        let mut tab = Self {
            label: target.label.clone(),
            profile_identifier: target.profile_id.clone(),
            launch_target: target,
            local_pane,
            remote_pane,
            pane_order,
            focused_pane: PaneFocus::Local,
            narrow_focus: PaneFocus::Local,
            connection_state: SftpConnectionState::Connecting,
            transfer_drawer: TransferDrawerState::default(),
            collision_dialog: None,
            command_sender,
            event_receiver,
            event_sender,
            repaint: context.clone(),
            next_local_request_id: 1,
            pending_host_key: None,
        };
        let initial_local = tab.local_pane.current_path.clone();
        load_path(&mut tab, PaneFocus::Local, initial_local, false);
        tab
    }

    pub(crate) fn poll(&mut self) {
        while let Ok(event) = self.event_receiver.try_recv() {
            self.apply_event(event);
        }
    }

    pub(crate) fn set_pane_order(&mut self, pane_order: SftpPaneOrderPreference) {
        self.pane_order = pane_order;
    }

    pub(crate) fn host_key_prompt(&self) -> Option<&HostKeyPrompt> {
        self.pending_host_key
            .as_ref()
            .map(|pending| &pending.prompt)
    }

    pub(crate) fn resolve_host_key_trust(
        &mut self,
        decision: crate::tabs::HostKeyTrustDecision,
    ) -> Result<(), crate::tabs::HostKeyTrustResolutionError> {
        let Some(pending) = self.pending_host_key.take() else {
            return Err(crate::tabs::HostKeyTrustResolutionError::NoPendingPrompt);
        };
        self.connection_state = SftpConnectionState::Connecting;
        pending
            .resolver
            .resolve(&pending.prompt, decision.into())
            .map_err(crate::tabs::HostKeyTrustResolutionError::Transport)
    }

    pub(crate) fn show(
        &mut self,
        ui: &mut Ui,
        tab_id: crate::tabs::TabId,
    ) -> Option<crate::tabs::AppCommand> {
        self.poll();
        self.handle_keyboard(ui.ctx());
        let narrow = ui.available_width() < 980.0;
        self.show_toolbar(ui, narrow);
        ui.add_space(8.0);
        if let Some(pending) = self.pending_host_key.as_ref() {
            return show_host_key_prompt(ui, tab_id, &self.launch_target, &pending.prompt);
        }
        if narrow {
            self.show_narrow(ui);
        } else {
            let available_width = ui.available_width();
            let pane_width =
                ((available_width - SFTP_TRANSFER_RAIL_WIDTH - SFTP_SECTION_GAP * 2.0) / 2.0)
                    .max(280.0);
            let pane_height = ui.available_height().max(260.0);
            ui.horizontal_top(|ui| match self.pane_order {
                SftpPaneOrderPreference::LocalLeft => {
                    ui.allocate_ui_with_layout(
                        egui::vec2(pane_width, pane_height),
                        Layout::top_down(Align::Min),
                        |ui| self.show_pane(ui, PaneFocus::Local),
                    );
                    ui.add_space(SFTP_SECTION_GAP);
                    ui.allocate_ui_with_layout(
                        egui::vec2(SFTP_TRANSFER_RAIL_WIDTH, pane_height),
                        Layout::top_down(Align::Center),
                        |ui| self.show_transfer_rail(ui),
                    );
                    ui.add_space(SFTP_SECTION_GAP);
                    ui.allocate_ui_with_layout(
                        egui::vec2(pane_width, pane_height),
                        Layout::top_down(Align::Min),
                        |ui| self.show_pane(ui, PaneFocus::Remote),
                    );
                }
                SftpPaneOrderPreference::RemoteLeft => {
                    ui.allocate_ui_with_layout(
                        egui::vec2(pane_width, pane_height),
                        Layout::top_down(Align::Min),
                        |ui| self.show_pane(ui, PaneFocus::Remote),
                    );
                    ui.add_space(SFTP_SECTION_GAP);
                    ui.allocate_ui_with_layout(
                        egui::vec2(SFTP_TRANSFER_RAIL_WIDTH, pane_height),
                        Layout::top_down(Align::Center),
                        |ui| self.show_transfer_rail(ui),
                    );
                    ui.add_space(SFTP_SECTION_GAP);
                    ui.allocate_ui_with_layout(
                        egui::vec2(pane_width, pane_height),
                        Layout::top_down(Align::Min),
                        |ui| self.show_pane(ui, PaneFocus::Local),
                    );
                }
            });
        }
        self.show_transfer_drawer(ui);
        self.show_collision_dialog(ui.ctx());
        None
    }

    fn show_toolbar(&mut self, ui: &mut Ui, narrow: bool) {
        ui.horizontal(|ui| {
            ui.heading(format!("SFTP · {}", self.label));
            ui.label(
                RichText::new(match &self.connection_state {
                    SftpConnectionState::Connecting => "Connecting…",
                    SftpConnectionState::AwaitingHostKey => "Trust required",
                    SftpConnectionState::Ready => "Connected",
                    SftpConnectionState::Failed { .. } => "Connection failed",
                    SftpConnectionState::Disconnected { .. } => "Disconnected",
                })
                .color(match self.connection_state {
                    SftpConnectionState::Ready => theme::ACCENT_PRIMARY,
                    SftpConnectionState::Connecting => theme::TEXT_SECONDARY,
                    SftpConnectionState::AwaitingHostKey => theme::STATUS_ERROR,
                    SftpConnectionState::Failed { .. }
                    | SftpConnectionState::Disconnected { .. } => theme::STATUS_ERROR,
                }),
            );
            if narrow {
                ui.add_space(16.0);
                ui.selectable_value(&mut self.narrow_focus, PaneFocus::Local, "Local");
                ui.selectable_value(&mut self.narrow_focus, PaneFocus::Remote, "Remote");
            }
        });
        show_connection_status_banner(ui, &self.connection_state);
    }

    fn show_narrow(&mut self, ui: &mut Ui) {
        self.show_pane(ui, self.narrow_focus);
        ui.add_space(SFTP_SECTION_GAP);
        self.show_transfer_rail(ui);
    }

    fn show_pane(&mut self, ui: &mut Ui, focus: PaneFocus) {
        let mut request_back = false;
        let mut request_up = false;
        let mut request_home = false;
        let mut request_refresh = false;
        let mut request_open: Option<SftpDirectoryItem> = None;
        let mut request_navigate_text = false;
        let mut request_breadcrumb: Option<SftpPath> = None;
        let mut focused_this_pane = false;
        let mut request_reconnect = false;
        let pane_focused = self.focused_pane == focus;
        let remote_state =
            (focus == PaneFocus::Remote).then(|| pane_state_text(&self.connection_state));
        let remote_identity = format!(
            "· {}@{}",
            self.launch_target.username, self.launch_target.host
        );
        let reconnect_visible = matches!(
            self.connection_state,
            SftpConnectionState::Disconnected { .. }
        );
        let frame = egui::Frame::new()
            .fill(theme::SURFACE_WINDOW)
            .stroke(egui::Stroke::new(
                1.0,
                if pane_focused {
                    theme::BORDER_ACTIVE
                } else {
                    theme::BORDER_SUBTLE
                },
            ))
            .corner_radius(8.0)
            .inner_margin(egui::Margin::same(SFTP_PANE_INNER_PADDING));
        frame.show(ui, |ui| {
            ui.style_mut().spacing.item_spacing = egui::vec2(0.0, 0.0);
            let pane = pane_mut(self, focus);
            let interactions_enabled = !(focus == PaneFocus::Remote && pane.stale);
            let error_summary = pane.error.clone();
            let error_details = pane.details.clone();
            let footer_items = pane.item_count();
            let footer_selection = footer_summary(pane);
            let table_entries = pane.visible_entries().to_vec();
            ui.vertical(|ui| {
                egui::Frame::new()
                    .fill(theme::SURFACE_TERMINAL)
                    .stroke(egui::Stroke::new(1.0, theme::BORDER_SUBTLE))
                    .corner_radius(8.0)
                    .inner_margin(egui::Margin::symmetric(11, 0))
                    .show(ui, |ui| {
                        ui.set_min_height(SFTP_PANE_HEADER_HEIGHT);
                        ui.set_max_height(SFTP_PANE_HEADER_HEIGHT);
                        ui.horizontal(|ui| {
                            paint_sftp_glyph(
                                ui.painter(),
                                match focus {
                                    PaneFocus::Local => SftpGlyph::LocalPane,
                                    PaneFocus::Remote => SftpGlyph::RemotePane,
                                },
                                egui::Rect::from_min_size(ui.cursor().min, egui::vec2(16.0, 16.0)),
                                if focus == PaneFocus::Remote {
                                    theme::ACCENT_PRIMARY
                                } else {
                                    theme::TEXT_SECONDARY
                                },
                            );
                            ui.add_space(20.0);
                            ui.label(
                                RichText::new(focus.label().to_ascii_uppercase())
                                    .font(font_for_text_role(SftpTextRole::PaneLabel))
                                    .strong(),
                            );
                            ui.label(
                                RichText::new(match focus {
                                    PaneFocus::Local => "· This computer".to_owned(),
                                    PaneFocus::Remote => remote_identity.clone(),
                                })
                                .font(font_for_text_role(SftpTextRole::PaneMeta))
                                .color(theme::TEXT_SECONDARY),
                            );
                            ui.add_space(ui.available_width().max(0.0) - 140.0);
                            if let Some((state, color)) = remote_state {
                                let dot_rect = egui::Rect::from_center_size(
                                    egui::pos2(
                                        ui.cursor().min.x + SFTP_STATUS_DOT_SIZE,
                                        ui.max_rect().center().y,
                                    ),
                                    egui::vec2(SFTP_STATUS_DOT_SIZE, SFTP_STATUS_DOT_SIZE),
                                );
                                ui.painter().circle_filled(
                                    dot_rect.center(),
                                    SFTP_STATUS_DOT_SIZE / 2.0,
                                    color,
                                );
                                ui.add_space(14.0);
                                ui.label(
                                    RichText::new(state)
                                        .font(font_for_text_role(SftpTextRole::PaneMeta))
                                        .color(theme::TEXT_SECONDARY),
                                );
                                if reconnect_visible {
                                    ui.add_space(8.0);
                                    if ui.small_button("Reconnect").clicked() {
                                        request_reconnect = true;
                                    }
                                }
                            } else {
                                ui.label(
                                    RichText::new(if pane.is_writable() {
                                        "Writable"
                                    } else {
                                        "Read only"
                                    })
                                    .font(font_for_text_role(SftpTextRole::PaneMeta))
                                    .color(theme::TEXT_SECONDARY),
                                );
                            }
                        });
                    });
                ui.add_enabled_ui(interactions_enabled, |ui| {
                    egui::Frame::new()
                        .fill(theme::SURFACE_WINDOW)
                        .stroke(egui::Stroke::new(1.0, theme::BORDER_SUBTLE))
                        .corner_radius(5.0)
                        .inner_margin(egui::Margin::symmetric(7, 5))
                        .show(ui, |ui| {
                            ui.set_min_height(SFTP_PANE_TOOLBAR_HEIGHT);
                            ui.set_max_height(SFTP_PANE_TOOLBAR_HEIGHT);
                            ui.horizontal(|ui| {
                                if toolbar_icon_button(
                                    ui,
                                    SftpGlyph::Back,
                                    &format!("Back {} folder", focus.label()),
                                )
                                .clicked()
                                {
                                    request_back = true;
                                }
                                if toolbar_icon_button(
                                    ui,
                                    SftpGlyph::Up,
                                    &format!("Up {} folder", focus.label()),
                                )
                                .clicked()
                                {
                                    request_up = true;
                                }
                                if toolbar_icon_button(
                                    ui,
                                    SftpGlyph::Home,
                                    &format!("Home {} folder", focus.label()),
                                )
                                .clicked()
                                {
                                    request_home = true;
                                }
                                if toolbar_icon_button(
                                    ui,
                                    SftpGlyph::Refresh,
                                    &format!("Refresh {} folder", focus.label()),
                                )
                                .clicked()
                                {
                                    request_refresh = true;
                                }
                                ui.add_space(2.0);
                                if pane.editing_path {
                                    let response = ui.add(
                                        TextEdit::singleline(&mut pane.path_text)
                                            .id(path_field_id(focus))
                                            .hint_text("Enter path")
                                            .desired_width(f32::INFINITY)
                                            .font(font_for_text_role(SftpTextRole::Breadcrumb)),
                                    );
                                    if pane.path_focus_requested {
                                        response.request_focus();
                                        pane.path_focus_requested = false;
                                    }
                                    if response.lost_focus()
                                        && ui.input(|input| input.key_pressed(Key::Enter))
                                    {
                                        pane.editing_path = false;
                                        request_navigate_text = true;
                                    }
                                } else {
                                    let bar = egui::Frame::new()
                                        .fill(theme::SURFACE_TAB_INACTIVE)
                                        .stroke(egui::Stroke::new(1.0, theme::BORDER_SUBTLE))
                                        .corner_radius(5.0)
                                        .inner_margin(egui::Margin::symmetric(7, 0))
                                        .show(ui, |ui| {
                                            ui.set_min_height(SFTP_BREADCRUMB_HEIGHT);
                                            ui.horizontal_wrapped(|ui| {
                                                for (index, segment) in
                                                    breadcrumb_segments(&pane.current_path)
                                                        .into_iter()
                                                        .enumerate()
                                                {
                                                    if index > 0 {
                                                        ui.label(
                                                            RichText::new("/")
                                                                .font(font_for_text_role(
                                                                    SftpTextRole::Breadcrumb,
                                                                ))
                                                                .color(theme::TEXT_MUTED),
                                                        );
                                                    }
                                                    let text = RichText::new(segment.label.clone())
                                                        .font(font_for_text_role(
                                                            SftpTextRole::Breadcrumb,
                                                        ))
                                                        .color(if segment.current {
                                                            theme::TEXT_PRIMARY
                                                        } else {
                                                            theme::TEXT_SECONDARY
                                                        });
                                                    if segment.current {
                                                        ui.label(text);
                                                    } else if ui
                                                        .add(
                                                            egui::Button::new(text)
                                                                .fill(Color32::TRANSPARENT)
                                                                .stroke(egui::Stroke::NONE)
                                                                .min_size(egui::vec2(0.0, 18.0)),
                                                        )
                                                        .clicked()
                                                    {
                                                        request_breadcrumb = Some(segment.path);
                                                    }
                                                }
                                            });
                                        });
                                    let response = bar.response.interact(Sense::click());
                                    response.widget_info(|| {
                                        WidgetInfo::labeled(
                                            WidgetType::Button,
                                            true,
                                            format!(
                                                "{} path {}",
                                                focus.label(),
                                                pane.current_path.display()
                                            ),
                                        )
                                    });
                                    if response.double_clicked() {
                                        pane.editing_path = true;
                                        pane.path_focus_requested = true;
                                    }
                                }
                            });
                        });
                    egui::Frame::new()
                        .fill(theme::SURFACE_WINDOW)
                        .stroke(egui::Stroke::new(1.0, theme::BORDER_SUBTLE))
                        .corner_radius(5.0)
                        .inner_margin(egui::Margin::symmetric(8, 5))
                        .show(ui, |ui| {
                            ui.set_min_height(SFTP_PANE_FILTER_ROW_HEIGHT);
                            ui.set_max_height(SFTP_PANE_FILTER_ROW_HEIGHT);
                            let mut filter_text = pane.filter.clone();
                            let filter_response = show_filter_field(ui, &mut filter_text, focus);
                            if pane.filter_focus_requested {
                                filter_response.request_focus();
                                pane.filter_focus_requested = false;
                            }
                            if filter_response.changed() {
                                pane.set_filter(filter_text);
                            }
                        });
                });
                if let Some(error) = error_summary {
                    egui::Frame::new()
                        .fill(theme::STATUS_ERROR.gamma_multiply(0.12))
                        .stroke(egui::Stroke::new(
                            1.0,
                            theme::STATUS_ERROR.gamma_multiply(0.65),
                        ))
                        .corner_radius(6.0)
                        .inner_margin(egui::Margin::symmetric(9, 7))
                        .show(ui, |ui| {
                            ui.horizontal_wrapped(|ui| {
                                ui.label(
                                    RichText::new(error)
                                        .font(font_for_text_role(SftpTextRole::Filter))
                                        .strong()
                                        .color(theme::TEXT_PRIMARY),
                                );
                                if let Some(details) = &error_details {
                                    ui.label(
                                        RichText::new(details)
                                            .font(font_for_text_role(SftpTextRole::Filter))
                                            .color(theme::TEXT_SECONDARY),
                                    );
                                }
                                if interactions_enabled && ui.small_button("Retry").clicked() {
                                    request_refresh = true;
                                }
                            });
                        });
                }
                egui::Frame::new()
                    .fill(theme::SURFACE_WINDOW)
                    .stroke(egui::Stroke::new(1.0, theme::BORDER_SUBTLE))
                    .corner_radius(5.0)
                    .show(ui, |ui| {
                        let columns = sftp_table_columns(ui.available_width());
                        ui.horizontal(|ui| {
                            for (index, (title, column, align)) in [
                                ("Name", SftpSortColumn::Name, CellAlign::Left),
                                ("Size", SftpSortColumn::Size, CellAlign::Right),
                                ("Modified", SftpSortColumn::Modified, CellAlign::Left),
                                ("Type", SftpSortColumn::Type, CellAlign::Left),
                            ]
                            .into_iter()
                            .enumerate()
                            {
                                let response = show_table_header_cell(
                                    ui,
                                    columns[index],
                                    align,
                                    title,
                                    pane.sort.column == column,
                                    pane.sort.descending,
                                );
                                if interactions_enabled && response.clicked() {
                                    pane.set_sort(column);
                                }
                            }
                        });
                        ui.separator();
                        ScrollArea::vertical()
                            .id_salt(("sftp-pane", focus))
                            .show(ui, |ui| {
                                if pane.loading {
                                    ui.add_space(8.0);
                                    ui.label(
                                        RichText::new("Loading…")
                                            .font(font_for_text_role(SftpTextRole::TableBody))
                                            .color(theme::TEXT_SECONDARY),
                                    );
                                    return;
                                }
                                if table_entries.is_empty() {
                                    ui.add_space(8.0);
                                    if pane
                                        .snapshot
                                        .as_ref()
                                        .is_some_and(|snapshot| snapshot.entries.is_empty())
                                    {
                                        ui.label(
                                            RichText::new("This folder is empty.")
                                                .font(font_for_text_role(SftpTextRole::TableBody))
                                                .color(theme::TEXT_SECONDARY),
                                        );
                                    } else {
                                        ui.label(
                                            RichText::new(format!(
                                                "No items match \"{}\".",
                                                pane.filter
                                            ))
                                            .font(font_for_text_role(SftpTextRole::TableBody))
                                            .color(theme::TEXT_SECONDARY),
                                        );
                                        if interactions_enabled
                                            && ui.button("Clear filter").clicked()
                                        {
                                            pane.clear_filter();
                                        }
                                    }
                                    return;
                                }
                                for item in table_entries.iter().cloned() {
                                    let key = path_key(&item.path);
                                    let selected = pane.selected_paths.contains(&key);
                                    let row = egui::Frame::new()
                                        .fill(if selected {
                                            theme::SURFACE_SELECTION
                                        } else {
                                            Color32::TRANSPARENT
                                        })
                                        .stroke(egui::Stroke::new(
                                            1.0,
                                            if selected && pane_focused {
                                                theme::ACCENT_PRIMARY
                                            } else {
                                                Color32::TRANSPARENT
                                            },
                                        ))
                                        .corner_radius(4.0)
                                        .inner_margin(egui::Margin::symmetric(7, 0))
                                        .show(ui, |ui| {
                                            ui.set_min_height(SFTP_TABLE_ROW_HEIGHT);
                                            ui.set_max_height(SFTP_TABLE_ROW_HEIGHT);
                                            ui.horizontal(|ui| {
                                                ui.allocate_ui_with_layout(
                                                    egui::vec2(columns[0], SFTP_TABLE_ROW_HEIGHT),
                                                    Layout::left_to_right(Align::Center),
                                                    |ui| {
                                                        paint_sftp_glyph(
                                                            ui.painter(),
                                                            item_glyph(&item),
                                                            egui::Rect::from_min_size(
                                                                ui.cursor().min,
                                                                egui::vec2(15.0, 15.0),
                                                            ),
                                                            if selected {
                                                                theme::TEXT_PRIMARY
                                                            } else {
                                                                theme::TEXT_SECONDARY
                                                            },
                                                        );
                                                        ui.add_space(20.0);
                                                        ui.label(
                                                            RichText::new(&item.name)
                                                                .font(font_for_text_role(
                                                                    SftpTextRole::TableBody,
                                                                ))
                                                                .color(theme::TEXT_PRIMARY),
                                                        );
                                                    },
                                                );
                                                show_table_text_cell(
                                                    ui,
                                                    columns[1],
                                                    CellAlign::Right,
                                                    RichText::new(format_size(item.size))
                                                        .font(font_for_text_role(
                                                            SftpTextRole::TableMetadata,
                                                        ))
                                                        .color(if selected {
                                                            theme::TEXT_PRIMARY
                                                        } else {
                                                            theme::TEXT_SECONDARY
                                                        }),
                                                );
                                                show_table_text_cell(
                                                    ui,
                                                    columns[2],
                                                    CellAlign::Left,
                                                    RichText::new(format_modified(
                                                        item.modified_at,
                                                    ))
                                                    .font(font_for_text_role(
                                                        SftpTextRole::TableMetadata,
                                                    ))
                                                    .color(if selected {
                                                        theme::TEXT_PRIMARY
                                                    } else {
                                                        theme::TEXT_SECONDARY
                                                    }),
                                                );
                                                show_table_text_cell(
                                                    ui,
                                                    columns[3],
                                                    CellAlign::Left,
                                                    RichText::new(item_type_label(&item))
                                                        .font(font_for_text_role(
                                                            SftpTextRole::TableBody,
                                                        ))
                                                        .color(if selected {
                                                            theme::TEXT_PRIMARY
                                                        } else {
                                                            theme::TEXT_SECONDARY
                                                        }),
                                                );
                                            });
                                        });
                                    let response = row.response.interact(Sense::click());
                                    response.widget_info(|| {
                                        WidgetInfo::labeled(
                                            WidgetType::SelectableLabel,
                                            true,
                                            format!("{} {}", focus.label(), item.name),
                                        )
                                    });
                                    if interactions_enabled && response.clicked() {
                                        focused_this_pane = true;
                                        if ui.input(|input| input.modifiers.shift) {
                                            let anchor_key = pane
                                                .selected_anchor
                                                .clone()
                                                .or_else(|| pane.cursor_path.clone())
                                                .unwrap_or_else(|| key.clone());
                                            let anchor_index = table_entries
                                                .iter()
                                                .position(|candidate| {
                                                    path_key(&candidate.path) == anchor_key
                                                })
                                                .unwrap_or_default();
                                            let current_index = table_entries
                                                .iter()
                                                .position(|candidate| {
                                                    path_key(&candidate.path) == key
                                                })
                                                .unwrap_or(anchor_index);
                                            let start = anchor_index.min(current_index);
                                            let end = anchor_index.max(current_index);
                                            pane.selected_paths = table_entries[start..=end]
                                                .iter()
                                                .map(|candidate| path_key(&candidate.path))
                                                .collect();
                                            pane.selected_anchor = Some(anchor_key);
                                        } else if ui.input(|input| input.modifiers.command) {
                                            if !pane.selected_paths.remove(&key) {
                                                pane.selected_paths.insert(key.clone());
                                            }
                                            pane.selected_anchor = Some(key.clone());
                                        } else {
                                            pane.select_single(&item.path);
                                        }
                                        pane.cursor_path = Some(key.clone());
                                    }
                                    if interactions_enabled && response.double_clicked() {
                                        request_open = Some(item.clone());
                                    }
                                }
                            });
                    });
                egui::Frame::new()
                    .fill(theme::SURFACE_TERMINAL)
                    .stroke(egui::Stroke::new(1.0, theme::BORDER_SUBTLE))
                    .corner_radius(5.0)
                    .inner_margin(egui::Margin::symmetric(9, 0))
                    .show(ui, |ui| {
                        ui.set_min_height(SFTP_PANE_FOOTER_HEIGHT);
                        ui.set_max_height(SFTP_PANE_FOOTER_HEIGHT);
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(format!("{footer_items} items"))
                                    .font(font_for_text_role(SftpTextRole::Footer))
                                    .color(theme::TEXT_MUTED),
                            );
                            ui.label(
                                RichText::new("·")
                                    .font(font_for_text_role(SftpTextRole::Footer))
                                    .color(theme::TEXT_MUTED),
                            );
                            ui.label(
                                RichText::new(footer_selection)
                                    .font(font_for_text_role(SftpTextRole::Footer))
                                    .color(theme::TEXT_MUTED),
                            );
                        });
                    });
            });
        });
        if focused_this_pane {
            self.focused_pane = focus;
        }
        if request_reconnect {
            self.connection_state = SftpConnectionState::Connecting;
            self.remote_pane.loading = true;
            let _ = self.command_sender.send(WorkerCommand::Reconnect);
        }
        if request_back {
            self.navigate_history(focus, -1);
        }
        if request_up {
            self.navigate_up(focus);
        }
        if request_home {
            self.navigate_home(focus);
        }
        if request_refresh {
            self.refresh_pane(focus);
        }
        if request_navigate_text {
            self.navigate_to_text(focus);
        }
        if let Some(path) = request_breadcrumb {
            load_path(self, focus, path, true);
        }
        if let Some(item) = request_open {
            self.open_item(focus, &item);
        }
    }

    fn show_transfer_rail(&mut self, ui: &mut Ui) {
        let upload = transfer_action(
            PaneFocus::Local,
            &self.local_pane,
            &self.remote_pane,
            &self.connection_state,
        );
        let download = transfer_action(
            PaneFocus::Remote,
            &self.remote_pane,
            &self.local_pane,
            &self.connection_state,
        );
        egui::Frame::new()
            .fill(theme::SURFACE_TERMINAL)
            .stroke(egui::Stroke::new(1.0, theme::BORDER_SUBTLE))
            .corner_radius(7.0)
            .inner_margin(egui::Margin::same(8))
            .show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(24.0);
                    let upload_button = transfer_button(
                        ui,
                        SftpGlyph::TransferToRemote,
                        "Upload\nto Remote",
                        "Upload to Remote",
                        upload.enabled,
                    );
                    if let Some(reason) = &upload.reason {
                        upload_button.clone().on_disabled_hover_text(reason);
                    }
                    if upload_button.clicked() {
                        self.queue_transfer(PaneFocus::Local);
                    }
                    ui.add_space(12.0);
                    let download_button = transfer_button(
                        ui,
                        SftpGlyph::TransferToLocal,
                        "Download\nto Local",
                        "Download to Local",
                        download.enabled,
                    );
                    if let Some(reason) = &download.reason {
                        download_button.clone().on_disabled_hover_text(reason);
                    }
                    if download_button.clicked() {
                        self.queue_transfer(PaneFocus::Remote);
                    }
                });
            });
    }

    fn show_transfer_drawer(&mut self, ui: &mut Ui) {
        if !self.transfer_drawer.has_work() {
            return;
        }
        let summary = self
            .transfer_drawer
            .summary()
            .expect("non-empty transfer drawer has a summary");
        ui.add_space(10.0);
        egui::Frame::new()
            .fill(theme::SURFACE_TERMINAL)
            .stroke(egui::Stroke::new(1.0, theme::BORDER_SUBTLE))
            .corner_radius(8.0)
            .inner_margin(egui::Margin::symmetric(12, 10))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("Transfers")
                            .font(FontId::new(12.0, FontFamily::Proportional))
                            .strong(),
                    );
                    ui.label(
                        RichText::new(&summary.summary)
                            .font(font_for_text_role(SftpTextRole::TransferMeta))
                            .color(theme::TEXT_SECONDARY),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button(summary.header_action.label()).clicked() {
                            match summary.header_action {
                                TransferDrawerHeaderAction::CancelActive => {
                                    for transfer_id in self.transfer_drawer.active_transfer_ids() {
                                        let _ = self
                                            .command_sender
                                            .send(WorkerCommand::CancelTransfer(transfer_id));
                                    }
                                }
                                TransferDrawerHeaderAction::ClearFinished
                                | TransferDrawerHeaderAction::ClearCompleted => {
                                    self.transfer_drawer.clear_finished();
                                }
                            }
                        }
                    });
                });
                if let Some(progress) = summary.progress {
                    ui.add(
                        egui::ProgressBar::new(progress)
                            .desired_width(f32::INFINITY)
                            .fill(transfer_state_color(&summary.current_state))
                            .show_percentage(),
                    );
                }
                for item in &self.transfer_drawer.items {
                    ui.separator();
                    ui.horizontal_wrapped(|ui| {
                        ui.label(
                            RichText::new(item.request.source.display())
                                .font(font_for_text_role(SftpTextRole::TableBody))
                                .color(theme::TEXT_PRIMARY),
                        );
                        ui.label(
                            RichText::new(format!(
                                "→ {}",
                                item.destination
                                    .as_ref()
                                    .unwrap_or(&item.request.destination)
                                    .display()
                            ))
                            .font(font_for_text_role(SftpTextRole::TableMetadata))
                            .color(theme::TEXT_SECONDARY),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(
                                RichText::new(transfer_state_label(&item.state))
                                    .font(font_for_text_role(SftpTextRole::TransferMeta))
                                    .color(transfer_state_color(&item.state)),
                            );
                        });
                    });
                    if let Some(total) = item.total_bytes {
                        let progress = if total == 0 {
                            0.0
                        } else {
                            item.bytes_transferred as f32 / total as f32
                        };
                        ui.add(
                            egui::ProgressBar::new(progress)
                                .desired_width(f32::INFINITY)
                                .fill(transfer_state_color(&item.state))
                                .text(format!(
                                    "{} / {}",
                                    format_size(Some(item.bytes_transferred)),
                                    format_size(Some(total))
                                )),
                        );
                    }
                    if let Some(details) = &item.details {
                        ui.label(
                            RichText::new(details)
                                .font(font_for_text_role(SftpTextRole::TransferMeta))
                                .color(theme::TEXT_MUTED),
                        );
                    }
                    ui.horizontal(|ui| {
                        if matches!(item.state, SftpTransferState::AwaitingCollision(_))
                            && item.pending_collision.is_some()
                            && ui.button("Resolve…").clicked()
                        {
                            self.collision_dialog = Some(SftpCollisionDialogState {
                                collision: item
                                    .pending_collision
                                    .clone()
                                    .expect("checked pending collision"),
                                apply_to_all: false,
                            });
                        }
                        if matches!(
                            item.state,
                            SftpTransferState::Queued
                                | SftpTransferState::Planning
                                | SftpTransferState::Running
                                | SftpTransferState::AwaitingCollision(_)
                        ) && ui.button("Cancel").clicked()
                        {
                            let _ = self
                                .command_sender
                                .send(WorkerCommand::CancelTransfer(item.transfer_id));
                        }
                        if matches!(item.state, SftpTransferState::Failed { .. })
                            && ui.button("Retry").clicked()
                        {
                            let _ = self
                                .command_sender
                                .send(WorkerCommand::Enqueue(vec![item.request.clone()]));
                        }
                    });
                }
            });
    }

    fn show_collision_dialog(&mut self, ctx: &egui::Context) {
        let Some(dialog) = self.collision_dialog.as_mut() else {
            return;
        };
        let collision = dialog.collision.clone();
        let mut decision = None;
        let mut close = false;
        egui::Modal::new(egui::Id::new(("sftp_collision", self.label.as_str())))
            .backdrop_color(egui::Color32::from_black_alpha(128))
            .show(ctx, |ui| {
                ui.set_width(510.0);
                egui::Frame::new()
                    .fill(theme::SURFACE_OVERLAY)
                    .stroke(egui::Stroke::new(1.0, theme::BORDER_ACTIVE))
                    .corner_radius(9.0)
                    .inner_margin(egui::Margin::symmetric(17, 15))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            icon::paint(
                                ui.painter(),
                                Icon::Warning,
                                egui::Rect::from_min_size(ui.cursor().min, egui::vec2(18.0, 18.0)),
                                theme::STATUS_STARTING,
                            );
                            ui.add_space(24.0);
                            ui.label(
                                RichText::new("A file with this name already exists")
                                    .font(font_for_text_role(SftpTextRole::DialogTitle))
                                    .strong(),
                            );
                        });
                        ui.add_space(8.0);
                        ui.label(
                            RichText::new(format!(
                                "Choose what to do with {}. Nothing is overwritten until you choose Replace.",
                                collision
                                    .source
                                    .path
                                    .file_name()
                                    .unwrap_or_else(|_| collision.source.path.display())
                            ))
                            .font(font_for_text_role(SftpTextRole::DialogBody))
                            .color(theme::TEXT_SECONDARY),
                        );
                        ui.add_space(12.0);
                        ui.columns(2, |columns| {
                            for (column, (title, metadata)) in columns.iter_mut().zip([
                                ("Source · Local", &collision.source),
                                ("Destination · Remote", &collision.destination),
                            ]) {
                                egui::Frame::new()
                                    .fill(theme::SURFACE_TAB_INACTIVE)
                                    .stroke(egui::Stroke::new(1.0, theme::BORDER_SUBTLE))
                                    .corner_radius(6.0)
                                    .inner_margin(egui::Margin::same(10))
                                    .show(column, |ui| {
                                        ui.label(
                                            RichText::new(title)
                                                .font(font_for_text_role(SftpTextRole::Footer))
                                                .color(theme::TEXT_MUTED),
                                        );
                                        ui.label(
                                            RichText::new(
                                                metadata
                                                    .path
                                                    .file_name()
                                                    .unwrap_or_else(|_| metadata.path.display()),
                                            )
                                                .font(font_for_text_role(SftpTextRole::DialogBody))
                                                .color(theme::TEXT_PRIMARY),
                                        );
                                        ui.label(
                                            RichText::new(format_size(metadata.size))
                                                .font(font_for_text_role(SftpTextRole::DialogMeta))
                                                .color(theme::TEXT_SECONDARY),
                                        );
                                        ui.label(
                                            RichText::new(format_modified(metadata.modified_at))
                                                .font(font_for_text_role(SftpTextRole::DialogMeta))
                                                .color(theme::TEXT_SECONDARY),
                                        );
                                    });
                            }
                        });
                        if collision.can_apply_to_all {
                            ui.add_space(12.0);
                            ui.checkbox(
                                &mut dialog.apply_to_all,
                                "Apply this choice to all conflicts in this batch",
                            );
                        }
                        ui.add_space(12.0);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("Cancel").clicked() {
                                close = true;
                            }
                            for candidate in collision_decision_order().into_iter().rev() {
                                if !collision.allowed_decisions.contains(&candidate) {
                                    continue;
                                }
                                let label = match candidate {
                                    SftpCollisionDecision::Skip => "Skip",
                                    SftpCollisionDecision::Replace => "Replace",
                                    SftpCollisionDecision::KeepBoth => "Keep Both",
                                    SftpCollisionDecision::MergeFolders => "Merge folders",
                                };
                                let button = ui.button(label);
                                if candidate == SftpCollisionDecision::Skip {
                                    button.request_focus();
                                }
                                if button.clicked() {
                                    decision = Some(candidate);
                                }
                            }
                        });
                    });
            });
        if let Some(decision) = decision {
            let _ = self.command_sender.send(WorkerCommand::ResolveCollision(
                SftpCollisionResolution {
                    collision_id: collision.id,
                    decision,
                    scope: collision_scope(dialog.apply_to_all),
                },
            ));
            self.collision_dialog = None;
        } else if close || ctx.input(|input| input.key_pressed(Key::Escape)) {
            self.collision_dialog = None;
        }
    }

    fn handle_keyboard(&mut self, ctx: &egui::Context) {
        if self.collision_dialog.is_some() {
            return;
        }
        let command = ctx.input(|input| input.modifiers.command);
        let alt = ctx.input(|input| input.modifiers.alt);
        let shift = ctx.input(|input| input.modifiers.shift);
        let editing_path = self.focused_pane_ref().editing_path;
        let filter_focused =
            ctx.memory(|memory| memory.has_focus(filter_field_id(self.focused_pane)));
        if ctx.input(|input| input.key_pressed(Key::Tab)) {
            self.focused_pane = self.focused_pane.opposite();
        }
        if command && ctx.input(|input| input.key_pressed(Key::Enter)) {
            self.queue_transfer(self.focused_pane);
        }
        if command && ctx.input(|input| input.key_pressed(Key::F)) {
            let pane = self.focused_pane_mut();
            pane.editing_path = false;
            pane.filter_focus_requested = true;
        }
        if command && ctx.input(|input| input.key_pressed(Key::L)) {
            let pane = self.focused_pane_mut();
            pane.editing_path = true;
            pane.path_focus_requested = true;
        }
        if command && ctx.input(|input| input.key_pressed(Key::R)) {
            self.refresh_pane(self.focused_pane);
        }
        if alt && ctx.input(|input| input.key_pressed(Key::ArrowUp)) {
            self.navigate_up(self.focused_pane);
        }
        if alt && ctx.input(|input| input.key_pressed(Key::Home)) {
            self.navigate_home(self.focused_pane);
        }
        if alt && ctx.input(|input| input.key_pressed(Key::ArrowLeft)) {
            self.navigate_history(self.focused_pane, -1);
        }
        if !editing_path && !filter_focused {
            if ctx.input(|input| input.key_pressed(Key::ArrowDown)) {
                let _ = self.focused_pane_mut().move_cursor(1, shift);
            }
            if ctx.input(|input| input.key_pressed(Key::ArrowUp)) {
                let _ = self.focused_pane_mut().move_cursor(-1, shift);
            }
            if ctx.input(|input| input.key_pressed(Key::Space)) {
                let _ = self.focused_pane_mut().toggle_cursor_selection();
            }
            if !command && ctx.input(|input| input.key_pressed(Key::Enter)) {
                if let Some(item) = self.focused_pane_mut().activate_cursor() {
                    self.open_item(self.focused_pane, &item);
                }
            }
        }
        if ctx.input(|input| input.key_pressed(Key::Escape)) {
            if self.focused_pane_mut().editing_path {
                let pane = self.focused_pane_mut();
                pane.editing_path = false;
                pane.path_text = pane.current_path.display();
            } else if !self.focused_pane_ref().filter.is_empty() {
                self.focused_pane_mut().clear_filter();
            } else {
                self.focused_pane_mut().clear_selection();
            }
        }
    }

    fn focused_pane_mut(&mut self) -> &mut SftpPaneState {
        match self.focused_pane {
            PaneFocus::Local => &mut self.local_pane,
            PaneFocus::Remote => &mut self.remote_pane,
        }
    }

    fn focused_pane_ref(&self) -> &SftpPaneState {
        match self.focused_pane {
            PaneFocus::Local => &self.local_pane,
            PaneFocus::Remote => &self.remote_pane,
        }
    }

    fn navigate_history(&mut self, focus: PaneFocus, offset: isize) {
        let pane = pane_mut(self, focus);
        if pane.history.is_empty() {
            return;
        }
        let next = if offset < 0 {
            pane.history_index.saturating_sub(offset.unsigned_abs())
        } else {
            (pane.history_index + offset as usize).min(pane.history.len().saturating_sub(1))
        };
        if next == pane.history_index {
            return;
        }
        pane.history_index = next;
        let target = pane.history[next].clone();
        load_path(self, focus, target, false);
    }

    fn navigate_up(&mut self, focus: PaneFocus) {
        let path = pane_ref(self, focus).current_path.parent_directory();
        load_path(self, focus, path, true);
    }

    fn navigate_home(&mut self, focus: PaneFocus) {
        match focus {
            PaneFocus::Local => {
                let target = std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from("/"));
                load_path(self, focus, SftpPath::local(target), true);
            }
            PaneFocus::Remote => {
                load_path(self, focus, SftpPath::remote("/"), true);
            }
        }
    }

    fn navigate_to_text(&mut self, focus: PaneFocus) {
        let text = pane_ref(self, focus).path_text.trim().to_owned();
        if text.is_empty() {
            return;
        }
        let path = match focus {
            PaneFocus::Local => SftpPath::local(PathBuf::from(text)),
            PaneFocus::Remote => SftpPath::remote(text),
        };
        load_path(self, focus, path, true);
    }

    fn refresh_pane(&mut self, focus: PaneFocus) {
        let target = pane_ref(self, focus).current_path.clone();
        load_path(self, focus, target, false);
    }

    fn open_item(&mut self, focus: PaneFocus, item: &SftpDirectoryItem) {
        if item.file_type == SftpEntryType::Directory {
            load_path(self, focus, item.path.clone(), true);
        }
    }

    fn queue_transfer(&mut self, source_focus: PaneFocus) {
        let destination_path = match source_focus {
            PaneFocus::Local => self.remote_pane.current_path.clone(),
            PaneFocus::Remote => self.local_pane.current_path.clone(),
        };
        let action = match source_focus {
            PaneFocus::Local => transfer_action(
                source_focus,
                &self.local_pane,
                &self.remote_pane,
                &self.connection_state,
            ),
            PaneFocus::Remote => transfer_action(
                source_focus,
                &self.remote_pane,
                &self.local_pane,
                &self.connection_state,
            ),
        };
        if !action.enabled {
            return;
        }
        let requests = pane_mut(self, source_focus)
            .selected_items()
            .into_iter()
            .filter_map(|item| SftpTransferRequest::new(item.path, destination_path.clone()).ok())
            .collect::<Vec<_>>();
        if !requests.is_empty() {
            let _ = self.command_sender.send(WorkerCommand::Enqueue(requests));
        }
    }

    fn apply_event(&mut self, event: WorkerEvent) {
        match event {
            WorkerEvent::HostKeyVerificationRequired { prompt, resolver } => {
                self.connection_state = SftpConnectionState::AwaitingHostKey;
                self.pending_host_key = Some(PendingHostKeyDecision { prompt, resolver });
                self.remote_pane.loading = false;
            }
            WorkerEvent::Connected {
                remote_directory,
                remote_metadata,
            } => {
                self.connection_state = SftpConnectionState::Ready;
                self.pending_host_key = None;
                self.remote_pane
                    .set_snapshot(remote_directory, remote_metadata);
                self.remote_pane
                    .push_history(self.remote_pane.current_path.clone());
            }
            WorkerEvent::LocalDirectoryLoaded {
                focus,
                request_id,
                snapshot,
                metadata,
            } => {
                let pane = pane_mut(self, focus);
                if pane.pending_request_id == request_id {
                    pane.set_snapshot(snapshot, metadata);
                }
            }
            WorkerEvent::LocalDirectoryFailed {
                focus,
                request_id,
                summary,
                details,
            } => {
                let pane = pane_mut(self, focus);
                if pane.pending_request_id == request_id {
                    pane.set_error(summary, details);
                }
            }
            WorkerEvent::RemoteDirectoryLoaded { snapshot, metadata } => {
                self.connection_state = SftpConnectionState::Ready;
                self.pending_host_key = None;
                self.remote_pane.set_snapshot(snapshot, metadata);
            }
            WorkerEvent::RemoteDirectoryFailed { summary, details } => {
                self.pending_host_key = None;
                self.remote_pane.set_error(summary.clone(), details.clone());
                self.connection_state = SftpConnectionState::Disconnected { summary, details };
                self.remote_pane.stale = true;
            }
            WorkerEvent::Transfer(event) => self.apply_transfer_event(event),
            WorkerEvent::ConnectionFailed { summary, details } => {
                self.pending_host_key = None;
                self.connection_state = SftpConnectionState::Failed { summary, details };
                self.remote_pane.loading = false;
            }
        }
    }

    fn apply_transfer_event(&mut self, event: SftpTransferEvent) {
        match event {
            SftpTransferEvent::BatchQueued { .. } | SftpTransferEvent::BatchFinished { .. } => {}
            SftpTransferEvent::ItemStarted {
                transfer_id,
                source,
                destination,
                ..
            } => {
                let request = SftpTransferRequest::new(source, destination.clone())
                    .expect("transfer event path pair stays valid");
                let item = self.transfer_drawer.upsert(transfer_id, request);
                item.state = SftpTransferState::Running;
                item.destination = Some(destination);
                item.pending_collision = None;
            }
            SftpTransferEvent::ItemProgress {
                transfer_id,
                bytes_transferred,
                total_bytes,
                ..
            } => {
                if let Some(item) = self
                    .transfer_drawer
                    .items
                    .iter_mut()
                    .find(|item| item.transfer_id == transfer_id)
                {
                    item.bytes_transferred = bytes_transferred;
                    item.total_bytes = total_bytes;
                    item.state = SftpTransferState::Running;
                }
            }
            SftpTransferEvent::Collision(collision) => {
                if let Some(item) = self
                    .transfer_drawer
                    .items
                    .iter_mut()
                    .find(|item| item.transfer_id == collision.transfer_id)
                {
                    item.pending_collision = Some(collision.clone());
                    item.state = SftpTransferState::AwaitingCollision(collision.id);
                }
                self.collision_dialog = Some(SftpCollisionDialogState {
                    collision,
                    apply_to_all: false,
                });
            }
            SftpTransferEvent::DestinationDirectoryRefreshRequested { directory, .. } => {
                if directory.location() == SftpLocation::Local
                    && directory == self.local_pane.current_path
                {
                    self.refresh_pane(PaneFocus::Local);
                }
                if directory.location() == SftpLocation::Remote
                    && directory == self.remote_pane.current_path
                {
                    self.refresh_pane(PaneFocus::Remote);
                }
            }
            SftpTransferEvent::ItemCompleted {
                transfer_id,
                destination,
                bytes_transferred,
                total_bytes,
                ..
            } => {
                if let Some(item) = self
                    .transfer_drawer
                    .items
                    .iter_mut()
                    .find(|item| item.transfer_id == transfer_id)
                {
                    item.state = SftpTransferState::Completed;
                    item.destination = Some(destination);
                    item.bytes_transferred = bytes_transferred;
                    item.total_bytes = total_bytes;
                    item.pending_collision = None;
                }
            }
            SftpTransferEvent::ItemFailed {
                transfer_id,
                destination,
                reason,
                ..
            } => {
                if let Some(item) = self
                    .transfer_drawer
                    .items
                    .iter_mut()
                    .find(|item| item.transfer_id == transfer_id)
                {
                    item.state = SftpTransferState::Failed {
                        reason: reason.clone(),
                    };
                    item.destination = destination;
                    item.details = Some(reason);
                    item.pending_collision = None;
                }
            }
            SftpTransferEvent::ItemCancelled {
                transfer_id,
                destination,
                bytes_transferred,
                total_bytes,
                ..
            } => {
                if let Some(item) = self
                    .transfer_drawer
                    .items
                    .iter_mut()
                    .find(|item| item.transfer_id == transfer_id)
                {
                    item.state = SftpTransferState::Cancelled;
                    item.destination = destination;
                    item.bytes_transferred = bytes_transferred;
                    item.total_bytes = total_bytes;
                    item.pending_collision = None;
                }
            }
            SftpTransferEvent::ItemSkipped {
                transfer_id,
                destination,
                ..
            } => {
                if let Some(item) = self
                    .transfer_drawer
                    .items
                    .iter_mut()
                    .find(|item| item.transfer_id == transfer_id)
                {
                    item.state = SftpTransferState::Skipped;
                    item.destination = destination;
                    item.pending_collision = None;
                }
            }
        }
    }
}

impl Drop for SftpFileManagerTab {
    fn drop(&mut self) {
        if let Some(pending) = self.pending_host_key.take() {
            let _ = pending.resolver.cancel(&pending.prompt);
        }
    }
}

#[derive(Debug)]
enum WorkerCommand {
    LoadRemote { path: String },
    Enqueue(Vec<SftpTransferRequest>),
    CancelTransfer(SftpTransferId),
    ResolveCollision(SftpCollisionResolution),
    Reconnect,
}

#[derive(Debug)]
enum WorkerEvent {
    HostKeyVerificationRequired {
        prompt: HostKeyPrompt,
        resolver: HostKeyDecisionResolver,
    },
    Connected {
        remote_directory: SftpDirectorySnapshot,
        remote_metadata: Option<SftpPathMetadata>,
    },
    LocalDirectoryLoaded {
        focus: PaneFocus,
        request_id: u64,
        snapshot: SftpDirectorySnapshot,
        metadata: Option<SftpPathMetadata>,
    },
    LocalDirectoryFailed {
        focus: PaneFocus,
        request_id: u64,
        summary: String,
        details: String,
    },
    RemoteDirectoryLoaded {
        snapshot: SftpDirectorySnapshot,
        metadata: Option<SftpPathMetadata>,
    },
    RemoteDirectoryFailed {
        summary: String,
        details: String,
    },
    ConnectionFailed {
        summary: String,
        details: String,
    },
    Transfer(SftpTransferEvent),
}

async fn run_worker(
    target: SftpFileManagerLaunchTarget,
    authentication: SftpFileManagerAuthentication,
    known_host_fingerprint: Option<String>,
    mut command_receiver: tokio::sync::mpsc::UnboundedReceiver<WorkerCommand>,
    event_sender: mpsc::Sender<WorkerEvent>,
    repaint: egui::Context,
) {
    let mut session_known_host_fingerprint = known_host_fingerprint;
    let (mut browsing, accepted_fingerprint) = match connect_remote_session(
        &target,
        &authentication,
        session_known_host_fingerprint.as_deref(),
        &event_sender,
        &repaint,
    )
    .await
    {
        Ok(result) => result,
        Err(error) => {
            let _ = event_sender.send(WorkerEvent::ConnectionFailed {
                summary: "Could not connect the SFTP file manager.".to_owned(),
                details: error,
            });
            repaint.request_repaint();
            return;
        }
    };
    if let Some(fingerprint) = accepted_fingerprint {
        session_known_host_fingerprint = Some(fingerprint);
    }
    let (transfer_session, accepted_fingerprint) = match connect_remote_session(
        &target,
        &authentication,
        session_known_host_fingerprint.as_deref(),
        &event_sender,
        &repaint,
    )
    .await
    {
        Ok(result) => result,
        Err(error) => {
            let _ = event_sender.send(WorkerEvent::ConnectionFailed {
                summary: "Could not start the SFTP transfer worker.".to_owned(),
                details: error,
            });
            repaint.request_repaint();
            return;
        }
    };
    if let Some(fingerprint) = accepted_fingerprint {
        session_known_host_fingerprint = Some(fingerprint);
    }
    let mut transfer_manager = SftpTransferManager::new(transfer_session);
    let mut current_remote = browsing.remote_working_directory().to_owned();
    if let Ok(snapshot) = browsing.remote_directory_snapshot(None).await {
        let metadata = browsing
            .remote_path_metadata(&current_remote)
            .await
            .ok()
            .flatten();
        let _ = event_sender.send(WorkerEvent::Connected {
            remote_directory: snapshot,
            remote_metadata: metadata,
        });
        repaint.request_repaint();
    }

    loop {
        tokio::select! {
            maybe_command = command_receiver.recv() => {
                let Some(command) = maybe_command else { break; };
                match command {
                    WorkerCommand::LoadRemote { path } => {
                        match browsing.remote_directory_snapshot(Some(&path)).await {
                            Ok(snapshot) => {
                                current_remote = path;
                                let metadata = browsing.remote_path_metadata(&current_remote).await.ok().flatten();
                                let _ = event_sender.send(WorkerEvent::RemoteDirectoryLoaded {
                                    snapshot,
                                    metadata,
                                });
                            }
                            Err(error) => {
                                let _ = event_sender.send(WorkerEvent::RemoteDirectoryFailed {
                                    summary: "Could not load the remote folder.".to_owned(),
                                    details: error.to_string(),
                                });
                            }
                        }
                        repaint.request_repaint();
                    }
                    WorkerCommand::Enqueue(requests) => {
                        let _ = transfer_manager.enqueue_batch(requests);
                    }
                    WorkerCommand::CancelTransfer(transfer_id) => {
                        let _ = transfer_manager.cancel_transfer(transfer_id);
                    }
                    WorkerCommand::ResolveCollision(resolution) => {
                        let _ = transfer_manager.resolve_collision(resolution);
                    }
                    WorkerCommand::Reconnect => {
                        match connect_remote_session(
                            &target,
                            &authentication,
                            session_known_host_fingerprint.as_deref(),
                            &event_sender,
                            &repaint,
                        )
                        .await
                        {
                            Ok((session, accepted_fingerprint)) => {
                                if let Some(fingerprint) = accepted_fingerprint {
                                    session_known_host_fingerprint = Some(fingerprint);
                                }
                                browsing = session;
                                match browsing.remote_directory_snapshot(Some(&current_remote)).await {
                                    Ok(snapshot) => {
                                        let metadata = browsing.remote_path_metadata(&current_remote).await.ok().flatten();
                                        let _ = event_sender.send(WorkerEvent::RemoteDirectoryLoaded { snapshot, metadata });
                                    }
                                    Err(error) => {
                                        let _ = event_sender.send(WorkerEvent::RemoteDirectoryFailed {
                                            summary: "Could not reconnect the remote SFTP session.".to_owned(),
                                            details: error.to_string(),
                                        });
                                    }
                                }
                            }
                            Err(error) => {
                                let _ = event_sender.send(WorkerEvent::RemoteDirectoryFailed {
                                    summary: "Could not reconnect the remote SFTP session.".to_owned(),
                                    details: error,
                                });
                            }
                        }
                        repaint.request_repaint();
                    }
                }
            }
            transfer_event = transfer_manager.recv_event() => {
                let Some(transfer_event) = transfer_event else { break; };
                let _ = event_sender.send(WorkerEvent::Transfer(transfer_event));
                repaint.request_repaint();
            }
        }
    }
}

async fn connect_remote_session(
    target: &SftpFileManagerLaunchTarget,
    authentication: &SftpFileManagerAuthentication,
    known_host_fingerprint: Option<&str>,
    event_sender: &mpsc::Sender<WorkerEvent>,
    repaint: &egui::Context,
) -> Result<(festerm_ssh::SftpSession, Option<String>), String> {
    let profile = target.connection_profile()?;
    let authentication = match authentication {
        SftpFileManagerAuthentication::Password(password) => {
            SshAuthentication::password(password.clone())
        }
        SftpFileManagerAuthentication::PrivateKey {
            key_text,
            passphrase,
        } => {
            let key = match passphrase {
                Some(passphrase) if !passphrase.is_empty() => {
                    SshPrivateKey::from_encrypted_openssh(
                        key_text.as_bytes(),
                        festerm_ssh::SshKeyPassphrase::new(passphrase.clone()),
                    )
                }
                _ => SshPrivateKey::from_openssh(key_text.as_bytes()),
            }
            .map_err(|error| error.to_string())?;
            SshAuthentication::public_key(key)
        }
        SftpFileManagerAuthentication::StoredPassword { store, reference } => {
            SshAuthentication::stored_password(Arc::clone(store), reference)
        }
        SftpFileManagerAuthentication::StoredPrivateKey { store, reference } => {
            SshAuthentication::stored_private_key(Arc::clone(store), reference)
        }
    };
    match connect_gui_sftp_session(
        profile,
        authentication,
        None,
        known_host_fingerprint.map(str::to_owned),
    )
    .await
    .map_err(|error| match error {
        GuiSftpSessionConnectError::InteractiveAuthenticationUnsupported => {
            "GUI SFTP currently needs an explicit password, private key, or stored credential."
                .to_owned()
        }
        GuiSftpSessionConnectError::HostKeyRejected => {
            "The SSH host key was rejected or the trust prompt expired.".to_owned()
        }
        GuiSftpSessionConnectError::ConnectionFailed => {
            "The SSH/SFTP connection could not be established.".to_owned()
        }
    })? {
        GuiSftpSessionConnectOutcome::Connected(session) => Ok((session, None)),
        GuiSftpSessionConnectOutcome::NeedsHostKeyDecision {
            prompt,
            resolver,
            completion,
        } => {
            let fingerprint = prompt.sha256_fingerprint().to_owned();
            let _ =
                event_sender.send(WorkerEvent::HostKeyVerificationRequired { prompt, resolver });
            repaint.request_repaint();
            completion.wait().await.map(|session| (session, Some(fingerprint))).map_err(
                |error| match error {
                    GuiSftpSessionConnectError::InteractiveAuthenticationUnsupported => "GUI SFTP currently needs an explicit password, private key, or stored credential.".to_owned(),
                    GuiSftpSessionConnectError::HostKeyRejected => {
                        "The SSH host key was rejected or the trust prompt expired.".to_owned()
                    }
                    GuiSftpSessionConnectError::ConnectionFailed => {
                        "The SSH/SFTP connection could not be established.".to_owned()
                    }
                },
            )
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SftpGlyph {
    Back,
    Up,
    Home,
    Refresh,
    Search,
    LocalPane,
    RemotePane,
    Folder,
    File,
    Code,
    Image,
    Archive,
    Executable,
    Symlink,
    TransferToRemote,
    TransferToLocal,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CellAlign {
    Left,
    Right,
}

fn pane_state_text(connection_state: &SftpConnectionState) -> (&'static str, Color32) {
    match connection_state {
        SftpConnectionState::Connecting => ("Connecting…", theme::TEXT_SECONDARY),
        SftpConnectionState::AwaitingHostKey => ("Trust required", theme::STATUS_STARTING),
        SftpConnectionState::Ready => ("Connected", theme::STATUS_RUNNING),
        SftpConnectionState::Failed { .. } => ("Connection failed", theme::STATUS_ERROR),
        SftpConnectionState::Disconnected { .. } => ("Disconnected", theme::STATUS_ERROR),
    }
}

fn transfer_state_color(state: &SftpTransferState) -> Color32 {
    match state {
        SftpTransferState::Completed => theme::STATUS_RUNNING,
        SftpTransferState::Failed { .. } => theme::STATUS_ERROR,
        SftpTransferState::AwaitingCollision(_) => theme::STATUS_STARTING,
        SftpTransferState::Skipped | SftpTransferState::Cancelled => theme::TEXT_MUTED,
        _ => theme::ACCENT_PRIMARY,
    }
}

fn toolbar_icon_button(ui: &mut Ui, glyph: SftpGlyph, label: &str) -> egui::Response {
    let button = egui::Button::new("")
        .min_size(egui::vec2(SFTP_TOOL_BUTTON_SIZE, SFTP_TOOL_BUTTON_SIZE))
        .fill(Color32::TRANSPARENT)
        .stroke(egui::Stroke::NONE)
        .corner_radius(5.0);
    let response = ui.add(button);
    let fill = if response.hovered() || response.has_focus() {
        theme::SURFACE_TAB_ACTIVE
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect(
        response.rect,
        5.0,
        fill,
        egui::Stroke::NONE,
        egui::StrokeKind::Inside,
    );
    paint_sftp_glyph(
        ui.painter(),
        glyph,
        response.rect.shrink(6.0),
        if response.hovered() || response.has_focus() {
            theme::TEXT_PRIMARY
        } else {
            theme::TEXT_SECONDARY
        },
    );
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, label));
    response.on_hover_text(label)
}

fn transfer_button(
    ui: &mut Ui,
    glyph: SftpGlyph,
    label: &str,
    accessible_label: &str,
    enabled: bool,
) -> egui::Response {
    let ready_fill = theme::SURFACE_TAB_INACTIVE;
    let response = ui.add_enabled(
        enabled,
        egui::Button::new(
            RichText::new(label)
                .font(font_for_text_role(SftpTextRole::TransferButton))
                .color(if enabled {
                    theme::TEXT_PRIMARY
                } else {
                    theme::TEXT_SECONDARY
                }),
        )
        .min_size(egui::vec2(
            SFTP_TRANSFER_BUTTON_WIDTH,
            SFTP_TRANSFER_BUTTON_HEIGHT,
        ))
        .fill(if enabled {
            theme::SURFACE_TAB_ACTIVE
        } else {
            ready_fill
        })
        .stroke(egui::Stroke::new(
            1.0,
            if enabled {
                theme::ACCENT_PRIMARY
            } else {
                theme::BORDER_SUBTLE
            },
        ))
        .corner_radius(7.0),
    );
    paint_sftp_glyph(
        ui.painter(),
        glyph,
        egui::Rect::from_center_size(
            egui::pos2(response.rect.center().x, response.rect.top() + 18.0),
            egui::vec2(16.0, 16.0),
        ),
        if enabled {
            theme::TEXT_PRIMARY
        } else {
            theme::TEXT_SECONDARY
        },
    );
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, accessible_label));
    response
}

fn show_filter_field(ui: &mut Ui, filter_text: &mut String, focus: PaneFocus) -> egui::Response {
    let frame = egui::Frame::new()
        .fill(theme::SURFACE_TERMINAL)
        .stroke(egui::Stroke::new(1.0, theme::BORDER_SUBTLE))
        .corner_radius(5.0)
        .inner_margin(egui::Margin::symmetric(8, 4));
    let inner = frame.show(ui, |ui| {
        ui.set_min_height(SFTP_FILTER_FIELD_HEIGHT);
        ui.horizontal(|ui| {
            paint_sftp_glyph(
                ui.painter(),
                SftpGlyph::Search,
                egui::Rect::from_min_size(ui.cursor().min, egui::vec2(13.0, 13.0)),
                theme::TEXT_MUTED,
            );
            ui.add_space(19.0);
            ui.scope(|ui| {
                ui.style_mut().visuals.extreme_bg_color = Color32::TRANSPARENT;
                ui.style_mut().visuals.widgets.inactive.bg_fill = Color32::TRANSPARENT;
                ui.style_mut().spacing.item_spacing.x = 0.0;
                ui.add(
                    TextEdit::singleline(filter_text)
                        .frame(egui::Frame::NONE)
                        .id(filter_field_id(focus))
                        .desired_width(f32::INFINITY)
                        .font(font_for_text_role(SftpTextRole::Filter))
                        .hint_text("Filter this folder"),
                )
            })
            .inner
        })
        .inner
    });
    inner.inner
}

fn sftp_table_columns(available_width: f32) -> [f32; 4] {
    let width = available_width.max(260.0);
    let mut columns = [width * 0.53, width * 0.15, width * 0.22, width * 0.10];
    let allocated = columns.iter().sum::<f32>();
    columns[0] += width - allocated;
    columns
}

fn footer_summary(pane: &SftpPaneState) -> String {
    match pane.selected_count() {
        0 => "0 selected".to_owned(),
        1 => pane
            .selected_total_size()
            .map(|size| format!("1 selected · {}", format_size(Some(size))))
            .unwrap_or_else(|| "1 selected".to_owned()),
        count => pane
            .selected_total_size()
            .map(|size| format!("{count} selected · {}", format_size(Some(size))))
            .unwrap_or_else(|| format!("{count} selected")),
    }
}

fn show_table_header_cell(
    ui: &mut Ui,
    width: f32,
    align: CellAlign,
    title: &str,
    active: bool,
    descending: bool,
) -> egui::Response {
    let suffix = if active {
        if descending {
            " ↓"
        } else {
            " ↑"
        }
    } else {
        ""
    };
    let text = RichText::new(format!("{title}{suffix}"))
        .font(font_for_text_role(SftpTextRole::TableHeader))
        .color(theme::TEXT_MUTED);
    ui.allocate_ui_with_layout(
        egui::vec2(width, SFTP_TABLE_HEADER_HEIGHT),
        match align {
            CellAlign::Left => Layout::left_to_right(Align::Center),
            CellAlign::Right => Layout::right_to_left(Align::Center),
        },
        |ui| {
            ui.add(
                egui::Button::new(text)
                    .min_size(egui::vec2(width, SFTP_TABLE_HEADER_HEIGHT))
                    .fill(theme::SURFACE_TERMINAL)
                    .stroke(egui::Stroke::NONE)
                    .corner_radius(0.0),
            )
        },
    )
    .inner
}

fn show_table_text_cell(ui: &mut Ui, width: f32, align: CellAlign, text: RichText) {
    ui.allocate_ui_with_layout(
        egui::vec2(width, SFTP_TABLE_ROW_HEIGHT),
        match align {
            CellAlign::Left => Layout::left_to_right(Align::Center),
            CellAlign::Right => Layout::right_to_left(Align::Center),
        },
        |ui| {
            ui.label(text);
        },
    );
}

fn item_type_label(item: &SftpDirectoryItem) -> &'static str {
    match item.file_type {
        SftpEntryType::Directory => "Folder",
        SftpEntryType::Symlink => "Symlink",
        SftpEntryType::Other => "Other",
        SftpEntryType::File => {
            let extension = Path::new(item.name.as_str())
                .extension()
                .and_then(|extension| extension.to_str())
                .unwrap_or_default()
                .to_ascii_lowercase();
            match extension.as_str() {
                "rs" | "c" | "cc" | "cpp" | "h" | "hpp" | "py" | "sh" | "bash" | "zsh" | "js"
                | "ts" | "tsx" | "jsx" | "json" | "toml" | "yaml" | "yml" | "xml" | "md"
                | "txt" | "log" => "Text/Code",
                "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "bmp" => "Image",
                "zip" | "gz" | "tgz" | "tar" | "bz2" | "xz" | "7z" | "rar" => "Archive",
                "exe" | "bat" | "cmd" | "com" | "app" | "bin" => "Executable",
                _ => "File",
            }
        }
    }
}

fn item_glyph(item: &SftpDirectoryItem) -> SftpGlyph {
    match item_type_label(item) {
        "Folder" => SftpGlyph::Folder,
        "Symlink" => SftpGlyph::Symlink,
        "Text/Code" => SftpGlyph::Code,
        "Image" => SftpGlyph::Image,
        "Archive" => SftpGlyph::Archive,
        "Executable" => SftpGlyph::Executable,
        _ => SftpGlyph::File,
    }
}

fn paint_sftp_glyph(painter: &egui::Painter, glyph: SftpGlyph, rect: egui::Rect, color: Color32) {
    match glyph {
        SftpGlyph::Back => icon::paint(painter, Icon::Back, rect, color),
        SftpGlyph::Search => icon::paint(painter, Icon::Search, rect, color),
        SftpGlyph::LocalPane => icon::paint(painter, Icon::LocalTerminal, rect, color),
        SftpGlyph::RemotePane => icon::paint(painter, Icon::SshRemote, rect, color),
        SftpGlyph::Refresh => icon::paint(painter, Icon::Reconnect, rect, color),
        SftpGlyph::Up => {
            let stroke = egui::Stroke::new(1.5, color);
            painter.line_segment(
                [
                    egui::pos2(rect.center().x, rect.top() + 3.0),
                    egui::pos2(rect.center().x, rect.bottom() - 3.0),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    egui::pos2(rect.center().x, rect.top() + 3.0),
                    egui::pos2(rect.left() + 4.0, rect.top() + 9.0),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    egui::pos2(rect.center().x, rect.top() + 3.0),
                    egui::pos2(rect.right() - 4.0, rect.top() + 9.0),
                ],
                stroke,
            );
        }
        SftpGlyph::Home => {
            let stroke = egui::Stroke::new(1.5, color);
            painter.line_segment(
                [
                    egui::pos2(rect.left() + 3.0, rect.center().y),
                    egui::pos2(rect.center().x, rect.top() + 3.0),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    egui::pos2(rect.center().x, rect.top() + 3.0),
                    egui::pos2(rect.right() - 3.0, rect.center().y),
                ],
                stroke,
            );
            painter.rect_stroke(
                egui::Rect::from_min_max(
                    egui::pos2(rect.left() + 5.0, rect.center().y),
                    egui::pos2(rect.right() - 5.0, rect.bottom() - 3.0),
                ),
                2.5,
                stroke,
                egui::StrokeKind::Inside,
            );
        }
        SftpGlyph::Folder => {
            let stroke = egui::Stroke::new(1.5, color);
            let body = egui::Rect::from_min_max(
                egui::pos2(rect.left() + 2.0, rect.top() + 7.0),
                egui::pos2(rect.right() - 2.0, rect.bottom() - 3.0),
            );
            painter.line_segment(
                [
                    egui::pos2(rect.left() + 2.0, rect.top() + 9.0),
                    egui::pos2(rect.left() + 7.0, rect.top() + 9.0),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    egui::pos2(rect.left() + 7.0, rect.top() + 9.0),
                    egui::pos2(rect.left() + 9.0, rect.top() + 6.0),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    egui::pos2(rect.left() + 9.0, rect.top() + 6.0),
                    egui::pos2(rect.right() - 2.0, rect.top() + 6.0),
                ],
                stroke,
            );
            painter.rect_stroke(body, 2.5, stroke, egui::StrokeKind::Inside);
        }
        SftpGlyph::File
        | SftpGlyph::Code
        | SftpGlyph::Image
        | SftpGlyph::Archive
        | SftpGlyph::Executable => {
            let stroke = egui::Stroke::new(1.5, color);
            let page = egui::Rect::from_min_max(
                egui::pos2(rect.left() + 4.0, rect.top() + 2.5),
                egui::pos2(rect.right() - 4.0, rect.bottom() - 2.5),
            );
            painter.rect_stroke(page, 2.0, stroke, egui::StrokeKind::Inside);
            painter.line_segment(
                [
                    egui::pos2(page.right() - 4.0, page.top()),
                    egui::pos2(page.right(), page.top() + 4.0),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    egui::pos2(page.right() - 4.0, page.top()),
                    egui::pos2(page.right() - 4.0, page.top() + 4.0),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    egui::pos2(page.right() - 4.0, page.top() + 4.0),
                    egui::pos2(page.right(), page.top() + 4.0),
                ],
                stroke,
            );
            match glyph {
                SftpGlyph::Code => {
                    painter.line_segment(
                        [
                            egui::pos2(page.left() + 3.0, rect.center().y),
                            egui::pos2(page.left() + 6.0, rect.center().y - 3.0),
                        ],
                        stroke,
                    );
                    painter.line_segment(
                        [
                            egui::pos2(page.left() + 3.0, rect.center().y),
                            egui::pos2(page.left() + 6.0, rect.center().y + 3.0),
                        ],
                        stroke,
                    );
                    painter.line_segment(
                        [
                            egui::pos2(page.right() - 3.0, rect.center().y),
                            egui::pos2(page.right() - 6.0, rect.center().y - 3.0),
                        ],
                        stroke,
                    );
                    painter.line_segment(
                        [
                            egui::pos2(page.right() - 3.0, rect.center().y),
                            egui::pos2(page.right() - 6.0, rect.center().y + 3.0),
                        ],
                        stroke,
                    );
                }
                SftpGlyph::Image => {
                    painter.circle_stroke(
                        egui::pos2(page.left() + 4.0, page.top() + 4.0),
                        1.2,
                        stroke,
                    );
                    painter.line_segment(
                        [
                            egui::pos2(page.left() + 2.5, page.bottom() - 4.0),
                            egui::pos2(page.left() + 6.0, page.center().y),
                        ],
                        stroke,
                    );
                    painter.line_segment(
                        [
                            egui::pos2(page.left() + 6.0, page.center().y),
                            egui::pos2(page.right() - 2.5, page.bottom() - 4.0),
                        ],
                        stroke,
                    );
                }
                SftpGlyph::Archive => {
                    painter.line_segment(
                        [
                            egui::pos2(page.left() + 3.0, page.center().y),
                            egui::pos2(page.right() - 3.0, page.center().y),
                        ],
                        stroke,
                    );
                    painter.line_segment(
                        [
                            egui::pos2(page.left() + 5.0, page.top() + 5.0),
                            egui::pos2(page.right() - 5.0, page.top() + 5.0),
                        ],
                        stroke,
                    );
                }
                SftpGlyph::Executable => {
                    painter.line_segment(
                        [
                            egui::pos2(page.left() + 3.0, page.bottom() - 4.0),
                            egui::pos2(page.right() - 3.0, page.top() + 4.0),
                        ],
                        stroke,
                    );
                    painter.line_segment(
                        [
                            egui::pos2(page.right() - 5.0, page.top() + 4.0),
                            egui::pos2(page.right() - 3.0, page.top() + 4.0),
                        ],
                        stroke,
                    );
                    painter.line_segment(
                        [
                            egui::pos2(page.right() - 3.0, page.top() + 4.0),
                            egui::pos2(page.right() - 3.0, page.top() + 6.0),
                        ],
                        stroke,
                    );
                }
                _ => {}
            }
        }
        SftpGlyph::Symlink => {
            let stroke = egui::Stroke::new(1.5, color);
            painter.circle_stroke(egui::pos2(rect.left() + 7.0, rect.center().y), 3.0, stroke);
            painter.circle_stroke(egui::pos2(rect.right() - 7.0, rect.center().y), 3.0, stroke);
            painter.line_segment(
                [
                    egui::pos2(rect.left() + 10.0, rect.center().y),
                    egui::pos2(rect.right() - 10.0, rect.center().y),
                ],
                stroke,
            );
        }
        SftpGlyph::TransferToRemote | SftpGlyph::TransferToLocal => {
            let stroke = egui::Stroke::new(1.6, color);
            let (from_x, to_x) = match glyph {
                SftpGlyph::TransferToRemote => (rect.left() + 2.0, rect.right() - 2.0),
                SftpGlyph::TransferToLocal => (rect.right() - 2.0, rect.left() + 2.0),
                _ => unreachable!(),
            };
            painter.line_segment(
                [
                    egui::pos2(from_x, rect.center().y),
                    egui::pos2(to_x, rect.center().y),
                ],
                stroke,
            );
            let tip = egui::pos2(to_x, rect.center().y);
            let head_x = if glyph == SftpGlyph::TransferToRemote {
                to_x - 4.0
            } else {
                to_x + 4.0
            };
            painter.line_segment([tip, egui::pos2(head_x, rect.center().y - 4.0)], stroke);
            painter.line_segment([tip, egui::pos2(head_x, rect.center().y + 4.0)], stroke);
        }
    }
}

fn pane_ref(tab: &SftpFileManagerTab, focus: PaneFocus) -> &SftpPaneState {
    match focus {
        PaneFocus::Local => &tab.local_pane,
        PaneFocus::Remote => &tab.remote_pane,
    }
}

fn pane_mut(tab: &mut SftpFileManagerTab, focus: PaneFocus) -> &mut SftpPaneState {
    match focus {
        PaneFocus::Local => &mut tab.local_pane,
        PaneFocus::Remote => &mut tab.remote_pane,
    }
}

fn load_path(tab: &mut SftpFileManagerTab, focus: PaneFocus, path: SftpPath, push_history: bool) {
    {
        let pane = pane_mut(tab, focus);
        pane.loading = true;
        pane.error = None;
        pane.details = None;
        pane.path_text = path.display();
        pane.current_path = path.clone();
        if push_history {
            pane.push_history(path.clone());
        }
    }
    match focus {
        PaneFocus::Local => {
            tab.next_local_request_id += 1;
            let request_id = tab.next_local_request_id;
            pane_mut(tab, focus).pending_request_id = request_id;
            spawn_local_load(
                tab.event_sender.clone(),
                tab.repaint.clone(),
                focus,
                request_id,
                path,
            );
        }
        PaneFocus::Remote => {
            if let SftpPath::Remote(path) = path {
                let _ = tab.command_sender.send(WorkerCommand::LoadRemote { path });
            }
        }
    }
}

fn spawn_local_load(
    event_sender: Sender<WorkerEvent>,
    repaint: egui::Context,
    focus: PaneFocus,
    request_id: u64,
    path: SftpPath,
) {
    thread::Builder::new()
        .name(format!("festerm-gui-sftp-local-{request_id}"))
        .spawn(move || {
            let event = match local_snapshot_and_metadata(&path) {
                Ok((snapshot, metadata)) => WorkerEvent::LocalDirectoryLoaded {
                    focus,
                    request_id,
                    snapshot,
                    metadata,
                },
                Err(error) => WorkerEvent::LocalDirectoryFailed {
                    focus,
                    request_id,
                    summary: "Could not load the local folder.".to_owned(),
                    details: error,
                },
            };
            let _ = event_sender.send(event);
            repaint.request_repaint();
        })
        .expect("could not spawn GUI SFTP local loader thread");
}

fn local_snapshot_and_metadata(
    path: &SftpPath,
) -> Result<(SftpDirectorySnapshot, Option<SftpPathMetadata>), String> {
    let SftpPath::Local(path) = path else {
        return Err("local snapshot requested for non-local path".to_owned());
    };
    let snapshot = read_local_snapshot(path)?;
    Ok((
        snapshot,
        Some(SftpPathMetadata {
            path: SftpPath::local(path.clone()),
            file_type: SftpEntryType::Directory,
            size: None,
            modified_at: None,
            permissions: None,
        }),
    ))
}

fn read_local_snapshot(path: &Path) -> Result<SftpDirectorySnapshot, String> {
    let metadata = fs::metadata(path).map_err(|error| error.to_string())?;
    if !metadata.is_dir() {
        return Err(format!("{} is not a directory", path.display()));
    }
    let mut entries = Vec::new();
    for entry in fs::read_dir(path).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        let entry_path = entry.path();
        let metadata = fs::symlink_metadata(&entry_path).map_err(|error| error.to_string())?;
        let file_type = if metadata.file_type().is_dir() {
            SftpEntryType::Directory
        } else if metadata.file_type().is_file() {
            SftpEntryType::File
        } else if metadata.file_type().is_symlink() {
            SftpEntryType::Symlink
        } else {
            SftpEntryType::Other
        };
        entries.push(SftpDirectoryItem {
            name: entry.file_name().to_string_lossy().into_owned(),
            path: SftpPath::local(entry_path),
            file_type,
            size: metadata.is_file().then_some(metadata.len()),
            modified_at: metadata.modified().ok(),
            permissions: None,
        });
    }
    Ok(SftpDirectorySnapshot {
        location: SftpLocation::Local,
        path: SftpPath::local(path.to_path_buf()),
        loaded_at: SystemTime::now(),
        entries,
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TransferActionState {
    pub(crate) enabled: bool,
    pub(crate) reason: Option<String>,
}

pub(crate) fn transfer_action(
    source_focus: PaneFocus,
    source: &SftpPaneState,
    destination: &SftpPaneState,
    connection_state: &SftpConnectionState,
) -> TransferActionState {
    if source.selected_paths.is_empty() {
        return TransferActionState {
            enabled: false,
            reason: Some(format!(
                "Select one or more {} items first.",
                source_focus.label().to_lowercase()
            )),
        };
    }
    if source_focus == PaneFocus::Local && !matches!(connection_state, SftpConnectionState::Ready) {
        return TransferActionState {
            enabled: false,
            reason: Some("The remote SFTP connection is unavailable.".to_owned()),
        };
    }
    if !destination.is_writable() {
        return TransferActionState {
            enabled: false,
            reason: Some(format!(
                "The {} destination is read-only.",
                if destination.current_path.location() == SftpLocation::Local {
                    "local"
                } else {
                    "remote"
                }
            )),
        };
    }
    TransferActionState {
        enabled: true,
        reason: None,
    }
}

#[allow(dead_code)]
pub(crate) fn compare_items(
    left: &SftpDirectoryItem,
    right: &SftpDirectoryItem,
    sort: SftpSortState,
) -> Ordering {
    compare_items_with_keys(
        left,
        &left.name.to_ascii_lowercase(),
        right,
        &right.name.to_ascii_lowercase(),
        sort,
    )
}

fn compare_items_with_keys(
    left: &SftpDirectoryItem,
    left_name_key: &str,
    right: &SftpDirectoryItem,
    right_name_key: &str,
    sort: SftpSortState,
) -> Ordering {
    let folders_first = folder_rank(left.file_type).cmp(&folder_rank(right.file_type));
    if folders_first != Ordering::Equal {
        return folders_first;
    }
    let ordering = match sort.column {
        SftpSortColumn::Name => left_name_key.cmp(right_name_key),
        SftpSortColumn::Size => left
            .size
            .cmp(&right.size)
            .then_with(|| left_name_key.cmp(right_name_key)),
        SftpSortColumn::Modified => left
            .modified_at
            .cmp(&right.modified_at)
            .then_with(|| left_name_key.cmp(right_name_key)),
        SftpSortColumn::Type => item_type_label(left)
            .cmp(item_type_label(right))
            .then_with(|| left_name_key.cmp(right_name_key)),
    };
    if sort.descending {
        ordering.reverse()
    } else {
        ordering
    }
}

fn folder_rank(file_type: SftpEntryType) -> u8 {
    if file_type == SftpEntryType::Directory {
        0
    } else {
        1
    }
}

fn collision_decision_order() -> [SftpCollisionDecision; 4] {
    [
        SftpCollisionDecision::Replace,
        SftpCollisionDecision::Skip,
        SftpCollisionDecision::KeepBoth,
        SftpCollisionDecision::MergeFolders,
    ]
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const fn collision_scope(apply_to_all: bool) -> SftpCollisionScope {
    if apply_to_all {
        SftpCollisionScope::RemainingConflictsInBatch
    } else {
        SftpCollisionScope::ThisItem
    }
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn keyboard_shortcut_matches(
    command: SftpShortcut,
    modifiers: egui::Modifiers,
    key: Key,
) -> bool {
    match command {
        SftpShortcut::CopySelection => modifiers.command && key == Key::Enter,
        SftpShortcut::FocusPath => modifiers.command && key == Key::L,
        SftpShortcut::FocusFilter => modifiers.command && key == Key::F,
        SftpShortcut::Refresh => modifiers.command && key == Key::R,
        SftpShortcut::Back => modifiers.alt && key == Key::ArrowLeft,
        SftpShortcut::Up => modifiers.alt && key == Key::ArrowUp,
        SftpShortcut::Home => modifiers.alt && key == Key::Home,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(dead_code)]
pub(crate) enum SftpShortcut {
    CopySelection,
    FocusPath,
    FocusFilter,
    Refresh,
    Back,
    Up,
    Home,
}

fn format_size(size: Option<u64>) -> String {
    let Some(size) = size else {
        return "—".to_owned();
    };
    match size {
        0..=1023 => format!("{size} B"),
        1024..=1_048_575 => format!("{:.1} KiB", size as f64 / 1024.0),
        1_048_576..=1_073_741_823 => format!("{:.1} MiB", size as f64 / 1_048_576.0),
        _ => format!("{:.1} GiB", size as f64 / 1_073_741_824.0),
    }
}

fn format_modified(timestamp: Option<SystemTime>) -> String {
    timestamp
        .and_then(|time| time.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|duration| format!("{}", duration.as_secs()))
        .unwrap_or_else(|| "—".to_owned())
}

fn transfer_state_label(state: &SftpTransferState) -> &'static str {
    match state {
        SftpTransferState::Queued => "Queued",
        SftpTransferState::Planning => "Planning",
        SftpTransferState::AwaitingCollision(_) => "Waiting on conflict",
        SftpTransferState::Running => "Running",
        SftpTransferState::Completed => "Completed",
        SftpTransferState::Failed { .. } => "Failed",
        SftpTransferState::Cancelled => "Cancelled",
        SftpTransferState::Skipped => "Skipped",
    }
}

fn path_key(path: &SftpPath) -> String {
    match path {
        SftpPath::Local(path) => format!("local:{}", path.display()),
        SftpPath::Remote(path) => format!("remote:{path}"),
    }
}

fn filter_field_id(focus: PaneFocus) -> egui::Id {
    egui::Id::new(("sftp-pane-filter", focus))
}

fn path_field_id(focus: PaneFocus) -> egui::Id {
    egui::Id::new(("sftp-pane-path", focus))
}

struct BreadcrumbSegment {
    label: String,
    path: SftpPath,
    current: bool,
}

fn breadcrumb_segments(path: &SftpPath) -> Vec<BreadcrumbSegment> {
    match path {
        SftpPath::Remote(path) => {
            let trimmed = path.trim_end_matches('/');
            let mut segments = vec![BreadcrumbSegment {
                label: "/".to_owned(),
                path: SftpPath::remote("/"),
                current: trimmed.is_empty() || trimmed == "/",
            }];
            if trimmed.is_empty() || trimmed == "/" {
                return segments;
            }
            let mut current = String::new();
            for segment in trimmed.trim_start_matches('/').split('/') {
                current.push('/');
                current.push_str(segment);
                segments.push(BreadcrumbSegment {
                    label: segment.to_owned(),
                    path: SftpPath::remote(current.clone()),
                    current: current == trimmed,
                });
            }
            segments
        }
        SftpPath::Local(path) => {
            let mut segments = Vec::new();
            let mut current = PathBuf::new();
            for component in path.components() {
                use std::path::Component;
                match component {
                    Component::Prefix(prefix) => {
                        current.push(prefix.as_os_str());
                        segments.push(BreadcrumbSegment {
                            label: prefix.as_os_str().to_string_lossy().into_owned(),
                            path: SftpPath::local(current.clone()),
                            current: false,
                        });
                    }
                    Component::RootDir => {
                        current.push(Path::new("/"));
                        segments.push(BreadcrumbSegment {
                            label: "/".to_owned(),
                            path: SftpPath::local(current.clone()),
                            current: path.parent().is_none(),
                        });
                    }
                    Component::Normal(part) => {
                        current.push(part);
                        segments.push(BreadcrumbSegment {
                            label: part.to_string_lossy().into_owned(),
                            path: SftpPath::local(current.clone()),
                            current: current == *path,
                        });
                    }
                    Component::CurDir | Component::ParentDir => {}
                }
            }
            if segments.is_empty() {
                segments.push(BreadcrumbSegment {
                    label: path.display().to_string(),
                    path: SftpPath::local(path.clone()),
                    current: true,
                });
            } else if let Some(last) = segments.last_mut() {
                last.current = true;
            }
            segments
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::{kittest::Queryable, Harness};

    fn item(name: &str, file_type: SftpEntryType, size: Option<u64>) -> SftpDirectoryItem {
        SftpDirectoryItem {
            name: name.to_owned(),
            path: SftpPath::local(PathBuf::from(name)),
            file_type,
            size,
            modified_at: None,
            permissions: None,
        }
    }

    #[test]
    fn authentication_surface_offers_the_saved_credential_for_gui_sftp() {
        let target = SftpFileManagerLaunchTarget {
            label: "production".to_owned(),
            username: "deploy".to_owned(),
            host: "sftp.example.test".to_owned(),
            port: 22,
            profile_id: Some("production".to_owned()),
            stored_credential_kind: Some(CredentialKind::Password),
            known_host_persisted: true,
        };
        let tab_id = crate::tabs::AppState::for_test().active();
        let mut harness = Harness::builder().build_ui_state(
            move |ui, command: &mut Option<crate::tabs::AppCommand>| {
                if let Some(next) = show_authentication_required(ui, tab_id, &target) {
                    *command = Some(next);
                }
            },
            None,
        );
        harness.run();

        harness.get_by_label("Use stored password").click();
        harness.run();

        assert!(matches!(
            harness.state(),
            Some(crate::tabs::AppCommand::StartStoredSftpFileManagerProfile {
                profile_id
            }) if profile_id == "production"
        ));
    }

    #[test]
    fn authentication_surface_uses_the_standard_inner_padding() {
        let target = SftpFileManagerLaunchTarget {
            label: "production".to_owned(),
            username: "deploy".to_owned(),
            host: "sftp.example.test".to_owned(),
            port: 22,
            profile_id: None,
            stored_credential_kind: None,
            known_host_persisted: true,
        };
        let tab_id = crate::tabs::AppState::for_test().active();
        let mut harness = Harness::builder().build_ui_state(
            move |ui, command: &mut Option<crate::tabs::AppCommand>| {
                if let Some(next) = show_authentication_required(ui, tab_id, &target) {
                    *command = Some(next);
                }
            },
            None,
        );

        harness.run();

        assert!(
            harness.get_by_label("Open GUI SFTP").rect().left() >= 16.0,
            "the GUI SFTP auth form should use the same inset as other full-tab forms"
        );
    }

    #[test]
    fn authentication_surface_allows_connecting_before_host_trust_is_persisted() {
        let target = SftpFileManagerLaunchTarget {
            label: "production".to_owned(),
            username: "deploy".to_owned(),
            host: "sftp.example.test".to_owned(),
            port: 22,
            profile_id: None,
            stored_credential_kind: None,
            known_host_persisted: false,
        };
        let tab_id = crate::tabs::AppState::for_test().active();
        let mut harness = Harness::builder().build_ui_state(
            move |ui, command: &mut Option<crate::tabs::AppCommand>| {
                ui.data_mut(|data| {
                    data.insert_temp(
                        ui.id().with(("gui_sftp_auth_state", tab_id)),
                        AuthenticationFormState {
                            password: "secret".to_owned(),
                            ..Default::default()
                        },
                    );
                });
                if let Some(next) = show_authentication_required(ui, tab_id, &target) {
                    *command = Some(next);
                }
            },
            None,
        );

        harness.run();
        harness.get_by_label("Open SFTP file manager").click();
        harness.run();

        assert!(matches!(
            harness.state(),
            Some(crate::tabs::AppCommand::StartSftpFileManager { .. })
        ));
    }

    #[test]
    fn unknown_host_key_prompt_shows_inline_trust_actions() {
        let target = SftpFileManagerLaunchTarget {
            label: "production".to_owned(),
            username: "deploy".to_owned(),
            host: "sftp.example.test".to_owned(),
            port: 22,
            profile_id: None,
            stored_credential_kind: None,
            known_host_persisted: false,
        };
        let prompt = HostKeyPrompt::new("sftp.example.test", 22, "SHA256:abcDef012+/");
        let tab_id = crate::tabs::AppState::for_test().active();
        let mut harness = Harness::builder().build_ui_state(
            move |ui, command: &mut Option<crate::tabs::AppCommand>| {
                *command = show_host_key_prompt(ui, tab_id, &target, &prompt);
            },
            None,
        );

        harness.run();
        assert!(harness.query_by_label("Accept Once").is_some());
        assert!(harness.query_by_label("Accept and Remember").is_some());
    }

    #[test]
    fn failed_connection_banner_shows_diagnostic_details() {
        let state = SftpConnectionState::Failed {
            summary: "Connection failed".to_owned(),
            details: "The SSH host key was rejected or the trust prompt expired.".to_owned(),
        };
        let mut harness = Harness::builder().build_ui_state(
            move |ui, command: &mut Option<crate::tabs::AppCommand>| {
                show_connection_status_banner(ui, &state);
                *command = None;
            },
            None,
        );

        harness.run();

        assert!(harness.query_by_label("Connection failed").is_some());
        assert!(harness
            .query_by_label("The SSH host key was rejected or the trust prompt expired.")
            .is_some());
    }

    #[test]
    fn disconnected_connection_banner_shows_diagnostic_details() {
        let state = SftpConnectionState::Disconnected {
            summary: "Disconnected".to_owned(),
            details: "The SSH/SFTP connection could not be established.".to_owned(),
        };
        let mut harness = Harness::builder().build_ui_state(
            move |ui, command: &mut Option<crate::tabs::AppCommand>| {
                show_connection_status_banner(ui, &state);
                *command = None;
            },
            None,
        );

        harness.run();

        assert!(harness.query_by_label("Disconnected").is_some());
        assert!(harness
            .query_by_label("The SSH/SFTP connection could not be established.")
            .is_some());
    }

    #[test]
    fn sort_and_filter_keep_folders_first() {
        let mut pane = SftpPaneState::new(SftpPath::local("/tmp"));
        pane.set_snapshot(
            SftpDirectorySnapshot {
                location: SftpLocation::Local,
                path: SftpPath::local("/tmp"),
                loaded_at: SystemTime::now(),
                entries: vec![
                    item("zeta.txt", SftpEntryType::File, Some(4)),
                    item("alpha", SftpEntryType::Directory, None),
                    item("beta.txt", SftpEntryType::File, Some(2)),
                ],
            },
            None,
        );
        pane.set_filter("ta".to_owned());
        let entries = pane.visible_entries();
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            vec!["beta.txt", "zeta.txt"]
        );
        pane.clear_filter();
        let entries = pane.visible_entries();
        assert_eq!(
            entries.first().map(|entry| entry.name.as_str()),
            Some("alpha")
        );
    }

    #[test]
    fn breadcrumb_segments_expose_clickable_ancestors() {
        let remote = breadcrumb_segments(&SftpPath::remote("/srv/releases/2026.09"));
        assert_eq!(
            remote
                .iter()
                .map(|segment| segment.label.as_str())
                .collect::<Vec<_>>(),
            vec!["/", "srv", "releases", "2026.09"]
        );
        assert!(remote.last().is_some_and(|segment| segment.current));
    }

    #[test]
    fn arrow_selection_extends_contiguous_range() {
        let mut pane = SftpPaneState::new(SftpPath::local("/tmp"));
        pane.set_snapshot(
            SftpDirectorySnapshot {
                location: SftpLocation::Local,
                path: SftpPath::local("/tmp"),
                loaded_at: SystemTime::now(),
                entries: vec![
                    item("alpha", SftpEntryType::File, Some(1)),
                    item("beta", SftpEntryType::File, Some(1)),
                    item("gamma", SftpEntryType::File, Some(1)),
                ],
            },
            None,
        );
        pane.select_single(&SftpPath::local("alpha"));
        let _ = pane.move_cursor(1, true);
        assert_eq!(pane.selected_paths.len(), 2);
        assert!(pane
            .selected_paths
            .contains(&path_key(&SftpPath::local("alpha"))));
        assert!(pane
            .selected_paths
            .contains(&path_key(&SftpPath::local("beta"))));
    }

    #[test]
    fn pane_order_preference_swaps_visual_sides_only() {
        let order = SftpPaneOrderPreference::RemoteLeft;
        assert_eq!(order, SftpPaneOrderPreference::RemoteLeft);
        assert_eq!(PaneFocus::Local.label(), "Local");
        assert_eq!(PaneFocus::Remote.label(), "Remote");
    }

    #[test]
    fn transfer_action_requires_selection_connection_and_writable_destination() {
        let mut source = SftpPaneState::new(SftpPath::local("/source"));
        let destination = SftpPaneState::new(SftpPath::remote("/dest"));
        let disabled = transfer_action(
            PaneFocus::Local,
            &source,
            &destination,
            &SftpConnectionState::Ready,
        );
        assert!(!disabled.enabled);
        source
            .selected_paths
            .insert("local:/source/file".to_owned());
        let disconnected = transfer_action(
            PaneFocus::Local,
            &source,
            &destination,
            &SftpConnectionState::Disconnected {
                summary: "down".to_owned(),
                details: "down".to_owned(),
            },
        );
        assert!(!disconnected.enabled);
    }

    #[test]
    fn collision_resolution_wires_apply_to_all_scope() {
        assert_eq!(
            collision_scope(true),
            SftpCollisionScope::RemainingConflictsInBatch
        );
        assert_eq!(collision_scope(false), SftpCollisionScope::ThisItem);
    }

    #[test]
    fn keyboard_shortcut_mapping_matches_expected_commands() {
        let modifiers = egui::Modifiers {
            alt: false,
            ctrl: false,
            shift: false,
            mac_cmd: false,
            command: true,
        };
        assert!(keyboard_shortcut_matches(
            SftpShortcut::CopySelection,
            modifiers,
            Key::Enter
        ));
        assert!(keyboard_shortcut_matches(
            SftpShortcut::FocusPath,
            modifiers,
            Key::L
        ));
        assert!(keyboard_shortcut_matches(
            SftpShortcut::FocusFilter,
            modifiers,
            Key::F
        ));
    }

    #[test]
    fn visual_spec_matches_mockup_metrics() {
        assert_eq!(SFTP_VISUAL_SPEC.pane_header_height, 35.0);
        assert_eq!(SFTP_VISUAL_SPEC.pane_toolbar_height, 39.0);
        assert_eq!(SFTP_VISUAL_SPEC.pane_filter_row_height, 37.0);
        assert_eq!(SFTP_VISUAL_SPEC.pane_footer_height, 26.0);
        assert_eq!(SFTP_VISUAL_SPEC.toolbar_button_size, 28.0);
        assert_eq!(SFTP_VISUAL_SPEC.breadcrumb_height, 28.0);
        assert_eq!(SFTP_VISUAL_SPEC.filter_field_height, 26.0);
        assert_eq!(SFTP_VISUAL_SPEC.table_header_height, 27.0);
        assert_eq!(SFTP_VISUAL_SPEC.table_row_height, 31.0);
        assert_eq!(SFTP_VISUAL_SPEC.transfer_rail_width, 76.0);
        assert_eq!(SFTP_VISUAL_SPEC.transfer_button_width, 54.0);
        assert_eq!(SFTP_VISUAL_SPEC.transfer_button_height, 57.0);
    }

    #[test]
    fn typography_uses_monospace_for_paths_and_metadata() {
        for role in [
            SftpTextRole::PaneMeta,
            SftpTextRole::Breadcrumb,
            SftpTextRole::TableMetadata,
            SftpTextRole::DialogMeta,
        ] {
            assert_eq!(font_for_text_role(role).family, FontFamily::Monospace);
        }
        for role in [
            SftpTextRole::PaneLabel,
            SftpTextRole::Filter,
            SftpTextRole::TableBody,
            SftpTextRole::DialogBody,
        ] {
            assert_eq!(font_for_text_role(role).family, FontFamily::Proportional);
        }
    }

    #[test]
    fn toolbar_and_transfer_controls_keep_mockup_sizing() {
        let mut harness = Harness::builder().build_ui_state(
            move |ui, command: &mut Option<()>| {
                let _ = toolbar_icon_button(ui, SftpGlyph::Back, "Back Local folder");
                let _ = transfer_button(
                    ui,
                    SftpGlyph::TransferToRemote,
                    "Upload\nto Remote",
                    "Upload to Remote",
                    true,
                );
                *command = None;
            },
            None,
        );

        harness.run();

        let toolbar = harness.get_by_label("Back Local folder").rect();
        assert_eq!(toolbar.width(), 28.0);
        assert_eq!(toolbar.height(), 28.0);

        let transfer = harness.get_by_label("Upload to Remote").rect();
        assert_eq!(transfer.width(), 54.0);
        assert_eq!(transfer.height(), 57.0);
    }

    #[test]
    fn table_columns_follow_mockup_proportions() {
        let columns = sftp_table_columns(1000.0);
        assert_eq!(columns, [530.0, 150.0, 220.0, 100.0]);
    }

    #[test]
    fn file_type_labels_and_icons_follow_entry_semantics() {
        let folder = item("release", SftpEntryType::Directory, None);
        assert_eq!(item_type_label(&folder), "Folder");
        assert_eq!(item_glyph(&folder), SftpGlyph::Folder);

        let archive = item("festerm.tar.gz", SftpEntryType::File, Some(1));
        assert_eq!(item_type_label(&archive), "Archive");
        assert_eq!(item_glyph(&archive), SftpGlyph::Archive);

        let code = item("README.md", SftpEntryType::File, Some(1));
        assert_eq!(item_type_label(&code), "Text/Code");
        assert_eq!(item_glyph(&code), SftpGlyph::Code);
    }

    #[test]
    fn collision_decisions_keep_skip_between_replace_and_keep_both() {
        assert_eq!(
            collision_decision_order(),
            [
                SftpCollisionDecision::Replace,
                SftpCollisionDecision::Skip,
                SftpCollisionDecision::KeepBoth,
                SftpCollisionDecision::MergeFolders,
            ]
        );
    }
}
