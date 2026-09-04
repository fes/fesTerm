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

use eframe::egui::{self, Key, RichText, ScrollArea, Sense, TextEdit, Ui, WidgetInfo, WidgetType};
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
use festerm_ui_egui::theme;

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
            ui.columns(3, |columns| match self.pane_order {
                SftpPaneOrderPreference::LocalLeft => {
                    self.show_pane(&mut columns[0], PaneFocus::Local);
                    self.show_transfer_rail(&mut columns[1]);
                    self.show_pane(&mut columns[2], PaneFocus::Remote);
                }
                SftpPaneOrderPreference::RemoteLeft => {
                    self.show_pane(&mut columns[0], PaneFocus::Remote);
                    self.show_transfer_rail(&mut columns[1]);
                    self.show_pane(&mut columns[2], PaneFocus::Local);
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
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if matches!(
                    self.connection_state,
                    SftpConnectionState::Disconnected { .. }
                ) && ui.button("Reconnect").clicked()
                {
                    self.connection_state = SftpConnectionState::Connecting;
                    self.remote_pane.loading = true;
                    let _ = self.command_sender.send(WorkerCommand::Reconnect);
                }
            });
        });
        show_connection_status_banner(ui, &self.connection_state);
    }

    fn show_narrow(&mut self, ui: &mut Ui) {
        self.show_pane(ui, self.narrow_focus);
        ui.add_space(8.0);
        self.show_transfer_rail(ui);
    }

    fn show_pane(&mut self, ui: &mut Ui, focus: PaneFocus) {
        let identity = match focus {
            PaneFocus::Local => "LOCAL · This computer".to_owned(),
            PaneFocus::Remote => format!(
                "REMOTE · {}@{}",
                self.launch_target.username, self.launch_target.host
            ),
        };
        let mut request_back = false;
        let mut request_up = false;
        let mut request_home = false;
        let mut request_refresh = false;
        let mut request_open: Option<SftpDirectoryItem> = None;
        let mut request_navigate_text = false;
        let mut request_breadcrumb: Option<SftpPath> = None;
        let mut focused_this_pane = false;
        let remote_state = match focus {
            PaneFocus::Local => None,
            PaneFocus::Remote => Some(match &self.connection_state {
                SftpConnectionState::Ready => "Connected",
                SftpConnectionState::Connecting => "Connecting…",
                SftpConnectionState::AwaitingHostKey => "Trust required",
                SftpConnectionState::Failed { .. } => "Connection failed",
                SftpConnectionState::Disconnected { .. } => "Disconnected",
            }),
        };
        let frame = egui::Frame::group(ui.style()).fill(if self.focused_pane == focus {
            theme::SURFACE_TAB_ACTIVE
        } else {
            theme::SURFACE_TAB_INACTIVE
        });
        frame.show(ui, |ui| {
            let pane = pane_mut(self, focus);
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(identity).strong());
                    if let Some(state) = remote_state {
                        ui.label(RichText::new(state).small().color(theme::TEXT_SECONDARY));
                    }
                });
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if toolbar_button(ui, &format!("Back {} folder", focus.label())).clicked() {
                        request_back = true;
                    }
                    if toolbar_button(ui, &format!("Up {} folder", focus.label())).clicked() {
                        request_up = true;
                    }
                    if toolbar_button(ui, &format!("Home {} folder", focus.label())).clicked() {
                        request_home = true;
                    }
                    if toolbar_button(ui, &format!("Refresh {} folder", focus.label())).clicked() {
                        request_refresh = true;
                    }
                });
                ui.add_space(4.0);
                if pane.editing_path {
                    let response = ui.add(
                        TextEdit::singleline(&mut pane.path_text)
                            .id(path_field_id(focus))
                            .hint_text("Enter path")
                            .desired_width(f32::INFINITY),
                    );
                    if pane.path_focus_requested {
                        response.request_focus();
                        pane.path_focus_requested = false;
                    }
                    if response.lost_focus() && ui.input(|input| input.key_pressed(Key::Enter)) {
                        pane.editing_path = false;
                        request_navigate_text = true;
                    }
                } else {
                    let response = ui
                        .horizontal_wrapped(|ui| {
                            for (index, segment) in breadcrumb_segments(&pane.current_path)
                                .into_iter()
                                .enumerate()
                            {
                                if index > 0 {
                                    ui.label("›");
                                }
                                if segment.current {
                                    ui.label(RichText::new(segment.label).strong());
                                } else if ui.button(segment.label).clicked() {
                                    request_breadcrumb = Some(segment.path);
                                }
                            }
                        })
                        .response;
                    response.widget_info(|| {
                        WidgetInfo::labeled(
                            WidgetType::Button,
                            true,
                            format!("{} path {}", focus.label(), pane.current_path.display()),
                        )
                    });
                    if response.double_clicked() {
                        pane.editing_path = true;
                        pane.path_focus_requested = true;
                    }
                }
                ui.add_space(6.0);
                ui.label(format!(
                    "Filter this {} folder",
                    focus.label().to_lowercase()
                ));
                let mut filter_text = pane.filter.clone();
                let filter_response = ui.add(
                    TextEdit::singleline(&mut filter_text)
                        .id(filter_field_id(focus))
                        .desired_width(f32::INFINITY)
                        .hint_text("Type to filter this folder"),
                );
                if pane.filter_focus_requested {
                    filter_response.request_focus();
                    pane.filter_focus_requested = false;
                }
                if filter_response.changed() {
                    pane.set_filter(filter_text);
                }
                ui.add_space(6.0);
                if pane.loading {
                    ui.label(RichText::new("Loading…").color(theme::TEXT_SECONDARY));
                    return;
                }
                if let Some(error) = &pane.error {
                    ui.colored_label(theme::STATUS_ERROR, error);
                    if let Some(details) = &pane.details {
                        ui.label(RichText::new(details).small().color(theme::TEXT_MUTED));
                    }
                    ui.add_space(6.0);
                }
                let entries = pane.visible_entries().to_vec();
                if entries.is_empty() {
                    if pane
                        .snapshot
                        .as_ref()
                        .is_some_and(|snapshot| snapshot.entries.is_empty())
                    {
                        ui.label("This folder is empty.");
                    } else {
                        ui.label(format!("No items match \"{}\".", pane.filter));
                        if ui.button("Clear filter").clicked() {
                            pane.clear_filter();
                        }
                    }
                    return;
                }
                ui.horizontal(|ui| {
                    for (title, column) in [
                        ("Name", SftpSortColumn::Name),
                        ("Size", SftpSortColumn::Size),
                        ("Modified", SftpSortColumn::Modified),
                        ("Type", SftpSortColumn::Type),
                    ] {
                        let suffix = if pane.sort.column == column {
                            if pane.sort.descending {
                                " ↓"
                            } else {
                                " ↑"
                            }
                        } else {
                            ""
                        };
                        if ui.button(format!("{title}{suffix}")).clicked() {
                            pane.set_sort(column);
                        }
                    }
                });
                ui.separator();
                ScrollArea::vertical()
                    .id_salt(("sftp-pane", focus))
                    .show(ui, |ui| {
                        for item in entries.iter().cloned() {
                            let key = path_key(&item.path);
                            let selected = pane.selected_paths.contains(&key);
                            let response = ui
                                .horizontal(|ui| {
                                    let _ = ui.selectable_label(selected, &item.name);
                                    ui.add_space(8.0);
                                    ui.label(format_size(item.size));
                                    ui.add_space(8.0);
                                    ui.label(format_modified(item.modified_at));
                                    ui.add_space(8.0);
                                    ui.label(type_label(item.file_type));
                                })
                                .response
                                .interact(Sense::click());
                            response.widget_info(|| {
                                WidgetInfo::labeled(
                                    WidgetType::SelectableLabel,
                                    true,
                                    format!("{} {}", focus.label(), item.name),
                                )
                            });
                            if response.clicked() {
                                focused_this_pane = true;
                                if ui.input(|input| input.modifiers.shift) {
                                    let anchor_key = pane
                                        .selected_anchor
                                        .clone()
                                        .or_else(|| pane.cursor_path.clone())
                                        .unwrap_or_else(|| key.clone());
                                    let anchor_index = entries
                                        .iter()
                                        .position(|candidate| {
                                            path_key(&candidate.path) == anchor_key
                                        })
                                        .unwrap_or_default();
                                    let current_index = entries
                                        .iter()
                                        .position(|candidate| path_key(&candidate.path) == key)
                                        .unwrap_or(anchor_index);
                                    let start = anchor_index.min(current_index);
                                    let end = anchor_index.max(current_index);
                                    pane.selected_paths = entries[start..=end]
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
                            if response.double_clicked() {
                                request_open = Some(item.clone());
                            }
                        }
                    });
            });
        });
        if focused_this_pane {
            self.focused_pane = focus;
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
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(24.0);
                let upload_label = match self.pane_order {
                    SftpPaneOrderPreference::LocalLeft => "Upload to Remote",
                    SftpPaneOrderPreference::RemoteLeft => "Upload to Remote",
                };
                let download_label = match self.pane_order {
                    SftpPaneOrderPreference::LocalLeft => "Download to Local",
                    SftpPaneOrderPreference::RemoteLeft => "Download to Local",
                };
                let upload_button = ui.add_enabled(upload.enabled, egui::Button::new(upload_label));
                if let Some(reason) = &upload.reason {
                    upload_button.clone().on_disabled_hover_text(reason);
                }
                if upload_button.clicked() {
                    self.queue_transfer(PaneFocus::Local);
                }
                ui.add_space(12.0);
                let download_button =
                    ui.add_enabled(download.enabled, egui::Button::new(download_label));
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
        ui.add_space(10.0);
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Transfers").strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Clear finished").clicked() {
                        self.transfer_drawer.clear_finished();
                    }
                });
            });
            for item in &self.transfer_drawer.items {
                ui.separator();
                ui.horizontal(|ui| {
                    ui.label(format!(
                        "{} → {}",
                        item.request.source.display(),
                        item.destination
                            .as_ref()
                            .unwrap_or(&item.request.destination)
                            .display()
                    ));
                    ui.label(RichText::new(transfer_state_label(&item.state)).small());
                });
                if let Some(total) = item.total_bytes {
                    let progress = if total == 0 {
                        0.0
                    } else {
                        item.bytes_transferred as f32 / total as f32
                    };
                    ui.add(
                        egui::ProgressBar::new(progress)
                            .show_percentage()
                            .text(format!(
                                "{} / {}",
                                format_size(Some(item.bytes_transferred)),
                                format_size(Some(total))
                            )),
                    );
                }
                if let Some(details) = &item.details {
                    ui.label(RichText::new(details).small().color(theme::TEXT_MUTED));
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
                ui.set_width(520.0);
                ui.heading("Resolve file conflict");
                ui.label(format!("Source: {}", collision.source.path.display()));
                ui.label(format!(
                    "Destination: {}",
                    collision.destination.path.display()
                ));
                ui.label(format!(
                    "Source size: {} · Destination size: {}",
                    format_size(collision.source.size),
                    format_size(collision.destination.size)
                ));
                ui.label(format!(
                    "Source modified: {} · Destination modified: {}",
                    format_modified(collision.source.modified_at),
                    format_modified(collision.destination.modified_at)
                ));
                ui.add_space(8.0);
                if collision.can_apply_to_all {
                    ui.checkbox(
                        &mut dialog.apply_to_all,
                        "Apply to all conflicts in this batch",
                    );
                }
                ui.add_space(8.0);
                for candidate in [
                    SftpCollisionDecision::Skip,
                    SftpCollisionDecision::Replace,
                    SftpCollisionDecision::KeepBoth,
                    SftpCollisionDecision::MergeFolders,
                ] {
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
                ui.add_space(8.0);
                if ui.button("Cancel").clicked() {
                    close = true;
                }
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

fn toolbar_button(ui: &mut Ui, label: &str) -> egui::Response {
    let response = ui.add_sized(
        [56.0, 24.0],
        egui::Button::new(label.split_whitespace().next().unwrap_or(label)),
    );
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, label));
    response
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
        SftpSortColumn::Type => type_label(left.file_type)
            .cmp(type_label(right.file_type))
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

fn type_label(file_type: SftpEntryType) -> &'static str {
    match file_type {
        SftpEntryType::Directory => "Folder",
        SftpEntryType::File => "File",
        SftpEntryType::Symlink => "Symlink",
        SftpEntryType::Other => "Other",
    }
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
}
