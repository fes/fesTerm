//! Launcher and Settings application-surface presentation.
//!
//! These are thin, product-specific screens rather than terminal chrome
//! (`crates/festerm-ui-egui/src/chrome.rs` owns the chip row). They translate
//! user gestures into `AppCommand`s per `docs/application-command-model.md`
//! and own no session or tab policy themselves; `AppState::dispatch` remains
//! the single command-handling path.

use std::{
    sync::{mpsc, Arc, Mutex},
    thread,
};

use eframe::egui::{self, vec2, ScrollArea, Sense, Stroke, TextEdit, Ui, WidgetInfo, WidgetType};
use festerm_config::{
    Configuration, CredentialKind, PersistenceConfiguration, PersistenceProviderKind, Profile,
    RemoteProfileKind, SshPortForwardDirection, SshProfileConfiguration,
};
use festerm_session::{PasswordPrompt, TerminalSize};
use festerm_ssh::{
    HostIdentity, PersistenceProvider, PersistenceProviderProbeAvailability, ReconnectPolicy,
    RecoveryPolicy, SessionStrategy, SshAuthentication, SshCertificate, SshConnectionProfile,
    SshKeyPassphrase, SshPrivateKey, SshPrivateKeyError, SshSessionOptions,
};
use festerm_ui_egui::{
    controls::{self, ActionButtonRole},
    icon,
    icon::Icon,
    theme,
};

#[cfg(test)]
use festerm_config::SshPortForwardConfiguration;

use crate::port_forward_draft::PortForwardDraft as SshPortForwardDraft;
use crate::tabs::{
    AppCommand, NewProfileKind, PasswordToStore, PrivateKeyToStore, ProfileCredentialToStore,
    SshPortForwardDraftSeed, SshProfileDraftSeed, TabId,
};

mod destination;
mod path_autocomplete;
mod profiles;
use destination::{DestinationFields, DestinationPane, FieldOptions, FieldStyle, DEFAULT_SSH_PORT};
use path_autocomplete::{local_executable_field, local_working_directory_field};
pub(crate) use profiles::show_profiles;
use profiles::{profile_text_edit_with_id, serial_enum_combo};
mod settings;
use settings::toggle_switch;
pub(crate) use settings::{show_settings, SettingsViewModel};

/// One selectable launch option in the Launcher list: the fixed default
/// local shell, or a saved local/SSH profile.
enum LauncherItemKind<'a> {
    LocalDefault,
    NewSsh,
    NewSftp,
    NewSerial,
    /// Opens a Markdown workspace: the file picker, then a viewer tab.
    NewMarkdown,
    LocalProfile(&'a str),
    SshProfile(&'a str),
    SftpProfile(&'a str),
    SerialProfile(&'a str),
    ResumeSession(&'a festerm_sessiond::UnattachedSession),
    /// A locally running tmux or GNU screen session offered from its own
    /// quick-connect widget (feature request: local tmux/screen quick
    /// connect). `provider` is `Tmux` or `Screen`; `display_name` is the
    /// user-facing label (e.g. `main`); `match_key` is the exact string
    /// re-passed to `PersistenceConfiguration::new` to reattach this
    /// specific session -- identical to `display_name` for tmux, but
    /// screen's full `pid.name` identifier for GNU screen (see
    /// `multiplexer_sessions::MultiplexerSession`).
    ResumeMultiplexerSession(
        PersistenceProviderKind,
        &'a crate::multiplexer_sessions::MultiplexerSession,
    ),
}

struct LauncherItem<'a> {
    label: String,
    description: String,
    kind: LauncherItemKind<'a>,
    /// "Type" column text for a Saved Profiles row (`Local`, `SSH`, `SFTP`,
    /// `Serial`). Empty for launch cards and running-session rows, which are
    /// not rendered as table rows.
    type_label: &'static str,
    /// "Host / Path" column text for a Saved Profiles row: the one field that
    /// distinguishes two same-named profiles of the same type.
    location: String,
    /// "Last Used" column value, absent until the profile has been launched
    /// at least once on this installation.
    last_used_unix_seconds: Option<u64>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum ProfileTableKind {
    Local,
    Ssh,
    Sftp,
    Serial,
}

impl ProfileTableKind {
    fn type_label(self) -> &'static str {
        match self {
            Self::Local => "Local",
            Self::Ssh => "SSH",
            Self::Sftp => "SFTP",
            Self::Serial => "Serial",
        }
    }

    fn mark(self) -> (Icon, egui::Color32) {
        match self {
            Self::Local => (Icon::LocalTerminal, theme::ICON_SESSION_LOCAL),
            Self::Ssh => (Icon::SshRemote, theme::ICON_SESSION_REMOTE),
            Self::Sftp => (Icon::FileTransfer, theme::ICON_SESSION_FILE_TRANSFER),
            Self::Serial => (Icon::Serial, theme::ICON_SESSION_SERIAL),
        }
    }
}

struct ProfileTableItem {
    identifier: String,
    label: String,
    subtitle: Option<String>,
    kind: ProfileTableKind,
    location: String,
    last_used_unix_seconds: Option<u64>,
}

impl ProfileTableItem {
    fn connect_command(&self) -> AppCommand {
        match self.kind {
            ProfileTableKind::Local => AppCommand::StartConfiguredLocalProfile {
                profile_id: self.identifier.clone(),
            },
            ProfileTableKind::Ssh => AppCommand::StartConfiguredSshProfile {
                profile_id: self.identifier.clone(),
            },
            ProfileTableKind::Sftp => AppCommand::StartConfiguredSftpProfile {
                profile_id: self.identifier.clone(),
            },
            ProfileTableKind::Serial => AppCommand::StartConfiguredSerialProfile {
                profile_id: self.identifier.clone(),
            },
        }
    }

    fn launcher_crossover(&self) -> Option<(&'static str, AppCommand)> {
        match self.kind {
            ProfileTableKind::Ssh => Some((
                "Connect SFTP",
                AppCommand::StartConfiguredSftpProfile {
                    profile_id: self.identifier.clone(),
                },
            )),
            ProfileTableKind::Sftp => Some((
                "Connect SSH",
                AppCommand::StartConfiguredSshProfile {
                    profile_id: self.identifier.clone(),
                },
            )),
            _ => None,
        }
    }
}

impl<'a> LauncherItem<'a> {
    /// Builds an item that is not a Saved Profiles row, leaving the three
    /// table-only columns empty.
    fn untabulated(label: String, description: String, kind: LauncherItemKind<'a>) -> Self {
        Self {
            label,
            description,
            kind,
            type_label: "",
            location: String::new(),
            last_used_unix_seconds: None,
        }
    }
}

impl LauncherItem<'_> {
    fn profile_table_item(&self) -> ProfileTableItem {
        let kind = match self.kind {
            LauncherItemKind::LocalProfile(_) => ProfileTableKind::Local,
            LauncherItemKind::SshProfile(_) => ProfileTableKind::Ssh,
            LauncherItemKind::SftpProfile(_) => ProfileTableKind::Sftp,
            LauncherItemKind::SerialProfile(_) => ProfileTableKind::Serial,
            _ => unreachable!("only saved profiles are rendered by the profile table"),
        };
        ProfileTableItem {
            identifier: self
                .profile_id()
                .expect("saved profile rows carry a profile id")
                .to_owned(),
            label: self.label.clone(),
            subtitle: None,
            kind,
            location: self.location.clone(),
            last_used_unix_seconds: self.last_used_unix_seconds,
        }
    }

    fn profile_id(&self) -> Option<&str> {
        match self.kind {
            LauncherItemKind::LocalDefault
            | LauncherItemKind::NewSsh
            | LauncherItemKind::NewSftp
            | LauncherItemKind::NewSerial
            | LauncherItemKind::NewMarkdown
            | LauncherItemKind::ResumeSession(_)
            | LauncherItemKind::ResumeMultiplexerSession(..) => None,
            LauncherItemKind::LocalProfile(id)
            | LauncherItemKind::SshProfile(id)
            | LauncherItemKind::SftpProfile(id)
            | LauncherItemKind::SerialProfile(id) => Some(id),
        }
    }

    /// The session-type mark and its identity color.
    ///
    /// Type is carried by the silhouette first and the color second, so the
    /// distinction survives for a user who cannot separate the hues.
    fn mark(&self) -> (Icon, egui::Color32) {
        match self.kind {
            LauncherItemKind::LocalDefault
            | LauncherItemKind::LocalProfile(_)
            | LauncherItemKind::ResumeSession(_)
            | LauncherItemKind::ResumeMultiplexerSession(..) => {
                (Icon::LocalTerminal, theme::ICON_SESSION_LOCAL)
            }
            LauncherItemKind::NewSsh | LauncherItemKind::SshProfile(_) => {
                (Icon::SshRemote, theme::ICON_SESSION_REMOTE)
            }
            LauncherItemKind::NewSftp | LauncherItemKind::SftpProfile(_) => {
                (Icon::FileTransfer, theme::ICON_SESSION_FILE_TRANSFER)
            }
            LauncherItemKind::NewSerial | LauncherItemKind::SerialProfile(_) => {
                (Icon::Serial, theme::ICON_SESSION_SERIAL)
            }
            LauncherItemKind::NewMarkdown => (Icon::MarkdownDocument, theme::ICON_SESSION_MARKDOWN),
        }
    }

    fn command(&self) -> AppCommand {
        match self.kind {
            LauncherItemKind::LocalDefault => {
                unreachable!("the Local Shell item opens a launch form, not an AppCommand")
            }
            LauncherItemKind::NewSsh => {
                unreachable!("the New SSH Connection item opens the SSH form, not an AppCommand")
            }
            LauncherItemKind::NewSftp => {
                unreachable!("the New SFTP Connection item opens the SFTP form, not an AppCommand")
            }
            LauncherItemKind::NewSerial => {
                unreachable!(
                    "the New Serial Connection item opens the serial form, not an AppCommand"
                )
            }
            LauncherItemKind::NewMarkdown => AppCommand::OpenMarkdownWorkspace,
            LauncherItemKind::LocalProfile(profile_id) => AppCommand::StartConfiguredLocalProfile {
                profile_id: profile_id.to_owned(),
            },
            LauncherItemKind::SshProfile(profile_id) => AppCommand::StartConfiguredSshProfile {
                profile_id: profile_id.to_owned(),
            },
            LauncherItemKind::SftpProfile(profile_id) => AppCommand::StartConfiguredSftpProfile {
                profile_id: profile_id.to_owned(),
            },
            LauncherItemKind::SerialProfile(profile_id) => {
                AppCommand::StartConfiguredSerialProfile {
                    profile_id: profile_id.to_owned(),
                }
            }
            LauncherItemKind::ResumeSession(session) => AppCommand::ResumeUnattachedSession {
                session: session.clone(),
            },
            LauncherItemKind::ResumeMultiplexerSession(provider, session) => {
                AppCommand::ResumeMultiplexerSession {
                    provider,
                    session: session.clone(),
                }
            }
        }
    }
}

/// A bordered "Back" control that renders the app's own chevron glyph
/// (`icon::Icon::Back`) instead of a Unicode arrow character, since the
/// bundled font has no glyph for the arrow and would otherwise show a tofu
/// box (`docs/icon-system.md`'s painter-drawn icons avoid exactly this).
fn ssh_back_button(ui: &mut Ui) -> egui::Response {
    let text = "Back";
    let font = egui::FontId::proportional(13.0);
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_owned(), font.clone(), theme::TEXT_PRIMARY);
    let icon_size = 14.0;
    let spacing = 6.0;
    let padding = vec2(10.0, 6.0);
    let size = vec2(
        icon_size + spacing + galley.size().x + padding.x * 2.0,
        galley.size().y.max(icon_size) + padding.y * 2.0,
    );
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let hovered = response.hovered();
    ui.painter().rect(
        rect,
        6.0,
        if hovered {
            theme::SURFACE_TAB_ACTIVE
        } else {
            theme::SURFACE_TAB_INACTIVE
        },
        Stroke::new(1.0, theme::BORDER_SUBTLE),
        egui::StrokeKind::Inside,
    );
    let icon_rect = egui::Rect::from_min_size(
        rect.left_top() + vec2(padding.x, (rect.height() - icon_size) / 2.0),
        vec2(icon_size, icon_size),
    );
    icon::paint(ui.painter(), Icon::Back, icon_rect, theme::TEXT_PRIMARY);
    ui.painter().galley(
        rect.left_top() + vec2(padding.x + icon_size + spacing, padding.y),
        galley,
        theme::TEXT_PRIMARY,
    );
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, text));
    response
}

/// Authentication method selected for one transient SSH connection attempt.
#[derive(Clone, Copy, Default, Eq, PartialEq)]
enum SshAuthenticationMethod {
    #[default]
    Password,
    PrivateKey,
    Certificate,
}

#[derive(Clone)]
struct DurableSessionDraft {
    enabled: bool,
    provider: PersistenceProviderKind,
    /// Set once the user explicitly picks a durable-session provider. Remote
    /// tmux-detection defaults may fill `provider` only while this remains
    /// false.
    provider_touched: bool,
    session_name: String,
    /// Set once the user manually edits the session name field directly.
    /// Until then, the session name auto-fills from the profile name as
    /// the user types it, so most profiles never need a separate manual
    /// entry.
    session_name_touched: bool,
    automatic_recovery: bool,
    /// The provider assigned when a Local profile's durable-session toggle
    /// is first switched on, detected once at composition-root time from
    /// what's actually available on the local `PATH`
    /// (see [`PersistenceProviderKind::default_for_local_session`]).
    /// Unused for [`DurableSessionTarget::Remote`], which detects its
    /// default separately via [`Self::apply_detected_remote_provider_default`].
    local_default_provider: PersistenceProviderKind,
}

impl Default for DurableSessionDraft {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: PersistenceProviderKind::Tmux,
            provider_touched: false,
            session_name: "main".to_owned(),
            session_name_touched: false,
            automatic_recovery: false,
            local_default_provider: PersistenceProviderKind::FestermSessiond,
        }
    }
}

impl DurableSessionDraft {
    fn from_persistence(persistence: Option<&PersistenceConfiguration>) -> Self {
        match persistence {
            Some(persistence) => Self {
                enabled: true,
                provider: persistence.provider(),
                provider_touched: true,
                session_name: persistence.session_name().to_owned(),
                // An existing profile's session name was explicitly chosen
                // (by the user or a prior save), so don't let subsequent
                // profile-name edits silently overwrite it.
                session_name_touched: true,
                automatic_recovery: false,
                local_default_provider: PersistenceProviderKind::FestermSessiond,
            },
            None => Self::default(),
        }
    }

    /// Auto-fills the session name from the profile name as the user types
    /// it, unless the session name has already been manually edited.
    ///
    /// Called whenever the profile-name field changes; sanitizes the
    /// profile name to the character set [`PersistentSessionName`] accepts
    /// (lowercase ASCII alphanumerics, `-`, `_`, `.`).
    fn sync_session_name_from_profile_name(&mut self, profile_name: &str) {
        if self.session_name_touched {
            return;
        }
        self.session_name = sanitize_session_name_from_profile_name(profile_name);
    }

    fn persistence(&self) -> Result<Option<PersistenceConfiguration>, String> {
        if !self.enabled {
            return Ok(None);
        }
        let persistence = PersistenceConfiguration::new(self.provider, self.session_name.trim());
        persistence
            .validate_session_name()
            .map_err(|error| error.to_string())?;
        Ok(Some(persistence))
    }

    fn session_options(&self) -> Result<SshSessionOptions, String> {
        let strategy = self
            .persistence()?
            .as_ref()
            .map(PersistenceConfiguration::to_session_strategy)
            .transpose()
            .map_err(|error| error.to_string())?
            .unwrap_or(SessionStrategy::PlainShell);
        if self.enabled && self.automatic_recovery {
            let recovery = RecoveryPolicy::Automatic(ReconnectPolicy::default_automatic());
            return SshSessionOptions::with_recovery_policy(strategy, recovery)
                .map_err(|error| error.to_string());
        }
        Ok(SshSessionOptions::manual_recovery(strategy))
    }

    fn select_provider(&mut self, provider: PersistenceProviderKind) {
        self.provider = provider;
        self.provider_touched = true;
    }

    fn apply_detected_remote_provider_default(&mut self, detection: RemoteTmuxDetectionResult) {
        if self.provider_touched {
            return;
        }
        self.provider = remote_provider_from_tmux_detection(detection);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RemoteTmuxDetectionResult {
    Detected,
    NotDetected,
}

const fn remote_provider_from_tmux_detection(
    detection: RemoteTmuxDetectionResult,
) -> PersistenceProviderKind {
    match detection {
        RemoteTmuxDetectionResult::Detected => PersistenceProviderKind::Tmux,
        RemoteTmuxDetectionResult::NotDetected => PersistenceProviderKind::Screen,
    }
}

#[derive(Clone, Eq, PartialEq)]
enum RemoteTmuxProbeAuthKey {
    Password(String),
    PrivateKey {
        private_key: String,
        key_passphrase: String,
    },
    Certificate {
        private_key: String,
        key_passphrase: String,
        certificate: String,
    },
}

#[derive(Clone, Eq, PartialEq)]
struct RemoteTmuxProbeKey {
    host: String,
    port: u16,
    username: String,
    known_host_fingerprint: String,
    authentication: RemoteTmuxProbeAuthKey,
}

struct RemoteTmuxProbeRequest {
    key: RemoteTmuxProbeKey,
    profile: SshConnectionProfile,
    authentication: SshAuthentication,
    known_host_fingerprint: String,
}

#[derive(Clone)]
struct RemoteTmuxProbeCompletion {
    key: RemoteTmuxProbeKey,
    availability: PersistenceProviderProbeAvailability,
}

#[derive(Clone)]
struct PendingRemoteTmuxProbe {
    key: RemoteTmuxProbeKey,
    receiver: Arc<Mutex<mpsc::Receiver<RemoteTmuxProbeCompletion>>>,
}

#[derive(Clone, Default)]
struct RemoteTmuxProbeState {
    pending: Option<PendingRemoteTmuxProbe>,
    completed: Option<RemoteTmuxProbeCompletion>,
}

impl RemoteTmuxProbeState {
    fn sync(
        &mut self,
        context: &egui::Context,
        durable_session: &mut DurableSessionDraft,
        request: Option<RemoteTmuxProbeRequest>,
    ) {
        if !durable_session.enabled || durable_session.provider_touched {
            return;
        }

        if let Some(completion) = self.poll_pending_probe() {
            self.completed = Some(completion);
        }

        if let (Some(request), Some(completion)) = (request.as_ref(), self.completed.as_ref()) {
            if completion.key == request.key {
                if let Some(detection) =
                    remote_tmux_detection_from_probe_availability(completion.availability)
                {
                    durable_session.apply_detected_remote_provider_default(detection);
                }
                return;
            }
        }

        let Some(request) = request else {
            return;
        };
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.key == request.key)
        {
            return;
        }
        if self
            .completed
            .as_ref()
            .is_some_and(|completion| completion.key == request.key)
        {
            return;
        }

        let repaint = context.clone();
        let key = request.key.clone();
        let (sender, receiver) = mpsc::sync_channel(1);
        match thread::Builder::new()
            .name("festerm-remote-tmux-probe".to_owned())
            .spawn(move || {
                let availability = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map(|runtime| {
                        runtime.block_on(festerm_ssh::probe_remote_persistence_provider(
                            request.profile,
                            request.authentication,
                            Some(request.known_host_fingerprint),
                            PersistenceProvider::Tmux,
                        ))
                    })
                    .unwrap_or(PersistenceProviderProbeAvailability::Indeterminate);
                let _ = sender.send(RemoteTmuxProbeCompletion { key, availability });
                repaint.request_repaint();
            }) {
            Ok(_) => {
                self.pending = Some(PendingRemoteTmuxProbe {
                    key: request.key,
                    receiver: Arc::new(Mutex::new(receiver)),
                });
            }
            Err(_) => {
                self.completed = Some(RemoteTmuxProbeCompletion {
                    key: request.key,
                    availability: PersistenceProviderProbeAvailability::Indeterminate,
                });
            }
        }
    }

    fn poll_pending_probe(&mut self) -> Option<RemoteTmuxProbeCompletion> {
        let pending = self.pending.as_ref()?;
        let key = pending.key.clone();
        let result = {
            let receiver = pending
                .receiver
                .lock()
                .expect("remote tmux probe receiver lock is not poisoned");
            receiver.try_recv()
        };
        match result {
            Ok(completion) => {
                self.pending = None;
                Some(completion)
            }
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => {
                self.pending = None;
                Some(RemoteTmuxProbeCompletion {
                    key,
                    availability: PersistenceProviderProbeAvailability::Indeterminate,
                })
            }
        }
    }
}

fn remote_tmux_detection_from_probe_availability(
    availability: PersistenceProviderProbeAvailability,
) -> Option<RemoteTmuxDetectionResult> {
    match availability {
        PersistenceProviderProbeAvailability::Available => {
            Some(RemoteTmuxDetectionResult::Detected)
        }
        PersistenceProviderProbeAvailability::Unavailable => {
            Some(RemoteTmuxDetectionResult::NotDetected)
        }
        PersistenceProviderProbeAvailability::Indeterminate => None,
    }
}

/// Sanitizes a profile name into a candidate durable-session name:
/// lowercased, with any character outside
/// [`PersistentSessionName`]'s accepted set (ASCII alphanumerics, `-`,
/// `_`, `.`) collapsed to a single `-`, leading/trailing `-` trimmed, and
/// truncated to the name's maximum length.
fn sanitize_session_name_from_profile_name(profile_name: &str) -> String {
    const MAXIMUM_BYTES: usize = 64;
    let mut sanitized = String::with_capacity(profile_name.len());
    let mut last_was_separator = false;
    for character in profile_name.trim().chars() {
        let lowered = character.to_ascii_lowercase();
        if lowered.is_ascii_alphanumeric() || matches!(lowered, '-' | '_' | '.') {
            sanitized.push(lowered);
            last_was_separator = false;
        } else if !last_was_separator && !sanitized.is_empty() {
            sanitized.push('-');
            last_was_separator = true;
        }
    }
    while sanitized.ends_with('-') {
        sanitized.pop();
    }
    sanitized.truncate(MAXIMUM_BYTES);
    while !sanitized.is_char_boundary(sanitized.len()) {
        sanitized.pop();
    }
    sanitized
}

/// Per-launcher, transient SSH authentication form state.
///
/// This belongs only to egui's temporary per-tab data. In particular, it is
/// never a profile, workspace, diagnostic, or application-state field.
#[derive(Clone)]
struct SshLauncherForm {
    host: String,
    port: String,
    username: String,
    /// The legacy SFTP quick-connect entry point: a single `user@host[:port]`
    /// field parsed by `parse_quick_connect`.
    quick_connect: String,
    /// Whether the SSH launcher's Advanced settings disclosure is open. The
    /// SFTP sibling still reuses this field for its legacy quick/full toggle.
    advanced_open: bool,
    /// Which notation the shared Connection pane is currently showing: the
    /// single `user@host:port` field (`false`) or the separate
    /// Username/Host/Port fields (`true`). Exactly one is ever visible, and
    /// the two are kept in step by `sync_*` when the user switches.
    ///
    /// Deliberately *not* `advanced_open`, which now means the port-forward
    /// disclosure rather than this notation choice.
    destination_expanded: bool,
    authentication_method: SshAuthenticationMethod,
    password: String,
    private_key: String,
    key_passphrase: String,
    certificate: String,
    saved_profile_id: Option<String>,
    saved_profile_has_credential: bool,
    saved_profile_credential_kind: Option<CredentialKind>,
    durable_session: DurableSessionDraft,
    remote_tmux_probe: Box<RemoteTmuxProbeState>,
    port_forwards: Vec<SshPortForwardDraft>,
    sftp_gui_mode: bool,
    remember_password: bool,
    feedback: Option<String>,
    /// Set whenever the form is (re)opened so the Username field can claim
    /// initial keyboard focus once, without re-stealing it on every frame.
    focus_username: bool,
    /// One-shot request to focus the Password field, armed when a form is
    /// prefilled from a saved/restored profile. In that flow the destination
    /// is already known and the credential is the only thing left to type,
    /// so focus belongs on the secret rather than the first field in tab
    /// order. One-shot because a per-frame `request_focus` traps focus.
    focus_password: bool,
}

impl Default for SshLauncherForm {
    /// Port starts prefilled with the actual default (`"22"`) rather than
    /// an empty field with "(default: 22)" wording, so the box always shows
    /// the value that will actually be used.
    ///
    /// The destination pane starts on the single `user@host:port` field:
    /// it is the fastest way to state a destination, and the separate
    /// fields are one click away.
    fn default() -> Self {
        Self {
            host: String::new(),
            port: Self::DEFAULT_PORT.to_string(),
            username: String::new(),
            quick_connect: String::new(),
            advanced_open: false,
            destination_expanded: false,
            authentication_method: SshAuthenticationMethod::default(),
            password: String::new(),
            private_key: String::new(),
            key_passphrase: String::new(),
            certificate: String::new(),
            saved_profile_id: None,
            saved_profile_has_credential: false,
            saved_profile_credential_kind: None,
            durable_session: DurableSessionDraft::default(),
            remote_tmux_probe: Box::default(),
            port_forwards: Vec::new(),
            sftp_gui_mode: true,
            remember_password: false,
            feedback: None,
            focus_username: false,
            focus_password: false,
        }
    }
}

impl SshLauncherForm {
    const DEFAULT_PORT: u16 = DEFAULT_SSH_PORT;

    fn destination(&mut self) -> DestinationFields<'_> {
        DestinationFields {
            username: &mut self.username,
            host: &mut self.host,
            port: &mut self.port,
            quick_connect: &mut self.quick_connect,
            expanded: &mut self.destination_expanded,
        }
    }

    /// Ordinary SSH sessions have no durable-session provider, so automatic
    /// recovery is not valid for them (ADR 0018); every reconnect is the
    /// user-initiated action available from the session Inspector once
    /// connected. A persistent session only gets automatic recovery when
    /// `self.durable_session.automatic_recovery` is explicitly set.
    fn session_options(&self) -> Result<SshSessionOptions, String> {
        let options = self.durable_session.session_options()?;
        let port_forwards = self
            .port_forwards
            .iter()
            .map(SshPortForwardDraft::build)
            .collect::<Result<Vec<_>, _>>()?;
        options
            .with_profile_port_forwards(port_forwards.iter())
            .map_err(|error| error.to_string())
    }

    fn prefill_from_profile(&mut self, profile: &SshProfileConfiguration) {
        self.host = profile.host().to_owned();
        self.port = profile.port().to_string();
        self.username = profile.username().to_owned();
        self.durable_session = DurableSessionDraft::from_persistence(profile.persistence());
        self.port_forwards = profile
            .port_forwards()
            .iter()
            .map(SshPortForwardDraft::from_configuration)
            .collect();
        self.advanced_open = true;
        // Compose the shorthand too, so the destination reads correctly in
        // whichever notation the user has the pane set to.
        self.sync_quick_connect_from_advanced();
    }

    fn sync_remote_durable_provider_default(
        &mut self,
        context: &egui::Context,
        configuration: Option<&Configuration>,
    ) {
        let request = self.remote_tmux_probe_request(configuration);
        self.remote_tmux_probe
            .sync(context, &mut self.durable_session, request);
    }

    fn prefill_saved_profile(&mut self, profile: &SshProfileConfiguration) {
        self.prefill_from_profile(profile);
        self.saved_profile_id = Some(profile.identifier().to_owned());
        self.saved_profile_has_credential = profile.credential_reference().is_some();
        self.saved_profile_credential_kind = profile
            .credential_reference()
            .is_some()
            .then_some(profile.credential_kind());
        self.focus_password = true;
    }

    fn prefill_restored_sftp_profile(&mut self, profile: &SshProfileConfiguration) {
        self.prefill_saved_profile(profile);
        self.sftp_gui_mode = false;
    }

    fn connection_profile(&self) -> Result<SshConnectionProfile, String> {
        let port = if self.port.trim().is_empty() {
            Self::DEFAULT_PORT
        } else {
            self.port
                .trim()
                .parse::<u16>()
                .map_err(|_| "SSH port must be a number between 1 and 65535".to_owned())?
        };
        let identity = HostIdentity::new(&self.host, port).map_err(|error| error.to_string())?;
        let initial_size =
            TerminalSize::new(80, 24).expect("the launcher default terminal size is valid");
        SshConnectionProfile::new(
            identity,
            self.username.clone(),
            SshConnectionProfile::DEFAULT_TERMINAL_TYPE,
            initial_size,
        )
        .map_err(|error| error.to_string())
    }

    fn remote_tmux_probe_request(
        &self,
        configuration: Option<&Configuration>,
    ) -> Option<RemoteTmuxProbeRequest> {
        let probe_form = self.clone();
        let profile = probe_form.connection_profile().ok()?;
        let known_host_fingerprint = configuration?
            .known_host_fingerprint(profile.identity().host(), profile.identity().port())?
            .to_owned();
        let (authentication, authentication_key) = match probe_form.authentication_method {
            SshAuthenticationMethod::Password => {
                if probe_form.password.is_empty() {
                    return None;
                }
                (
                    SshAuthentication::password(probe_form.password.clone()),
                    RemoteTmuxProbeAuthKey::Password(probe_form.password),
                )
            }
            SshAuthenticationMethod::PrivateKey => {
                if probe_form.private_key.is_empty() {
                    return None;
                }
                (
                    Self::parse_private_key(
                        probe_form.private_key.clone(),
                        probe_form.key_passphrase.clone(),
                    )
                    .ok()?,
                    RemoteTmuxProbeAuthKey::PrivateKey {
                        private_key: probe_form.private_key,
                        key_passphrase: probe_form.key_passphrase,
                    },
                )
            }
            SshAuthenticationMethod::Certificate => {
                if probe_form.private_key.is_empty() || probe_form.certificate.is_empty() {
                    return None;
                }
                (
                    Self::parse_certificate(
                        probe_form.private_key.clone(),
                        probe_form.key_passphrase.clone(),
                        probe_form.certificate.clone(),
                    )
                    .ok()?,
                    RemoteTmuxProbeAuthKey::Certificate {
                        private_key: probe_form.private_key,
                        key_passphrase: probe_form.key_passphrase,
                        certificate: probe_form.certificate,
                    },
                )
            }
        };
        let key = RemoteTmuxProbeKey {
            host: profile.identity().host().to_owned(),
            port: profile.identity().port(),
            username: profile.username().to_owned(),
            known_host_fingerprint: known_host_fingerprint.clone(),
            authentication: authentication_key,
        };
        Some(RemoteTmuxProbeRequest {
            key,
            profile,
            authentication,
            known_host_fingerprint,
        })
    }

    fn profile_draft_seed(&self) -> SshProfileDraftSeed {
        let trimmed_host = self.host.trim();
        let trimmed_username = self.username.trim();
        let name = if trimmed_host.is_empty() {
            "SSH profile".to_owned()
        } else {
            trimmed_host.to_owned()
        };
        SshProfileDraftSeed {
            name,
            host: trimmed_host.to_owned(),
            port: self.port.trim().to_owned(),
            username: trimmed_username.to_owned(),
            port_forwards: self
                .port_forwards
                .iter()
                .map(|forward| SshPortForwardDraftSeed {
                    direction: forward.direction,
                    bind_host: forward.bind_host.clone(),
                    bind_port: forward.bind_port.clone(),
                    destination_host: forward.destination_host.clone(),
                    destination_port: forward.destination_port.clone(),
                })
                .collect(),
            durable_session_enabled: self.durable_session.enabled,
            durable_session_provider: self.durable_session.provider,
            durable_session_name: self.durable_session.session_name.clone(),
        }
    }

    /// Converts the transient form into the application's typed SSH command.
    ///
    /// Taking every secret first ensures each submit attempt removes it from UI
    /// state, including attempts rejected by non-secret input validation.
    fn submit(&mut self) -> Result<AppCommand, String> {
        let password = std::mem::take(&mut self.password);
        let private_key = std::mem::take(&mut self.private_key);
        let key_passphrase = std::mem::take(&mut self.key_passphrase);
        let certificate = std::mem::take(&mut self.certificate);
        let profile = self.connection_profile()?;
        let options = self.session_options()?;

        match self.authentication_method {
            SshAuthenticationMethod::Password
                if self.remember_password
                    && self.saved_profile_id.is_some()
                    && !password.is_empty() =>
            {
                Ok(AppCommand::StoreSshPassword {
                    profile_id: self
                        .saved_profile_id
                        .clone()
                        .expect("saved profile was checked above"),
                    password: PasswordToStore::new(password),
                    options,
                })
            }
            SshAuthenticationMethod::Password if password.is_empty() => {
                Ok(AppCommand::StartSshSession {
                    profile,
                    authentication: SshAuthentication::interactive(),
                    options,
                })
            }
            SshAuthenticationMethod::Password => Ok(AppCommand::StartSshSession {
                profile,
                authentication: SshAuthentication::password(password),
                options,
            }),
            SshAuthenticationMethod::PrivateKey => Ok(AppCommand::StartSshSession {
                profile,
                authentication: Self::parse_private_key(private_key, key_passphrase)?,
                options,
            }),
            SshAuthenticationMethod::Certificate => Ok(AppCommand::StartSshSession {
                profile,
                authentication: Self::parse_certificate(private_key, key_passphrase, certificate)?,
                options,
            }),
        }
    }

    fn submit_stored_credential(&self) -> Result<AppCommand, String> {
        let profile_id = self
            .saved_profile_id
            .clone()
            .ok_or_else(|| "A saved profile is required for stored credentials.".to_owned())?;
        Ok(AppCommand::StartStoredPasswordSshProfile {
            profile_id,
            options: self.session_options()?,
        })
    }

    /// Converts the transient form into the application's typed SFTP command.
    fn submit_sftp(&mut self) -> Result<AppCommand, String> {
        if self.sftp_gui_mode {
            let profile = self.connection_profile()?;
            return Ok(AppCommand::OpenSftpFileManager {
                target: crate::sftp_file_manager::SftpFileManagerLaunchTarget {
                    label: format!("{}@{}", self.username, self.host),
                    username: self.username.clone(),
                    host: profile.identity().host().to_owned(),
                    port: profile.identity().port(),
                    profile_id: None,
                    stored_credential_kind: None,
                    known_host_persisted: false,
                },
            });
        }
        let password = std::mem::take(&mut self.password);
        let private_key = std::mem::take(&mut self.private_key);
        let key_passphrase = std::mem::take(&mut self.key_passphrase);
        let certificate = std::mem::take(&mut self.certificate);
        let profile = self.connection_profile()?;

        match self.authentication_method {
            SshAuthenticationMethod::Password
                if self.remember_password
                    && self.saved_profile_id.is_some()
                    && !password.is_empty() =>
            {
                Ok(AppCommand::StoreSftpPassword {
                    profile_id: self
                        .saved_profile_id
                        .clone()
                        .expect("saved profile was checked above"),
                    password: PasswordToStore::new(password),
                })
            }
            SshAuthenticationMethod::Password if password.is_empty() => {
                Ok(AppCommand::StartSftpSession {
                    profile,
                    authentication: SshAuthentication::interactive(),
                })
            }
            SshAuthenticationMethod::Password => Ok(AppCommand::StartSftpSession {
                profile,
                authentication: SshAuthentication::password(password),
            }),
            SshAuthenticationMethod::PrivateKey => Ok(AppCommand::StartSftpSession {
                profile,
                authentication: Self::parse_private_key(private_key, key_passphrase)?,
            }),
            SshAuthenticationMethod::Certificate => Ok(AppCommand::StartSftpSession {
                profile,
                authentication: Self::parse_certificate(private_key, key_passphrase, certificate)?,
            }),
        }
    }

    /// Parses `quick_connect` ("user@host" or "user@host:port") into the
    /// same `host`/`port`/`username` fields the advanced form edits
    /// directly, then submits through the same `submit()` path with no
    /// password. That empty password is exactly what routes the connection
    /// to the in-terminal password prompt (see `submit()`'s Password
    /// branch) rather than attempting to connect with no credential.
    #[cfg(test)]
    fn submit_quick_connect(&mut self) -> Result<AppCommand, String> {
        self.parse_quick_connect()?;
        self.password.clear();
        self.authentication_method = SshAuthenticationMethod::Password;
        self.submit()
    }

    fn submit_quick_connect_sftp(&mut self) -> Result<AppCommand, String> {
        self.parse_quick_connect()?;
        self.password.clear();
        self.authentication_method = SshAuthenticationMethod::Password;
        self.submit_sftp()
    }

    /// See `quick_connect`'s doc comment for the notation this accepts.
    fn parse_quick_connect(&mut self) -> Result<(), String> {
        self.destination().parse_quick_connect()
    }

    /// Best-effort version of `parse_quick_connect` for switching notation.
    fn sync_advanced_from_quick_connect(&mut self) {
        self.destination().sync_expanded_from_quick_connect();
    }

    /// Inverse of `sync_advanced_from_quick_connect`.
    fn sync_quick_connect_from_advanced(&mut self) {
        self.destination().sync_quick_connect_from_expanded();
    }

    /// Reveals the surface's advanced settings, carrying forward whatever
    /// destination the user already typed and clearing any stale feedback
    /// from the surface it replaces.
    fn open_advanced_settings(&mut self) {
        self.sync_advanced_from_quick_connect();
        self.feedback = None;
        self.advanced_open = true;
        self.focus_username = true;
    }

    /// Inverse of `open_advanced_settings`.
    fn close_advanced_settings(&mut self) {
        self.sync_quick_connect_from_advanced();
        self.feedback = None;
        self.advanced_open = false;
    }

    /// Parses an in-memory key while retaining neither its text nor passphrase.
    ///
    /// Encrypted OpenSSH keys use the SSH crate's explicit passphrase API; the
    /// parser distinguishes that case without trying to persist or log either
    /// source string.
    fn parse_private_key_material(
        private_key: &str,
        key_passphrase: String,
    ) -> Result<SshPrivateKey, String> {
        match SshPrivateKey::from_openssh(private_key) {
            Ok(private_key) if key_passphrase.is_empty() => Ok(private_key),
            Ok(_) => Err("SSH private key is unencrypted; clear the passphrase".to_owned()),
            Err(SshPrivateKeyError::Encrypted) => SshPrivateKey::from_encrypted_openssh(
                private_key,
                SshKeyPassphrase::new(key_passphrase),
            )
            .map_err(|error| error.to_string()),
            Err(error) => Err(error.to_string()),
        }
    }

    fn parse_private_key(
        private_key: String,
        key_passphrase: String,
    ) -> Result<SshAuthentication, String> {
        Self::parse_private_key_material(&private_key, key_passphrase)
            .map(SshAuthentication::public_key)
    }

    fn parse_certificate(
        private_key: String,
        key_passphrase: String,
        certificate: String,
    ) -> Result<SshAuthentication, String> {
        let private_key = Self::parse_private_key_material(&private_key, key_passphrase)?;
        let certificate =
            SshCertificate::from_openssh(&certificate).map_err(|error| error.to_string())?;
        Ok(SshAuthentication::certificate(private_key, certificate))
    }
}

#[derive(Clone)]
struct SerialLauncherForm {
    device: String,
    baud_rate: String,
    data_bits: festerm_config::SerialDataBits,
    parity: festerm_config::SerialParity,
    stop_bits: festerm_config::SerialStopBits,
    flow_control: festerm_config::SerialFlowControl,
    discovered_ports: Vec<festerm_serial::DiscoveredPort>,
    feedback: Option<String>,
}

impl Default for SerialLauncherForm {
    fn default() -> Self {
        let discovered_ports = festerm_serial::discover_ports().unwrap_or_default();
        Self {
            device: String::new(),
            baud_rate: "115200".to_owned(),
            data_bits: festerm_config::SerialDataBits::Eight,
            parity: festerm_config::SerialParity::None,
            stop_bits: festerm_config::SerialStopBits::One,
            flow_control: festerm_config::SerialFlowControl::None,
            discovered_ports,
            feedback: None,
        }
    }
}

#[derive(Clone, Default)]
struct LocalLauncherForm {
    executable: String,
    arguments: String,
    working_directory: String,
    feedback: Option<String>,
    initialized: bool,
    focus_working_directory: bool,
}

impl LocalLauncherForm {
    fn initialize_default(&mut self, prefer_powershell: bool) {
        if self.initialized {
            return;
        }
        self.initialized = true;
        match festerm_pty::default_local_profile_with_powershell_preference(prefer_powershell) {
            Ok(profile) => {
                self.executable = profile.executable().display().to_string();
                self.arguments = profile
                    .arguments()
                    .iter()
                    .map(|argument| argument.to_string_lossy())
                    .collect::<Vec<_>>()
                    .join(" ");
            }
            Err(error) => self.feedback = Some(error.to_string()),
        }
    }

    fn submit(&mut self) -> Result<AppCommand, String> {
        let executable = self.executable.trim();
        if executable.is_empty() {
            return Err("Enter a non-empty executable.".to_owned());
        }
        let mut profile = festerm_pty::LocalProfile::new(executable)
            .with_arguments(self.arguments.split_whitespace());
        if !self.working_directory.trim().is_empty() {
            profile = profile.with_working_directory(self.working_directory.trim());
        }
        profile.validate().map_err(|error| error.to_string())?;
        Ok(AppCommand::StartLocalSessionWithProfile { profile })
    }
}

#[derive(Clone)]
struct LauncherState {
    selected: usize,
    local_open: bool,
    local: LocalLauncherForm,
    ssh_open: bool,
    ssh: SshLauncherForm,
    ssh_profile_prefilled: bool,
    sftp_open: bool,
    sftp: SshLauncherForm,
    sftp_profile_prefilled: bool,
    serial_open: bool,
    serial: SerialLauncherForm,
    /// Filters the Saved Profiles table by name, type, or host/path.
    profile_search: String,
    profile_sort: ProfileSortOrder,
    /// A launch card's request to open an in-tab connection form, applied
    /// after rendering so the card row does not need mutable access to the
    /// form state it would otherwise have to borrow mid-layout.
    pending_form: Option<LauncherForm>,
    festerm_group_expanded: bool,
    tmux_group_expanded: bool,
    screen_group_expanded: bool,
}

impl Default for LauncherState {
    /// Session groups start expanded: the panel exists to show what is
    /// running, and a collapsed default would hide the whole point of it
    /// behind three extra clicks.
    fn default() -> Self {
        Self {
            selected: 0,
            local_open: false,
            local: LocalLauncherForm::default(),
            ssh_open: false,
            ssh: SshLauncherForm::default(),
            ssh_profile_prefilled: false,
            sftp_open: false,
            sftp: SshLauncherForm::default(),
            sftp_profile_prefilled: false,
            serial_open: false,
            serial: SerialLauncherForm::default(),
            profile_search: String::new(),
            profile_sort: ProfileSortOrder::default(),
            pending_form: None,
            festerm_group_expanded: true,
            tmux_group_expanded: true,
            screen_group_expanded: true,
        }
    }
}

fn launcher_state_id(tab_id: TabId) -> egui::Id {
    egui::Id::new(("launcher_state", tab_id))
}

fn ssh_field_id(ui: &Ui, tab_id: TabId, field: &'static str) -> egui::Id {
    ui.make_persistent_id(("launcher_ssh", tab_id, field))
}

fn show_local_form(ui: &mut Ui, tab_id: TabId, form: &mut LocalLauncherForm) -> Option<AppCommand> {
    ui.add_space(16.0);
    let mut result = None;
    egui::Frame::new()
        .fill(theme::SURFACE_TAB_INACTIVE)
        .stroke(Stroke::new(1.0, theme::BORDER_SUBTLE))
        .corner_radius(8.0)
        .inner_margin(egui::Margin::same(16))
        .show(ui, |ui| {
            ui.set_width(340.0);
            local_executable_field(
                ui,
                launcher_state_id(tab_id).with("local_executable_autocomplete"),
                &mut form.executable,
            );
            profile_text_edit_with_id(
                ui,
                tab_id,
                "local_arguments",
                "Arguments (space-separated)",
                &mut form.arguments,
            );
            let working_directory = local_working_directory_field(
                ui,
                launcher_state_id(tab_id).with("local_working_directory_autocomplete"),
                &mut form.working_directory,
            );
            if std::mem::take(&mut form.focus_working_directory) {
                working_directory.request_focus();
            }
            let submit_from_directory = working_directory.lost_focus()
                && ui.input(|input| input.key_pressed(egui::Key::Enter));
            ui.add_space(12.0);
            if let Some(feedback) = &form.feedback {
                ui.colored_label(theme::STATUS_ERROR, feedback);
                ui.add_space(8.0);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if launcher_text_button(ui, "Start", None, true).clicked() || submit_from_directory
                {
                    match form.submit() {
                        Ok(command) => {
                            form.feedback = None;
                            result = Some(command);
                        }
                        Err(feedback) => form.feedback = Some(feedback),
                    }
                }
            });
        });
    result
}

pub(super) const CONTENT_SCROLLBAR_LANE: f32 = 26.0;

/// Trailing space inside every bounded scroll, so content that has been
/// scrolled all the way down ends with the same breathing room it has at
/// the top, rather than resting flush against the bottom of the viewport.
const CONTENT_BOTTOM_GUTTER: f32 = 24.0;

fn content_viewport_bottom(ui: &Ui) -> f32 {
    let mut bottom = ui.ctx().content_rect().bottom();
    if let Some(status_bar) =
        egui::containers::panel::PanelState::load(ui.ctx(), egui::Id::new("status_bar"))
    {
        bottom = bottom.min(status_bar.outer_rect.top());
    }
    bottom
}

fn configure_content_scrollbar(ui: &mut Ui) {
    let mut scroll_style = egui::style::ScrollStyle::floating();
    scroll_style.active_handle_opacity = 0.0;
    scroll_style.active_background_opacity = 0.0;
    ui.spacing_mut().scroll = scroll_style;
}

fn show_bounded_content_scroll<R>(
    ui: &mut Ui,
    id: impl std::hash::Hash + std::fmt::Debug,
    body: impl FnOnce(&mut Ui) -> R,
) -> R {
    let top = ui.cursor().top();
    let height = (content_viewport_bottom(ui) - top).max(0.0);
    let scroll_rect =
        egui::Rect::from_min_size(ui.cursor().min, egui::vec2(ui.available_width(), height));
    ui.scope_builder(egui::UiBuilder::new().max_rect(scroll_rect), |ui| {
        configure_content_scrollbar(ui);
        ScrollArea::vertical()
            .id_salt(id)
            .max_height(height)
            .show(ui, |ui| {
                ui.set_max_width((ui.available_width() - CONTENT_SCROLLBAR_LANE).max(0.0));
                let inner = body(ui);
                ui.add_space(CONTENT_BOTTOM_GUTTER);
                inner
            })
            .inner
    })
    .inner
}

fn ssh_form_has_focus(ui: &Ui, tab_id: TabId) -> bool {
    [
        "host",
        "port",
        "username",
        "password",
        "private_key",
        "key_passphrase",
        "certificate",
    ]
    .into_iter()
    .map(|field| ssh_field_id(ui, tab_id, field))
    .any(|id| ui.memory(|memory| memory.has_focus(id)))
}

/// A small uppercase, muted sub-section label, matching the Session
/// Inspector's grouped-row convention (`inspector.rs`'s `section_heading`)
/// so launcher surfaces share one "quiet section" visual language rather
/// than each screen inventing its own heading style.
///
/// Unlike a plain label, this never adds space above itself: callers add
/// spacing between sections explicitly, so the very first heading in a card
/// sits right under the card's own top padding instead of compounding it.
pub(super) fn ssh_section_heading(ui: &mut Ui, heading: &str) {
    ui.label(
        egui::RichText::new(heading.to_uppercase())
            .size(10.0)
            .color(theme::TEXT_MUTED),
    );
    ui.add_space(4.0);
}

/// Body copy sized and colored to match the rest of the launcher, and
/// wrapped to the card's width instead of the full window so long
/// explanatory text stays legible and doesn't visually escape its section.
fn ssh_paragraph(ui: &mut Ui, text: &str) {
    ui.add(
        egui::Label::new(
            egui::RichText::new(text)
                .size(12.0)
                .color(theme::TEXT_SECONDARY),
        )
        .wrap(),
    );
}

fn ssh_text_edit(
    ui: &mut Ui,
    tab_id: TabId,
    field: &'static str,
    label: &str,
    value: &mut String,
    password: bool,
    request_focus: bool,
) -> egui::Response {
    ui.horizontal(|ui| {
        ui.add_space(2.0);
        let label = ui.add(
            egui::Label::new(egui::RichText::new(label).color(theme::TEXT_SECONDARY))
                .selectable(false),
        );
        let field = ui.add(
            TextEdit::singleline(value)
                .id_salt(("launcher_ssh", tab_id, field))
                .password(password)
                .desired_width(180.0),
        );
        if request_focus {
            field.request_focus();
        }
        field.labelled_by(label.id)
    })
    .inner
}

fn ssh_labeled_text_edit(
    ui: &mut Ui,
    tab_id: TabId,
    field: &'static str,
    label: &str,
    hint: &'static str,
    value: &mut String,
    desired_width: f32,
) -> egui::Response {
    destination::labeled_text_edit(
        ui,
        ("launcher_ssh", tab_id, field),
        label,
        value,
        FieldOptions::stacked().hint(hint).width(desired_width),
    )
}

fn ssh_password_edit_with_hint(
    ui: &mut Ui,
    tab_id: TabId,
    value: &mut String,
    request_focus: bool,
) -> egui::Response {
    ui.vertical(|ui| {
        let label = ui
            .horizontal(|ui| {
                let primary = ui.add(
                    egui::Label::new(egui::RichText::new("Password").color(theme::TEXT_PRIMARY))
                        .selectable(false),
                );
                ui.label(
                    egui::RichText::new(" (optional — leave blank to prompt securely)")
                        .color(theme::TEXT_SECONDARY),
                );
                primary
            })
            .inner;
        let field = ui.add(
            TextEdit::singleline(value)
                .id_salt(("launcher_ssh", tab_id, "password"))
                .password(true)
                .desired_width(f32::INFINITY),
        );
        if request_focus {
            field.request_focus();
        }
        field.labelled_by(label.id)
    })
    .inner
}

fn ssh_multiline_text_edit(
    ui: &mut Ui,
    tab_id: TabId,
    field: &'static str,
    label: &str,
    value: &mut String,
    password: bool,
) -> egui::Response {
    ui.vertical(|ui| {
        let label = ui.label(label);
        ui.add(
            TextEdit::multiline(value)
                .id_salt(("launcher_ssh", tab_id, field))
                .password(password)
                .desired_width(360.0)
                .desired_rows(5),
        )
        .labelled_by(label.id)
    })
    .inner
}

fn ssh_multiline_secret_text_edit(
    ui: &mut Ui,
    tab_id: TabId,
    field: &'static str,
    label: &str,
    value: &mut String,
) -> egui::Response {
    ssh_multiline_text_edit(ui, tab_id, field, label, value, true)
}

/// The quick (non-advanced) SFTP submit row: file-manager choice, Connect
/// button and feedback. The destination itself comes from the shared
/// `show_destination_fields` pane above it.
fn show_sftp_quick_submit(
    ui: &mut Ui,
    form: &mut SshLauncherForm,
    submit_with_enter: bool,
) -> Option<AppCommand> {
    let mut result = None;
    ui.add_space(8.0);
    ui.checkbox(&mut form.sftp_gui_mode, "Use graphical file manager");
    ui.add_space(8.0);
    if ui.button("Connect").clicked() || submit_with_enter {
        match form.submit_quick_connect_sftp() {
            Ok(command) => {
                form.feedback = None;
                result = Some(command);
            }
            Err(feedback) => form.feedback = Some(feedback),
        }
    }
    if let Some(feedback) = &form.feedback {
        ui.add_space(4.0);
        ui.colored_label(theme::STATUS_ERROR, feedback);
    }
    result
}

#[derive(Clone, Copy)]
enum DurableSessionTarget {
    Local,
    Remote,
}

#[derive(Clone, Copy)]
enum DurableSessionLayout {
    Inline,
    Band,
}

fn show_durable_session_controls(
    ui: &mut Ui,
    tab_id: TabId,
    draft: &mut DurableSessionDraft,
    target: DurableSessionTarget,
    layout: DurableSessionLayout,
    show_automatic_recovery: bool,
) {
    let (heading, toggle_label, description) = match target {
        DurableSessionTarget::Local => (
            "Durable local session",
            "Use a durable local session",
            "Keep the named shell in fesTerm's local daemon, or use tmux/GNU screen.",
        ),
        DurableSessionTarget::Remote => (
            "Durable remote session",
            "Use a durable remote session",
            "Attach to the named remote tmux or screen session, creating it when needed.",
        ),
    };
    // Both layouts share one toggle meaning; only the arrangement of the
    // heading, description and switch differs.
    let mut toggled = false;
    match layout {
        DurableSessionLayout::Inline => {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(heading).color(theme::TEXT_PRIMARY));
                toggled = toggle_switch(ui, draft.enabled, toggle_label).clicked();
            });
            ssh_paragraph(ui, description);
        }
        DurableSessionLayout::Band => {
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label(egui::RichText::new(heading).color(theme::TEXT_PRIMARY));
                    ssh_paragraph(ui, description);
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    toggled = toggle_switch(ui, draft.enabled, toggle_label).clicked();
                });
            });
        }
    }
    if toggled {
        draft.enabled = !draft.enabled;
        if draft.enabled && matches!(target, DurableSessionTarget::Local) {
            draft.provider = draft.local_default_provider;
        }
    }
    if !draft.enabled {
        return;
    }

    ui.add_space(6.0);
    ui.horizontal(|ui| {
        if matches!(target, DurableSessionTarget::Local)
            && ui
                .radio(
                    draft.provider == PersistenceProviderKind::FestermSessiond,
                    "fesTerm native",
                )
                .clicked()
        {
            draft.select_provider(PersistenceProviderKind::FestermSessiond);
        }
        if ui
            .radio(draft.provider == PersistenceProviderKind::Tmux, "tmux")
            .clicked()
        {
            draft.select_provider(PersistenceProviderKind::Tmux);
        }
        if ui
            .radio(
                draft.provider == PersistenceProviderKind::Screen,
                "GNU screen",
            )
            .clicked()
        {
            draft.select_provider(PersistenceProviderKind::Screen);
        }
    });
    let name_changed = match layout {
        DurableSessionLayout::Inline => ssh_text_edit(
            ui,
            tab_id,
            "durable_session_name",
            "Session name",
            &mut draft.session_name,
            false,
            false,
        )
        .changed(),
        DurableSessionLayout::Band => ssh_labeled_text_edit(
            ui,
            tab_id,
            "durable_session_name",
            "Session name",
            "",
            &mut draft.session_name,
            f32::INFINITY,
        )
        .changed(),
    };
    if name_changed {
        draft.session_name_touched = true;
    }
    ssh_paragraph(
        ui,
        "Use only letters, digits, hyphens, underscores, or periods.",
    );

    if show_automatic_recovery {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("Recover after connection loss").color(theme::TEXT_PRIMARY),
            );
            if toggle_switch(
                ui,
                draft.automatic_recovery,
                "Automatically resume after connection loss",
            )
            .clicked()
            {
                draft.automatic_recovery = !draft.automatic_recovery;
            }
        });
    }
}

fn show_port_forward_drafts(
    ui: &mut Ui,
    tab_id: TabId,
    id_namespace: &'static str,
    port_forwards: &mut Vec<SshPortForwardDraft>,
) {
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(
                "New rows start on 127.0.0.1 and apply when this connection starts.",
            )
            .size(11.0)
            .color(theme::TEXT_SECONDARY),
        );
        if ui.button("Add port forward").clicked() {
            port_forwards.push(SshPortForwardDraft::default());
        }
    });
    let mut remove_forward = None;
    for (index, forward) in port_forwards.iter_mut().enumerate() {
        ui.add_space(8.0);
        egui::Frame::new()
            .fill(theme::SURFACE_TAB_ACTIVE)
            .stroke(Stroke::new(1.0, theme::BORDER_SUBTLE))
            .corner_radius(6.0)
            .inner_margin(egui::Margin::same(12))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(format!("Forward {}", index + 1))
                            .color(theme::TEXT_SECONDARY),
                    );
                    if ui.button(format!("Remove forward {}", index + 1)).clicked() {
                        remove_forward = Some(index);
                    }
                });
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label("Direction");
                    ui.radio_value(
                        &mut forward.direction,
                        SshPortForwardDirection::Local,
                        "Local",
                    );
                    ui.radio_value(
                        &mut forward.direction,
                        SshPortForwardDirection::Remote,
                        "Remote",
                    );
                });
                profile_text_edit_with_id(
                    ui,
                    tab_id,
                    (id_namespace, "bind_host", index),
                    "Bind host",
                    &mut forward.bind_host,
                );
                profile_text_edit_with_id(
                    ui,
                    tab_id,
                    (id_namespace, "bind_port", index),
                    "Bind port",
                    &mut forward.bind_port,
                );
                profile_text_edit_with_id(
                    ui,
                    tab_id,
                    (id_namespace, "destination_host", index),
                    "Destination host",
                    &mut forward.destination_host,
                );
                profile_text_edit_with_id(
                    ui,
                    tab_id,
                    (id_namespace, "destination_port", index),
                    "Destination port",
                    &mut forward.destination_port,
                );
            });
    }
    if let Some(index) = remove_forward {
        port_forwards.remove(index);
    }
}

/// Which launcher a shared section is being rendered into. Only the few
/// genuinely surface-specific behaviours branch on this; everything else is
/// identical by construction rather than by two copies staying in step.
#[derive(Clone, Copy, PartialEq, Eq)]
enum LauncherSurface {
    Ssh,
    Sftp,
}

/// What `show_authentication_section` reports back to its launcher.
struct AuthenticationOutcome {
    /// Enter was pressed in a credential field.
    submit_with_enter: bool,
    /// The user started a session from a stored credential, which bypasses
    /// the launcher's own submit button.
    command: Option<AppCommand>,
}

/// The credential section shared by the SSH and SFTP launchers: method
/// radios, the fields for the selected method, and the stored-credential
/// shortcut. Both launchers offer exactly the same authentication choices,
/// so they render exactly the same section and differ only in which
/// command a stored credential starts.
fn show_authentication_section(
    ui: &mut Ui,
    tab_id: TabId,
    form: &mut SshLauncherForm,
    surface: LauncherSurface,
    focus_password: bool,
    native_store_available: bool,
) -> AuthenticationOutcome {
    let mut command = None;
    ssh_section_heading(ui, "Authentication");
    ui.horizontal(|ui| {
        ui.radio_value(
            &mut form.authentication_method,
            SshAuthenticationMethod::Password,
            "Password or prompt",
        );
        ui.radio_value(
            &mut form.authentication_method,
            SshAuthenticationMethod::PrivateKey,
            "Private key",
        );
        ui.radio_value(
            &mut form.authentication_method,
            SshAuthenticationMethod::Certificate,
            "Certificate",
        );
    });
    ui.add_space(12.0);
    let submit_with_enter = match form.authentication_method {
        SshAuthenticationMethod::Password => {
            let submit =
                ssh_password_edit_with_hint(ui, tab_id, &mut form.password, focus_password)
                    .lost_focus()
                    && ui.input(|input| input.key_pressed(egui::Key::Enter));
            ui.add_space(4.0);
            if form.saved_profile_id.is_some() {
                ui.checkbox(
                    &mut form.remember_password,
                    "Remember this password in native secure storage",
                );
            } else {
                ssh_paragraph(ui, "Saving a password requires a saved profile.");
            }
            if form.saved_profile_has_credential {
                ui.add_space(4.0);
                let stored_credential_label = match form.saved_profile_credential_kind {
                    Some(CredentialKind::PrivateKey) => "Use stored private key",
                    Some(CredentialKind::Password) | None => "Use stored password",
                };
                if ui
                    .add_enabled(
                        native_store_available,
                        egui::Button::new(stored_credential_label),
                    )
                    .clicked()
                {
                    match surface {
                        LauncherSurface::Ssh => match form.submit_stored_credential() {
                            Ok(started) => {
                                command = Some(started);
                                form.feedback = None;
                            }
                            Err(feedback) => form.feedback = Some(feedback),
                        },
                        LauncherSurface::Sftp => {
                            command = form.saved_profile_id.as_ref().map(|profile_id| {
                                AppCommand::StartStoredPasswordSftpProfile {
                                    profile_id: profile_id.clone(),
                                }
                            });
                        }
                    }
                }
                if !native_store_available {
                    ssh_paragraph(ui, "Native secure storage is unavailable.");
                }
            }
            submit
        }
        SshAuthenticationMethod::PrivateKey => {
            ssh_multiline_secret_text_edit(
                ui,
                tab_id,
                "private_key",
                "OpenSSH private key",
                &mut form.private_key,
            );
            ssh_paragraph(ui, "The key is kept in memory only, never saved.");
            ssh_text_edit(
                ui,
                tab_id,
                "key_passphrase",
                "Key passphrase (optional)",
                &mut form.key_passphrase,
                true,
                false,
            )
            .lost_focus()
                && ui.input(|input| input.key_pressed(egui::Key::Enter))
        }
        SshAuthenticationMethod::Certificate => {
            ssh_multiline_secret_text_edit(
                ui,
                tab_id,
                "private_key",
                "OpenSSH private key",
                &mut form.private_key,
            );
            ssh_paragraph(
                ui,
                "The private key and certificate are kept in memory only, never saved.",
            );
            ssh_text_edit(
                ui,
                tab_id,
                "key_passphrase",
                "Key passphrase (optional)",
                &mut form.key_passphrase,
                true,
                false,
            );
            ui.add_space(4.0);
            ssh_multiline_text_edit(
                ui,
                tab_id,
                "certificate",
                "OpenSSH certificate",
                &mut form.certificate,
                false,
            );
            ssh_paragraph(
                ui,
                "Paste the signed OpenSSH certificate text from the matching -cert.pub file.",
            );
            false
        }
    };
    AuthenticationOutcome {
        submit_with_enter,
        command,
    }
}

fn show_ssh_form(
    ui: &mut Ui,
    tab_id: TabId,
    form: &mut SshLauncherForm,
    configuration: Option<&Configuration>,
    native_store_available: bool,
) -> Option<AppCommand> {
    ui.add_space(16.0);
    let mut result = None;
    // The Quick connect field leads the form, so it takes the opening focus;
    // the SFTP surface already routes its own quick field the same way.
    let focus_quick_connect = form.focus_username;
    form.focus_username = false;
    // Username wins if both are somehow armed: it is the earlier field, so
    // focusing the password would strand the user mid-form.
    let focus_password = std::mem::take(&mut form.focus_password) && !focus_quick_connect;
    egui::Frame::new()
        .fill(theme::SURFACE_TAB_INACTIVE)
        .stroke(Stroke::new(1.0, theme::BORDER_SUBTLE))
        .corner_radius(8.0)
        .inner_margin(egui::Margin::same(16))
        .show(ui, |ui| {
            let card_width = ui.available_width().clamp(340.0, 720.0);
            ui.set_min_width(card_width);
            ui.set_max_width(card_width);

            let destination_enter = DestinationPane::new(
                form.destination(),
                tab_id,
                "launcher_ssh",
                FieldStyle::Stacked,
            )
            .show(ui, focus_quick_connect);

            ui.add_space(14.0);
            ui.separator();
            ui.add_space(14.0);
            let authentication = show_authentication_section(
                ui,
                tab_id,
                form,
                LauncherSurface::Ssh,
                focus_password,
                native_store_available,
            );
            if authentication.command.is_some() {
                result = authentication.command;
            }
            let submit_with_enter = destination_enter | authentication.submit_with_enter;

            ui.add_space(14.0);
            ui.separator();
            ui.add_space(14.0);
            form.sync_remote_durable_provider_default(ui.ctx(), configuration);
            show_durable_session_controls(
                ui,
                tab_id,
                &mut form.durable_session,
                DurableSessionTarget::Remote,
                DurableSessionLayout::Band,
                true,
            );

            ui.add_space(14.0);
            let was_advanced_open = form.advanced_open;
            let advanced_response = egui::CollapsingHeader::new(
                egui::RichText::new("Advanced settings").color(theme::TEXT_SECONDARY),
            )
            .id_salt(("ssh_launcher_advanced_settings", tab_id))
            .open(Some(was_advanced_open))
            .show(ui, |ui| {
                ui.add_space(4.0);
                ssh_section_heading(ui, "Port forwards");
                show_port_forward_drafts(
                    ui,
                    tab_id,
                    "ssh_launcher_port_forward",
                    &mut form.port_forwards,
                );
            });
            // The header is fully controlled by `advanced_open`, so the click is the only
            // authority on its state. Deriving it back from `body_response` would re-open
            // the section every frame of the close animation, which never finishes.
            if advanced_response.header_response.clicked() {
                form.advanced_open = !was_advanced_open;
                form.feedback = None;
            }

            if result.is_none() {
                ui.add_space(14.0);
                ui.separator();
                ui.add_space(12.0);
                if let Some(feedback) = &form.feedback {
                    ui.colored_label(theme::STATUS_ERROR, feedback);
                    ui.add_space(8.0);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if launcher_text_button(ui, "Connect", None, true).clicked()
                        || submit_with_enter
                    {
                        match form.submit() {
                            Ok(command) => {
                                form.feedback = None;
                                result = Some(command);
                            }
                            Err(feedback) => form.feedback = Some(feedback),
                        }
                    }
                    if launcher_text_button(ui, "Save as Profile…", None, false).clicked() {
                        form.feedback = None;
                        result = Some(AppCommand::CreateSshProfileFromDraft {
                            draft: form.profile_draft_seed(),
                        });
                    }
                });
            }
        });
    result
}

fn show_sftp_form(
    ui: &mut Ui,
    tab_id: TabId,
    form: &mut SshLauncherForm,
    native_store_available: bool,
) -> Option<AppCommand> {
    ui.add_space(16.0);
    let mut result = None;
    let focus_username = form.focus_username;
    form.focus_username = false;
    // See `show_ssh_form`: username wins if both are armed.
    let focus_password = std::mem::take(&mut form.focus_password) && !focus_username;
    egui::Frame::new()
        .fill(theme::SURFACE_TAB_INACTIVE)
        .stroke(Stroke::new(1.0, theme::BORDER_SUBTLE))
        .corner_radius(8.0)
        .inner_margin(egui::Margin::same(16))
        .show(ui, |ui| {
            ui.set_width(340.0);
            // See the matching comment in `show_ssh_form`: rendered once in
            // a fixed spot so toggling never moves the checkbox itself.
            let mut show_advanced = form.advanced_open;
            if ui
                .checkbox(&mut show_advanced, "Show advanced settings")
                .changed()
            {
                if show_advanced {
                    form.open_advanced_settings();
                } else {
                    form.close_advanced_settings();
                }
            }
            ui.add_space(10.0);
            // The same Connection pane the SSH launcher uses, so the two
            // launchers offer one destination surface with one notation
            // toggle rather than diverging quick/full layouts.
            let destination_enter = DestinationPane::new(
                form.destination(),
                tab_id,
                "launcher_ssh",
                FieldStyle::Stacked,
            )
            .show(ui, focus_username);
            if !form.advanced_open {
                result = show_sftp_quick_submit(ui, form, destination_enter);
                return;
            }

            ui.add_space(10.0);
            ui.checkbox(&mut form.sftp_gui_mode, "Use graphical file manager");
            if form.sftp_gui_mode {
                ssh_paragraph(
                    ui,
                    "Opens the two-pane SFTP browser. Turn this off to use terminal commands.",
                );
                ui.add_space(12.0);
                if ui.button("Open SFTP file manager").clicked() {
                    match form.submit_sftp() {
                        Ok(command) => {
                            form.feedback = None;
                            result = Some(command);
                        }
                        Err(feedback) => form.feedback = Some(feedback),
                    }
                }
                if let Some(feedback) = &form.feedback {
                    ui.add_space(4.0);
                    ui.colored_label(theme::STATUS_ERROR, feedback);
                }
                return;
            }

            ui.add_space(10.0);
            let authentication = show_authentication_section(
                ui,
                tab_id,
                form,
                LauncherSurface::Sftp,
                focus_password,
                native_store_available,
            );
            if authentication.command.is_some() {
                result = authentication.command;
            }
            let submit_with_enter = destination_enter | authentication.submit_with_enter;

            if result.is_none() {
                ui.add_space(12.0);
                let submit_label = match form.authentication_method {
                    SshAuthenticationMethod::Password => "Connect with password",
                    SshAuthenticationMethod::PrivateKey => "Connect with private key",
                    SshAuthenticationMethod::Certificate => "Connect with certificate",
                };
                if ui.button(submit_label).clicked() || submit_with_enter {
                    match form.submit_sftp() {
                        Ok(command) => {
                            form.feedback = None;
                            result = Some(command);
                        }
                        Err(feedback) => form.feedback = Some(feedback),
                    }
                }
                if let Some(feedback) = &form.feedback {
                    ui.add_space(4.0);
                    ui.colored_label(theme::STATUS_ERROR, feedback);
                }
            }
        });
    result
}

/// Renders the session launcher content and returns any dispatched command.
///
/// `docs/gui-design.md` ("Session Launcher"): fast, compact, and usable
/// repeatedly rather than a wizard or onboarding flow. The SSH form is a
/// one-off connection surface: it creates no profile and retains password,
/// key text, and key passphrases only in temporary UI state until submit.
/// Saved local and SSH profiles both launch directly from their own card,
/// with no password prompt in this surface: a saved SSH profile with a
/// stored native credential launches with it, and one without launches
/// through the same openssh-style in-terminal interactive prompt Quick
/// Connect uses. Entering or replacing a saved SSH profile's stored
/// password lives in the Profiles editor, not here.
///
/// The list is keyboard-navigable: Up/Down moves a highlighted selection
/// (persisted against the singleton Launcher's `tab_id`), Tab cycles through
/// it the same way, and Enter launches the highlighted item without
/// requiring the mouse. The id prevents this temporary state from colliding
/// with other application-surface widgets.
/// Geometry for the New Session surface, taken from the design mockup.
///
/// These are absolute sizes rather than fractions of the window because the
/// surface is a fixed-density layout: a launch card holds a mark, a title,
/// and two lines of description whatever the window does, and a profile row
/// holds one line of text. Only the horizontal split between the two panels
/// and the card row's column count respond to width.
/// Height of a card's two-line description block, used to place both the
/// description and the proceed arrow that shares its vertical centre.
const LAUNCH_CARD_DESCRIPTION_HEIGHT: f32 = 32.0;
/// Width reserved on a card's right edge for the proceed arrow, so the
/// description wraps beside it rather than underneath it.
const LAUNCH_CARD_ARROW_LANE: f32 = 24.0;
const LAUNCH_CARD_HEIGHT: f32 = 112.0;
/// The height a card takes for users who have turned the compact New Session
/// layout on. Compact trims the mark and the padding; it keeps the
/// description, because a card that only says "SSH" does not tell a new user
/// what activating it will do.
const LAUNCH_CARD_COMPACT_HEIGHT: f32 = 96.0;
const LAUNCH_CARD_GAP: f32 = 12.0;
const LAUNCH_CARD_PADDING: f32 = 16.0;
const LAUNCH_CARD_COMPACT_PADDING: f32 = 12.0;
const LAUNCH_CARD_MARK_SIZE: f32 = 40.0;
const LAUNCH_CARD_COMPACT_MARK_SIZE: f32 = 34.0;
const LAUNCH_CARD_TITLE_SIZE: f32 = 16.0;
/// Narrower than this and a card's title starts eliding, so the row wraps to
/// fewer columns instead.
const LAUNCH_CARD_MIN_WIDTH: f32 = 176.0;
const LAUNCHER_PANEL_GAP: f32 = 14.0;
/// Saved Profiles takes the larger share: it carries four columns of text,
/// while Running Sessions is a name and a button.
const LAUNCHER_PROFILES_PANEL_SHARE: f32 = 0.59;
/// Narrower than this the two panels stack instead of sitting side by side,
/// so the profile columns never collapse into each other.
const LAUNCHER_PANEL_MIN_WIDTH: f32 = 400.0;
const LAUNCHER_PANEL_PADDING: f32 = 16.0;
/// Both panels reserve the same heading row and place their mark, title, and
/// controls on the same line within it, so a heading that carries a subtitle
/// still lines up with one that does not.
const LAUNCHER_PANEL_HEADING_HEIGHT: f32 = 44.0;
const LAUNCHER_PANEL_HEADING_LINE: f32 = 16.0;
const LAUNCHER_HEADING_TEXT_SIZE: f32 = 17.0;
const LAUNCHER_HEADING_MARK_SIZE: f32 = 28.0;
/// Vertical inset from a panel's frame to its heading row. Shared by both
/// panels so their headings start at the same height.
const LAUNCHER_PANEL_TOP_MARGIN: i8 = 12;
/// Text size inside the Saved Profiles search pill.
const LAUNCHER_SEARCH_TEXT_SIZE: f32 = 13.0;
const LAUNCHER_BODY_TEXT_SIZE: f32 = 13.0;
const LAUNCHER_DETAIL_TEXT_SIZE: f32 = 12.0;
const LAUNCHER_CONTROL_HEIGHT: f32 = 32.0;
const LAUNCHER_CONTROL_ICON_SIZE: f32 = 16.0;
const LAUNCHER_PANEL_CORNER: f32 = 10.0;
const LAUNCHER_PROFILE_ROW_HEIGHT: f32 = 34.0;
const LAUNCHER_PROFILE_MARK_SIZE: f32 = 24.0;
/// Horizontal centre of the per-row overflow control, measured in from the
/// Saved Profiles panel's right edge.
const LAUNCHER_ROW_MENU_INSET: f32 = 26.0;
/// Saved Profiles column origins as a fraction of the panel's width.
///
/// Column headers are positioned from these same values as the cells beneath
/// them, so a header can never drift away from the data it labels.
const LAUNCHER_PROFILE_COLUMNS: [f32; 4] = [0.103, 0.355, 0.482, 0.757];
/// On narrower panels, names take priority over relative-use metadata.
const LAUNCHER_NARROW_PROFILE_COLUMNS: [f32; 4] = [0.103, 0.400, 0.520, 0.820];
/// Gutter kept between one column's text and the next column's origin.
/// Height reserved for the Saved Profiles footer row when pinning it to the
/// panel's bottom edge.
const LAUNCHER_FOOTER_HEIGHT: f32 = LAUNCHER_CONTROL_HEIGHT;
const LAUNCHER_FOOTER_GAP: f32 = 12.0;
/// Inset kept between the window's content edge and the New Session surface,
/// so the launch cards and panels do not sit flush against the frame.
const LAUNCHER_SURFACE_MARGIN: f32 = 12.0;
const LAUNCHER_COLUMN_GUTTER: f32 = 12.0;

fn launcher_profile_columns(width: f32) -> [f32; 4] {
    if width < 600.0 {
        LAUNCHER_NARROW_PROFILE_COLUMNS
    } else {
        LAUNCHER_PROFILE_COLUMNS
    }
}

/// How the Saved Profiles list is ordered.
#[derive(Clone, Copy, Default, Eq, PartialEq)]
enum ProfileSortOrder {
    /// Most recently launched first, then never-launched profiles by name.
    /// The default because the strongest predictor of what a user wants to
    /// open is what they opened last.
    #[default]
    RecentlyUsed,
    Name,
}

impl ProfileSortOrder {
    fn label(self) -> &'static str {
        match self {
            Self::RecentlyUsed => "Sorted by last used",
            Self::Name => "Sorted by name",
        }
    }

    fn toggled(self) -> Self {
        match self {
            Self::RecentlyUsed => Self::Name,
            Self::Name => Self::RecentlyUsed,
        }
    }
}

/// Whole seconds since the Unix epoch.
///
/// `None` when the host clock is set before the epoch; callers then omit
/// relative ages rather than printing a guess.
pub(crate) fn unix_now_seconds() -> Option<u64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|since_epoch| since_epoch.as_secs())
}

/// Formats an absolute instant as the coarse relative age shown in the
/// "Last Used" column and under each running session's name.
///
/// `now` is a parameter rather than being read here so the result is
/// deterministic under test. Months and years use nominal 30- and 365-day
/// lengths: at that distance the label is a rough ordering cue, and being
/// exact would cost a calendar dependency for no reader-visible gain.
fn relative_age(now_unix_seconds: u64, then_unix_seconds: u64) -> String {
    const MINUTE: u64 = 60;
    const HOUR: u64 = 60 * MINUTE;
    const DAY: u64 = 24 * HOUR;
    const WEEK: u64 = 7 * DAY;
    const MONTH: u64 = 30 * DAY;
    const YEAR: u64 = 365 * DAY;

    let elapsed = now_unix_seconds.saturating_sub(then_unix_seconds);
    let (count, unit) = if elapsed < MINUTE {
        return "Just now".to_owned();
    } else if elapsed < HOUR {
        (elapsed / MINUTE, "minute")
    } else if elapsed < DAY {
        (elapsed / HOUR, "hour")
    } else if elapsed < WEEK {
        (elapsed / DAY, "day")
    } else if elapsed < MONTH {
        (elapsed / WEEK, "week")
    } else if elapsed < YEAR {
        (elapsed / MONTH, "month")
    } else {
        (elapsed / YEAR, "year")
    };
    if count == 1 {
        format!("1 {unit} ago")
    } else {
        format!("{count} {unit}s ago")
    }
}

/// Lays text out on a bounded number of lines, eliding the remainder.
///
/// Table cells and card descriptions have fixed heights, so overflowing text
/// must be cut rather than allowed to reflow a row out of its slot.
fn elided_galley(
    ui: &Ui,
    text: &str,
    size: f32,
    color: egui::Color32,
    max_width: f32,
    max_rows: usize,
) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::single_section(
        text.to_owned(),
        egui::TextFormat::simple(egui::FontId::proportional(size), color),
    );
    job.wrap = egui::text::TextWrapping {
        max_width,
        max_rows,
        break_anywhere: false,
        overflow_character: Some('…'),
    };
    ui.painter().layout_job(job)
}

fn session_mark_size(mark: Icon, height: f32) -> egui::Vec2 {
    let aspect = match mark {
        Icon::SshRemote | Icon::Serial => 1.3,
        _ => 1.0,
    };
    vec2(height * aspect, height)
}

/// Paints a session-type mark in its optically sized slot.
///
/// Remote marks get the accent globe painted over them: the asset layer is
/// monochrome by contract, so the two-tone treatment is composed here out of
/// the complete `SshRemote` mark and the `RemoteGlobe` badge that shares its
/// coordinates exactly.
fn paint_session_mark(painter: &egui::Painter, item: &LauncherItem<'_>, rect: egui::Rect) {
    let (mark, color) = item.mark();
    paint_mark(painter, mark, color, rect);
}

fn paint_profile_table_mark(painter: &egui::Painter, item: &ProfileTableItem, rect: egui::Rect) {
    let (mark, color) = item.kind.mark();
    paint_mark(painter, mark, color, rect);
}

fn paint_mark(painter: &egui::Painter, mark: Icon, color: egui::Color32, rect: egui::Rect) {
    // Wide silhouettes occupy more horizontal space, not a smaller terminal
    // or connector squeezed into the same square as a document.
    let rect = egui::Rect::from_center_size(rect.center(), egui::Vec2::splat(rect.width()));
    icon::paint(painter, mark, rect, color);
    if matches!(mark, Icon::SshRemote) {
        icon::paint(painter, Icon::RemoteGlobe, rect, theme::ICON_SESSION_GLOBE);
    }
}

/// Renders one launch card: a session-type mark, a title, a description, and
/// a corner arrow marking the card as an entry into a flow rather than a
/// toggle.
fn show_launch_card(
    ui: &mut Ui,
    size: egui::Vec2,
    compact: bool,
    item: &LauncherItem<'_>,
    selected: bool,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    // The accessible name carries the description too: on this surface the
    // title alone ("SSH") does not say what activating the card will do.
    response.widget_info(|| {
        WidgetInfo::labeled(
            WidgetType::Button,
            ui.is_enabled(),
            format!("{} — {}", item.label, item.description),
        )
    });
    if !ui.is_rect_visible(rect) {
        return response;
    }

    let active = selected || response.hovered();
    // Every card carries the same quiet border whether or not it is the
    // keyboard selection: a heavier outline on one card reads as a disabled
    // or modal state rather than as a cursor. Selection and hover lift the
    // card's fill and its arrow instead.
    ui.painter().rect(
        rect,
        LAUNCHER_PANEL_CORNER,
        if active {
            theme::SURFACE_TAB_ACTIVE
        } else {
            theme::SURFACE_CARD
        },
        Stroke::new(1.0, theme::BORDER_SUBTLE),
        egui::StrokeKind::Inside,
    );

    // The mark and the title share one row. Stacking them cost the card a
    // whole mark's worth of height for no more information, and the row of
    // cards is a navigation strip rather than the surface's content.
    let padding = if compact {
        LAUNCH_CARD_COMPACT_PADDING
    } else {
        LAUNCH_CARD_PADDING
    };
    let mark_size = if compact {
        LAUNCH_CARD_COMPACT_MARK_SIZE
    } else {
        LAUNCH_CARD_MARK_SIZE
    };
    let mark_rect = egui::Rect::from_min_size(
        egui::pos2(rect.left() + padding, rect.top() + padding),
        session_mark_size(item.mark().0, mark_size),
    );
    paint_session_mark(ui.painter(), item, mark_rect);

    let title_left = mark_rect.right() + 10.0;
    let title = elided_galley(
        ui,
        &item.label,
        LAUNCH_CARD_TITLE_SIZE,
        theme::TEXT_PRIMARY,
        (rect.right() - padding - title_left).max(0.0),
        1,
    );
    let title_height = title.size().y;
    ui.painter().galley(
        egui::pos2(title_left, mark_rect.center().y - title_height / 2.0),
        title,
        theme::TEXT_PRIMARY,
    );

    // The description stays in the compact layout too: it is the only text
    // that says what the card does, and the arrow does not replace it.
    let text_left = rect.left() + padding;
    let description = elided_galley(
        ui,
        &item.description,
        LAUNCHER_BODY_TEXT_SIZE,
        theme::TEXT_SECONDARY,
        // Every description line stops short of the proceed arrow's column
        // rather than the last line alone, so a two-line description keeps a
        // straight right edge instead of stepping in on its final row.
        (rect.width() - padding * 2.0 - LAUNCH_CARD_ARROW_LANE).max(0.0),
        2,
    );
    let description_top = mark_rect.bottom() + if compact { 6.0 } else { 8.0 };
    ui.painter().galley(
        egui::pos2(text_left, description_top),
        description,
        theme::TEXT_SECONDARY,
    );

    let arrow = egui::Rect::from_center_size(
        egui::pos2(
            rect.right() - padding - LAUNCHER_CONTROL_ICON_SIZE / 2.0,
            rect.bottom() - padding - LAUNCH_CARD_DESCRIPTION_HEIGHT / 2.0,
        ),
        egui::Vec2::splat(LAUNCHER_CONTROL_ICON_SIZE),
    );
    icon::paint(
        ui.painter(),
        Icon::Proceed,
        arrow,
        if active {
            theme::TEXT_PRIMARY
        } else {
            theme::TEXT_SECONDARY
        },
    );
    response
}

/// Renders a panel heading: an identity mark, a title, and an optional
/// subtitle, with the caller's own controls laid out along the same baseline
/// on the right.
fn show_panel_heading(
    ui: &mut Ui,
    mark: Icon,
    title: &str,
    subtitle: Option<&str>,
    controls: impl FnOnce(&mut Ui),
) {
    let height = LAUNCHER_PANEL_HEADING_HEIGHT;
    // The text block is measured up front so it can be placed against the
    // heading line rather than centred in the row. Left to egui's own
    // alignment it would sit at the top of the row, and a heading with a
    // subtitle would ride higher than one without — which is why the two
    // panels' titles did not line up with each other.
    let title_size = ui
        .painter()
        .layout_no_wrap(
            title.to_owned(),
            egui::FontId::proportional(LAUNCHER_HEADING_TEXT_SIZE),
            theme::TEXT_PRIMARY,
        )
        .size();
    let subtitle_width = subtitle.map(|subtitle| {
        ui.painter()
            .layout_no_wrap(
                subtitle.to_owned(),
                egui::FontId::proportional(LAUNCHER_DETAIL_TEXT_SIZE),
                theme::TEXT_SECONDARY,
            )
            .size()
            .x
    });
    let text_width = title_size.x.max(subtitle_width.unwrap_or(0.0)).ceil() + 1.0;
    ui.horizontal(|ui| {
        ui.set_height(height);
        let (mark_slot, _) =
            ui.allocate_exact_size(vec2(LAUNCHER_HEADING_MARK_SIZE, height), Sense::hover());
        let mark_rect = egui::Rect::from_center_size(
            egui::pos2(
                mark_slot.center().x,
                mark_slot.top() + LAUNCHER_PANEL_HEADING_LINE,
            ),
            egui::Vec2::splat(LAUNCHER_HEADING_MARK_SIZE),
        );
        icon::paint(ui.painter(), mark, mark_rect, theme::ACCENT_ACTION);
        ui.add_space(6.0);
        let (text_slot, _) = ui.allocate_exact_size(vec2(text_width, height), Sense::hover());
        let text_rect = egui::Rect::from_min_max(
            egui::pos2(
                text_slot.left(),
                text_slot.top() + LAUNCHER_PANEL_HEADING_LINE - title_size.y / 2.0,
            ),
            text_slot.max,
        );
        // Painted galleys would drop the heading out of the accessibility
        // tree, so the labels stay real widgets inside a placed rect.
        ui.scope_builder(
            egui::UiBuilder::new()
                .max_rect(text_rect)
                .layout(egui::Layout::top_down(egui::Align::Min)),
            |ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.label(
                    egui::RichText::new(title)
                        .size(LAUNCHER_HEADING_TEXT_SIZE)
                        .color(theme::TEXT_PRIMARY),
                );
                if let Some(subtitle) = subtitle {
                    ui.label(
                        egui::RichText::new(subtitle)
                            .size(LAUNCHER_DETAIL_TEXT_SIZE)
                            .color(theme::TEXT_SECONDARY),
                    );
                }
            },
        );
        // The controls centre on the heading line as well, so a search field
        // or a refresh button shares the title's centre rather than the row's.
        let rest = ui.available_rect_before_wrap();
        let control_rect = egui::Rect::from_min_max(
            rest.min,
            egui::pos2(rest.right(), rest.top() + LAUNCHER_PANEL_HEADING_LINE * 2.0),
        );
        ui.scope_builder(
            egui::UiBuilder::new()
                .max_rect(control_rect)
                .layout(egui::Layout::right_to_left(egui::Align::Center)),
            controls,
        );
    });
}

#[derive(Clone, Copy)]
struct ProfileTableOptions {
    row_height: f32,
    show_mark: bool,
    show_subtitle: bool,
    column_origins: [f32; 4],
}

impl ProfileTableOptions {
    fn launcher(width: f32) -> Self {
        Self {
            row_height: LAUNCHER_PROFILE_ROW_HEIGHT,
            show_mark: true,
            show_subtitle: false,
            column_origins: launcher_profile_columns(width),
        }
    }

    fn profiles(width: f32) -> Self {
        let column_origins = if width < 760.0 {
            [0.028, 0.340, 0.500, 0.775]
        } else {
            [0.028, 0.295, 0.445, 0.770]
        };
        Self {
            row_height: 64.0,
            show_mark: false,
            show_subtitle: true,
            column_origins,
        }
    }
}

#[derive(Clone, Copy)]
enum ProfileTableMenu {
    Launcher,
    Profiles,
}

enum ProfileTableAction {
    Connect,
    LauncherCrossover(Box<AppCommand>),
    OpenSftpFileManager,
    Edit,
    Duplicate,
    Delete,
}

struct ProfileTableRowResponse {
    response: egui::Response,
    action: Option<ProfileTableAction>,
}

/// Renders one Saved Profiles row and its menu.
///
/// The same renderer is used by the Launcher and the full Profiles tab; the
/// options decide whether the compact launcher mark or the wider tab subtitle
/// is shown, and the menu kind decides which action set the row exposes.
fn show_profile_row(
    ui: &mut Ui,
    width: f32,
    item: &ProfileTableItem,
    selected: bool,
    now_unix_seconds: Option<u64>,
    options: ProfileTableOptions,
    menu_kind: ProfileTableMenu,
) -> ProfileTableRowResponse {
    let mut action = None;
    let (rect, _) = ui.allocate_exact_size(vec2(width, options.row_height), Sense::hover());
    let menu_center = egui::pos2(rect.right() - LAUNCHER_ROW_MENU_INSET, rect.center().y);
    let menu_rect = egui::Rect::from_center_size(menu_center, egui::Vec2::splat(24.0));
    let response = ui.interact(
        rect.with_max_x(menu_rect.left()),
        ui.id().with(("profile_row", &item.identifier)),
        Sense::click_and_drag(),
    );
    response.widget_info(|| {
        WidgetInfo::labeled(
            WidgetType::Button,
            ui.is_enabled(),
            format!(
                "{} — {} · {}",
                item.label,
                item.kind.type_label(),
                item.location
            ),
        )
    });

    let active = selected || response.hovered();
    if active {
        ui.painter()
            .rect_filled(rect.expand2(vec2(6.0, 0.0)), 6.0, theme::SURFACE_TAB_ACTIVE);
    }
    ui.painter().hline(
        rect.x_range(),
        rect.bottom() - 0.5,
        Stroke::new(1.0, theme::BORDER_SUBTLE.gamma_multiply(0.5)),
    );

    if options.show_mark {
        let mark_size = session_mark_size(item.kind.mark().0, LAUNCHER_PROFILE_MARK_SIZE);
        let mark_rect = egui::Rect::from_min_size(
            egui::pos2(
                rect.left() + LAUNCHER_PANEL_PADDING - 1.0,
                rect.center().y - mark_size.y / 2.0,
            ),
            mark_size,
        );
        paint_profile_table_mark(ui.painter(), item, mark_rect);
    }

    let last_used = item
        .last_used_unix_seconds
        .zip(now_unix_seconds)
        .map(|(then, now)| relative_age(now, then))
        .unwrap_or_else(|| "Never".to_owned());
    let name_left = rect.left() + width * options.column_origins[0];
    let name_right = rect.left() + width * options.column_origins[1];
    if options.show_subtitle {
        let title = elided_galley(
            ui,
            &item.label,
            16.0,
            theme::TEXT_PRIMARY,
            (name_right - name_left - LAUNCHER_COLUMN_GUTTER).max(0.0),
            1,
        );
        let detail = elided_galley(
            ui,
            item.subtitle.as_deref().unwrap_or(""),
            14.0,
            theme::TEXT_SECONDARY,
            (name_right - name_left - LAUNCHER_COLUMN_GUTTER).max(0.0),
            1,
        );
        let text_height = title.size().y + 4.0 + detail.size().y;
        let top = rect.center().y - text_height / 2.0;
        ui.painter()
            .galley(egui::pos2(name_left, top), title, theme::TEXT_PRIMARY);
        let detail_y = top + text_height - detail.size().y;
        ui.painter().galley(
            egui::pos2(name_left, detail_y),
            detail,
            theme::TEXT_SECONDARY,
        );
    } else {
        let galley = elided_galley(
            ui,
            &item.label,
            LAUNCHER_BODY_TEXT_SIZE,
            theme::TEXT_PRIMARY,
            (name_right - name_left - LAUNCHER_COLUMN_GUTTER).max(0.0),
            1,
        );
        let top = rect.center().y - galley.size().y / 2.0;
        ui.painter()
            .galley(egui::pos2(name_left, top), galley, theme::TEXT_PRIMARY);
    }

    for (offset, (text, size, color)) in [
        (
            item.kind.type_label().to_owned(),
            LAUNCHER_BODY_TEXT_SIZE,
            if options.show_subtitle {
                theme::TEXT_PRIMARY
            } else {
                theme::TEXT_SECONDARY
            },
        ),
        (
            item.location.clone(),
            LAUNCHER_BODY_TEXT_SIZE,
            theme::TEXT_SECONDARY,
        ),
        (last_used, LAUNCHER_DETAIL_TEXT_SIZE, theme::TEXT_SECONDARY),
    ]
    .into_iter()
    .enumerate()
    {
        let index = offset + 1;
        let left = rect.left() + width * options.column_origins[index];
        let right = options
            .column_origins
            .get(index + 1)
            .map(|fraction| rect.left() + width * fraction)
            .unwrap_or(menu_center.x - 12.0);
        let galley = elided_galley(
            ui,
            &text,
            size,
            color,
            (right - left - LAUNCHER_COLUMN_GUTTER).max(0.0),
            1,
        );
        let top = rect.center().y - galley.size().y / 2.0;
        ui.painter().galley(egui::pos2(left, top), galley, color);
    }

    let menu_response = ui.interact(
        menu_rect,
        ui.id().with(("profile_row_menu", &item.identifier)),
        Sense::click(),
    );
    let menu_name = format!("More actions for {}", item.label);
    menu_response
        .widget_info(|| WidgetInfo::labeled(WidgetType::Button, ui.is_enabled(), &menu_name));
    icon::paint(
        ui.painter(),
        Icon::OverflowVertical,
        menu_rect.shrink(4.0),
        if menu_response.hovered() {
            theme::TEXT_PRIMARY
        } else {
            theme::TEXT_SECONDARY
        },
    );
    let menu_response = menu_response.on_hover_text("More actions");

    egui::Popup::menu(&menu_response).show(|ui| {
        action = show_profile_row_menu(ui, item, menu_kind).or(action.take());
    });
    response.context_menu(|ui| {
        action = show_profile_row_menu(ui, item, menu_kind).or(action.take());
    });

    if response.clicked() {
        // Activating a row means different things on the two surfaces it
        // appears on: the Launcher exists to start sessions, while the
        // Profiles tab exists to manage them. Connecting from Profiles
        // stays available from the row menu.
        action = Some(match menu_kind {
            ProfileTableMenu::Launcher => ProfileTableAction::Connect,
            ProfileTableMenu::Profiles => ProfileTableAction::Edit,
        });
    }

    ProfileTableRowResponse { response, action }
}

fn show_profile_row_menu(
    ui: &mut Ui,
    item: &ProfileTableItem,
    menu_kind: ProfileTableMenu,
) -> Option<ProfileTableAction> {
    if ui.button("Connect").clicked() {
        ui.close();
        return Some(ProfileTableAction::Connect);
    }
    match menu_kind {
        ProfileTableMenu::Launcher => {
            if let Some((label, crossover)) = item.launcher_crossover() {
                if ui.button(label).clicked() {
                    ui.close();
                    return Some(ProfileTableAction::LauncherCrossover(Box::new(crossover)));
                }
            }
        }
        ProfileTableMenu::Profiles => {
            if item.kind == ProfileTableKind::Ssh && ui.button("Open SFTP").clicked() {
                ui.close();
                return Some(ProfileTableAction::OpenSftpFileManager);
            }
        }
    }
    if ui.button("Edit").clicked() {
        ui.close();
        return Some(ProfileTableAction::Edit);
    }
    if matches!(menu_kind, ProfileTableMenu::Profiles) {
        if ui.button("Duplicate").clicked() {
            ui.close();
            return Some(ProfileTableAction::Duplicate);
        }
        if ui.button("Delete").clicked() {
            ui.close();
            return Some(ProfileTableAction::Delete);
        }
    }
    None
}

/// Renders one running-session group: a disclosure header naming the
/// provider, a count, and a row per session.
///
/// Providers stay in separate groups rather than one merged list because
/// reattaching goes through a different mechanism for each, and the group a
/// session sits in is the only thing that tells the user which.
#[allow(clippy::too_many_arguments)]
fn show_session_group(
    ui: &mut Ui,
    width: f32,
    title: &str,
    items: &[LauncherItem<'_>],
    first_index: usize,
    selected: usize,
    expanded: &mut bool,
    now_unix_seconds: Option<u64>,
    command: &mut Option<AppCommand>,
) {
    if items.is_empty() {
        return;
    }
    egui::Frame::new()
        .fill(theme::SURFACE_CARD)
        .corner_radius(8.0)
        .inner_margin(egui::Margin::symmetric(10, 8))
        .show(ui, |ui| {
            ui.set_width((width - 20.0).max(0.0));
            ui.horizontal(|ui| {
                let (chevron, _) = ui.allocate_exact_size(egui::Vec2::splat(16.0), Sense::hover());
                icon::paint(
                    ui.painter(),
                    if *expanded {
                        Icon::SectionExpanded
                    } else {
                        Icon::SectionCollapsed
                    },
                    chevron,
                    theme::TEXT_SECONDARY,
                );
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new(title)
                        .size(LAUNCHER_BODY_TEXT_SIZE)
                        .color(theme::TEXT_PRIMARY),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let (badge, _) = ui.allocate_exact_size(vec2(22.0, 24.0), Sense::hover());
                    ui.painter()
                        .rect_filled(badge, 11.0, theme::SURFACE_TAB_ACTIVE);
                    ui.painter().text(
                        badge.center(),
                        egui::Align2::CENTER_CENTER,
                        items.len().to_string(),
                        egui::FontId::proportional(LAUNCHER_DETAIL_TEXT_SIZE),
                        theme::TEXT_SECONDARY,
                    );
                });
            });
            let header = ui.interact(
                ui.min_rect(),
                ui.id().with(("session_group", title)),
                Sense::click(),
            );
            let header_name = if *expanded {
                format!("Collapse {title}")
            } else {
                format!("Expand {title}")
            };
            header.widget_info(|| {
                WidgetInfo::labeled(WidgetType::Button, ui.is_enabled(), header_name.clone())
            });
            if header.clicked() {
                *expanded = !*expanded;
            }
            if !*expanded {
                return;
            }
            ui.add_space(6.0);
            let stride = 44.0 + ui.spacing().item_spacing.y;
            let top = ui.cursor().top();
            let first = (((ui.clip_rect().top() - top) / stride).floor().max(0.0) as usize)
                .min(items.len());
            let end = ((((ui.clip_rect().bottom() - top) / stride).ceil().max(0.0) as usize) + 1)
                .min(items.len())
                .max(first);
            ui.add_space(first as f32 * stride);
            for (offset, item) in items.iter().enumerate().take(end).skip(first) {
                let identity = match item.kind {
                    LauncherItemKind::ResumeSession(session) => {
                        format!("native:{}", session.endpoint)
                    }
                    LauncherItemKind::ResumeMultiplexerSession(provider, session) => {
                        format!("{provider:?}:{}", session.match_key)
                    }
                    _ => unreachable!("running groups contain only resumable sessions"),
                };
                ui.push_id(identity, |ui| {
                    show_session_row(
                        ui,
                        first_index + offset == selected,
                        item,
                        now_unix_seconds,
                        command,
                    );
                });
            }
            ui.add_space((items.len() - end) as f32 * stride);
        });
}

/// Renders one reattachable session: its mark, its name, how long it has been
/// running, and the control that attaches it to a tab.
fn show_session_row(
    ui: &mut Ui,
    selected: bool,
    item: &LauncherItem<'_>,
    now_unix_seconds: Option<u64>,
    command: &mut Option<AppCommand>,
) {
    ui.horizontal(|ui| {
        ui.set_height(44.0);
        let (mark, _) =
            ui.allocate_exact_size(session_mark_size(item.mark().0, 24.0), Sense::hover());
        paint_session_mark(ui.painter(), item, mark);
        ui.add_space(8.0);
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.label(
                egui::RichText::new(&item.label)
                    .size(LAUNCHER_BODY_TEXT_SIZE)
                    .color(theme::TEXT_PRIMARY),
            );
            let subtitle = item
                .last_used_unix_seconds
                .zip(now_unix_seconds)
                .map(|(started, now)| {
                    let age = format!("Started {}", relative_age(now, started));
                    if item.description == "Attached elsewhere" {
                        format!("Attached elsewhere · {age}")
                    } else {
                        age
                    }
                })
                .unwrap_or_else(|| item.description.clone());
            ui.label(
                egui::RichText::new(subtitle)
                    .size(LAUNCHER_DETAIL_TEXT_SIZE)
                    .color(theme::TEXT_SECONDARY),
            );
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let name = format!("Reattach {}", item.label);
            if launcher_button(ui, Icon::Reattach, "Reattach", Some(&name), selected).clicked() {
                *command = Some(item.command());
            }
        });
    });
}

/// A labelled icon button in the New Session surface's own visual language.
///
/// `accent` paints the affirmative variant used for the primary action in a
/// panel; everything else is a quiet surface button.
fn launcher_button(
    ui: &mut Ui,
    mark: Icon,
    label: &str,
    accessible_name: Option<&str>,
    accent: bool,
) -> egui::Response {
    launcher_button_with_optional_icon(ui, Some(mark), label, accessible_name, accent, 0.0)
}

fn launcher_text_button(
    ui: &mut Ui,
    label: &str,
    accessible_name: Option<&str>,
    accent: bool,
) -> egui::Response {
    launcher_button_with_optional_icon(ui, None, label, accessible_name, accent, 0.0)
}

fn launcher_dropdown_button(
    ui: &mut Ui,
    mark: Icon,
    label: &str,
    accessible_name: Option<&str>,
    accent: bool,
) -> egui::Response {
    let response = launcher_button_with_optional_icon(
        ui,
        Some(mark),
        label,
        accessible_name,
        accent,
        LAUNCHER_DROPDOWN_CHEVRON_LANE,
    );
    if ui.is_rect_visible(response.rect) {
        let chevron = egui::Rect::from_center_size(
            egui::pos2(response.rect.right() - 12.0, response.rect.center().y + 1.0),
            egui::Vec2::splat(10.0),
        );
        icon::paint(
            ui.painter(),
            Icon::NextMatch,
            chevron,
            if accent {
                theme::TEXT_ON_ACCENT
            } else {
                theme::TEXT_PRIMARY
            },
        );
    }
    response
}

/// Width reserved at a dropdown button's right edge for its chevron, so the
/// chevron sits beside the label instead of on top of it.
const LAUNCHER_DROPDOWN_CHEVRON_LANE: f32 = 18.0;

fn launcher_button_with_optional_icon(
    ui: &mut Ui,
    mark: Option<Icon>,
    label: &str,
    accessible_name: Option<&str>,
    accent: bool,
    trailing_lane: f32,
) -> egui::Response {
    let text_width = ui
        .painter()
        .layout_no_wrap(
            label.to_owned(),
            egui::FontId::proportional(LAUNCHER_BODY_TEXT_SIZE),
            theme::TEXT_PRIMARY,
        )
        .size()
        .x
        .ceil();
    let button_width = text_width
        + trailing_lane
        + if mark.is_some() {
            48.0
        } else {
            LAUNCHER_PANEL_PADDING * 2.0
        };
    let (rect, response) =
        ui.allocate_exact_size(vec2(button_width, LAUNCHER_CONTROL_HEIGHT), Sense::click());
    // Several buttons on this surface share one visible word ("Reattach"),
    // so the caller may name them apart for anyone navigating by label.
    let name = accessible_name.unwrap_or(label);
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, ui.is_enabled(), name));
    if !ui.is_rect_visible(rect) {
        return response;
    }
    let role = if accent {
        ActionButtonRole::Accent
    } else {
        ActionButtonRole::Secondary
    };
    let visuals = controls::action_button_visuals(role, response.hovered());
    ui.painter().rect(
        rect,
        controls::ACTION_BUTTON_CORNER_RADIUS,
        visuals.fill,
        visuals.stroke,
        egui::StrokeKind::Inside,
    );
    let text_left = if let Some(mark) = mark {
        let mark_rect = egui::Rect::from_center_size(
            egui::pos2(rect.left() + 18.0, rect.center().y),
            egui::Vec2::splat(LAUNCHER_CONTROL_ICON_SIZE),
        );
        icon::paint(ui.painter(), mark, mark_rect, visuals.foreground);
        mark_rect.right() + 8.0
    } else {
        rect.center().x - trailing_lane / 2.0 - text_width / 2.0
    };
    ui.painter().text(
        egui::pos2(text_left, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(LAUNCHER_BODY_TEXT_SIZE),
        visuals.foreground,
    );
    response
}

/// A bare icon control, for the panel headings' secondary actions.
fn launcher_icon_button(ui: &mut Ui, mark: Icon, tooltip: &str) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::Vec2::splat(28.0), Sense::click());
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, ui.is_enabled(), tooltip));
    icon::paint(
        ui.painter(),
        mark,
        rect.shrink(6.0),
        if response.hovered() {
            theme::TEXT_PRIMARY
        } else {
            theme::TEXT_SECONDARY
        },
    );
    response.on_hover_text(tooltip)
}

fn show_profile_search_field(ui: &mut Ui, field_width: f32, search_text: &mut String) {
    controls::SearchField {
        width: field_width,
        height: LAUNCHER_CONTROL_HEIGHT,
        icon_inset: 16.0,
        icon_size: LAUNCHER_CONTROL_ICON_SIZE,
        text_size: LAUNCHER_SEARCH_TEXT_SIZE,
        hint: "Search profiles…",
    }
    .show(ui, search_text);
}

#[allow(clippy::too_many_arguments)]
pub fn show_launcher(
    ui: &mut Ui,
    tab_id: TabId,
    configuration: &Configuration,
    native_store_available: bool,
    secure_storage_status: Option<&str>,
    compact_cards: bool,
    resumable_sessions: &[festerm_sessiond::UnattachedSession],
    tmux_sessions: &[crate::multiplexer_sessions::MultiplexerSession],
    screen_sessions: &[crate::multiplexer_sessions::MultiplexerSession],
) -> Option<AppCommand> {
    let profiles = configuration.profiles();
    let customize_local_shell = configuration.interface_settings().customize_local_shell();
    let now_unix_seconds = unix_now_seconds();
    // The five launch cards: one per session type fesTerm can start from
    // nothing, in the order a new user meets them.
    let mut items = vec![
        LauncherItem::untabulated(
            "Local Shell".to_owned(),
            "Start a local terminal session".to_owned(),
            LauncherItemKind::LocalDefault,
        ),
        LauncherItem::untabulated(
            "SSH".to_owned(),
            "Connect to a remote host over SSH".to_owned(),
            LauncherItemKind::NewSsh,
        ),
        LauncherItem::untabulated(
            "SFTP".to_owned(),
            "Browse and transfer files".to_owned(),
            LauncherItemKind::NewSftp,
        ),
        LauncherItem::untabulated(
            "Serial".to_owned(),
            "Connect to a serial device".to_owned(),
            LauncherItemKind::NewSerial,
        ),
        LauncherItem::untabulated(
            "Markdown".to_owned(),
            "Open a Markdown workspace".to_owned(),
            LauncherItemKind::NewMarkdown,
        ),
    ];
    let fixed_end = items.len();
    // Resumable, unattached `festerm-sessiond` sessions (feature request
    // #70) lead the Running Sessions panel. An already-attached
    // fesTerm-sessiond session is never enumerated in the first place (its
    // single-client "steal" semantics make offering it here redundant with
    // Reconnect/Inspector Resume), unlike tmux/screen below.
    items.extend(resumable_sessions.iter().map(|session| {
        let mut item = LauncherItem::untabulated(
            session.name.clone(),
            session
                .working_directory
                .as_deref()
                .map(|directory| format!("{} · {directory}", session.shell))
                .unwrap_or_else(|| session.shell.clone()),
            LauncherItemKind::ResumeSession(session),
        );
        // Milliseconds are the sessiond wire unit; the surface only ever
        // shows a coarse relative age, so narrowing here keeps one
        // representation flowing through the whole screen.
        item.last_used_unix_seconds = u64::try_from(session.created_at_unix_ms / 1_000).ok();
        item
    }));
    let festerm_sessions_end = items.len();
    // Unlike fesTerm-sessiond above, tmux and GNU screen natively support
    // more than one attached client, so an already-attached session is still
    // offered here -- annotated rather than omitted.
    items.extend(tmux_sessions.iter().map(|session| {
        let mut item = LauncherItem::untabulated(
            session.name.clone(),
            if session.attached {
                "Attached elsewhere".to_owned()
            } else {
                "tmux session".to_owned()
            },
            LauncherItemKind::ResumeMultiplexerSession(PersistenceProviderKind::Tmux, session),
        );
        item.last_used_unix_seconds = session.started_at_unix_seconds();
        item
    }));
    let tmux_sessions_end = items.len();
    items.extend(screen_sessions.iter().map(|session| {
        let mut item = LauncherItem::untabulated(
            session.name.clone(),
            if session.attached {
                "Attached elsewhere".to_owned()
            } else {
                "GNU screen session".to_owned()
            },
            LauncherItemKind::ResumeMultiplexerSession(PersistenceProviderKind::Screen, session),
        );
        item.last_used_unix_seconds = session.started_at_unix_seconds();
        item
    }));
    // Saved profiles fill the Saved Profiles table. Each profile appears
    // exactly once, under the type it was saved as: an SSH profile's SFTP
    // launch (and the reverse) is offered from the row menu instead of as a
    // second row, so the table's length matches the number of things the
    // user actually saved.
    let profiles_start = items.len();
    items.extend(profiles.iter().map(|profile| {
        let (kind, type_label, location) = match profile {
            Profile::Local(local) => (
                LauncherItemKind::LocalProfile(profile.identifier()),
                "Local",
                local
                    .working_directory()
                    .map(|directory| directory.display().to_string())
                    .unwrap_or_else(|| local.executable().to_owned()),
            ),
            Profile::Ssh(ssh) => match ssh.profile_kind() {
                RemoteProfileKind::Ssh => (
                    LauncherItemKind::SshProfile(profile.identifier()),
                    "SSH",
                    ssh.host().to_owned(),
                ),
                RemoteProfileKind::Sftp => (
                    LauncherItemKind::SftpProfile(profile.identifier()),
                    "SFTP",
                    ssh.host().to_owned(),
                ),
            },
            Profile::Serial(serial) => (
                LauncherItemKind::SerialProfile(profile.identifier()),
                "Serial",
                serial.device().to_owned(),
            ),
        };
        LauncherItem {
            label: profile.identifier().to_owned(),
            description: format!("{type_label} · {location}"),
            kind,
            type_label,
            location,
            last_used_unix_seconds: configuration.profile_last_used(profile.identifier()),
        }
    }));
    let state_id = launcher_state_id(tab_id);
    let mut state = ui.data(|data| data.get_temp::<LauncherState>(state_id).unwrap_or_default());
    state.selected = state.selected.min(items.len().saturating_sub(1));

    if state.local_open {
        state
            .local
            .initialize_default(configuration.interface_settings().prefer_powershell());
        let mut command = None;
        let mut back_clicked = false;
        show_bounded_content_scroll(ui, (tab_id, "local_connection_surface"), |ui| {
            ui.vertical(|ui| {
                ui.add_space(24.0);
                ui.horizontal(|ui| {
                    ui.add_space(34.0);
                    ui.vertical(|ui| {
                        if ssh_back_button(ui).clicked() {
                            back_clicked = true;
                        }
                        ui.add_space(8.0);
                        ui.label(
                            egui::RichText::new("Start Local Shell")
                                .size(24.0)
                                .color(theme::TEXT_PRIMARY),
                        );
                        ui.label(
                            egui::RichText::new(
                                "Choose the shell and optional initial working directory.",
                            )
                            .size(11.0)
                            .color(theme::TEXT_SECONDARY),
                        );
                        if !back_clicked {
                            command = show_local_form(ui, tab_id, &mut state.local);
                        }
                    });
                });
            });
        });
        if back_clicked || ui.input(|input| input.key_pressed(egui::Key::Escape)) {
            state.local_open = false;
        }
        ui.data_mut(|data| data.insert_temp(state_id, state));
        return command;
    }

    if state.ssh_open {
        let mut command = None;
        let mut back_clicked = false;
        show_bounded_content_scroll(ui, (tab_id, "ssh_connection_surface"), |ui| {
            ui.vertical(|ui| {
                ui.add_space(24.0);
                ui.horizontal(|ui| {
                    ui.add_space(34.0);
                    ui.vertical(|ui| {
                        if ssh_back_button(ui).clicked() {
                            back_clicked = true;
                        }
                        ui.add_space(8.0);
                        ui.label(
                            egui::RichText::new("Connect with SSH")
                                .size(24.0)
                                .color(theme::TEXT_PRIMARY),
                        );
                        ui.label(
                            egui::RichText::new(
                                "Enter the destination now. Authentication is requested when the server needs it.",
                            )
                                .size(11.0)
                                .color(theme::TEXT_SECONDARY),
                        );
                        if !back_clicked {
                            command = show_ssh_form(
                                ui,
                                tab_id,
                                &mut state.ssh,
                                Some(configuration),
                                native_store_available,
                            );
                        }
                    });
                });
            });
        });
        if back_clicked {
            state.ssh_open = false;
        }
        ui.data_mut(|data| data.insert_temp(state_id, state));
        return command;
    }

    if state.sftp_open {
        let mut command = None;
        let mut back_clicked = false;
        show_bounded_content_scroll(ui, (tab_id, "sftp_connection_surface"), |ui| {
            ui.vertical(|ui| {
                ui.add_space(24.0);
                ui.horizontal(|ui| {
                    ui.add_space(34.0);
                    ui.vertical(|ui| {
                        if ssh_back_button(ui).clicked() {
                            back_clicked = true;
                        }
                        ui.add_space(8.0);
                        ui.label(
                            egui::RichText::new("Connect with SFTP")
                                .size(24.0)
                                .color(theme::TEXT_PRIMARY),
                        );
                        ui.label(
                            egui::RichText::new("Enter connection details.")
                                .size(11.0)
                                .color(theme::TEXT_SECONDARY),
                        );
                        if !back_clicked {
                            command =
                                show_sftp_form(ui, tab_id, &mut state.sftp, native_store_available);
                        }
                    });
                });
            });
        });
        if back_clicked {
            state.sftp_open = false;
        }
        ui.data_mut(|data| data.insert_temp(state_id, state));
        return command;
    }

    if state.serial_open {
        let mut command = None;
        let mut back_clicked = false;
        show_bounded_content_scroll(ui, (tab_id, "serial_connection_surface"), |ui| {
            ui.vertical(|ui| {
                ui.add_space(24.0);
                ui.horizontal(|ui| {
                    ui.add_space(34.0);
                    ui.vertical(|ui| {
                        if ssh_back_button(ui).clicked() {
                            back_clicked = true;
                        }
                        ui.add_space(8.0);
                        ui.label(
                            egui::RichText::new("Open Serial Port")
                                .size(24.0)
                                .color(theme::TEXT_PRIMARY),
                        );
                        ui.label(
                            egui::RichText::new("Select a device and configure line settings.")
                                .size(11.0)
                                .color(theme::TEXT_SECONDARY),
                        );
                        ui.add_space(16.0);

                        // Device field with discovered-port picker
                        ui.label("Device");
                        ui.text_edit_singleline(&mut state.serial.device);
                        if !state.serial.discovered_ports.is_empty() {
                            ui.add_space(4.0);
                            ui.label(
                                egui::RichText::new("Available ports:")
                                    .size(11.0)
                                    .color(theme::TEXT_SECONDARY),
                            );
                            for port in &state.serial.discovered_ports {
                                let port_label = match port.description() {
                                    Some(desc) => {
                                        format!("{} — {}", port.identifier(), desc)
                                    }
                                    None => port.identifier().to_owned(),
                                };
                                if ui
                                    .selectable_label(
                                        state.serial.device == port.identifier(),
                                        &port_label,
                                    )
                                    .clicked()
                                {
                                    state.serial.device = port.identifier().to_owned();
                                }
                            }
                        }
                        if ui.button("Refresh").clicked() {
                            state.serial.discovered_ports =
                                festerm_serial::discover_ports().unwrap_or_default();
                        }

                        ui.add_space(8.0);
                        ui.label("Baud rate");
                        ui.text_edit_singleline(&mut state.serial.baud_rate);

                        ui.add_space(8.0);
                        serial_enum_combo(ui, "Data bits", &mut state.serial.data_bits);
                        serial_enum_combo(ui, "Parity", &mut state.serial.parity);
                        serial_enum_combo(ui, "Stop bits", &mut state.serial.stop_bits);
                        serial_enum_combo(ui, "Flow control", &mut state.serial.flow_control);

                        if let Some(feedback) = &state.serial.feedback {
                            ui.add_space(8.0);
                            ui.colored_label(theme::STATUS_ERROR, feedback.as_str());
                        }

                        ui.add_space(12.0);
                        if ui.button("Open").clicked() && !back_clicked {
                            let device = state.serial.device.trim();
                            let baud: Result<u32, _> = state.serial.baud_rate.trim().parse();
                            match baud {
                                Ok(baud) if baud > 0 && !device.is_empty() => {
                                    match festerm_serial::LineSettings::new(
                                        device,
                                        baud,
                                        state.serial.data_bits.into(),
                                        state.serial.parity.into(),
                                        state.serial.stop_bits.into(),
                                        state.serial.flow_control.into(),
                                    ) {
                                        Ok(settings) => {
                                            command =
                                                Some(AppCommand::StartSerialSession { settings });
                                        }
                                        Err(error) => {
                                            state.serial.feedback = Some(error.to_string());
                                        }
                                    }
                                }
                                Ok(_) => {
                                    state.serial.feedback =
                                        Some("Device and baud rate are required".to_owned());
                                }
                                Err(_) => {
                                    state.serial.feedback =
                                        Some("Baud rate must be a positive number".to_owned());
                                }
                            }
                        }
                    });
                });
            });
        });
        if back_clicked {
            state.serial_open = false;
        }
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            state.serial_open = false;
        }
        ui.data_mut(|data| data.insert_temp(state_id, state));
        return command;
    }

    let form_has_focus = ssh_form_has_focus(ui, tab_id);
    let cycle_forward = !form_has_focus
        && ui.input(|i| {
            i.key_pressed(egui::Key::ArrowDown)
                || (i.key_pressed(egui::Key::Tab) && !i.modifiers.shift)
        });
    let cycle_backward = !form_has_focus
        && ui.input(|i| {
            i.key_pressed(egui::Key::ArrowUp)
                || (i.key_pressed(egui::Key::Tab) && i.modifiers.shift)
        });
    if cycle_forward {
        state.selected = (state.selected + 1) % items.len();
    }
    if cycle_backward {
        state.selected = (state.selected + items.len() - 1) % items.len();
    }
    let launch_via_keyboard = !form_has_focus && ui.input(|i| i.key_pressed(egui::Key::Enter));

    // Ordering and filtering are applied to indices rather than to `items`
    // so the keyboard's selection index keeps meaning the same entry no
    // matter how the table is sorted or searched.
    let query = state.profile_search.trim().to_lowercase();
    let mut profile_order: Vec<usize> = (profiles_start..items.len())
        .filter(|index| {
            let item = &items[*index];
            query.is_empty()
                || item.label.to_lowercase().contains(&query)
                || item.location.to_lowercase().contains(&query)
                || item.type_label.to_lowercase().contains(&query)
        })
        .collect();
    match state.profile_sort {
        // Never-launched profiles sort after every launched one, then
        // alphabetically, so the tail of the list stays stable instead of
        // shuffling as unrelated profiles are used.
        ProfileSortOrder::RecentlyUsed => profile_order.sort_by(|left, right| {
            items[*right]
                .last_used_unix_seconds
                .cmp(&items[*left].last_used_unix_seconds)
                .then_with(|| items[*left].label.cmp(&items[*right].label))
        }),
        ProfileSortOrder::Name => {
            profile_order.sort_by(|left, right| items[*left].label.cmp(&items[*right].label));
        }
    }

    let mut command = None;
    ui.vertical(|ui| {
        ui.add_space(LAUNCH_CARD_GAP);
        let top = ui.cursor().top();
        let height = (content_viewport_bottom(ui) - top).max(0.0);
        // The surface is inset from the window's content edge on both sides
        // so the cards and panels read as objects on a background rather
        // than as panes bolted to the frame.
        let surface_rect = egui::Rect::from_min_size(
            ui.cursor().min + vec2(LAUNCHER_SURFACE_MARGIN, 0.0),
            vec2(
                (ui.available_width() - LAUNCHER_SURFACE_MARGIN * 2.0).max(0.0),
                height,
            ),
        );
        // Below the stacking threshold the two panels are laid out one above
        // the other: side by side they would each be too narrow for their
        // own columns, and a horizontal scrollbar would hide the right-hand
        // panel entirely.
        let side_by_side =
            surface_rect.width() >= LAUNCHER_PANEL_MIN_WIDTH * 2.0 + LAUNCHER_PANEL_GAP;
        ui.scope_builder(egui::UiBuilder::new().max_rect(surface_rect), |ui| {
            configure_content_scrollbar(ui);
            let body =
                |ui: &mut Ui, state: &mut LauncherState, command: &mut Option<AppCommand>| {
                    ui.spacing_mut().item_spacing = egui::Vec2::ZERO;
                    let content_width = ui.available_width();

                    show_launch_card_row(
                        ui,
                        content_width,
                        compact_cards,
                        customize_local_shell,
                        &items[..fixed_end],
                        state,
                        command,
                    );
                    ui.add_space(16.0);

                    // Side by side, the panels fill the rest of the surface so
                    // their footers sit on one line at the bottom edge rather
                    // than floating under short content, and so each panel knows
                    // the height it has to scroll its own list inside. Stacked,
                    // each panel is only as tall as it needs to be.
                    let panel_height = side_by_side.then(|| {
                        (height - (ui.cursor().top() - top) - LAUNCHER_SURFACE_MARGIN).max(0.0)
                    });
                    let (profiles_width, sessions_width) = if side_by_side {
                        let usable = content_width - LAUNCHER_PANEL_GAP;
                        let profiles = (usable * LAUNCHER_PROFILES_PANEL_SHARE).floor();
                        (profiles, usable - profiles)
                    } else {
                        (content_width, content_width)
                    };

                    if side_by_side {
                        ui.horizontal_top(|ui| {
                            // The panels are columns: without an explicit
                            // top-down layout they would inherit the row's
                            // left-to-right flow and lay their own contents out
                            // sideways.
                            ui.spacing_mut().item_spacing.x = 0.0;
                            show_saved_profiles_panel(
                                ui,
                                profiles_width,
                                panel_height,
                                &items,
                                &profile_order,
                                now_unix_seconds,
                                state,
                                command,
                            );
                            ui.add_space(LAUNCHER_PANEL_GAP);
                            show_running_sessions_panel(
                                ui,
                                sessions_width,
                                panel_height,
                                &items,
                                fixed_end,
                                festerm_sessions_end,
                                tmux_sessions_end,
                                profiles_start,
                                now_unix_seconds,
                                state,
                                command,
                            );
                        });
                    } else {
                        show_saved_profiles_panel(
                            ui,
                            profiles_width,
                            panel_height,
                            &items,
                            &profile_order,
                            now_unix_seconds,
                            state,
                            command,
                        );
                        ui.add_space(LAUNCHER_PANEL_GAP);
                        show_running_sessions_panel(
                            ui,
                            sessions_width,
                            panel_height,
                            &items,
                            fixed_end,
                            festerm_sessions_end,
                            tmux_sessions_end,
                            profiles_start,
                            now_unix_seconds,
                            state,
                            command,
                        );
                        ui.add_space(20.0);
                    }
                };
            if side_by_side {
                // The launch cards stay put. They are this surface's primary
                // actions, so they must not scroll out from under a user who
                // is reading a long profile list; each panel scrolls its own
                // list instead.
                body(ui, &mut state, &mut command);
            } else {
                ScrollArea::vertical()
                    .id_salt((tab_id, "launcher_surface"))
                    .max_height(height)
                    .show(ui, |ui| {
                        ui.set_max_width((ui.available_width() - CONTENT_SCROLLBAR_LANE).max(0.0));
                        body(ui, &mut state, &mut command);
                    });
            }
        });
        if let Some(status) = secure_storage_status {
            ui.add_space(8.0);
            ui.colored_label(egui::Color32::from_rgb(220, 150, 80), status);
        }
    });

    if command.is_none() && launch_via_keyboard {
        if matches!(items[state.selected].kind, LauncherItemKind::LocalDefault) {
            if customize_local_shell {
                state.local_open = true;
                state.local.focus_working_directory = true;
            } else {
                command = Some(AppCommand::StartLocalSession);
            }
        } else if matches!(items[state.selected].kind, LauncherItemKind::NewSsh) {
            state.ssh_open = true;
            state.ssh.focus_username = true;
        } else if matches!(items[state.selected].kind, LauncherItemKind::NewSftp) {
            state.sftp_open = true;
            state.sftp.focus_username = true;
        } else if matches!(items[state.selected].kind, LauncherItemKind::NewSerial) {
            state.serial_open = true;
        } else {
            command = Some(items[state.selected].command());
        }
    }
    // The four fixed entries open an in-tab form rather than dispatching a
    // command, so a card click is translated here for the same reason the
    // keyboard path above translates it: `command()` has no form variant.
    if let Some(opened) = state.pending_form.take() {
        match opened {
            LauncherForm::Local => {
                if customize_local_shell {
                    state.local_open = true;
                    state.local.focus_working_directory = true;
                } else {
                    command = Some(AppCommand::StartLocalSession);
                }
            }
            LauncherForm::Ssh => {
                state.ssh_open = true;
                state.ssh.focus_username = true;
            }
            LauncherForm::Sftp => {
                state.sftp_open = true;
                state.sftp.focus_username = true;
            }
            LauncherForm::Serial => state.serial_open = true,
        }
    }

    ui.data_mut(|data| data.insert_temp(state_id, state));
    command
}

/// Which in-tab connection form a launch card asked to open.
#[derive(Clone, Copy)]
enum LauncherForm {
    Local,
    Ssh,
    Sftp,
    Serial,
}

/// Lays the launch cards out in equal columns, wrapping to as many rows as
/// the available width needs.
fn show_launch_card_row(
    ui: &mut Ui,
    width: f32,
    compact: bool,
    customize_local_shell: bool,
    cards: &[LauncherItem<'_>],
    state: &mut LauncherState,
    command: &mut Option<AppCommand>,
) {
    let columns = (((width + LAUNCH_CARD_GAP) / (LAUNCH_CARD_MIN_WIDTH + LAUNCH_CARD_GAP)).floor()
        as usize)
        .clamp(1, cards.len().max(1));
    let card_width =
        ((width - LAUNCH_CARD_GAP * (columns.saturating_sub(1)) as f32) / columns as f32).max(0.0);
    for (row_index, row) in cards.chunks(columns).enumerate() {
        if row_index > 0 {
            ui.add_space(LAUNCH_CARD_GAP);
        }
        ui.horizontal_top(|ui| {
            for (column, item) in row.iter().enumerate() {
                if column > 0 {
                    ui.add_space(LAUNCH_CARD_GAP);
                }
                let index = row_index * columns + column;
                let response = show_launch_card(
                    ui,
                    vec2(
                        card_width,
                        if compact {
                            LAUNCH_CARD_COMPACT_HEIGHT
                        } else {
                            LAUNCH_CARD_HEIGHT
                        },
                    ),
                    compact,
                    item,
                    index == state.selected,
                );
                if response.clicked() {
                    match item.kind {
                        LauncherItemKind::LocalDefault => {
                            if customize_local_shell {
                                state.pending_form = Some(LauncherForm::Local);
                            } else {
                                *command = Some(AppCommand::StartLocalSession);
                            }
                        }
                        LauncherItemKind::NewSsh => state.pending_form = Some(LauncherForm::Ssh),
                        LauncherItemKind::NewSftp => state.pending_form = Some(LauncherForm::Sftp),
                        LauncherItemKind::NewSerial => {
                            state.pending_form = Some(LauncherForm::Serial);
                        }
                        _ => *command = Some(item.command()),
                    }
                }
            }
        });
    }
}

/// The Saved Profiles panel: search and sort controls, a column-aligned
/// table of every saved profile, and the two profile-management actions.
#[allow(clippy::too_many_arguments)]
fn show_saved_profiles_panel(
    ui: &mut Ui,
    width: f32,
    height: Option<f32>,
    items: &[LauncherItem<'_>],
    order: &[usize],
    now_unix_seconds: Option<u64>,
    state: &mut LauncherState,
    command: &mut Option<AppCommand>,
) {
    ui.scope_builder(
        egui::UiBuilder::new().layout(egui::Layout::top_down(egui::Align::Min)),
        |ui| {
            egui::Frame::new()
                .fill(theme::SURFACE_PANEL)
                .corner_radius(LAUNCHER_PANEL_CORNER)
                .inner_margin(egui::Margin::symmetric(0, LAUNCHER_PANEL_TOP_MARGIN))
                .show(ui, |ui| {
                    ui.set_width(width);
                    if let Some(height) = height {
                        ui.set_min_height(
                            (height - LAUNCHER_PANEL_TOP_MARGIN as f32 * 2.0).max(0.0),
                        );
                    }
                    let inner = (width - LAUNCHER_PANEL_PADDING * 2.0).max(0.0);
                    ui.vertical(|ui| {
                        ui.add_space(0.0);
                        // Placed from the cursor rather than nested inside a
                        // horizontal row: an extra layout between the panel
                        // and its heading shifted this title down relative to
                        // Running Sessions, which calls the heading directly.
                        let heading = egui::Rect::from_min_size(
                            ui.cursor().min + vec2(LAUNCHER_PANEL_PADDING, 0.0),
                            vec2(inner, LAUNCHER_PANEL_HEADING_HEIGHT),
                        );
                        ui.scope_builder(egui::UiBuilder::new().max_rect(heading), |ui| {
                            show_panel_heading(
                                ui,
                                Icon::SavedProfiles,
                                "Saved Profiles",
                                None,
                                |ui| {
                                    if launcher_icon_button(
                                        ui,
                                        Icon::SortOrder,
                                        state.profile_sort.label(),
                                    )
                                    .clicked()
                                    {
                                        state.profile_sort = state.profile_sort.toggled();
                                    }
                                    ui.add_space(8.0);
                                    let field_width = (ui.available_width() - 28.0).max(80.0);
                                    show_profile_search_field(
                                        ui,
                                        field_width,
                                        &mut state.profile_search,
                                    );
                                },
                            );
                        });
                        ui.add_space(8.0);

                        show_profile_column_headers(
                            ui,
                            width,
                            inner,
                            ProfileTableOptions::launcher(width),
                        );
                        let selected = state.selected;
                        let search_is_empty = state.profile_search.trim().is_empty();
                        let rows = |ui: &mut Ui, command: &mut Option<AppCommand>| {
                            if order.is_empty() {
                                ui.add_space(12.0);
                                ui.horizontal(|ui| {
                                    ui.add_space(LAUNCHER_PANEL_PADDING);
                                    ui.label(
                                        egui::RichText::new(if search_is_empty {
                                            "No saved profiles yet."
                                        } else {
                                            "No profiles match this search."
                                        })
                                        .size(LAUNCHER_BODY_TEXT_SIZE)
                                        .color(theme::TEXT_MUTED),
                                    );
                                });
                            }
                            for index in order {
                                let item = items[*index].profile_table_item();
                                let row = show_profile_row(
                                    ui,
                                    width,
                                    &item,
                                    *index == selected,
                                    now_unix_seconds,
                                    ProfileTableOptions::launcher(width),
                                    ProfileTableMenu::Launcher,
                                );
                                if let Some(action) = row.action {
                                    *command = Some(match action {
                                        ProfileTableAction::Connect => item.connect_command(),
                                        ProfileTableAction::LauncherCrossover(command) => *command,
                                        ProfileTableAction::Edit => AppCommand::OpenProfileEditor {
                                            identifier: item.identifier.clone(),
                                        },
                                        ProfileTableAction::OpenSftpFileManager
                                        | ProfileTableAction::Duplicate
                                        | ProfileTableAction::Delete => {
                                            unreachable!(
                                                "launcher profile rows do not expose this action"
                                            )
                                        }
                                    });
                                }
                            }
                        };

                        // The list scrolls inside the panel rather than
                        // scrolling the whole surface, so the launch cards
                        // above and this panel's own footer stay in place
                        // however many profiles are saved.
                        match height {
                            Some(height) => {
                                let list_height = (height
                                    - LAUNCHER_PANEL_TOP_MARGIN as f32 * 2.0
                                    - ui.min_rect().height()
                                    - LAUNCHER_FOOTER_HEIGHT
                                    - LAUNCHER_FOOTER_GAP)
                                    .max(0.0);
                                configure_content_scrollbar(ui);
                                ScrollArea::vertical()
                                    .id_salt("launcher_profile_list")
                                    .max_height(list_height)
                                    .min_scrolled_height(list_height)
                                    .auto_shrink([false, false])
                                    .show(ui, |ui| {
                                        ui.set_width(width);
                                        rows(ui, command);
                                    });
                                ui.add_space(LAUNCHER_FOOTER_GAP);
                            }
                            None => {
                                rows(ui, command);
                                ui.add_space(LAUNCHER_FOOTER_GAP);
                            }
                        }
                        ui.horizontal(|ui| {
                            ui.add_space(LAUNCHER_PANEL_PADDING);
                            if launcher_button(ui, Icon::Settings, "Manage Profiles…", None, false)
                                .clicked()
                            {
                                *command = Some(AppCommand::OpenProfiles);
                            }
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.add_space(LAUNCHER_PANEL_PADDING);
                                    // New Profile has to say *what* it is
                                    // creating, otherwise it would land on
                                    // the same list as Manage Profiles and
                                    // the two controls would be one control
                                    // wearing two labels.
                                    let new_profile = launcher_dropdown_button(
                                        ui,
                                        Icon::NewProfile,
                                        "New Profile",
                                        None,
                                        true,
                                    );
                                    egui::Popup::menu(&new_profile).show(|ui| {
                                        for (label, kind) in [
                                            ("Local Shell", NewProfileKind::Local),
                                            ("SSH", NewProfileKind::Ssh),
                                            ("SFTP", NewProfileKind::Sftp),
                                            ("Serial", NewProfileKind::Serial),
                                        ] {
                                            if ui.button(label).clicked() {
                                                *command = Some(AppCommand::CreateProfile { kind });
                                                ui.close();
                                            }
                                        }
                                    });
                                },
                            );
                        });
                    });
                });
        },
    );
}

/// Paints the table's column headers from the same column origins the rows
/// use, so the two can never drift apart.
fn show_profile_column_headers(ui: &mut Ui, width: f32, inner: f32, options: ProfileTableOptions) {
    let _ = inner;
    let (rect, _) = ui.allocate_exact_size(vec2(width, 24.0), Sense::hover());
    for (index, heading) in ["Name", "Type", "Host / Path", "Last Used"]
        .into_iter()
        .enumerate()
    {
        let galley = elided_galley(
            ui,
            heading,
            LAUNCHER_DETAIL_TEXT_SIZE,
            theme::TEXT_MUTED,
            width * 0.2,
            1,
        );
        let left = rect.left() + width * options.column_origins[index];
        let top = rect.center().y - galley.size().y / 2.0;
        ui.painter()
            .galley(egui::pos2(left, top), galley, theme::TEXT_MUTED);
    }
}

/// The Running Sessions panel: one disclosure group per durable-session
/// provider that currently has something to reattach.
#[allow(clippy::too_many_arguments)]
fn show_running_sessions_panel(
    ui: &mut Ui,
    width: f32,
    height: Option<f32>,
    items: &[LauncherItem<'_>],
    fixed_end: usize,
    festerm_sessions_end: usize,
    tmux_sessions_end: usize,
    profiles_start: usize,
    now_unix_seconds: Option<u64>,
    state: &mut LauncherState,
    command: &mut Option<AppCommand>,
) {
    let inner_width = (width - LAUNCHER_PANEL_TOP_MARGIN as f32 * 2.0).max(0.0);
    ui.scope_builder(
        egui::UiBuilder::new().layout(egui::Layout::top_down(egui::Align::Min)),
        |ui| {
            egui::Frame::new()
                .fill(theme::SURFACE_PANEL)
                .corner_radius(LAUNCHER_PANEL_CORNER)
                .inner_margin(egui::Margin::same(LAUNCHER_PANEL_TOP_MARGIN))
                .show(ui, |ui| {
                    ui.set_width(inner_width);
                    if let Some(height) = height {
                        ui.set_min_height(
                            (height - LAUNCHER_PANEL_TOP_MARGIN as f32 * 2.0).max(0.0),
                        );
                    }
                    ui.spacing_mut().item_spacing.y = 8.0;
                    // Measured from the cursor rather than from `min_rect`,
                    // which `set_min_height` above has already stretched to
                    // the panel's full height.
                    let content_top = ui.cursor().top();
                    show_panel_heading(
                        ui,
                        Icon::RunningSessions,
                        "Running Sessions",
                        Some("Local sessions available to reattach"),
                        |ui| {
                            if launcher_icon_button(ui, Icon::Refresh, "Refresh").clicked() {
                                *command = Some(AppCommand::RefreshRunningSessions);
                            }
                        },
                    );
                    let ranges = [
                        ("fesTerm Native (sessiond)", fixed_end, festerm_sessions_end),
                        ("tmux", festerm_sessions_end, tmux_sessions_end),
                        ("screen", tmux_sessions_end, profiles_start),
                    ];
                    // The expansion flags travel through the closure as one
                    // array rather than as three borrows of `state`, so the
                    // body can be called from either the scrolled or the
                    // unscrolled branch below.
                    let mut expanded = [
                        state.festerm_group_expanded,
                        state.tmux_group_expanded,
                        state.screen_group_expanded,
                    ];
                    let selected = state.selected;
                    let body = |ui: &mut Ui,
                                expanded: &mut [bool; 3],
                                command: &mut Option<AppCommand>| {
                        let mut any = false;
                        for (index, (title, start, end)) in ranges.into_iter().enumerate() {
                            if start >= end {
                                continue;
                            }
                            any = true;
                            show_session_group(
                                ui,
                                inner_width,
                                title,
                                &items[start..end],
                                start,
                                selected,
                                &mut expanded[index],
                                now_unix_seconds,
                                command,
                            );
                        }
                        if !any {
                            ui.label(
                                egui::RichText::new(
                                    "Nothing is running locally that can be reattached right now.",
                                )
                                .size(LAUNCHER_BODY_TEXT_SIZE)
                                .color(theme::TEXT_MUTED),
                            );
                        }
                    };
                    // As in Saved Profiles, a long list scrolls inside this
                    // panel so the launch cards above it stay on screen.
                    match height {
                        Some(height) => {
                            let list_height = (height
                                - LAUNCHER_PANEL_TOP_MARGIN as f32 * 2.0
                                - (ui.cursor().top() - content_top))
                                .max(0.0);
                            configure_content_scrollbar(ui);
                            ScrollArea::vertical()
                                .id_salt("launcher_running_sessions_list")
                                .max_height(list_height)
                                .min_scrolled_height(list_height)
                                .auto_shrink([false, false])
                                .show(ui, |ui| {
                                    ui.set_width(inner_width);
                                    ui.spacing_mut().item_spacing.y = 8.0;
                                    body(ui, &mut expanded, command);
                                });
                        }
                        None => body(ui, &mut expanded, command),
                    }
                    [
                        state.festerm_group_expanded,
                        state.tmux_group_expanded,
                        state.screen_group_expanded,
                    ] = expanded;
                });
        },
    );
}

/// Shared scaffolding for rendering a restored workspace tab (SSH or SFTP)
/// without creating a transport.
///
/// The destination metadata is copied into the existing transient form only
/// once (via `prefill_once`, which itself checks and updates the relevant
/// "already prefilled" flag). Passwords, keys, and host trust remain absent
/// until the user enters them and explicitly submits the form (rendered by
/// `render_form`).
fn show_restored_authentication_required(
    ui: &mut Ui,
    tab_id: TabId,
    heading: &str,
    destination_description: &str,
    prefill_once: impl FnOnce(&mut LauncherState),
    render_form: impl FnOnce(&mut Ui, &mut LauncherState) -> Option<AppCommand>,
) -> Option<AppCommand> {
    let state_id = launcher_state_id(tab_id);
    let mut state = ui.data(|data| data.get_temp::<LauncherState>(state_id).unwrap_or_default());
    prefill_once(&mut state);

    let command = ui
        .vertical(|ui| {
            ui.add_space(24.0);
            ui.heading(heading);
            ui.label(destination_description);
            ui.label(
                "This workspace restored destination metadata only. Enter fresh authentication \
                 below to connect; no prior connection, credential, or host trust was restored.",
            );
            render_form(ui, &mut state)
        })
        .inner;

    ui.data_mut(|data| data.insert_temp(state_id, state));
    command
}

/// Renders a restored SSH workspace tab without creating a transport.
///
/// The destination metadata is copied into the existing transient form only
/// once. Passwords, keys, and host trust remain absent until the user enters
/// them and explicitly submits the form.
pub fn show_ssh_authentication_required(
    ui: &mut Ui,
    tab_id: TabId,
    profile: &SshProfileConfiguration,
    native_store_available: bool,
) -> Option<AppCommand> {
    show_restored_authentication_required(
        ui,
        tab_id,
        "SSH authentication required",
        &format!(
            "Restored SSH destination: {}@{}:{}",
            profile.username(),
            profile.host(),
            profile.port()
        ),
        |state| {
            if !state.ssh_profile_prefilled {
                state.ssh.prefill_saved_profile(profile);
                state.ssh_profile_prefilled = true;
            }
        },
        |ui, state| show_ssh_form(ui, tab_id, &mut state.ssh, None, native_store_available),
    )
}

/// Renders a restored SFTP workspace tab without creating a transport.
pub fn show_sftp_authentication_required(
    ui: &mut Ui,
    tab_id: TabId,
    profile: &SshProfileConfiguration,
    native_store_available: bool,
) -> Option<AppCommand> {
    show_restored_authentication_required(
        ui,
        tab_id,
        "SFTP authentication required",
        &format!(
            "Restored SFTP destination: {}@{}:{}",
            profile.username(),
            profile.host(),
            profile.port()
        ),
        |state| {
            if !state.sftp_profile_prefilled {
                state.sftp.prefill_restored_sftp_profile(profile);
                state.sftp_profile_prefilled = true;
            }
        },
        |ui, state| show_sftp_form(ui, tab_id, &mut state.sftp, native_store_available),
    )
}

/// Ephemeral per-tab state for the live-session in-terminal password prompt
/// below: only a transient input buffer and the most recently seen prompt
/// attempt, so a fresh attempt (e.g. after a rejected retry) claims focus
/// and starts from an empty field. Following the same `ui.data_mut`/
/// `insert_temp` pattern as `show_ssh_form`'s state, since resolving the
/// prompt is owned entirely by the already-connected SSH worker — no
/// persistent UI struct is needed on `SessionTab`.
#[derive(Clone, Default)]
struct SshLivePasswordPromptState {
    password: String,
    last_attempt: Option<u8>,
    /// Completed prompt/outcome lines from earlier attempts on this same
    /// connection, appended to (never replaced) so failed retries scroll
    /// upward exactly like a real terminal transcript.
    history: Vec<String>,
}

/// Renders a blinking block cursor at roughly the rate real terminal
/// emulators use (~530ms), requesting a repaint so the blink keeps
/// animating a pty-styled prompt that has no live terminal view underneath
/// it to drive redraws on its own.
pub(crate) fn pty_cursor_glyph(ui: &Ui) -> &'static str {
    const BLINK_INTERVAL_SECS: f64 = 0.53;
    let time = ui.input(|input| input.time);
    let visible = (time / BLINK_INTERVAL_SECS) as i64 % 2 == 0;
    ui.ctx()
        .request_repaint_after(std::time::Duration::from_secs_f64(BLINK_INTERVAL_SECS));
    if visible {
        "█"
    } else {
        " "
    }
}

/// Renders the openssh-style, in-terminal password prompt for a session
/// that is already connected (host key already verified) and now waiting
/// for a password, mimicking `ssh`'s own `user@host's password:` line
/// instead of collecting a credential before a connection exists.
///
/// Submitting dispatches `AppCommand::ResolveSshPassword`, feeding the
/// value directly into the live worker rather than starting a fresh
/// connection.
pub fn show_ssh_live_password_prompt(
    ui: &mut Ui,
    tab_id: TabId,
    prompt: &PasswordPrompt,
) -> Option<AppCommand> {
    if !festerm_ui_egui::terminal_fonts_installed(ui.ctx()) {
        // Mirrors `TerminalView`'s own guard: the named terminal font
        // family only becomes usable after egui rebuilds its atlas at the
        // next pass boundary, so skip laying out text with it this frame.
        festerm_ui_egui::install_terminal_fonts(ui.ctx());
        ui.ctx().request_repaint();
        return None;
    }
    let state_id = ui.id().with(("ssh_live_password_prompt", tab_id));
    let mut state: SshLivePasswordPromptState =
        ui.data_mut(|data| data.get_temp(state_id).unwrap_or_default());
    let prompt_line = format!("{}@{}'s password:", prompt.username(), prompt.host());
    if state.last_attempt != Some(prompt.attempt()) {
        // A new attempt arrived: fold the just-finished attempt's prompt
        // (and, if it was rejected, ssh's own "Permission denied" line)
        // into the growing transcript, then start the next attempt fresh.
        if state.last_attempt.is_some() {
            state.history.push(prompt_line.clone());
            if prompt.previous_attempt_failed() {
                state
                    .history
                    .push("Permission denied, please try again.".to_owned());
            }
        }
        state.last_attempt = Some(prompt.attempt());
        state.password.clear();
    }

    let font = festerm_ui_egui::terminal_font(festerm_ui_egui::DEFAULT_TERMINAL_FONT_SIZE);
    let mono = |text: String, color: egui::Color32| {
        egui::RichText::new(text).font(font.clone()).color(color)
    };
    let mut command = None;
    egui::Frame::new()
        .fill(theme::SURFACE_TERMINAL)
        .inner_margin(egui::Margin::same(16))
        .show(ui, |ui| {
            ui.set_min_size(ui.available_size());
            ui.label(mono(
                format!("ssh {}@{}", prompt.username(), prompt.host()),
                theme::TEXT_SECONDARY,
            ));
            ui.add_space(4.0);
            for line in &state.history {
                ui.label(mono(line.clone(), theme::TEXT_PRIMARY));
            }
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                ui.label(mono(prompt_line, theme::TEXT_PRIMARY));
                ui.label(mono(pty_cursor_glyph(ui).to_owned(), theme::TEXT_PRIMARY));
            });
        });

    // Keyboard-driven and unechoed, matching real `ssh`: no terminal view
    // or text field is shown this frame to compete for these keys, and
    // typed characters are captured without ever being reflected on
    // screen (not even as masked dots).
    let submitted = ui.ctx().input_mut(|input| {
        let mut submit = false;
        input.events.retain(|event| match event {
            egui::Event::Text(text) => {
                state.password.push_str(text);
                false
            }
            egui::Event::Key {
                key: egui::Key::Backspace,
                pressed: true,
                ..
            } => {
                state.password.pop();
                false
            }
            egui::Event::Key {
                key: egui::Key::Enter,
                pressed: true,
                ..
            } => {
                submit = true;
                false
            }
            _ => true,
        });
        submit
    });
    if submitted {
        let password = std::mem::take(&mut state.password);
        command = Some(AppCommand::ResolveSshPassword {
            tab: tab_id,
            password,
        });
    }

    ui.data_mut(|data| data.insert_temp(state_id, state));
    command
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tabs::AppState;
    use egui_kittest::{kittest::Queryable, Harness};

    struct LauncherHarnessState {
        tab_id: TabId,
        configuration: Configuration,
        command: Option<AppCommand>,
    }

    fn harness() -> Harness<'static, LauncherHarnessState> {
        harness_with_profiles(Vec::new())
    }

    fn harness_with_profiles(profiles: Vec<Profile>) -> Harness<'static, LauncherHarnessState> {
        harness_with_profiles_and_grid(profiles, false, 1240.0)
    }

    fn harness_with_configuration(
        configuration: Configuration,
        width: f32,
    ) -> Harness<'static, LauncherHarnessState> {
        Harness::builder()
            .with_size(egui::vec2(width, 880.0))
            .build_ui_state(
                move |ui, state: &mut LauncherHarnessState| {
                    if let Some(command) = show_launcher(
                        ui,
                        state.tab_id,
                        &state.configuration,
                        true,
                        None,
                        false,
                        &[],
                        &[],
                        &[],
                    ) {
                        state.command = Some(command);
                    }
                },
                LauncherHarnessState {
                    tab_id: AppState::for_test().active(),
                    configuration,
                    command: None,
                },
            )
    }

    fn harness_with_profiles_and_grid(
        profiles: Vec<Profile>,
        compact_launcher_grid: bool,
        width: f32,
    ) -> Harness<'static, LauncherHarnessState> {
        harness_with_profiles_grid_and_resumable(profiles, compact_launcher_grid, width, Vec::new())
    }

    fn harness_with_profiles_grid_and_resumable(
        profiles: Vec<Profile>,
        compact_launcher_grid: bool,
        width: f32,
        resumable_sessions: Vec<festerm_sessiond::UnattachedSession>,
    ) -> Harness<'static, LauncherHarnessState> {
        let configuration = Configuration::new(profiles).expect("test configuration is valid");
        Harness::builder()
            .with_size(egui::vec2(width, 880.0))
            .build_ui_state(
                move |ui, state: &mut LauncherHarnessState| {
                    if let Some(command) = show_launcher(
                        ui,
                        state.tab_id,
                        &state.configuration,
                        true,
                        None,
                        compact_launcher_grid,
                        &resumable_sessions,
                        &[],
                        &[],
                    ) {
                        state.command = Some(command);
                    }
                },
                LauncherHarnessState {
                    tab_id: AppState::for_test().active(),
                    configuration,
                    command: None,
                },
            )
    }

    fn harness_with_multiplexer_sessions(
        profiles: Vec<Profile>,
        tmux_sessions: Vec<crate::multiplexer_sessions::MultiplexerSession>,
        screen_sessions: Vec<crate::multiplexer_sessions::MultiplexerSession>,
    ) -> Harness<'static, LauncherHarnessState> {
        let configuration = Configuration::new(profiles).expect("test configuration is valid");
        Harness::builder()
            .with_size(egui::vec2(1240.0, 880.0))
            .build_ui_state(
                move |ui, state: &mut LauncherHarnessState| {
                    if let Some(command) = show_launcher(
                        ui,
                        state.tab_id,
                        &state.configuration,
                        true,
                        None,
                        false,
                        &[],
                        &tmux_sessions,
                        &screen_sessions,
                    ) {
                        state.command = Some(command);
                    }
                },
                LauncherHarnessState {
                    tab_id: AppState::for_test().active(),
                    configuration,
                    command: None,
                },
            )
    }

    fn wait_for_launcher_suggestion(
        harness: &mut Harness<'static, LauncherHarnessState>,
        label: &str,
    ) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        loop {
            harness.run();
            if harness.query_by_label(label).is_some() {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for launcher suggestion {label:?}"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    #[test]
    fn local_shell_form_autocompletes_working_directory_and_returns_typed_profile() {
        let root = std::env::temp_dir().join(format!(
            "festerm-launcher-directory-autocomplete-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let expected_path = root.join("workspace-alpha");
        std::fs::create_dir_all(&expected_path).expect("test directory can be created");

        let configuration = Configuration::empty()
            .with_interface_settings(
                festerm_config::InterfaceSettings::DEFAULT.with_customize_local_shell(true),
            )
            .expect("custom local shell setting is valid");
        let mut harness = harness_with_configuration(configuration, 1240.0);
        harness.run();
        harness
            .get_by_label("Local Shell — Start a local terminal session")
            .click();
        harness.run();
        assert!(harness
            .get_by_label("Working directory (optional)")
            .is_focused());

        harness
            .get_by_label("Working directory (optional)")
            .type_text(&root.join("workspace").display().to_string());
        let expected_label = expected_path.display().to_string();
        wait_for_launcher_suggestion(&mut harness, &expected_label);
        harness
            .get_by_role_and_label(accesskit::Role::Button, &expected_label)
            .click_accesskit();
        harness.run();
        harness.get_by_label("Start").click();
        harness.run();

        let Some(AppCommand::StartLocalSessionWithProfile { profile }) =
            harness.state().command.as_ref()
        else {
            panic!("the Local Shell form must return a typed local profile command");
        };
        assert_eq!(profile.working_directory(), Some(expected_path.as_path()));
        drop(harness);
        std::fs::remove_dir_all(root).expect("test directory can be removed");
    }

    #[test]
    fn local_shell_starts_immediately_by_default() {
        let mut harness = harness();
        harness.run();

        harness
            .get_by_label("Local Shell — Start a local terminal session")
            .click();
        harness.run();

        assert!(matches!(
            harness.state().command,
            Some(AppCommand::StartLocalSession)
        ));
        assert!(harness
            .query_by_label("Working directory (optional)")
            .is_none());
    }

    #[test]
    fn customized_local_shell_form_focuses_a_field_and_enter_submits_the_last_field() {
        let configuration = Configuration::empty()
            .with_interface_settings(
                festerm_config::InterfaceSettings::DEFAULT.with_customize_local_shell(true),
            )
            .expect("custom local shell setting is valid");
        let mut harness = harness_with_configuration(configuration, 1240.0);
        harness.run();

        harness
            .get_by_label("Local Shell — Start a local terminal session")
            .click();
        harness.run();
        assert!(harness
            .get_by_label("Working directory (optional)")
            .is_focused());

        harness.key_press(egui::Key::Enter);
        harness.run();

        assert!(matches!(
            harness.state().command,
            Some(AppCommand::StartLocalSessionWithProfile { .. })
        ));
    }

    fn populated_launcher_harness(
        width: f32,
        compact: bool,
    ) -> Harness<'static, LauncherHarnessState> {
        use crate::multiplexer_sessions::MultiplexerSession;

        let mut profiles = Vec::new();
        for index in 1..=3 {
            profiles.extend([
                Profile::local(
                    format!("Local development {index}"),
                    "/bin/zsh",
                    Vec::new(),
                    None,
                )
                .expect("review profile is valid"),
                Profile::ssh(
                    format!("Production server {index}"),
                    format!("server-{index}.development.example.com"),
                    22,
                    "deploy",
                    "xterm-256color",
                    100,
                    40,
                )
                .expect("review profile is valid"),
                Profile::sftp(
                    format!("Project files {index}"),
                    "artifacts.example.com",
                    22,
                    "build",
                    true,
                )
                .expect("review profile is valid"),
                Profile::serial(
                    format!("Network switch {index}"),
                    format!("/dev/tty.usbserial-{index}"),
                    115_200,
                    festerm_config::SerialDataBits::Eight,
                    festerm_config::SerialParity::None,
                    festerm_config::SerialStopBits::One,
                    festerm_config::SerialFlowControl::None,
                )
                .expect("review profile is valid"),
            ]);
        }
        let native =
            ["dev-work", "build-process"].map(|name| festerm_sessiond::UnattachedSession {
                pid: 123,
                endpoint: String::new(),
                name: name.to_owned(),
                shell: "/bin/zsh".to_owned(),
                arguments: Vec::new(),
                working_directory: None,
                created_at_unix_ms: u128::from(
                    unix_now_seconds()
                        .expect("review clock is after the Unix epoch")
                        .saturating_sub(2 * 60 * 60),
                ) * 1000,
            });
        let tmux = [MultiplexerSession {
            name: "research".to_owned(),
            match_key: "research".to_owned(),
            attached: false,
            started_at_unix_seconds: None,
        }];
        let screen = [MultiplexerSession {
            name: "legacy".to_owned(),
            match_key: "12345.legacy".to_owned(),
            attached: true,
            started_at_unix_seconds: None,
        }];
        Harness::builder()
            .with_size(vec2(width, 880.0))
            .build_ui_state(
                move |ui, state: &mut LauncherHarnessState| {
                    ui.ctx().set_visuals(theme::default_visuals());
                    if let Some(command) = show_launcher(
                        ui,
                        state.tab_id,
                        &state.configuration,
                        true,
                        None,
                        compact,
                        &native,
                        &tmux,
                        &screen,
                    ) {
                        state.command = Some(command);
                    }
                },
                LauncherHarnessState {
                    tab_id: AppState::for_test().active(),
                    configuration: Configuration::new(profiles)
                        .expect("review configuration is valid"),
                    command: None,
                },
            )
    }

    #[test]
    #[ignore = "manual GUI density review capture"]
    fn capture_populated_launcher_for_density_review() {
        let output = std::env::temp_dir().join("festerm-gui-review");
        let mut snapshots = egui_kittest::SnapshotResults::new();
        for width in [1240.0, 1000.0, 800.0, 560.0] {
            for compact in [false, true] {
                let mut harness = populated_launcher_harness(width, compact);
                harness.run();
                let mode = if compact { "compact" } else { "roomy" };
                harness.snapshot_options(
                    format!("launcher-populated-{width}-{mode}"),
                    &egui_kittest::SnapshotOptions::default().output_path(&output),
                );
                snapshots.extend(harness.take_snapshot_results());
            }
        }
        snapshots.unwrap();
    }

    fn enter_text(harness: &mut Harness<'static, LauncherHarnessState>, label: &str, text: &str) {
        harness.get_by_label(label).click();
        harness.run();
        harness.get_by_label(label).type_text(text);
        harness.run();
    }

    fn open_ssh_form(harness: &mut Harness<'static, LauncherHarnessState>) {
        harness
            .get_by_label("SSH — Connect to a remote host over SSH")
            .click();
        harness.run();
    }

    /// Switches the destination pane from its default `user@host:port`
    /// field to the separate Username/Host/Port fields, for tests that
    /// address those fields individually.
    fn use_separate_destination_fields(harness: &mut Harness<'static, LauncherHarnessState>) {
        harness.get_by_label("Use separate fields").click();
        harness.run();
    }

    fn generated_openssh_private_key() -> String {
        let mut random = russh::keys::key::safe_rng();
        let key = russh::keys::PrivateKey::random(&mut random, russh::keys::Algorithm::Ed25519)
            .expect("could not generate test SSH key");
        key.to_openssh(russh::keys::ssh_key::LineEnding::LF)
            .expect("could not encode test SSH key")
            .to_string()
    }

    fn generated_encrypted_openssh_private_key() -> (String, String) {
        let passphrase = "test encrypted-key passphrase".to_owned();
        let mut random = russh::keys::key::safe_rng();
        let key = russh::keys::PrivateKey::random(&mut random, russh::keys::Algorithm::Ed25519)
            .expect("could not generate encrypted test SSH key");
        let encrypted = key
            .encrypt(&mut random, &passphrase)
            .expect("could not encrypt test SSH key");
        (
            encrypted
                .to_openssh(russh::keys::ssh_key::LineEnding::LF)
                .expect("could not encode encrypted test SSH key")
                .to_string(),
            passphrase,
        )
    }

    fn generated_openssh_certificate() -> String {
        concat!(
            "ssh-ed25519-cert-v01@openssh.com ",
            "AAAAIHNzaC1lZDI1NTE5LWNlcnQtdjAxQG9wZW5zc2guY29tAAAAIP4NAgsSLiXrpfby",
            "oGxrF9cZN7UiLq2EJ80DswScilD2AAAAIHRw0uaN2ad8NjeXNK5BLHkiXmMpuUjwYVr5",
            "+9/9Rnz2AAAAAAAAAAAAAAABAAAACXRlc3QtY2VydAAAAA0AAAAJdGVzdC11c2VyAAAA",
            "AAAAAAD//////////wAAAAAAAACCAAAAFXBlcm1pdC1YMTEtZm9yd2FyZGluZwAAAAAA",
            "AAAXcGVybWl0LWFnZW50LWZvcndhcmRpbmcAAAAAAAAAFnBlcm1pdC1wb3J0LWZvcndh",
            "cmRpbmcAAAAAAAAACnBlcm1pdC1wdHkAAAAAAAAADnBlcm1pdC11c2VyLXJjAAAAAAAA",
            "AAAAAAAzAAAAC3NzaC1lZDI1NTE5AAAAINmASn24MXIM7yAjzI0wX348PDFGJo5lhB0F",
            "6R99e8jvAAAAUwAAAAtzc2gtZWQyNTUxOQAAAECrYKMa8P+MB7+swJDRQ2t0++H73Vih",
            "OwvkL2nqJ+N2JIo9WVnInnZvEr81Pi1q9LTcupbTegAQCgFqhB34vrMK",
            " fes@fes-m4.feslabs.com"
        )
        .to_owned()
    }

    #[test]
    fn opening_the_ssh_form_focuses_the_leading_destination_field() {
        let mut harness = harness();
        harness.run();
        open_ssh_form(&mut harness);

        assert!(
            harness.get_by_label("Quick connect").is_focused(),
            "the single user@host:port field leads the SSH form's default notation, so it \
             must take the initial keyboard focus"
        );

        harness.get_by_label("Use separate fields").click();
        harness.run();

        assert!(
            harness.query_by_label("Quick connect").is_none(),
            "toggling to the separate fields must replace the one-line field, not join it"
        );
        assert!(
            harness.get_by_label("Username").is_focused(),
            "focus must follow the toggle onto the newly leading field"
        );
    }

    #[test]
    fn ssh_form_orders_fields_username_host_then_port_and_prefills_port_with_22() {
        // Pins the field order and that Port is prefilled with the actual
        // default value (not left empty with "(default: 22)"-style wording).
        // Username leads so the fields read in the same order as the
        // `user@host:port` Quick connect field above them.
        let mut harness = harness();
        harness.run();
        open_ssh_form(&mut harness);
        use_separate_destination_fields(&mut harness);

        let host_top = harness.get_by_label("Host").rect().top();
        let port_top = harness.get_by_label("Port").rect().top();
        let username_top = harness.get_by_label("Username").rect().top();

        assert!(
            (host_top - port_top).abs() < 2.0,
            "Host and Port must share a row on wide launchers"
        );
        assert!(
            username_top < host_top,
            "Username must be positioned above Host/Port"
        );

        assert!(
            harness.query_by_label("Port (optional)").is_none(),
            "the old 'Port (optional)' wording must not be present"
        );
        assert!(
            harness.query_by_label("Port (default: 22)").is_none(),
            "the old 'Port (default: 22)' wording must not be present"
        );
    }

    #[test]
    fn ssh_launcher_form_prefills_the_port_field_with_22() {
        assert_eq!(
            SshLauncherForm::default().port,
            "22",
            "the Port field must show the actual default value rather than staying empty"
        );
    }

    #[test]
    fn ssh_form_returns_a_typed_password_command_with_default_port() {
        let mut harness = harness();
        harness.run();
        open_ssh_form(&mut harness);
        use_separate_destination_fields(&mut harness);
        enter_text(&mut harness, "Host", "example.invalid");
        enter_text(&mut harness, "Username", "test-user");
        enter_text(&mut harness, "Password", "transient-test-password");

        harness.get_by_label("Connect").scroll_to_me();
        harness.run();
        harness.get_by_label("Connect").click();
        harness.run();

        let Some(AppCommand::StartSshSession {
            profile,
            authentication,
            options,
        }) = harness.state().command.as_ref()
        else {
            panic!("the valid SSH form must return a typed SSH command");
        };
        assert_eq!(profile.identity().host(), "example.invalid");
        assert_eq!(profile.identity().port(), 22);
        assert_eq!(profile.username(), "test-user");
        assert_eq!(
            format!("{authentication:?}"),
            "SshAuthentication::Password([REDACTED])"
        );
        assert_eq!(
            options.reconnect_policy(),
            None,
            "plain SSH sessions default to manual-only reconnect (ADR 0018)"
        );
    }

    #[test]
    fn ssh_launcher_defaults_to_the_squashed_destination_field() {
        let mut harness = harness();
        harness.run();
        harness
            .get_by_label("SSH — Connect to a remote host over SSH")
            .click();
        harness.run();

        assert!(
            harness.query_by_label("Quick connect").is_some(),
            "a freshly opened SSH launcher must show the squashed user@host:port field"
        );
        assert!(
            harness.query_by_label("Host").is_none(),
            "the two notations are alternatives: the separate fields must stay hidden \
             until the user asks for them"
        );
        assert!(
            harness.query_by_label("Use separate fields").is_some(),
            "the SSH launcher must still offer the separate destination fields"
        );
    }

    #[test]
    fn ssh_quick_connect_and_the_individual_fields_stay_in_step_in_both_directions() {
        let mut harness = harness();
        harness.run();
        open_ssh_form(&mut harness);

        enter_text(
            &mut harness,
            "Quick connect",
            "devuser@web-1.example.com:2222",
        );
        harness.get_by_label("Use separate fields").click();
        harness.run();
        assert_eq!(
            harness.get_by_label("Username").value().as_deref(),
            Some("devuser"),
            "typing a squashed destination must fill Username"
        );
        assert_eq!(
            harness.get_by_label("Host").value().as_deref(),
            Some("web-1.example.com"),
            "typing a squashed destination must fill Host"
        );
        assert_eq!(
            harness.get_by_label("Port").value().as_deref(),
            Some("2222"),
            "typing a squashed destination must fill Port"
        );

        enter_text(&mut harness, "Host", ".internal");
        harness.get_by_label("Use user@host:port").click();
        harness.run();
        assert_eq!(
            harness.get_by_label("Quick connect").value().as_deref(),
            Some("devuser@web-1.example.com.internal:2222"),
            "editing an individual field must recompose the Quick connect field"
        );
    }

    #[test]
    fn ssh_launcher_focuses_its_leading_destination_field_when_it_opens() {
        let mut harness = harness();
        harness.run();
        harness
            .get_by_label("SSH — Connect to a remote host over SSH")
            .click();
        harness.run();

        assert!(
            harness.get_by_label("Quick connect").is_focused(),
            "the preserved opening-focus affordance must land on the leading field"
        );
    }

    #[test]
    fn ssh_form_with_no_password_opens_the_in_terminal_password_prompt() {
        let mut harness = harness();
        harness.run();
        harness
            .get_by_label("SSH — Connect to a remote host over SSH")
            .click();
        harness.run();
        use_separate_destination_fields(&mut harness);

        enter_text(&mut harness, "Host", "10.1.2.3");
        enter_text(&mut harness, "Username", "fes");
        harness.run();
        harness.get_by_label("Connect").scroll_to_me();
        harness.run();
        harness.get_by_label("Connect").click();
        harness.run();

        let Some(AppCommand::StartSshSession {
            profile,
            authentication: SshAuthentication::Interactive,
            ..
        }) = harness.state().command.as_ref()
        else {
            panic!(
                "Quick Connect with no password must start an interactive (host-key-first) SSH session"
            );
        };
        assert_eq!(profile.username(), "fes");
        assert_eq!(profile.identity().host(), "10.1.2.3");
        assert_eq!(profile.identity().port(), 22);
    }

    #[test]
    fn sftp_launcher_defaults_to_quick_connect_not_the_advanced_form() {
        let mut harness = harness();
        harness.run();
        harness
            .get_by_label("SFTP — Browse and transfer files")
            .click();
        harness.run();

        assert!(
            harness.query_by_label("Quick connect").is_some(),
            "a freshly opened SFTP launcher must show the compact destination field"
        );
        assert!(
            harness.query_by_label("Username").is_none(),
            "the two notations are alternatives, so the separate fields must stay hidden \
             until the user toggles to them"
        );
        assert!(
            harness.query_by_label("Password").is_none(),
            "authentication must stay hidden until 'Show advanced settings' is checked"
        );
    }

    #[test]
    fn terminal_sftp_quick_connect_with_no_password_starts_an_interactive_session() {
        let mut harness = harness();
        harness.run();
        harness
            .get_by_label("SFTP — Browse and transfer files")
            .click();
        harness.run();

        harness
            .get_by_label("Quick connect")
            .type_text("fes@10.1.2.3:2222");
        harness.run();
        harness.get_by_label("Use graphical file manager").click();
        harness.run();
        harness.get_by_label("Connect").click();
        harness.run();

        let Some(AppCommand::StartSftpSession {
            profile,
            authentication: SshAuthentication::Interactive,
            ..
        }) = harness.state().command.as_ref()
        else {
            panic!("terminal SFTP quick connect must start an interactive SFTP session");
        };
        assert_eq!(profile.username(), "fes");
        assert_eq!(profile.identity().host(), "10.1.2.3");
        assert_eq!(profile.identity().port(), 2222);
    }

    #[test]
    fn ssh_form_uses_an_explicit_port() {
        let mut harness = harness();
        harness.run();
        harness
            .get_by_label("SSH — Connect to a remote host over SSH")
            .click();
        harness.run();
        use_separate_destination_fields(&mut harness);

        enter_text(&mut harness, "Host", "10.1.2.3");
        harness.get_by_label("Port").click();
        harness.run();
        harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
        harness.get_by_label("Port").type_text("2222");
        harness.run();
        enter_text(&mut harness, "Username", "fes");
        harness.get_by_label("Connect").click();
        harness.run();

        let Some(AppCommand::StartSshSession {
            profile,
            authentication: SshAuthentication::Interactive,
            ..
        }) = harness.state().command.as_ref()
        else {
            panic!("a valid quick-connect destination must start an interactive SSH session");
        };
        assert_eq!(profile.identity().port(), 2222);
    }

    #[test]
    fn ssh_form_can_attach_to_a_named_tmux_session() {
        let mut harness = harness();
        harness.run();
        harness
            .get_by_label("SSH — Connect to a remote host over SSH")
            .click();
        harness.run();

        harness.get_by_label("Use a durable remote session").click();
        harness.run();
        assert!(harness.query_by_label("tmux").is_some());
        assert!(harness.query_by_label("GNU screen").is_some());
        assert!(harness.query_by_label("Session name").is_some());
        assert!(harness
            .query_by_label("Automatically resume after connection loss")
            .is_some());

        let mut form = SshLauncherForm {
            quick_connect: "fes@10.1.2.3".to_owned(),
            durable_session: DurableSessionDraft {
                enabled: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let AppCommand::StartSshSession { options, .. } = form
            .submit_quick_connect()
            .expect("Quick Connect must return an SSH command")
        else {
            unreachable!("the SSH launcher only returns SSH commands");
        };
        assert_eq!(
            options.strategy(),
            SessionStrategy::Persistent {
                provider: festerm_ssh::PersistenceProvider::Tmux,
                session_name: festerm_ssh::PersistentSessionName::new("main").unwrap(),
            }
        );
    }

    #[test]
    fn advanced_ssh_launch_attaches_validated_port_forwards_to_session_options() {
        let mut form = SshLauncherForm {
            host: "ssh.example.test".to_owned(),
            username: "deploy".to_owned(),
            port_forwards: vec![SshPortForwardDraft {
                direction: SshPortForwardDirection::Remote,
                bind_host: "127.0.0.1".to_owned(),
                bind_port: "18080".to_owned(),
                destination_host: "app.internal".to_owned(),
                destination_port: "8080".to_owned(),
            }],
            ..Default::default()
        };

        let AppCommand::StartSshSession { options, .. } =
            form.submit().expect("valid SSH launch should succeed")
        else {
            panic!("advanced SSH launch must return a session command");
        };
        let expected_forward = SshPortForwardConfiguration::new(
            SshPortForwardDirection::Remote,
            "127.0.0.1",
            18080,
            "app.internal",
            8080,
        )
        .expect("expected forward should validate");
        let expected = SshSessionOptions::new()
            .with_profile_port_forwards([&expected_forward])
            .expect("expected options should validate");
        assert_eq!(options, expected);
    }

    #[test]
    fn advanced_ssh_launch_rejects_invalid_port_forwards_before_connecting() {
        let mut form = SshLauncherForm {
            host: "ssh.example.test".to_owned(),
            username: "deploy".to_owned(),
            port_forwards: vec![SshPortForwardDraft {
                bind_port: "0".to_owned(),
                destination_host: "app.internal".to_owned(),
                destination_port: "8080".to_owned(),
                ..Default::default()
            }],
            ..Default::default()
        };

        assert_eq!(
            form.submit().unwrap_err(),
            "SSH port forwards must use non-empty, safe bind and destination hosts with nonzero ports"
        );
    }

    #[test]
    fn stored_credential_launch_preserves_advanced_port_forward_edits() {
        let profile = Profile::ssh(
            "production",
            "ssh.example.test",
            22,
            "deploy",
            "xterm-256color",
            80,
            24,
        )
        .expect("profile should validate");
        let mut form = SshLauncherForm::default();
        form.prefill_saved_profile(profile.as_ssh().expect("profile remains SSH"));
        form.port_forwards.push(SshPortForwardDraft {
            bind_port: "18080".to_owned(),
            destination_host: "app.internal".to_owned(),
            destination_port: "8080".to_owned(),
            ..Default::default()
        });

        let AppCommand::StartStoredPasswordSshProfile {
            profile_id,
            options,
        } = form
            .submit_stored_credential()
            .expect("stored credential launch should validate")
        else {
            panic!("stored credential launch must retain its session options");
        };
        let expected_forward = SshPortForwardConfiguration::new(
            SshPortForwardDirection::Local,
            "127.0.0.1",
            18080,
            "app.internal",
            8080,
        )
        .expect("expected forward should validate");
        let expected_options = SshSessionOptions::new()
            .with_profile_port_forwards([&expected_forward])
            .expect("expected options should validate");
        assert_eq!(profile_id, "production");
        assert_eq!(options, expected_options);
    }

    #[test]
    fn quick_connect_rejects_an_invalid_durable_session_name_before_launch() {
        let mut form = SshLauncherForm {
            quick_connect: "fes@10.1.2.3".to_owned(),
            durable_session: DurableSessionDraft {
                enabled: true,
                session_name: "not valid".to_owned(),
                ..Default::default()
            },
            ..Default::default()
        };

        assert_eq!(
            form.submit_quick_connect().unwrap_err(),
            "a persistent session name may only contain ASCII letters, digits, '-', '_', or '.', and must be 1-64 bytes"
        );
    }

    #[test]
    fn ssh_form_rejects_a_missing_host() {
        let mut harness = harness();
        harness.run();
        harness
            .get_by_label("SSH — Connect to a remote host over SSH")
            .click();
        harness.run();
        use_separate_destination_fields(&mut harness);

        enter_text(&mut harness, "Username", "fes");
        harness.get_by_label("Connect").click();
        harness.run();

        assert!(
            harness.state().command.is_none(),
            "an invalid quick-connect destination must not dispatch a command"
        );
        assert!(harness
            .query_by_label("SSH host must not be empty")
            .is_some());
    }

    #[test]
    fn advanced_settings_disclosure_reveals_port_forwards() {
        let mut harness = harness();
        harness.run();
        harness
            .get_by_label("SSH — Connect to a remote host over SSH")
            .click();
        harness.run();

        assert!(
            harness.query_by_label("Add port forward").is_none(),
            "port forwards must start hidden behind Advanced settings"
        );

        harness.get_by_label("Advanced settings").click();
        harness.run();

        assert!(
            harness.query_by_label("Add port forward").is_some(),
            "Advanced settings must reveal the Port forwards section"
        );
    }

    #[test]
    fn save_as_profile_opens_profiles_with_a_secret_free_ssh_draft() {
        let mut harness = harness();
        harness.run();
        open_ssh_form(&mut harness);
        use_separate_destination_fields(&mut harness);
        enter_text(&mut harness, "Host", "ssh.example.test");
        enter_text(&mut harness, "Username", "deploy");
        enter_text(&mut harness, "Password", "transient-secret");

        harness.get_by_label("Save as Profile…").click();
        harness.run();

        let Some(AppCommand::CreateSshProfileFromDraft { draft }) =
            harness.state().command.as_ref()
        else {
            panic!("Save as Profile must open the SSH profile editor with a draft");
        };
        assert_eq!(draft.name, "ssh.example.test");
        assert_eq!(draft.host, "ssh.example.test");
        assert_eq!(draft.port, "22");
        assert_eq!(draft.username, "deploy");
        assert!(
            !format!("{draft:?}").contains("transient-secret"),
            "live authentication secrets must not be copied into the profile draft"
        );
    }

    #[test]
    fn close_advanced_settings_carries_a_non_default_port_into_quick_connect() {
        let mut form = SshLauncherForm {
            username: "example".to_owned(),
            host: "169.254.1.1".to_owned(),
            port: "4096".to_owned(),
            advanced_open: true,
            ..Default::default()
        };

        form.close_advanced_settings();

        assert_eq!(form.quick_connect, "example@169.254.1.1:4096");
        assert!(!form.advanced_open);
    }

    #[test]
    fn close_advanced_settings_omits_the_default_port() {
        let mut form = SshLauncherForm {
            username: "example".to_owned(),
            host: "169.254.1.1".to_owned(),
            advanced_open: true,
            ..Default::default()
        };

        form.close_advanced_settings();

        assert_eq!(form.quick_connect, "example@169.254.1.1");
    }

    #[test]
    fn open_advanced_settings_parses_the_quick_connect_destination() {
        let mut form = SshLauncherForm {
            quick_connect: "example@169.254.1.1:4096".to_owned(),
            ..Default::default()
        };

        form.open_advanced_settings();

        assert_eq!(form.username, "example");
        assert_eq!(form.host, "169.254.1.1");
        assert_eq!(form.port, "4096");
        assert!(form.advanced_open);
    }

    #[test]
    fn open_advanced_settings_clears_stale_feedback() {
        let mut form = SshLauncherForm {
            feedback: Some("Enter a destination, e.g. user@host".to_owned()),
            ..Default::default()
        };

        form.open_advanced_settings();

        assert!(form.feedback.is_none());
    }

    #[test]
    fn close_advanced_settings_clears_stale_feedback() {
        let mut form = SshLauncherForm {
            feedback: Some("SSH host must not contain whitespace".to_owned()),
            advanced_open: true,
            ..Default::default()
        };

        form.close_advanced_settings();

        assert!(form.feedback.is_none());
    }

    #[test]
    fn toggling_advanced_settings_clears_form_feedback() {
        let mut harness = harness();
        harness.run();
        harness
            .get_by_label("SSH — Connect to a remote host over SSH")
            .click();
        harness.run();
        use_separate_destination_fields(&mut harness);

        enter_text(&mut harness, "Host", "invalid host");
        harness.get_by_label("Connect").click();
        harness.run();
        assert!(harness
            .query_by_label("SSH host must not contain whitespace")
            .is_some());

        harness.get_by_label("Advanced settings").click();
        harness.run();

        assert!(
            harness
                .query_by_label("SSH host must not contain whitespace")
                .is_none(),
            "stale form feedback must not survive a toggle of Advanced settings"
        );
    }

    #[test]
    fn closing_advanced_settings_clears_form_feedback() {
        let mut harness = harness();
        harness.run();
        open_ssh_form(&mut harness);
        use_separate_destination_fields(&mut harness);
        enter_text(&mut harness, "Host", "invalid host");

        harness.get_by_label("Connect").click();
        harness.run();
        assert!(harness
            .query_by_label("SSH host must not contain whitespace")
            .is_some());

        harness.get_by_label("Advanced settings").click();
        harness.run();
        harness.get_by_label("Advanced settings").click();
        harness.run();

        assert!(
            harness
                .query_by_label("SSH host must not contain whitespace")
                .is_none(),
            "stale form feedback must not survive closing Advanced settings"
        );
    }

    #[test]
    fn advanced_settings_collapses_again_even_while_its_close_animation_runs() {
        let mut harness = harness();
        // Test harnesses disable animation, which hides the real-app behaviour: while the
        // close animation runs the body is still rendered, so any state derived from the
        // body's presence re-opens the section every frame and it can never be closed.
        harness
            .ctx
            .all_styles_mut(|style| style.animation_time = 0.5);
        harness.run();
        open_ssh_form(&mut harness);

        harness.get_by_label("Advanced settings").click();
        harness.run();
        assert!(
            harness.query_by_label("Add port forward").is_some(),
            "expanding Advanced settings must reveal the port-forward controls"
        );

        harness.get_by_label("Advanced settings").click();
        harness.step();
        assert!(
            harness.query_by_label("Add port forward").is_some(),
            "this test is only meaningful while the body is still rendered mid-close; if \
             the animation now finishes within one frame the regression window is gone \
             and this test must be rewritten rather than deleted"
        );

        for _ in 0..10 {
            harness.run();
        }

        assert!(
            harness.query_by_label("Add port forward").is_none(),
            "clicking Advanced settings a second time must collapse it, even though the \
             body stays rendered while the close animation runs"
        );
    }

    #[test]
    fn advanced_form_with_an_empty_password_starts_an_interactive_session_instead_of_connecting() {
        let mut harness = harness();
        harness.run();
        open_ssh_form(&mut harness);
        use_separate_destination_fields(&mut harness);
        enter_text(&mut harness, "Host", "example.invalid");
        enter_text(&mut harness, "Username", "test-user");

        harness.get_by_label("Connect").click();
        harness.run();

        let Some(AppCommand::StartSshSession {
            profile,
            authentication: SshAuthentication::Interactive,
            ..
        }) = harness.state().command.as_ref()
        else {
            panic!(
                "submitting the advanced form with no password must start an interactive \
                 (host-key-first) session, not attempt to connect with no credential"
            );
        };
        assert_eq!(profile.username(), "test-user");
        assert_eq!(profile.identity().host(), "example.invalid");
    }

    #[test]
    fn ssh_form_never_offers_automatic_reconnect_for_a_plain_shell() {
        let mut harness = harness();
        harness.run();
        open_ssh_form(&mut harness);

        assert!(
            harness.query_by_label("Reconnect").is_none(),
            "a plain SSH session has no durable-session provider, so automatic \
             recovery is not offered (ADR 0018); only the manual Inspector \
             Reconnect action applies once connected"
        );
        assert!(
            harness
                .query_by_label("Automatically resume this session after a lost connection")
                .is_none(),
            "the automatic-recovery opt-in must not be offered without a durable-session provider"
        );
    }

    #[test]
    fn saved_ssh_profile_card_launches_directly_without_a_stored_credential() {
        let profile = Profile::ssh(
            "build",
            "ssh.example.test",
            2200,
            "deploy",
            "xterm-256color",
            100,
            40,
        )
        .expect("test profile is valid")
        .with_persistence(festerm_config::PersistenceProviderKind::Tmux, "build")
        .expect("persistence config is valid");
        let mut harness = harness_with_profiles(vec![profile]);
        harness.run();

        harness
            .get_by_label("build — SSH · ssh.example.test")
            .click();
        harness.run();

        assert!(matches!(
            harness.state().command,
            Some(AppCommand::StartConfiguredSshProfile { ref profile_id })
                if profile_id == "build"
        ));
    }

    #[test]
    fn saved_profiles_are_listed_below_the_launch_cards() {
        let profiles = vec![
            Profile::local("development", "cargo", vec!["run".to_owned()], None)
                .expect("test profile is valid"),
        ];
        let mut harness = harness_with_profiles(profiles);
        harness.run();

        let local_shell = harness
            .get_by_label("Local Shell — Start a local terminal session")
            .rect();
        let ssh = harness
            .get_by_label("SSH — Connect to a remote host over SSH")
            .rect();
        let profile = harness.get_by_label("development — Local · cargo").rect();

        assert!(
            local_shell.right() <= ssh.left(),
            "the launch cards sit side by side in one row, Local Shell first, got {local_shell:?} \
             and {ssh:?}"
        );
        assert!(
            ssh.bottom() <= profile.top(),
            "saved profiles sit below the launch card row, got {ssh:?} and {profile:?}"
        );
    }

    #[test]
    fn resumable_sessions_appear_in_the_running_sessions_panel_and_dispatch_resume() {
        // Feature request #70: unattached, locally running festerm-sessiond
        // sessions should surface as one-click reattach entries. They live in
        // the Running Sessions panel, grouped under the provider that owns
        // them, because reattaching differs per provider.
        let profiles = vec![
            Profile::local("development", "cargo", vec!["run".to_owned()], None)
                .expect("test profile is valid"),
        ];
        let resumable_sessions = vec![festerm_sessiond::UnattachedSession {
            pid: 123,
            endpoint: String::new(),
            name: "orphaned".to_owned(),
            shell: "/bin/bash".to_owned(),
            arguments: Vec::new(),
            working_directory: Some("/tmp".to_owned()),
            created_at_unix_ms: 0,
        }];
        let mut harness =
            harness_with_profiles_grid_and_resumable(profiles, false, 1240.0, resumable_sessions);
        harness.run();

        let cards_bottom = harness
            .get_by_label("SSH — Connect to a remote host over SSH")
            .rect()
            .bottom();
        let group_top = harness
            .get_by_label("fesTerm Native (sessiond)")
            .rect()
            .top();
        let resume_node = harness.get_by_label("Reattach orphaned");
        let profile_rect = harness.get_by_label("development — Local · cargo").rect();

        assert!(
            cards_bottom < group_top && group_top < resume_node.rect().top(),
            "reattachable sessions belong under their provider group, below the launch cards"
        );
        assert!(
            profile_rect.right() <= resume_node.rect().left(),
            "saved profiles and running sessions are side-by-side panels, profiles on the left"
        );

        resume_node.click();
        harness.run();

        assert!(matches!(
            &harness.state().command,
            Some(AppCommand::ResumeUnattachedSession { session }) if session.name == "orphaned"
        ));
    }

    #[test]
    fn tmux_and_screen_sessions_appear_in_their_own_labeled_widgets_and_dispatch_resume() {
        use crate::multiplexer_sessions::MultiplexerSession;

        let profiles = vec![
            Profile::local("development", "cargo", vec!["run".to_owned()], None)
                .expect("test profile is valid"),
        ];
        let tmux_sessions = vec![MultiplexerSession {
            name: "build".to_owned(),
            match_key: "build".to_owned(),
            attached: false,
            started_at_unix_seconds: None,
        }];
        let screen_sessions = vec![MultiplexerSession {
            name: "main".to_owned(),
            match_key: "12345.main".to_owned(),
            attached: true,
            started_at_unix_seconds: None,
        }];
        let mut harness =
            harness_with_multiplexer_sessions(profiles, tmux_sessions, screen_sessions);
        harness.run();

        let tmux_heading_top = harness.get_by_label("tmux").rect().top();
        let tmux_top = harness.get_by_label("Reattach build").rect().top();
        let screen_heading_top = harness.get_by_label("screen").rect().top();
        let screen_top = harness.get_by_label("Reattach main").rect().top();

        assert!(
            tmux_heading_top < tmux_top
                && tmux_top < screen_heading_top
                && screen_heading_top < screen_top,
            "each multiplexer keeps its own labelled group, tmux above GNU screen:              {tmux_heading_top} {tmux_top} {screen_heading_top} {screen_top}"
        );

        harness.get_by_label("Reattach build").click();
        harness.run();
        assert!(matches!(
            &harness.state().command,
            Some(AppCommand::ResumeMultiplexerSession { provider, session })
                if *provider == PersistenceProviderKind::Tmux
                    && session.match_key == "build"
                    && session.name == "build"
        ));

        harness.state_mut().command = None;
        harness.get_by_label("Reattach main").click();
        harness.run();
        assert!(
            matches!(
                &harness.state().command,
                Some(AppCommand::ResumeMultiplexerSession { provider, session })
                    if *provider == PersistenceProviderKind::Screen
                        && session.match_key == "12345.main"
                        && session.name == "main"
            ),
            "an already-attached screen session is still resumable, using its full pid.name \
             match key to reattach while still showing the friendly display name (not the \
             pid.name) as the resulting tab's label, since screen's matching is substring-based"
        );
    }

    #[test]
    fn saved_profiles_always_stack_one_row_per_profile() {
        // The Saved Profiles panel is a table, so every profile occupies a
        // full-width row whose columns line up with the ones above it. A
        // multi-column card grid (the original shape of feature request #64)
        // cannot do that, so the preference no longer changes this.
        let profiles = vec![
            Profile::local("alpha", "cargo", vec!["run".to_owned()], None)
                .expect("test profile is valid"),
            Profile::local("beta", "cargo", vec!["run".to_owned()], None)
                .expect("test profile is valid"),
        ];
        let mut harness = harness_with_profiles_and_grid(profiles, true, 1240.0);
        harness.run();

        let alpha_rect = harness.get_by_label("alpha — Local · cargo").rect();
        let beta_rect = harness.get_by_label("beta — Local · cargo").rect();

        assert!(
            alpha_rect.top() < beta_rect.top()
                && (alpha_rect.left() - beta_rect.left()).abs() < 1.0,
            "expected alpha above beta in one column of aligned rows"
        );
    }

    #[test]
    fn compact_new_session_layout_shortens_the_launch_cards() {
        // Feature request #64 asked for a denser New Session surface. The
        // preference trades card height for a smaller mark and tighter
        // padding; it keeps the description, because a card that only says
        // "SSH" does not tell a new user what activating it will do.
        let profiles = vec![
            Profile::local("alpha", "cargo", vec!["run".to_owned()], None)
                .expect("test profile is valid"),
        ];

        let mut roomy = harness_with_profiles_and_grid(profiles.clone(), false, 1240.0);
        roomy.run();
        let roomy_card = roomy
            .get_by_label("Local Shell — Start a local terminal session")
            .rect();
        let roomy_profile = roomy.get_by_label("alpha — Local · cargo").rect();

        let mut compact = harness_with_profiles_and_grid(profiles, true, 1240.0);
        compact.run();
        let compact_card = compact
            .get_by_label("Local Shell — Start a local terminal session")
            .rect();
        let compact_profile = compact.get_by_label("alpha — Local · cargo").rect();

        assert!(
            compact_card.height() < roomy_card.height(),
            "the compact layout must shrink the launch cards, got {compact_card:?} against \
             {roomy_card:?}"
        );
        assert!(
            compact_profile.top() < roomy_profile.top(),
            "shorter cards must pull the panels below them upward"
        );
    }

    #[test]
    fn launcher_marks_keep_the_mockups_relative_widths_at_card_and_row_sizes() {
        for height in [
            LAUNCHER_PROFILE_MARK_SIZE,
            LAUNCH_CARD_COMPACT_MARK_SIZE,
            LAUNCH_CARD_MARK_SIZE,
        ] {
            let local = session_mark_size(Icon::LocalTerminal, height);
            let ssh = session_mark_size(Icon::SshRemote, height);
            let serial = session_mark_size(Icon::Serial, height);
            assert_eq!(local, egui::Vec2::splat(height));
            assert_eq!(ssh.y, height);
            assert_eq!(serial, ssh);
            assert!((ssh.x / local.x - 1.3).abs() < 0.001);
            // The SSH composite is wider, but its terminal body remains
            // the same optical size as Local's 19.2-unit square.
            assert!((ssh.x * 14.8 / 24.0 - local.x * 19.2 / 24.0).abs() < 0.1);
        }
    }

    #[test]
    fn tighter_launcher_preserves_alignment_and_click_targets_at_desktop_widths() {
        for width in [860.0, 1000.0, 1240.0] {
            for compact in [false, true] {
                let mut harness = populated_launcher_harness(width, compact);
                harness.run();
                let saved = harness.get_by_label("Saved Profiles").rect();
                let running = harness.get_by_label("Running Sessions").rect();
                let search = harness.get_by_label("Search profiles…").rect();
                assert!((saved.center().y - running.center().y).abs() < 1.0);
                assert!((saved.center().y - search.center().y).abs() < 3.0);
                for label in [
                    "Manage Profiles…",
                    "New Profile",
                    "Reattach dev-work",
                    "Reattach research",
                    "Refresh",
                    "Sorted by last used",
                    "More actions for Production server 1",
                    "Collapse fesTerm Native (sessiond)",
                ] {
                    let rect = harness
                        .query_by_label(label)
                        .unwrap_or_else(|| panic!("{label} must be rendered at width {width}"))
                        .rect();
                    assert!(
                        rect.width() >= 24.0 && rect.height() >= 24.0,
                        "{label}: {rect:?}"
                    );
                    assert!(
                        rect.right() <= width && rect.bottom() <= 880.0,
                        "{label}: {rect:?}"
                    );
                }
                let row = harness
                    .get_by_label("Local development 1 — Local · /bin/zsh")
                    .rect();
                assert_eq!(row.height(), 34.0);
                assert_eq!(harness.get_by_label("New Profile").rect().height(), 32.0);
            }
        }
    }

    #[test]
    fn tighter_launcher_fits_all_five_cards_on_a_thousand_pixel_window() {
        let labels = [
            "Local Shell — Start a local terminal session",
            "SSH — Connect to a remote host over SSH",
            "SFTP — Browse and transfer files",
            "Serial — Connect to a serial device",
            "Markdown — Open a Markdown workspace",
        ];
        for compact in [false, true] {
            let mut harness = populated_launcher_harness(1000.0, compact);
            harness.run();
            let first = harness.get_by_label(labels[0]).rect();
            assert_eq!(first.height(), if compact { 96.0 } else { 112.0 });
            let mut previous_right = first.left();
            for label in labels {
                let rect = harness.get_by_label(label).rect();
                assert_eq!(rect.top(), first.top(), "{label} wrapped to another row");
                assert!(rect.left() >= previous_right && rect.right() <= 1000.0);
                previous_right = rect.right();
            }
        }
    }

    #[test]
    fn tighter_launcher_prioritizes_distinguishable_names_in_narrow_panels() {
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            let name = ui.painter().layout_no_wrap(
                "Local development 1".to_owned(),
                egui::FontId::proportional(LAUNCHER_BODY_TEXT_SIZE),
                theme::TEXT_PRIMARY,
            );
            for width in [480.0, 500.0, 560.0] {
                let columns = launcher_profile_columns(width);
                let name_width = width * (columns[1] - columns[0]) - LAUNCHER_COLUMN_GUTTER;
                assert!(name_width >= name.size().x);
                assert!(columns.windows(2).all(|pair| pair[0] < pair[1]));
            }
            assert_eq!(launcher_profile_columns(700.0), LAUNCHER_PROFILE_COLUMNS);
        });
        output.textures_delta.clear();
    }

    #[test]
    fn tighter_launcher_titles_fit_the_minimum_card_width() {
        let ctx = egui::Context::default();
        let painter = ctx.layer_painter(egui::LayerId::background());
        for (mark_size, padding) in [
            (LAUNCH_CARD_MARK_SIZE, LAUNCH_CARD_PADDING),
            (LAUNCH_CARD_COMPACT_MARK_SIZE, LAUNCH_CARD_COMPACT_PADDING),
        ] {
            for (icon, title) in [
                (Icon::LocalTerminal, "Local Shell"),
                (Icon::SshRemote, "SSH"),
                (Icon::FileTransfer, "SFTP"),
                (Icon::Serial, "Serial"),
                (Icon::MarkdownDocument, "Markdown"),
            ] {
                let available = LAUNCH_CARD_MIN_WIDTH
                    - padding * 2.0
                    - session_mark_size(icon, mark_size).x
                    - 10.0;
                // Font layout needs an initialized frame.
                let mut output = ctx.run_ui(egui::RawInput::default(), |_| {
                    let galley = painter.layout_no_wrap(
                        title.to_owned(),
                        egui::FontId::proportional(LAUNCH_CARD_TITLE_SIZE),
                        theme::TEXT_PRIMARY,
                    );
                    assert!(galley.size().x <= available, "{title} would elide");
                });
                output.textures_delta.clear();
            }
        }
    }

    #[test]
    fn a_launch_card_is_a_wide_strip_rather_than_a_tall_tile() {
        // Stacking the mark above the title cost a card a whole mark's worth
        // of height for no more information. The card row is a navigation
        // strip, not the surface's content, so the mark and title share a
        // line and the card stays short.
        let mut harness = harness_with_profiles(vec![Profile::local(
            "alpha",
            "cargo",
            vec!["run".to_owned()],
            None,
        )
        .expect("test profile is valid")]);
        harness.run();
        let card = harness
            .get_by_label("Local Shell — Start a local terminal session")
            .rect();
        assert!(
            card.width() > card.height() * 1.4,
            "expected a wide launch card rather than a tall tile, got {card:?}"
        );
    }

    #[test]
    fn both_panel_headings_share_one_line_with_their_own_controls() {
        // Running Sessions carries a subtitle and Saved Profiles does not.
        // Centring each heading block in its row therefore pushed the
        // subtitled title higher than the other, and the two panels read as
        // misaligned. Both now place mark, title, and controls on the same
        // line measured from the panel's top edge.
        let mut harness = harness_with_profiles(vec![Profile::local(
            "alpha",
            "cargo",
            vec!["run".to_owned()],
            None,
        )
        .expect("test profile is valid")]);
        harness.run();

        let profiles = harness.get_by_label("Saved Profiles").rect();
        let sessions = harness.get_by_label("Running Sessions").rect();
        assert!(
            (profiles.center().y - sessions.center().y).abs() < 3.0,
            "expected the two panel titles on one line, got {profiles:?} against {sessions:?}"
        );

        let search = harness.get_by_label("Search profiles…").rect();
        assert!(
            (search.center().y - profiles.center().y).abs() < 3.0,
            "expected the search text centred on the heading line, got {search:?} against \
             {profiles:?}"
        );
    }

    #[test]
    fn the_new_session_surface_is_inset_from_the_window_edge() {
        // Flush against the frame the cards read as panes bolted to the
        // window rather than as objects sitting on a background.
        let mut harness = harness_with_profiles(vec![Profile::local(
            "alpha",
            "cargo",
            vec!["run".to_owned()],
            None,
        )
        .expect("test profile is valid")]);
        harness.run();
        let card = harness
            .get_by_label("Local Shell — Start a local terminal session")
            .rect();
        assert!(
            card.left() >= LAUNCHER_SURFACE_MARGIN,
            "expected the launch cards inset from the window edge, got {card:?}"
        );
    }

    #[test]
    fn the_launch_cards_stay_in_place_while_a_long_profile_list_scrolls() {
        // The launch cards are this surface's primary actions, so a long
        // profile list has to scroll inside its own panel rather than
        // scrolling the cards and the panel footers off the top and bottom
        // of the window.
        let long: Vec<Profile> = (0..40)
            .map(|index| {
                Profile::local(
                    format!("profile-{index:02}"),
                    "cargo",
                    vec!["run".to_owned()],
                    None,
                )
                .expect("test profile is valid")
            })
            .collect();

        let mut short = harness_with_profiles(vec![Profile::local(
            "alpha",
            "cargo",
            vec!["run".to_owned()],
            None,
        )
        .expect("test profile is valid")]);
        short.run();
        let short_card = short
            .get_by_label("Local Shell — Start a local terminal session")
            .rect();
        let short_footer = short.get_by_label("Manage Profiles…").rect();

        let mut long = harness_with_profiles(long);
        long.run();
        let long_card = long
            .get_by_label("Local Shell — Start a local terminal session")
            .rect();
        let long_footer = long.get_by_label("Manage Profiles…").rect();

        assert!(
            (long_card.top() - short_card.top()).abs() < 1.0
                && (long_card.height() - short_card.height()).abs() < 1.0,
            "a long profile list must not move the launch cards, got {long_card:?} against \
             {short_card:?}"
        );
        assert!(
            (long_footer.top() - short_footer.top()).abs() < 1.0,
            "the Saved Profiles footer stays pinned to the panel's bottom edge, got \
             {long_footer:?} against {short_footer:?}"
        );
        assert!(
            long_footer.bottom() <= 880.0,
            "the footer must remain inside the window rather than being pushed below it, got \
             {long_footer:?}"
        );
    }

    #[test]
    fn the_two_panels_stack_when_the_window_is_too_narrow_for_two_columns() {
        // Side by side, each panel needs room for its own columns. Below
        // that threshold they stack instead, because two cramped columns
        // would clip their contents or force a horizontal scrollbar that
        // hides the right-hand panel entirely.
        let profiles = vec![
            Profile::local("alpha", "cargo", vec!["run".to_owned()], None)
                .expect("test profile is valid"),
        ];
        let resumable = vec![festerm_sessiond::UnattachedSession {
            pid: 123,
            endpoint: String::new(),
            name: "orphaned".to_owned(),
            shell: "/bin/bash".to_owned(),
            arguments: Vec::new(),
            working_directory: Some("/tmp".to_owned()),
            created_at_unix_ms: 0,
        }];

        let mut narrow =
            harness_with_profiles_grid_and_resumable(profiles.clone(), false, 560.0, resumable);
        narrow.run();

        let profile_rect = narrow.get_by_label("alpha — Local · cargo").rect();
        let session_rect = narrow.get_by_label("Reattach orphaned").rect();

        assert!(
            profile_rect.bottom() <= session_rect.top(),
            "expected the Running Sessions panel below the Saved Profiles panel in a narrow \
             window, got {profile_rect:?} and {session_rect:?}"
        );
    }

    #[test]
    fn local_profile_is_keyboard_accessible_and_returns_a_typed_command() {
        let profiles = vec![
            Profile::local("development", "cargo", vec!["run".to_owned()], None)
                .expect("test profile is valid"),
        ];
        let mut harness = harness_with_profiles(profiles);
        harness.run();

        assert!(harness
            .query_by_label("development — Local · cargo")
            .is_some());
        // Past the five launch cards to the first saved profile.
        for _ in 0..5 {
            harness.key_press(egui::Key::ArrowDown);
            harness.run();
        }
        harness.key_press(egui::Key::Enter);
        harness.run();

        assert!(matches!(
            harness.state().command,
            Some(AppCommand::StartConfiguredLocalProfile { ref profile_id })
                if profile_id == "development"
        ));
    }

    #[test]
    fn saved_ssh_profile_appears_as_a_launcher_card_without_password_ui() {
        let profiles = vec![Profile::ssh(
            "production",
            "ssh.example.test",
            2200,
            "deploy",
            "xterm-256color",
            100,
            40,
        )
        .expect("test profile is valid")];
        let mut harness = harness_with_profiles(profiles);
        harness.run();

        assert!(harness
            .query_by_label("production — SSH · ssh.example.test")
            .is_some());
        assert!(
            harness
                .query_by_label("Enter or replace password for production")
                .is_none(),
            "password entry belongs to the Profiles editor, not the Launcher"
        );
        assert!(harness.state().command.is_none());
    }

    #[test]
    fn searching_saved_profiles_filters_the_table_without_dispatching_a_command() {
        let profiles = vec![
            Profile::local("alpha", "cargo", vec!["run".to_owned()], None)
                .expect("test profile is valid"),
            Profile::local("beta", "npm", vec!["start".to_owned()], None)
                .expect("test profile is valid"),
        ];
        let mut harness = harness_with_profiles(profiles);
        harness.run();

        assert!(harness.query_by_label("beta — Local · npm").is_some());

        // Typing filters on name, type, and host/path alike.
        harness.get_by_label("Search profiles…").focus();
        harness.run();
        harness.get_by_label("Search profiles…").type_text("npm");
        harness.run();

        assert!(
            harness.query_by_label("alpha — Local · cargo").is_none(),
            "a profile matching neither name, type, nor host/path must drop out of the table"
        );
        assert!(harness.query_by_label("beta — Local · npm").is_some());
        assert!(
            harness.state().command.is_none(),
            "filtering is a view concern and must not dispatch an AppCommand"
        );
    }

    #[test]
    fn the_sort_toggle_switches_between_last_used_and_name_without_dispatching() {
        let profiles = vec![
            Profile::local("alpha", "cargo", vec!["run".to_owned()], None)
                .expect("test profile is valid"),
        ];
        let mut harness = harness_with_profiles(profiles);
        harness.run();

        // Recently used is the default, so never-launched profiles sort by
        // name after every launched one.
        harness.get_by_label("Sorted by last used").click();
        harness.run();

        assert!(harness.query_by_label("Sorted by name").is_some());
        assert!(harness.state().command.is_none());

        harness.get_by_label("Sorted by name").click();
        harness.run();
        assert!(harness.query_by_label("Sorted by last used").is_some());
    }

    #[test]
    fn a_running_session_group_collapses_and_expands_from_its_header() {
        use crate::multiplexer_sessions::MultiplexerSession;

        let tmux_sessions = vec![MultiplexerSession {
            name: "build".to_owned(),
            match_key: "build".to_owned(),
            attached: false,
            started_at_unix_seconds: None,
        }];
        let mut harness = harness_with_multiplexer_sessions(Vec::new(), tmux_sessions, Vec::new());
        harness.run();

        assert!(harness.query_by_label("Reattach build").is_some());

        harness.get_by_label("Collapse tmux").click();
        harness.run();

        assert!(
            harness.query_by_label("Reattach build").is_none(),
            "a collapsed group hides its rows but keeps its header and count"
        );
        assert!(harness.query_by_label("Expand tmux").is_some());
        assert!(harness.state().command.is_none());

        harness.get_by_label("Expand tmux").click();
        harness.run();
        assert!(harness.query_by_label("Reattach build").is_some());
    }

    #[test]
    fn the_running_sessions_refresh_control_dispatches_a_refresh_command() {
        let mut harness = harness_with_profiles(Vec::new());
        harness.run();

        harness.get_by_label("Refresh").click();
        harness.run();

        assert!(
            matches!(
                harness.state().command,
                Some(AppCommand::RefreshRunningSessions)
            ),
            "Refresh requests coalesced background inventory work through the command model"
        );
    }

    #[test]
    fn large_running_inventory_only_materializes_visible_rows() {
        let sessions = (0..5000)
            .map(|index| festerm_sessiond::UnattachedSession {
                name: format!("owned-{index:04}"),
                shell: "test-shell".into(),
                arguments: Vec::new(),
                working_directory: None,
                created_at_unix_ms: 0,
                pid: index + 1,
                endpoint: format!("generation-{index}"),
            })
            .collect();
        let mut harness =
            harness_with_profiles_grid_and_resumable(Vec::new(), false, 1240.0, sessions);
        harness.run();
        harness.get_by_label("Reattach owned-0000");
        assert!(harness.query_by_label("Reattach owned-4999").is_none());
        harness
            .get_by_label("Collapse fesTerm Native (sessiond)")
            .click();
        harness.run();
        assert!(harness.query_by_label("Reattach owned-0000").is_none());
    }

    #[test]
    fn manage_profiles_opens_the_profiles_surface() {
        let mut harness = harness_with_profiles(Vec::new());
        harness.run();

        harness.get_by_label("Manage Profiles…").click();
        harness.run();

        assert!(matches!(
            harness.state().command,
            Some(AppCommand::OpenProfiles)
        ));
    }

    #[test]
    fn a_markdown_launch_card_opens_a_markdown_workspace() {
        let mut harness = harness_with_profiles(Vec::new());
        harness.run();

        harness
            .get_by_label("Markdown — Open a Markdown workspace")
            .click();
        harness.run();

        assert!(
            matches!(
                harness.state().command,
                Some(AppCommand::OpenMarkdownWorkspace)
            ),
            "Markdown is one of the session types fesTerm can start from nothing, so it earns a \
             launch card alongside the other four"
        );
    }

    #[test]
    fn new_profile_asks_which_kind_rather_than_repeating_manage_profiles() {
        // New Profile and Manage Profiles... sit side by side, so New
        // Profile has to actually create something rather than landing on
        // the same list.
        let mut harness = harness_with_profiles(Vec::new());
        harness.run();

        harness.get_by_label("New Profile").click();
        harness.run();
        harness.run();
        harness.get_by_label("SFTP").click();
        harness.run();

        assert!(matches!(
            harness.state().command,
            Some(AppCommand::CreateProfile {
                kind: NewProfileKind::Sftp
            })
        ));
    }

    #[test]
    fn a_profile_rows_menu_connects_the_profile() {
        let profiles = vec![
            Profile::local("development", "cargo", vec!["run".to_owned()], None)
                .expect("test profile is valid"),
        ];
        let mut harness = harness_with_profiles(profiles);
        harness.run();

        harness.get_by_label("More actions for development").click();
        harness.run();
        harness.run();
        harness.get_by_label("Connect").click();
        harness.run();

        assert!(
            matches!(
                harness.state().command,
                Some(AppCommand::StartConfiguredLocalProfile { ref profile_id })
                    if profile_id == "development"
            ),
            "the row menu's Connect entry must take the same path as clicking the row"
        );
    }

    #[test]
    fn right_clicking_a_profile_row_opens_the_same_menu_as_its_overflow_control() {
        // The overflow control and a right-click are one affordance, so they
        // must offer the same entries rather than drifting apart.
        let profiles = vec![
            Profile::local("development", "cargo", vec!["run".to_owned()], None)
                .expect("test profile is valid"),
        ];
        let mut harness = harness_with_profiles(profiles);
        harness.run();

        harness
            .get_by_label("development — Local · cargo")
            .click_secondary();
        harness.run();
        harness.run();

        assert!(harness.query_by_label("Connect").is_some());
        assert!(harness.query_by_label("Edit").is_some());
    }

    #[test]
    fn an_sftp_profiles_row_menu_offers_an_ssh_connection() {
        let profiles = vec![
            Profile::sftp("files", "ssh.example.test", 22, "deploy", true)
                .expect("test profile is valid"),
        ];
        let mut harness = harness_with_profiles(profiles);
        harness.run();

        harness.get_by_label("More actions for files").click();
        harness.run();
        harness.run();
        harness.get_by_label("Connect SSH").click();
        harness.run();

        assert!(
            matches!(
                harness.state().command,
                Some(AppCommand::StartConfiguredSshProfile { ref profile_id })
                    if profile_id == "files"
            ),
            "an SFTP profile names the same host an SSH session would, so its row offers both"
        );
    }

    #[test]
    fn an_ssh_profiles_sftp_launch_is_offered_from_its_row_menu_not_a_second_row() {
        let profiles = vec![Profile::ssh(
            "production",
            "ssh.example.test",
            2200,
            "deploy",
            "xterm-256color",
            100,
            40,
        )
        .expect("test profile is valid")];
        let mut harness = harness_with_profiles(profiles);
        harness.run();

        assert_eq!(
            harness
                .get_all_by_label("production — SSH · ssh.example.test")
                .count(),
            1,
            "one saved profile must occupy exactly one row, whatever protocols it can serve"
        );

        harness.get_by_label("More actions for production").click();
        harness.run();
        harness.run();
        harness.get_by_label("Connect SFTP").click();
        harness.run();

        assert!(
            matches!(
                harness.state().command,
                Some(AppCommand::StartConfiguredSftpProfile { ref profile_id })
                    if profile_id == "production"
            ),
            "an SSH profile's SFTP launch must reuse that profile's identity from its row menu"
        );
    }

    #[test]
    fn saved_ssh_profile_card_dispatches_a_configured_launch_regardless_of_stored_credential() {
        let profile = Profile::ssh(
            "production",
            "ssh.example.test",
            2200,
            "deploy",
            "xterm-256color",
            100,
            40,
        )
        .expect("test profile is valid")
        .with_credential_reference(festerm_secret_store::SecretReference::generate())
        .expect("SSH profile accepts an opaque reference");
        let mut harness = harness_with_profiles(vec![profile]);
        harness.run();
        harness
            .get_by_label("production — SSH · ssh.example.test")
            .click();
        harness.run();

        assert!(matches!(
            harness.state().command,
            Some(AppCommand::StartConfiguredSshProfile { ref profile_id })
                if profile_id == "production"
        ));
    }

    #[test]
    fn clicking_a_saved_profiles_edit_icon_opens_its_editor_instead_of_launching() {
        let profile = Profile::ssh(
            "production",
            "ssh.example.test",
            2200,
            "deploy",
            "xterm-256color",
            100,
            40,
        )
        .expect("test profile is valid");
        let mut harness = harness_with_profiles(vec![profile]);
        harness.run();

        harness.get_by_label("More actions for production").click();
        harness.run();
        harness.get_by_label("Edit").click();
        harness.run();

        assert!(matches!(
            harness.state().command,
            Some(AppCommand::OpenProfileEditor { ref identifier })
                if identifier == "production"
        ));
    }

    #[test]
    fn launcher_list_stays_above_a_visible_status_bar_without_clipping_saved_profiles() {
        // Regression test: the item list previously had no height bound at
        // all, so saved profiles (and their edit icons) could silently run
        // into -- or past -- the bottom status bar instead of the list
        // staying above it (or scrolling, once there's too much content to
        // fit).
        let profiles: Vec<Profile> = (0..1)
            .map(|i| {
                Profile::ssh(
                    format!("host-{i}"),
                    "ssh.example.test",
                    22,
                    "deploy",
                    "xterm-256color",
                    100,
                    40,
                )
                .expect("test profile is valid")
            })
            .collect();
        let configuration = Configuration::new(profiles).expect("test configuration is valid");
        let mut harness = Harness::builder()
            .with_size(egui::vec2(1240.0, 880.0))
            .build_ui_state(
                |ui, state: &mut LauncherHarnessState| {
                    egui::Panel::bottom("status_bar")
                        .resizable(false)
                        .show_separator_line(false)
                        .show(ui, |ui| {
                            ui.set_min_height(24.0);
                            ui.set_max_height(24.0);
                        });
                    if let Some(command) = show_launcher(
                        ui,
                        state.tab_id,
                        &state.configuration,
                        true,
                        None,
                        false,
                        &[],
                        &[],
                        &[],
                    ) {
                        state.command = Some(command);
                    }
                },
                LauncherHarnessState {
                    tab_id: AppState::for_test().active(),
                    configuration,
                    command: None,
                },
            );
        harness.run();
        harness.run();

        let status_bar_top =
            egui::containers::panel::PanelState::load(&harness.ctx, egui::Id::new("status_bar"))
                .expect("status bar panel state should be recorded")
                .outer_rect
                .top();
        let last_row_menu_rect = harness.get_by_label("More actions for host-0").rect();
        assert!(
            last_row_menu_rect.max.y <= status_bar_top,
            "saved profile row controls must stay above the status bar rather than overlapping it"
        );

        harness.get_by_label("More actions for host-0").click();
        harness.run();
        harness.run();
        harness.get_by_label("Edit").click();
        harness.run();
        assert!(matches!(
            harness.state().command,
            Some(AppCommand::OpenProfileEditor { ref identifier })
                if identifier == "host-0"
        ));
    }

    #[test]
    fn restored_ssh_surface_focuses_the_password_field() {
        #[derive(Default)]
        struct RestoredSshHarnessState {
            tab_id: Option<TabId>,
            profile: Option<SshProfileConfiguration>,
            command: Option<AppCommand>,
        }

        let profile = Profile::ssh(
            "production",
            "ssh.example.test",
            2200,
            "deploy",
            "xterm-256color",
            100,
            40,
        )
        .expect("test SSH profile is valid")
        .as_ssh()
        .expect("test profile is SSH")
        .clone();
        let mut harness = Harness::builder()
            .with_size(egui::vec2(520.0, 560.0))
            .build_ui_state(
                |ui, state: &mut RestoredSshHarnessState| {
                    let tab_id = state.tab_id.expect("test tab id is set");
                    let profile = state.profile.as_ref().expect("test profile is set");
                    if let Some(command) =
                        show_ssh_authentication_required(ui, tab_id, profile, true)
                    {
                        state.command = Some(command);
                    }
                },
                RestoredSshHarnessState {
                    tab_id: Some(AppState::for_test().active()),
                    profile: Some(profile),
                    command: None,
                },
            );
        harness.run();

        // The destination is already restored, so the password is the only
        // thing left to type: it should accept keystrokes without a click.
        assert!(
            harness.get_by_label("Password").is_focused(),
            "a restored destination should focus the password field it still needs"
        );
        harness.get_by_label("Use separate fields").click();
        harness.run();

        harness.get_by_label("Username").focus();
        harness.run();
        assert!(
            !harness.get_by_label("Password").is_focused(),
            "the password focus request must be one-shot so Tab still works"
        );
    }

    #[test]
    fn restored_ssh_surface_prefills_destination_and_requires_fresh_authentication() {
        #[derive(Default)]
        struct RestoredSshHarnessState {
            tab_id: Option<TabId>,
            profile: Option<SshProfileConfiguration>,
            command: Option<AppCommand>,
        }

        let profile = Profile::ssh(
            "production",
            "ssh.example.test",
            2200,
            "deploy",
            "xterm-256color",
            100,
            40,
        )
        .expect("test SSH profile is valid")
        .as_ssh()
        .expect("test profile is SSH")
        .clone();
        let mut harness = Harness::builder()
            .with_size(egui::vec2(520.0, 560.0))
            .build_ui_state(
                |ui, state: &mut RestoredSshHarnessState| {
                    let tab_id = state.tab_id.expect("test tab id is set");
                    let profile = state.profile.as_ref().expect("test profile is set");
                    if let Some(command) =
                        show_ssh_authentication_required(ui, tab_id, profile, true)
                    {
                        state.command = Some(command);
                    }
                },
                RestoredSshHarnessState {
                    tab_id: Some(AppState::for_test().active()),
                    profile: Some(profile),
                    command: None,
                },
            );
        harness.run();

        assert!(harness
            .query_by_label("SSH authentication required")
            .is_some());
        assert!(harness
            .query_by_label(
                "This workspace restored destination metadata only. Enter fresh authentication \
                 below to connect; no prior connection, credential, or host trust was restored."
            )
            .is_some());
        harness.get_by_label("Password").click();
        harness
            .get_by_label("Password")
            .type_text("transient-test-password");
        harness.key_press(egui::Key::Enter);
        harness.run();

        let Some(AppCommand::StartSshSession {
            profile,
            authentication,
            ..
        }) = harness.state().command.as_ref()
        else {
            panic!("fresh authentication must be required before creating a command");
        };
        assert_eq!(profile.identity().host(), "ssh.example.test");
        assert_eq!(profile.identity().port(), 2200);
        assert_eq!(profile.username(), "deploy");
        assert_eq!(
            format!("{authentication:?}"),
            "SshAuthentication::Password([REDACTED])"
        );
    }

    #[test]
    fn ssh_form_shows_the_masked_multiline_key_input_only_for_key_authentication() {
        let mut harness = harness();
        harness.run();
        open_ssh_form(&mut harness);

        assert!(harness.query_by_label("OpenSSH private key").is_none());

        harness.get_by_label("Private key").click();
        harness.run();

        assert!(harness.query_by_label("Password").is_none());
        assert!(harness.query_by_label("OpenSSH private key").is_some());
        assert!(harness
            .query_by_label("Key passphrase (optional)")
            .is_some());
        assert!(harness
            .query_by_label("The key is kept in memory only, never saved.")
            .is_some());
    }

    #[test]
    fn ssh_form_shows_certificate_fields_only_for_certificate_authentication() {
        let mut harness = harness();
        harness.run();
        open_ssh_form(&mut harness);

        assert!(harness.query_by_label("OpenSSH certificate").is_none());

        harness.get_by_label("Certificate").click();
        harness.run();

        assert!(harness.query_by_label("Password").is_none());
        assert!(harness.query_by_label("OpenSSH private key").is_some());
        assert!(harness.query_by_label("OpenSSH certificate").is_some());
        assert!(harness
            .query_by_label("The private key and certificate are kept in memory only, never saved.")
            .is_some());
    }

    #[test]
    fn ssh_form_shows_constructor_validation_feedback() {
        let mut harness = harness();
        harness.run();
        open_ssh_form(&mut harness);
        use_separate_destination_fields(&mut harness);
        enter_text(&mut harness, "Host", "invalid host");

        harness.get_by_label("Connect").click();
        harness.run();

        assert!(harness.state().command.is_none());
        assert!(harness
            .query_by_label("SSH host must not contain whitespace")
            .is_some());
    }

    #[test]
    fn prefilling_from_a_persistent_profile_yields_persistent_session_options() {
        let profile = Profile::ssh(
            "remote",
            "example.invalid",
            22,
            "test-user",
            "xterm-256color",
            80,
            24,
        )
        .unwrap()
        .with_persistence(festerm_config::PersistenceProviderKind::Tmux, "build")
        .unwrap();

        let mut form = SshLauncherForm::default();
        form.prefill_saved_profile(profile.as_ssh().unwrap());

        assert_eq!(
            form.session_options().unwrap().strategy(),
            SessionStrategy::Persistent {
                provider: festerm_ssh::PersistenceProvider::Tmux,
                session_name: festerm_ssh::PersistentSessionName::new("build").unwrap(),
            }
        );
        assert_eq!(form.session_options().unwrap().reconnect_policy(), None);
    }

    #[test]
    fn prefilling_a_persistent_profile_never_defaults_automatic_recovery_on() {
        let profile = Profile::ssh(
            "remote",
            "example.invalid",
            22,
            "test-user",
            "xterm-256color",
            80,
            24,
        )
        .unwrap()
        .with_persistence(festerm_config::PersistenceProviderKind::Tmux, "build")
        .unwrap();

        let mut form = SshLauncherForm::default();
        form.prefill_saved_profile(profile.as_ssh().unwrap());

        assert!(
            !form.durable_session.automatic_recovery,
            "ADR 0018 requires automatic recovery to be an explicit, separate opt-in"
        );
    }

    #[test]
    fn opting_into_automatic_recovery_only_takes_effect_for_a_persistent_profile() {
        let profile = Profile::ssh(
            "remote",
            "example.invalid",
            22,
            "test-user",
            "xterm-256color",
            80,
            24,
        )
        .unwrap()
        .with_persistence(festerm_config::PersistenceProviderKind::Screen, "editor")
        .unwrap();

        let mut form = SshLauncherForm::default();
        form.prefill_saved_profile(profile.as_ssh().unwrap());
        form.durable_session.automatic_recovery = true;

        assert!(form.session_options().unwrap().reconnect_policy().is_some());
    }

    #[test]
    fn opting_into_automatic_recovery_without_persistence_has_no_effect() {
        let form = SshLauncherForm {
            host: "example.invalid".to_owned(),
            username: "test-user".to_owned(),
            durable_session: DurableSessionDraft {
                automatic_recovery: true,
                ..Default::default()
            },
            ..Default::default()
        };

        assert_eq!(
            form.session_options().unwrap().strategy(),
            SessionStrategy::PlainShell
        );
        assert_eq!(form.session_options().unwrap().reconnect_policy(), None);
    }

    #[test]
    fn prefilling_from_an_ordinary_profile_yields_plain_shell_session_options() {
        let profile = Profile::ssh(
            "remote",
            "example.invalid",
            22,
            "test-user",
            "xterm-256color",
            80,
            24,
        )
        .unwrap();

        let mut form = SshLauncherForm::default();
        form.prefill_saved_profile(profile.as_ssh().unwrap());

        assert_eq!(
            form.session_options().unwrap().strategy(),
            SessionStrategy::PlainShell
        );
    }

    #[test]
    fn ssh_form_submit_clears_the_transient_password() {
        let password = "transient-test-password";
        let mut form = SshLauncherForm {
            host: "example.invalid".to_owned(),
            username: "test-user".to_owned(),
            password: password.to_owned(),
            ..Default::default()
        };

        let command = form.submit().expect("valid form must submit");

        assert!(form.password.is_empty());
        assert!(!format!("{command:?}").contains(password));
    }

    #[test]
    fn ssh_form_submits_a_parsed_transient_private_key_and_clears_all_secret_text() {
        let private_key = generated_openssh_private_key();
        let mut form = SshLauncherForm {
            host: "example.invalid".to_owned(),
            username: "test-user".to_owned(),
            authentication_method: SshAuthenticationMethod::PrivateKey,
            password: "discarded-password".to_owned(),
            private_key,
            ..Default::default()
        };

        let AppCommand::StartSshSession { authentication, .. } =
            form.submit().expect("valid private-key form must submit")
        else {
            unreachable!("the form only creates SSH commands");
        };

        assert_eq!(
            format!("{authentication:?}"),
            "SshAuthentication::PublicKey([REDACTED])"
        );
        assert!(form.password.is_empty());
        assert!(form.private_key.is_empty());
        assert!(form.key_passphrase.is_empty());
    }

    #[test]
    fn ssh_form_parses_an_encrypted_private_key_with_a_transient_passphrase() {
        let (private_key, key_passphrase) = generated_encrypted_openssh_private_key();
        let mut form = SshLauncherForm {
            host: "example.invalid".to_owned(),
            username: "test-user".to_owned(),
            authentication_method: SshAuthenticationMethod::PrivateKey,
            private_key,
            key_passphrase,
            ..Default::default()
        };

        let AppCommand::StartSshSession { authentication, .. } = form
            .submit()
            .expect("encrypted private-key form must submit")
        else {
            unreachable!("the form only creates SSH commands");
        };

        assert_eq!(
            format!("{authentication:?}"),
            "SshAuthentication::PublicKey([REDACTED])"
        );
        assert!(form.private_key.is_empty());
        assert!(form.key_passphrase.is_empty());
    }

    #[test]
    fn ssh_form_rejects_an_invalid_private_key_and_clears_all_secret_text() {
        let mut form = SshLauncherForm {
            host: "example.invalid".to_owned(),
            username: "test-user".to_owned(),
            authentication_method: SshAuthenticationMethod::PrivateKey,
            password: "discarded-password".to_owned(),
            private_key: "not an OpenSSH private key".to_owned(),
            key_passphrase: "discarded-passphrase".to_owned(),
            ..Default::default()
        };

        assert_eq!(
            form.submit().expect_err("invalid key must not submit"),
            "SSH private key is not in OpenSSH format"
        );
        assert!(form.password.is_empty());
        assert!(form.private_key.is_empty());
        assert!(form.key_passphrase.is_empty());
    }

    #[test]
    fn ssh_form_submits_transient_certificate_authentication_and_clears_all_auth_material() {
        let private_key = generated_openssh_private_key();
        let certificate = generated_openssh_certificate();
        let mut form = SshLauncherForm {
            host: "example.invalid".to_owned(),
            username: "test-user".to_owned(),
            authentication_method: SshAuthenticationMethod::Certificate,
            private_key,
            certificate,
            ..Default::default()
        };

        let AppCommand::StartSshSession { authentication, .. } = form
            .submit()
            .expect("valid certificate-auth form must submit")
        else {
            unreachable!("the form only creates SSH commands");
        };

        assert_eq!(
            format!("{authentication:?}"),
            "SshAuthentication::Certificate([REDACTED])"
        );
        assert!(form.private_key.is_empty());
        assert!(form.key_passphrase.is_empty());
        assert!(form.certificate.is_empty());
    }

    #[test]
    fn ssh_form_rejects_an_invalid_certificate_and_clears_all_auth_material() {
        let mut form = SshLauncherForm {
            host: "example.invalid".to_owned(),
            username: "test-user".to_owned(),
            authentication_method: SshAuthenticationMethod::Certificate,
            private_key: generated_openssh_private_key(),
            certificate: "not an OpenSSH certificate".to_owned(),
            ..Default::default()
        };

        assert_eq!(
            form.submit()
                .expect_err("invalid certificate must not submit"),
            "SSH certificate is not in OpenSSH certificate format"
        );
        assert!(form.private_key.is_empty());
        assert!(form.key_passphrase.is_empty());
        assert!(form.certificate.is_empty());
    }

    fn test_ssh_password_prompt() -> PasswordPrompt {
        PasswordPrompt::new("test-user", "192.0.2.1", 1, false)
    }

    #[test]
    fn ssh_live_password_prompt_shows_the_ssh_style_prompt_line() {
        #[derive(Default)]
        struct PromptHarnessState {
            prompt: Option<PasswordPrompt>,
            command: Option<AppCommand>,
        }

        let mut harness = Harness::builder()
            .with_size(egui::vec2(420.0, 200.0))
            .build_ui_state(
                |ui, state: &mut PromptHarnessState| {
                    let prompt = state.prompt.as_ref().expect("prompt must be set");
                    if let Some(command) =
                        show_ssh_live_password_prompt(ui, AppState::for_test().active(), prompt)
                    {
                        state.command = Some(command);
                    }
                },
                PromptHarnessState {
                    prompt: Some(test_ssh_password_prompt()),
                    command: None,
                },
            );
        // Installing the bundled terminal font family binds it to the atlas
        // only after the pass boundary, so a first frame is needed before
        // the pty-styled prompt can lay out text with it.
        harness.run();
        harness.run();

        assert!(harness
            .query_by_label("test-user@192.0.2.1's password:")
            .is_some());
    }

    #[test]
    fn ssh_live_password_prompt_submits_a_typed_password_on_enter() {
        struct PromptHarnessState {
            prompt: PasswordPrompt,
            tab_id: TabId,
            command: Option<AppCommand>,
        }

        let tab_id = AppState::for_test().active();
        let mut harness = Harness::builder()
            .with_size(egui::vec2(420.0, 200.0))
            .build_ui_state(
                |ui, state: &mut PromptHarnessState| {
                    if let Some(command) =
                        show_ssh_live_password_prompt(ui, state.tab_id, &state.prompt)
                    {
                        state.command = Some(command);
                    }
                },
                PromptHarnessState {
                    prompt: test_ssh_password_prompt(),
                    tab_id,
                    command: None,
                },
            );
        // Installing the bundled terminal font family binds it to the atlas
        // only after the pass boundary, so a first frame is needed before
        // the pty-styled prompt can lay out text with it.
        harness.run();
        harness.run();

        // Typed characters are never reflected on screen (matching real
        // `ssh`), so there is no field to type into — the prompt captures
        // raw text/key events directly, exactly like the host-key [y/N]
        // prompt does.
        for character in "typed-test-password".chars() {
            harness.event(egui::Event::Text(character.to_string()));
        }
        harness.key_press(egui::Key::Enter);
        harness.run();

        let Some(AppCommand::ResolveSshPassword { tab, password }) =
            harness.state().command.as_ref()
        else {
            panic!("submitting the live password prompt must return a resolve command");
        };
        assert_eq!(*tab, tab_id);
        assert_eq!(password, "typed-test-password");
    }
}
