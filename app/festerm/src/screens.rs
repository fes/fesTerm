//! Launcher and Settings application-surface presentation.
//!
//! These are thin, product-specific screens rather than terminal chrome
//! (`crates/festerm-ui-egui/src/chrome.rs` owns the chip row). They translate
//! user gestures into `AppCommand`s per `docs/application-command-model.md`
//! and own no session or tab policy themselves; `AppState::dispatch` remains
//! the single command-handling path.

use std::{
    path::PathBuf,
    sync::{mpsc, Arc, Mutex},
    thread,
};

use eframe::egui::{self, vec2, ScrollArea, Sense, Stroke, TextEdit, Ui, WidgetInfo, WidgetType};
use festerm_config::{
    Configuration, CredentialKind, EmojiPresentationPreference, PersistenceConfiguration,
    PersistenceProviderKind, Profile, RemoteProfileKind, ScrollSpeedPreference,
    ScrollbackLimitPreference, SftpPaneOrderPreference, SshPortForwardDirection,
    SshProfileConfiguration, TerminalFontPreference,
};
use festerm_session::{PasswordPrompt, TerminalSize};
use festerm_ssh::{
    HostIdentity, PersistenceProvider, PersistenceProviderProbeAvailability, ReconnectPolicy,
    RecoveryPolicy, SessionStrategy, SshAuthentication, SshCertificate, SshConnectionProfile,
    SshKeyPassphrase, SshPrivateKey, SshPrivateKeyError, SshSessionOptions,
};
use festerm_ui_egui::{chrome::ChipLayout, icon, icon::Icon, theme};

#[cfg(test)]
use festerm_config::SshPortForwardConfiguration;

use crate::port_forward_draft::PortForwardDraft as SshPortForwardDraft;
use crate::tabs::{
    AppCommand, NewProfileKind, PasswordToStore, PrivateKeyToStore, ProfileCredentialToStore, TabId,
};

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

    /// The command that connects this saved profile over its *other*
    /// protocol, for the row menu's cross-protocol entry.
    ///
    /// An SSH profile already carries everything an SFTP session needs (and
    /// the reverse), so offering the crossover here saves duplicating one
    /// host under two profiles. Only the two remote kinds have a crossover;
    /// local and serial profiles return `None` and their menus simply omit
    /// the entry rather than showing it disabled.
    fn crossover(&self) -> Option<(&'static str, AppCommand)> {
        match self.kind {
            LauncherItemKind::SshProfile(profile_id) => Some((
                "Connect SFTP",
                AppCommand::StartConfiguredSftpProfile {
                    profile_id: profile_id.to_owned(),
                },
            )),
            LauncherItemKind::SftpProfile(profile_id) => Some((
                "Connect SSH",
                AppCommand::StartConfiguredSshProfile {
                    profile_id: profile_id.to_owned(),
                },
            )),
            _ => None,
        }
    }

    fn command(&self) -> AppCommand {
        match self.kind {
            LauncherItemKind::LocalDefault => AppCommand::StartLocalSession,
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
    /// The default, minimal entry point: a single `user@host[:port]` field
    /// parsed by `parse_quick_connect`. Shown instead of the full form until
    /// `advanced_open` is set, matching how most SSH clients' fast path
    /// works; IPv6 bracket notation (`user@[::1]:22`) is not specially
    /// handled and needs the advanced form's separate Host field instead.
    quick_connect: String,
    /// Whether the full connection form (separate Host/Port/persistence/
    /// authentication-method fields) is shown instead of the single Quick
    /// Connect field. Always `true` once a saved or restored profile is
    /// prefilled (`prefill_from_profile`), since its host/username are
    /// already known and Quick Connect's only purpose is fast ad-hoc entry.
    advanced_open: bool,
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
    fn default() -> Self {
        Self {
            host: String::new(),
            port: Self::DEFAULT_PORT.to_string(),
            username: String::new(),
            quick_connect: String::new(),
            advanced_open: false,
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
    const DEFAULT_PORT: u16 = 22;

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
        let mut probe_form = self.clone();
        if !probe_form.advanced_open {
            probe_form.sync_advanced_from_quick_connect();
        }
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
        let input = self.quick_connect.trim();
        if input.is_empty() {
            return Err("Enter a destination, e.g. user@host".to_owned());
        }
        let (username, remainder) = input
            .split_once('@')
            .ok_or_else(|| "Enter a destination as user@host".to_owned())?;
        if username.is_empty() {
            return Err("Enter a username before @".to_owned());
        }
        let (host, port) = match remainder.rsplit_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (remainder, None),
        };
        if host.is_empty() {
            return Err("Enter a host after @".to_owned());
        }
        self.username = username.to_owned();
        self.host = host.to_owned();
        self.port = port
            .map(str::to_owned)
            .unwrap_or_else(|| Self::DEFAULT_PORT.to_string());
        Ok(())
    }

    /// Best-effort version of `parse_quick_connect` for toggling to the
    /// advanced form: fills in whatever `username`/`host`/`port` it can from
    /// `quick_connect`, but never surfaces or blocks on a parse error, since
    /// revealing the advanced form must always succeed.
    fn sync_advanced_from_quick_connect(&mut self) {
        let _ = self.parse_quick_connect();
    }

    /// Inverse of `sync_advanced_from_quick_connect`: composes the advanced
    /// form's `username`/`host`/`port` back into the single quick-connect
    /// field, omitting the port when it is still the default, so toggling
    /// back and forth round-trips what the user actually typed.
    fn sync_quick_connect_from_advanced(&mut self) {
        if self.username.is_empty() && self.host.is_empty() {
            return;
        }
        let port = self.port.trim();
        self.quick_connect = if port.is_empty() || port == Self::DEFAULT_PORT.to_string() {
            format!("{}@{}", self.username, self.host)
        } else {
            format!("{}@{}:{}", self.username, self.host, port)
        };
    }

    /// Reveals the advanced form, carrying forward whatever destination the
    /// user already typed into Quick Connect and clearing any stale
    /// feedback from that surface (item 4: feedback must not persist across
    /// a toggle it no longer describes).
    fn open_advanced_settings(&mut self) {
        self.sync_advanced_from_quick_connect();
        self.feedback = None;
        self.advanced_open = true;
        self.focus_username = true;
    }

    /// Inverse of `open_advanced_settings`: returns to Quick Connect,
    /// carrying the advanced form's destination back into the single field
    /// and clearing any stale feedback from the advanced form.
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

#[derive(Clone)]
struct LauncherState {
    selected: usize,
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

const CONTENT_SCROLLBAR_LANE: f32 = 26.0;

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
                body(ui)
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
fn ssh_section_heading(ui: &mut Ui, heading: &str) {
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

/// The default, minimal launcher surface for a fresh SSH connection: a
/// single `user@host[:port]` field and a Connect button, matching how most
/// SSH clients' fast path works (`SshLauncherForm::quick_connect`).
/// Submitting always goes through `submit_quick_connect`, which leaves the
/// password empty so the connection starts interactively (host-key-first,
/// then `show_ssh_live_password_prompt`) rather than collecting a password
/// blind before a connection exists — exactly mirroring how other SSH
/// clients defer the password prompt until it's actually needed.
fn show_ssh_quick_connect(
    ui: &mut Ui,
    tab_id: TabId,
    form: &mut SshLauncherForm,
    focus_quick_connect: bool,
    configuration: Option<&Configuration>,
) -> Option<AppCommand> {
    let mut result = None;
    ssh_section_heading(ui, "Quick Connect");
    let submit_with_enter = ui
        .horizontal(|ui| {
            ui.add_space(2.0);
            let label = ui.add(
                egui::Label::new(egui::RichText::new("user@host").color(theme::TEXT_SECONDARY))
                    .selectable(false),
            );
            let field = ui.add(
                TextEdit::singleline(&mut form.quick_connect)
                    .id_salt(("launcher_ssh", tab_id, "quick_connect"))
                    .hint_text("example@169.254.1.1")
                    .desired_width(220.0),
            );
            if focus_quick_connect {
                field.request_focus();
            }
            let field = field.labelled_by(label.id);
            field.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter))
        })
        .inner;
    ui.add_space(8.0);
    form.sync_remote_durable_provider_default(ui.ctx(), configuration);
    show_durable_session_controls(
        ui,
        tab_id,
        &mut form.durable_session,
        DurableSessionTarget::Remote,
        true,
    );
    ui.add_space(8.0);
    if ui.button("Connect").clicked() || submit_with_enter {
        match form.submit_quick_connect() {
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

fn show_sftp_quick_connect(
    ui: &mut Ui,
    tab_id: TabId,
    form: &mut SshLauncherForm,
    focus_quick_connect: bool,
) -> Option<AppCommand> {
    let mut result = None;
    ssh_section_heading(ui, "Quick Connect");
    let submit_with_enter = ui
        .horizontal(|ui| {
            ui.add_space(2.0);
            let label = ui.add(
                egui::Label::new(egui::RichText::new("user@host").color(theme::TEXT_SECONDARY))
                    .selectable(false),
            );
            let field = ui.add(
                TextEdit::singleline(&mut form.quick_connect)
                    .id_salt(("launcher_sftp", tab_id, "quick_connect"))
                    .hint_text("example@169.254.1.1")
                    .desired_width(220.0),
            );
            if focus_quick_connect {
                field.request_focus();
            }
            let field = field.labelled_by(label.id);
            field.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter))
        })
        .inner;
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

fn show_durable_session_controls(
    ui: &mut Ui,
    tab_id: TabId,
    draft: &mut DurableSessionDraft,
    target: DurableSessionTarget,
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
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(heading).color(theme::TEXT_PRIMARY));
        if toggle_switch(ui, draft.enabled, toggle_label).clicked() {
            draft.enabled = !draft.enabled;
            if draft.enabled && matches!(target, DurableSessionTarget::Local) {
                draft.provider = draft.local_default_provider;
            }
        }
    });
    ssh_paragraph(ui, description);
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
    if ssh_text_edit(
        ui,
        tab_id,
        "durable_session_name",
        "Session name",
        &mut draft.session_name,
        false,
        false,
    )
    .changed()
    {
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

fn show_ssh_form(
    ui: &mut Ui,
    tab_id: TabId,
    form: &mut SshLauncherForm,
    configuration: Option<&Configuration>,
    native_store_available: bool,
) -> Option<AppCommand> {
    ui.add_space(16.0);
    let mut result = None;
    let focus_username = form.focus_username;
    form.focus_username = false;
    // Username wins if both are somehow armed: it is the earlier field, so
    // focusing the password would strand the user mid-form.
    let focus_password = std::mem::take(&mut form.focus_password) && !focus_username;
    egui::Frame::new()
        .fill(theme::SURFACE_TAB_INACTIVE)
        .stroke(Stroke::new(1.0, theme::BORDER_SUBTLE))
        .corner_radius(8.0)
        .inner_margin(egui::Margin::same(16))
        .show(ui, |ui| {
            ui.set_width(340.0);
            // Rendered once, in a single fixed spot, regardless of which
            // form (quick connect vs. advanced) is showing below it -- so
            // toggling this checkbox never makes the checkbox itself jump
            // to a different position.
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
            if !form.advanced_open {
                result = show_ssh_quick_connect(ui, tab_id, form, focus_username, configuration);
                return;
            }
            ssh_section_heading(ui, "Connection");
            ssh_text_edit(
                ui,
                tab_id,
                "username",
                "Username",
                &mut form.username,
                false,
                focus_username,
            );
            ssh_text_edit(ui, tab_id, "host", "Host", &mut form.host, false, false);
            ssh_text_edit(ui, tab_id, "port", "Port", &mut form.port, false, false);

            ui.add_space(10.0);
            ssh_section_heading(ui, "Durable session");
            form.sync_remote_durable_provider_default(ui.ctx(), configuration);
            show_durable_session_controls(
                ui,
                tab_id,
                &mut form.durable_session,
                DurableSessionTarget::Remote,
                true,
            );

            ui.add_space(10.0);
            ssh_section_heading(ui, "Port forwards");
            show_port_forward_drafts(
                ui,
                tab_id,
                "ssh_launcher_port_forward",
                &mut form.port_forwards,
            );

            ui.add_space(10.0);
            ssh_section_heading(ui, "Authentication");
            ui.horizontal(|ui| {
                ui.radio_value(
                    &mut form.authentication_method,
                    SshAuthenticationMethod::Password,
                    "Password authentication",
                );
                ui.radio_value(
                    &mut form.authentication_method,
                    SshAuthenticationMethod::PrivateKey,
                    "Private-key authentication",
                );
                ui.radio_value(
                    &mut form.authentication_method,
                    SshAuthenticationMethod::Certificate,
                    "Certificate authentication",
                );
            });
            ui.add_space(4.0);
            let submit_with_enter = match form.authentication_method {
                SshAuthenticationMethod::Password => {
                    let submit = ssh_text_edit(
                        ui,
                        tab_id,
                        "password",
                        "Password",
                        &mut form.password,
                        true,
                        focus_password,
                    )
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
                            match form.submit_stored_credential() {
                                Ok(command) => {
                                    result = Some(command);
                                    form.feedback = None;
                                }
                                Err(feedback) => form.feedback = Some(feedback),
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

            if result.is_none() {
                ui.add_space(12.0);
                let submit_label = match form.authentication_method {
                    SshAuthenticationMethod::Password => "Connect with password",
                    SshAuthenticationMethod::PrivateKey => "Connect with private key",
                    SshAuthenticationMethod::Certificate => "Connect with certificate",
                };
                if ui.button(submit_label).clicked() || submit_with_enter {
                    match form.submit() {
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
            if !form.advanced_open {
                result = show_sftp_quick_connect(ui, tab_id, form, focus_username);
                return;
            }
            ssh_section_heading(ui, "Connection");
            ssh_text_edit(
                ui,
                tab_id,
                "username",
                "Username",
                &mut form.username,
                false,
                focus_username,
            );
            ssh_text_edit(ui, tab_id, "host", "Host", &mut form.host, false, false);
            ssh_text_edit(ui, tab_id, "port", "Port", &mut form.port, false, false);

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
            ssh_section_heading(ui, "Authentication");
            ui.horizontal(|ui| {
                ui.radio_value(
                    &mut form.authentication_method,
                    SshAuthenticationMethod::Password,
                    "Password authentication",
                );
                ui.radio_value(
                    &mut form.authentication_method,
                    SshAuthenticationMethod::PrivateKey,
                    "Private-key authentication",
                );
                ui.radio_value(
                    &mut form.authentication_method,
                    SshAuthenticationMethod::Certificate,
                    "Certificate authentication",
                );
            });
            ui.add_space(4.0);
            let submit_with_enter = match form.authentication_method {
                SshAuthenticationMethod::Password => {
                    let submit = ssh_text_edit(
                        ui,
                        tab_id,
                        "password",
                        "Password",
                        &mut form.password,
                        true,
                        focus_password,
                    )
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
                            result = form.saved_profile_id.as_ref().map(|profile_id| {
                                AppCommand::StartStoredPasswordSftpProfile {
                                    profile_id: profile_id.clone(),
                                }
                            });
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

/// Renders one Saved Profiles row and its menu.
///
/// The menu opens from either the row's overflow control or a right-click
/// anywhere on the row: the two gestures are the same affordance, so they
/// share one popup rather than diverging.
fn show_profile_row(
    ui: &mut Ui,
    width: f32,
    item: &LauncherItem<'_>,
    selected: bool,
    now_unix_seconds: Option<u64>,
    command: &mut Option<AppCommand>,
) {
    let (rect, _) =
        ui.allocate_exact_size(vec2(width, LAUNCHER_PROFILE_ROW_HEIGHT), Sense::hover());
    let menu_center = egui::pos2(rect.right() - LAUNCHER_ROW_MENU_INSET, rect.center().y);
    let menu_rect = egui::Rect::from_center_size(menu_center, egui::Vec2::splat(24.0));
    // The row's own click target stops short of the overflow control so the
    // two never contend for the same pointer press.
    let response = ui.interact(
        rect.with_max_x(menu_rect.left()),
        ui.id().with(("profile_row", &item.label)),
        Sense::click(),
    );
    // Matches the launch cards: the row's columns are visual, so the
    // accessible name has to restate the type and host the eye reads across.
    response.widget_info(|| {
        WidgetInfo::labeled(
            WidgetType::Button,
            ui.is_enabled(),
            format!("{} — {}", item.label, item.description),
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

    let mark_size = session_mark_size(item.mark().0, LAUNCHER_PROFILE_MARK_SIZE);
    let mark_rect = egui::Rect::from_min_size(
        egui::pos2(
            rect.left() + LAUNCHER_PANEL_PADDING - 1.0,
            rect.center().y - mark_size.y / 2.0,
        ),
        mark_size,
    );
    paint_session_mark(ui.painter(), item, mark_rect);

    let last_used = item
        .last_used_unix_seconds
        .zip(now_unix_seconds)
        .map(|(then, now)| relative_age(now, then))
        .unwrap_or_else(|| "Never".to_owned());
    let columns = [
        (
            item.label.clone(),
            LAUNCHER_BODY_TEXT_SIZE,
            theme::TEXT_PRIMARY,
        ),
        (
            item.type_label.to_owned(),
            LAUNCHER_BODY_TEXT_SIZE,
            theme::TEXT_SECONDARY,
        ),
        (
            item.location.clone(),
            LAUNCHER_BODY_TEXT_SIZE,
            theme::TEXT_SECONDARY,
        ),
        (last_used, LAUNCHER_DETAIL_TEXT_SIZE, theme::TEXT_SECONDARY),
    ];
    let column_origins = launcher_profile_columns(width);
    for (index, (text, size, color)) in columns.into_iter().enumerate() {
        let left = rect.left() + width * column_origins[index];
        let right = column_origins
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
        ui.id().with(("profile_row_menu", &item.label)),
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

    // The overflow control and a right-click on the row are the same
    // affordance, so both open one menu built in one place rather than two
    // that could drift apart.
    egui::Popup::menu(&menu_response).show(|ui| show_profile_row_menu(ui, item, command));
    response.context_menu(|ui| show_profile_row_menu(ui, item, command));

    if response.clicked() {
        *command = Some(item.command());
    }
}

/// The row menu: launch, edit, and -- for the two remote profile kinds --
/// connect over the other protocol.
///
/// Every entry dispatches an existing `AppCommand`, so a profile launched
/// from here takes exactly the path it takes from anywhere else.
fn show_profile_row_menu(ui: &mut Ui, item: &LauncherItem<'_>, command: &mut Option<AppCommand>) {
    if ui.button("Connect").clicked() {
        *command = Some(item.command());
        ui.close();
    }
    if let Some((label, crossover)) = item.crossover() {
        if ui.button(label).clicked() {
            *command = Some(crossover);
            ui.close();
        }
    }
    if ui.button("Edit").clicked() {
        *command = Some(AppCommand::OpenProfileEditor {
            identifier: item
                .profile_id()
                .expect("Saved Profiles rows always carry a profile id")
                .to_owned(),
        });
        ui.close();
    }
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
    let (rect, response) = ui.allocate_exact_size(
        vec2(text_width + 48.0, LAUNCHER_CONTROL_HEIGHT),
        Sense::click(),
    );
    // Several buttons on this surface share one visible word ("Reattach"),
    // so the caller may name them apart for anyone navigating by label.
    let name = accessible_name.unwrap_or(label);
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, ui.is_enabled(), name));
    if !ui.is_rect_visible(rect) {
        return response;
    }
    let fill = if accent {
        theme::ACCENT_ACTION
    } else if response.hovered() {
        theme::SURFACE_OVERLAY
    } else {
        theme::SURFACE_TAB_ACTIVE
    };
    let foreground = if accent {
        egui::Color32::WHITE
    } else {
        theme::TEXT_PRIMARY
    };
    ui.painter().rect(
        rect,
        6.0,
        fill,
        if accent {
            Stroke::NONE
        } else {
            Stroke::new(1.0, theme::BORDER_SUBTLE)
        },
        egui::StrokeKind::Inside,
    );
    let mark_rect = egui::Rect::from_center_size(
        egui::pos2(rect.left() + 18.0, rect.center().y),
        egui::Vec2::splat(LAUNCHER_CONTROL_ICON_SIZE),
    );
    icon::paint(ui.painter(), mark, mark_rect, foreground);
    ui.painter().text(
        egui::pos2(mark_rect.right() + 8.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(LAUNCHER_BODY_TEXT_SIZE),
        foreground,
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
                            egui::RichText::new("Enter connection details.")
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
        if matches!(items[state.selected].kind, LauncherItemKind::NewSsh) {
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
    // The three fixed entries open an in-tab form rather than dispatching a
    // command, so a card click is translated here for the same reason the
    // keyboard path above translates it: `command()` has no form variant.
    if let Some(opened) = state.pending_form.take() {
        match opened {
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
                                    let (field, _) = ui.allocate_exact_size(
                                        vec2(field_width, LAUNCHER_CONTROL_HEIGHT),
                                        Sense::hover(),
                                    );
                                    ui.painter().rect(
                                        field,
                                        8.0,
                                        theme::SURFACE_FIELD,
                                        Stroke::new(1.0, theme::BORDER_SUBTLE),
                                        egui::StrokeKind::Inside,
                                    );
                                    let glass = egui::Rect::from_center_size(
                                        egui::pos2(field.left() + 16.0, field.center().y),
                                        egui::Vec2::splat(LAUNCHER_CONTROL_ICON_SIZE),
                                    );
                                    icon::paint(
                                        ui.painter(),
                                        Icon::Search,
                                        glass,
                                        theme::TEXT_MUTED,
                                    );
                                    // The entry is sized to one line of
                                    // text and centred on the pill, so
                                    // the text sits on the pill's centre
                                    // line instead of hanging from a
                                    // fixed top margin.
                                    let line = ui
                                        .painter()
                                        .layout_no_wrap(
                                            "Ag".to_owned(),
                                            egui::FontId::proportional(LAUNCHER_SEARCH_TEXT_SIZE),
                                            theme::TEXT_PRIMARY,
                                        )
                                        .size()
                                        .y;
                                    let entry = egui::Rect::from_min_max(
                                        egui::pos2(
                                            glass.right() + 8.0,
                                            field.center().y - line / 2.0,
                                        ),
                                        egui::pos2(
                                            field.right() - 10.0,
                                            field.center().y + line / 2.0,
                                        ),
                                    );
                                    ui.scope_builder(
                                        egui::UiBuilder::new().max_rect(entry),
                                        |ui| {
                                            let search = ui.add_sized(
                                                entry.size(),
                                                TextEdit::singleline(&mut state.profile_search)
                                                    // The pill around the
                                                    // field is painted by
                                                    // this panel, so the
                                                    // widget must not draw
                                                    // a second frame
                                                    // inside it.
                                                    .frame(egui::Frame::NONE)
                                                    .background_color(egui::Color32::TRANSPARENT)
                                                    .font(egui::FontId::proportional(
                                                        LAUNCHER_SEARCH_TEXT_SIZE,
                                                    ))
                                                    .hint_text("Search profiles…")
                                                    .margin(egui::Margin::ZERO),
                                            );
                                            // The magnifier is painted,
                                            // not a label widget, so the
                                            // field would otherwise reach
                                            // assistive technology
                                            // unnamed.
                                            let value = state.profile_search.clone();
                                            search.widget_info(|| {
                                                let mut info = WidgetInfo::text_edit(
                                                    ui.is_enabled(),
                                                    &value,
                                                    &value,
                                                    "Search profiles…",
                                                );
                                                info.label = Some("Search profiles…".to_owned());
                                                info
                                            });
                                        },
                                    );
                                },
                            );
                        });
                        ui.add_space(8.0);

                        show_profile_column_headers(ui, width, inner);
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
                                show_profile_row(
                                    ui,
                                    width,
                                    &items[*index],
                                    *index == selected,
                                    now_unix_seconds,
                                    command,
                                );
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
                                    let new_profile = launcher_button(
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
fn show_profile_column_headers(ui: &mut Ui, width: f32, inner: f32) {
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
        let left = rect.left() + width * launcher_profile_columns(width)[index];
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
    let state_id = launcher_state_id(tab_id);
    let mut state = ui.data(|data| data.get_temp::<LauncherState>(state_id).unwrap_or_default());
    if !state.ssh_profile_prefilled {
        state.ssh.prefill_saved_profile(profile);
        state.ssh_profile_prefilled = true;
    }

    let command = ui
        .vertical(|ui| {
            ui.add_space(24.0);
            ui.heading("SSH authentication required");
            ui.label(format!(
                "Restored SSH destination: {}@{}:{}",
                profile.username(),
                profile.host(),
                profile.port()
            ));
            ui.label(
                "This workspace restored destination metadata only. Enter fresh authentication \
                 below to connect; no prior connection, credential, or host trust was restored.",
            );
            show_ssh_form(ui, tab_id, &mut state.ssh, None, native_store_available)
        })
        .inner;

    ui.data_mut(|data| data.insert_temp(state_id, state));
    command
}

/// Renders a restored SFTP workspace tab without creating a transport.
pub fn show_sftp_authentication_required(
    ui: &mut Ui,
    tab_id: TabId,
    profile: &SshProfileConfiguration,
    native_store_available: bool,
) -> Option<AppCommand> {
    let state_id = launcher_state_id(tab_id);
    let mut state = ui.data(|data| data.get_temp::<LauncherState>(state_id).unwrap_or_default());
    if !state.sftp_profile_prefilled {
        state.sftp.prefill_restored_sftp_profile(profile);
        state.sftp_profile_prefilled = true;
    }

    let command = ui
        .vertical(|ui| {
            ui.add_space(24.0);
            ui.heading("SFTP authentication required");
            ui.label(format!(
                "Restored SFTP destination: {}@{}:{}",
                profile.username(),
                profile.host(),
                profile.port()
            ));
            ui.label(
                "This workspace restored destination metadata only. Enter fresh authentication \
                 below to connect; no prior connection, credential, or host trust was restored.",
            );
            show_sftp_form(ui, tab_id, &mut state.sftp, native_store_available)
        })
        .inner;

    ui.data_mut(|data| data.insert_temp(state_id, state));
    command
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

/// Renders the Settings application surface.
///
/// `chip_layout`, `status_bar_visible`, and `show_session_details` reflect
/// the current interface preferences (`docs/gui-design.md` "Wrapping must
/// remain user-configurable"). Unlike profiles/workspace metadata, these
/// preferences are saved automatically by the composition root as soon as
/// they change; there is no separate explicit save step for them. Returns
/// commands for Settings actions; the application composition root owns
/// configuration I/O and applies successful replacements to `AppState`.
#[derive(Clone)]
pub struct SettingsViewModel {
    pub chip_layout: ChipLayout,
    pub status_bar_visible: bool,
    pub show_session_details: bool,
    pub confirm_session_close: bool,
    pub prefer_powershell: bool,
    pub restore_workspace: bool,
    pub terminal_font: TerminalFontPreference,
    pub terminal_ligatures: bool,
    pub emoji_presentation: EmojiPresentationPreference,
    pub scroll_speed: ScrollSpeedPreference,
    pub scrollback_limit: ScrollbackLimitPreference,
    pub quick_switch_overlay: bool,
    pub compact_launcher_grid: bool,
    pub pulse_new_output_dot: bool,
    pub show_resumable_sessions: bool,
    pub default_sftp_local_directory: Option<String>,
    pub sftp_pane_order: SftpPaneOrderPreference,
}

#[derive(Clone, Default)]
struct SettingsState {
    default_sftp_local_directory: String,
    sftp_pane_order: Option<SftpPaneOrderPreference>,
    synced_value: Option<String>,
    feedback: Option<String>,
}

fn settings_sftp_directory_field_id(ui: &Ui) -> egui::Id {
    ui.make_persistent_id("settings_default_sftp_local_directory")
}

pub fn show_settings(
    ui: &mut Ui,
    settings: SettingsViewModel,
    command_palette_shortcut: &str,
    settings_shortcut: &str,
) -> Option<AppCommand> {
    let SettingsViewModel {
        chip_layout,
        status_bar_visible,
        show_session_details,
        confirm_session_close,
        prefer_powershell,
        restore_workspace,
        terminal_font,
        terminal_ligatures,
        emoji_presentation,
        scroll_speed,
        scrollback_limit,
        quick_switch_overlay,
        compact_launcher_grid,
        pulse_new_output_dot,
        show_resumable_sessions,
        default_sftp_local_directory,
        sftp_pane_order,
    } = settings;
    #[cfg(not(windows))]
    let _ = prefer_powershell;
    let state_id = ui.id().with("settings_state");
    let field_id = settings_sftp_directory_field_id(ui);
    let mut state = ui.data(|data| data.get_temp::<SettingsState>(state_id).unwrap_or_default());
    let model_value = default_sftp_local_directory.unwrap_or_default();
    let field_focused = ui.memory(|memory| memory.has_focus(field_id));
    if state.synced_value.as_deref() != Some(model_value.as_str()) && !field_focused {
        state.default_sftp_local_directory = model_value.clone();
        state.synced_value = Some(model_value);
        state.feedback = None;
    }
    state.sftp_pane_order.get_or_insert(sftp_pane_order);
    let mut command = None;
    ui.horizontal(|ui| {
        ui.add_space(26.0);
        // Bound Settings' own height to whatever room is actually left
        // above the status bar (queried from its persisted panel state,
        // the same technique the SSH profile editor panel uses): the
        // card-based layout is taller than the old flat button list, and
        // without this it can paint straight into - or past - the status
        // bar instead of stopping short of it.
        let panel_top = ui.cursor().top();
        let mut viewport_bottom = ui.ctx().content_rect().bottom();
        if let Some(status_bar) =
            egui::containers::panel::PanelState::load(ui.ctx(), egui::Id::new("status_bar"))
        {
            viewport_bottom = viewport_bottom.min(status_bar.outer_rect.top());
        }
        let available_height = (viewport_bottom - panel_top).max(0.0);
        // `ScrollArea` computes its own available space via
        // `ui.available_rect_before_wrap()`. Handing it a `ui` whose
        // `max_rect` isn't already a real, bounded rect (as is the case
        // here, directly inside a `ui.horizontal`) leads to a degenerate
        // sizing pass that -- besides being wrong for layout -- also
        // breaks click routing for widgets painted via `egui::Frame`
        // inside the scroll area. Giving the scroll area its own child
        // `Ui` with an explicit, non-degenerate `max_rect` (the same
        // technique the SSH profile editor uses) avoids both problems.
        let scroll_rect = egui::Rect::from_min_size(
            ui.cursor().min,
            egui::vec2(ui.available_width(), available_height),
        );
        ui.scope_builder(egui::UiBuilder::new().max_rect(scroll_rect), |ui| {
            // egui's default floating scroll style reveals the bar for
            // *any* hover inside the scroll area's content, not just when
            // the pointer is actually near the bar - unlike the terminal
            // view's own history scrollbar, which stays hidden until it's
            // scrolled away from rest or the pointer is right over it.
            // Zeroing the "active" (any-content-hover) opacities while
            // keeping "interact" (hovering/dragging the bar itself) at
            // full strength reproduces that same narrower reveal condition
            // here.
            let mut scroll_style = egui::style::ScrollStyle::floating();
            scroll_style.active_handle_opacity = 0.0;
            scroll_style.active_background_opacity = 0.0;
            ui.spacing_mut().scroll = scroll_style;
            egui::ScrollArea::vertical()
                .max_height(available_height)
                .show(ui, |ui| {
                    // The scroll bar itself belongs to this scroll *frame*,
                    // not to Settings' own content: it is given its own
                    // reserved lane on the right, by keeping the cards
                    // narrower than the frame instead of shrinking the
                    // frame itself. That way the (invisible until needed)
                    // scroll bar never has to sit on top of the cards' own
                    // right edge.
                    ui.set_max_width((ui.available_width() - CONTENT_SCROLLBAR_LANE).max(0.0));
                    ui.vertical(|ui| {
                        ui.add_space(24.0);
                        ui.heading("Settings");
                        ui.add_space(2.0);

                        settings_card(ui, "Interface", |ui| {
                            if settings_segmented_row(
                                ui,
                                "Session chip layout",
                                "Keep terminal height stable with one scrolling row.",
                                &[
                                    ("Single row", !matches!(chip_layout, ChipLayout::Wrap)),
                                    ("Wrap", matches!(chip_layout, ChipLayout::Wrap)),
                                ],
                            )
                            .is_some()
                            {
                                command = Some(AppCommand::ToggleChipLayout);
                            }
                            ui.add_space(10.0);
                            ui.separator();
                            ui.add_space(10.0);
                            if settings_toggle_row(
                                ui,
                                "Show session details in chips",
                                "Show the terminal title or launch context beneath the \
                                 session name. Off makes every chip compact and single-line, \
                                 moving the active session's detail to the status bar.",
                                show_session_details,
                            ) {
                                command = Some(AppCommand::ToggleShowSessionDetails);
                            }
                            ui.add_space(10.0);
                            ui.separator();
                            ui.add_space(10.0);
                            if settings_toggle_row(
                                ui,
                                "Show status bar",
                                "Display sourced session state, terminal dimensions, and \
                                 the active session detail when compact chips require it.",
                                status_bar_visible,
                            ) {
                                command = Some(AppCommand::ToggleStatusBar);
                            }
                            ui.add_space(10.0);
                            ui.separator();
                            ui.add_space(10.0);
                            if settings_toggle_row(
                                ui,
                                "Confirm before closing live sessions",
                                "Ask before terminating a running local process or \
                                 disconnecting an active remote session.",
                                confirm_session_close,
                            ) {
                                command = Some(AppCommand::ToggleConfirmSessionClose);
                            }
                            #[cfg(windows)]
                            {
                                ui.add_space(10.0);
                                ui.separator();
                                ui.add_space(10.0);
                                if settings_toggle_row(
                                    ui,
                                    "Prefer PowerShell when available",
                                    "Use the current user's standard Windows app-execution \
                                     alias for pwsh.exe when it exists. Turn this off to use \
                                     COMSPEC for new default local sessions.",
                                    prefer_powershell,
                                ) {
                                    command = Some(AppCommand::TogglePreferPowershell);
                                }
                            }
                            ui.add_space(10.0);
                            ui.separator();
                            ui.add_space(10.0);
                            if settings_toggle_row(
                                ui,
                                "Workspace restore",
                                "Reopen your previously open tabs and the active tab \
                                 automatically on launch. Off by default.",
                                restore_workspace,
                            ) {
                                command = Some(AppCommand::ToggleRestoreWorkspace);
                            }
                            ui.add_space(10.0);
                            ui.separator();
                            ui.add_space(10.0);
                            if settings_toggle_row(
                                ui,
                                "Compact New Session layout",
                                "Drop the descriptions from the New Session tab's launch \
                                 cards so the saved-profile and running-session panels \
                                 start higher up the window. Off by default.",
                                compact_launcher_grid,
                            ) {
                                command = Some(AppCommand::ToggleCompactLauncherGrid);
                            }
                            ui.add_space(10.0);
                            ui.separator();
                            ui.add_space(10.0);
                            if settings_toggle_row(
                                ui,
                                "Pulse status dot on new background output",
                                "Slow-pulse a background tab's chip status dot when that \
                                 session has produced output since you last looked at it, \
                                 so it can quietly draw your eye without changing its \
                                 connection-state color. The active tab's own chip never \
                                 pulses. Off by default.",
                                pulse_new_output_dot,
                            ) {
                                command = Some(AppCommand::TogglePulseNewOutputDot);
                            }
                            ui.add_space(10.0);
                            ui.separator();
                            ui.add_space(10.0);
                            if settings_toggle_row(
                                ui,
                                "Resume unattached local sessions from New Session",
                                "Surface locally running festerm-sessiond persistence \
                                 sessions that have no attached window, plus locally \
                                 running tmux and GNU screen sessions, as one-click \
                                 \"Resume\" entries in their own labeled widgets on the \
                                 New Session tab. Off by default.",
                                show_resumable_sessions,
                            ) {
                                command = Some(AppCommand::ToggleShowResumableSessions);
                            }
                            ui.add_space(10.0);
                            if ui.button("Reset interface settings to defaults").clicked() {
                                command = Some(AppCommand::ResetInterfaceSettings);
                            }
                        });

                        ui.add_space(12.0);

                        settings_card(ui, "Scrolling", |ui| {
                            if let Some(selected) = settings_segmented_row(
                                ui,
                                "Scrollback limit",
                                "Maximum retained history for newly created sessions. \
                                 Existing sessions keep their current limit.",
                                &ScrollbackLimitPreference::ALL
                                    .map(|limit| (limit.label(), limit == scrollback_limit)),
                            ) {
                                command = Some(AppCommand::SetScrollbackLimit(
                                    ScrollbackLimitPreference::ALL[selected],
                                ));
                            }
                            ui.add_space(10.0);
                            ui.separator();
                            ui.add_space(10.0);
                            let mut selected_speed = scroll_speed;
                            if let Some(new_speed) = settings_clickstop_row(
                                ui,
                                "Scroll speed",
                                "How far one trackpad or mouse wheel scroll step moves \
                                 through scrollback history.",
                                &ScrollSpeedPreference::ALL,
                                selected_speed,
                            ) {
                                selected_speed = new_speed;
                            }
                            if selected_speed != scroll_speed {
                                command = Some(AppCommand::SetScrollSpeed(selected_speed));
                            }
                        });

                        ui.add_space(12.0);

                        settings_card(ui, "Terminal typography", |ui| {
                            let mut selected_font = terminal_font;
                            egui::Sides::new().show(
                                ui,
                                |ui| {
                                    ui.set_max_width(ui.available_width() - 190.0);
                                    ui.vertical(|ui| {
                                        ui.label(
                                            egui::RichText::new("Terminal font")
                                                .color(theme::TEXT_PRIMARY),
                                        );
                                        ssh_paragraph(
                                            ui,
                                            "Choose the bundled primary face used by terminal \
                                             cells. Application text is unchanged.",
                                        );
                                    });
                                },
                                |ui| {
                                    egui::ComboBox::from_id_salt("terminal-font-family")
                                        .selected_text(terminal_font_label(selected_font))
                                        .width(160.0)
                                        .show_ui(ui, |ui| {
                                            for font in [
                                                TerminalFontPreference::JetBrainsMono,
                                                TerminalFontPreference::IosevkaTerm,
                                                TerminalFontPreference::JuliaMono,
                                                TerminalFontPreference::MapleMono,
                                            ] {
                                                ui.selectable_value(
                                                    &mut selected_font,
                                                    font,
                                                    terminal_font_label(font),
                                                );
                                            }
                                        });
                                },
                            );
                            if selected_font != terminal_font {
                                command = Some(AppCommand::SetTerminalFont(selected_font));
                            }
                            ui.add_space(10.0);
                            ui.separator();
                            ui.add_space(10.0);
                            if settings_toggle_row(
                                ui,
                                "Programming ligatures",
                                "Shape eligible adjacent cells together while preserving cursor, \
                                 selection, and terminal grid ownership.",
                                terminal_ligatures,
                            ) {
                                command = Some(AppCommand::ToggleTerminalLigatures);
                            }
                            ui.add_space(10.0);
                            ui.separator();
                            ui.add_space(10.0);
                            if let Some(selected) = settings_segmented_row(
                                ui,
                                "Emoji presentation",
                                "Use bundled color artwork or deterministic monochrome fallback. \
                                 Terminal cell geometry is unchanged.",
                                &[
                                    (
                                        "Color",
                                        emoji_presentation == EmojiPresentationPreference::Color,
                                    ),
                                    (
                                        "Monochrome",
                                        emoji_presentation
                                            == EmojiPresentationPreference::Monochrome,
                                    ),
                                ],
                            ) {
                                command =
                                    Some(AppCommand::SetEmojiPresentation(if selected == 0 {
                                        EmojiPresentationPreference::Color
                                    } else {
                                        EmojiPresentationPreference::Monochrome
                                    }));
                            }
                        });

                        ui.add_space(12.0);

                        settings_card(ui, "Keyboard", |ui| {
                            ui.horizontal(|ui| {
                                ssh_paragraph(ui, "Command palette");
                                ui.add_space(8.0);
                                ui.label(
                                    egui::RichText::new(command_palette_shortcut)
                                        .size(12.0)
                                        .color(theme::TEXT_MUTED),
                                );
                            });
                            ui.add_space(6.0);
                            ui.horizontal(|ui| {
                                ssh_paragraph(ui, "Open Settings");
                                ui.add_space(8.0);
                                ui.label(
                                    egui::RichText::new(settings_shortcut)
                                        .size(12.0)
                                        .color(theme::TEXT_MUTED),
                                );
                            });
                            ui.add_space(10.0);
                            ui.separator();
                            ui.add_space(10.0);
                            if settings_toggle_row(
                                ui,
                                "Show quick-switch numbers",
                                "While the quick-switch modifier is held, briefly overlay each \
                                 eligible chip's number (1-9) in place of its usual status \
                                 presentation.",
                                quick_switch_overlay,
                            ) {
                                command = Some(AppCommand::ToggleQuickSwitchOverlay);
                            }
                        });

                        ui.add_space(12.0);

                        settings_card(ui, "SFTP", |ui| {
                            ui.horizontal_top(|ui| {
                                ui.vertical(|ui| {
                                    ui.set_max_width((ui.available_width() - 190.0).max(0.0));
                                    ui.label(
                                        egui::RichText::new("SFTP pane order")
                                            .color(theme::TEXT_PRIMARY),
                                    );
                                    ssh_paragraph(
                                        ui,
                                        "Visual order for the GUI SFTP file manager. Commands and accessibility labels still refer to Local and Remote, never Left and Right.",
                                    );
                                });
                                ui.add_space(16.0);
                                ui.vertical(|ui| {
                                    for (value, label) in [
                                        (
                                            SftpPaneOrderPreference::LocalLeft,
                                            "Local left · Remote right",
                                        ),
                                        (
                                            SftpPaneOrderPreference::RemoteLeft,
                                            "Remote left · Local right",
                                        ),
                                    ] {
                                        let response = ui.radio_value(
                                            state
                                                .sftp_pane_order
                                                .get_or_insert(sftp_pane_order),
                                            value,
                                            label,
                                        );
                                        if response.changed() {
                                            command = Some(AppCommand::SetSftpPaneOrder(value));
                                        }
                                    }
                                });
                            });
                            ui.add_space(10.0);
                            ui.horizontal_top(|ui| {
                                let mut label_id = None;
                                ui.vertical(|ui| {
                                    ui.set_max_width((ui.available_width() - 190.0).max(0.0));
                                    let label = ui.label(
                                        egui::RichText::new("Default local SFTP directory")
                                            .color(theme::TEXT_PRIMARY),
                                    );
                                    label_id = Some(label.id);
                                    ssh_paragraph(
                                        ui,
                                        "Starting local directory for new SFTP tabs. \
                                         Changing it updates only future sessions; `lcd` \
                                         affects the live session only.",
                                    );
                                });
                                ui.add_space(16.0);
                                ui.vertical(|ui| {
                                    let response = ui.add(
                                        TextEdit::singleline(
                                            &mut state.default_sftp_local_directory,
                                        )
                                        .id(field_id)
                                        .hint_text("Path to local directory")
                                        .desired_width(180.0),
                                    );
                                    let response = response
                                        .labelled_by(label_id.expect("label should be rendered"));
                                    if response.changed() {
                                        let trimmed = state.default_sftp_local_directory.trim();
                                        if trimmed.is_empty() {
                                            state.feedback = None;
                                            state.synced_value = Some(String::new());
                                            command =
                                                Some(AppCommand::SetDefaultSftpLocalDirectory(
                                                    None,
                                                ));
                                        } else if trimmed.chars().any(char::is_control) {
                                            state.feedback = Some(
                                                "Default local SFTP directory must not contain control characters."
                                                    .to_owned(),
                                            );
                                        } else {
                                            state.feedback = None;
                                            state.synced_value = Some(trimmed.to_owned());
                                            command = Some(
                                                AppCommand::SetDefaultSftpLocalDirectory(Some(
                                                    PathBuf::from(trimmed),
                                                )),
                                            );
                                        }
                                    }
                                });
                            });
                            if let Some(feedback) = &state.feedback {
                                ui.add_space(6.0);
                                ui.colored_label(theme::STATUS_ERROR, feedback);
                            }
                        });
                    });
                });
        });
    });
    ui.data_mut(|data| data.insert_temp(state_id, state));
    command
}

const fn terminal_font_label(font: TerminalFontPreference) -> &'static str {
    match font {
        TerminalFontPreference::JetBrainsMono => "JetBrains Mono",
        TerminalFontPreference::IosevkaTerm => "Iosevka Term",
        TerminalFontPreference::JuliaMono => "JuliaMono",
        TerminalFontPreference::MapleMono => "Maple Mono",
    }
}

/// A titled card matching the launcher/profile-editor "quiet section" visual
/// language (`ssh_section_heading` + a bordered, rounded surface), so
/// Settings groups related controls the same way the rest of the app does
/// instead of a flat, plain list of buttons.
fn settings_card(ui: &mut Ui, title: &str, body: impl FnOnce(&mut Ui)) {
    egui::Frame::new()
        .fill(theme::SURFACE_TAB_INACTIVE)
        .stroke(Stroke::new(1.0, theme::BORDER_SUBTLE))
        .corner_radius(8.0)
        .inner_margin(egui::Margin::same(16))
        .show(ui, |ui| {
            ssh_section_heading(ui, title);
            ui.add_space(6.0);
            body(ui);
        });
}

/// One labeled on/off preference row: a fixed title and state-independent
/// description on the left, and a pill-shaped toggle switch on the right
/// (`docs/images/gui-mockups/settings.png`) - replacing a plain text button
/// whose entire label used to flip between "shown"/"hidden" copy. Returns
/// whether the switch was clicked this frame; the caller still owns
/// dispatching the actual `AppCommand`, matching every other control here.
fn settings_toggle_row(ui: &mut Ui, title: &str, description: &str, value: bool) -> bool {
    let mut clicked = false;
    egui::Sides::new().show(
        ui,
        |ui| {
            // Reserve room for the switch itself (and the `Sides` gap) so
            // the description wraps at measurement time instead of laying
            // out as one long unwrapped line that pushes the switch off
            // the right edge of the card.
            ui.set_max_width(ui.available_width() - 60.0);
            ui.vertical(|ui| {
                ui.label(egui::RichText::new(title).color(theme::TEXT_PRIMARY));
                ssh_paragraph(ui, description);
            });
        },
        |ui| {
            clicked = toggle_switch(ui, value, title).clicked();
        },
    );
    clicked
}

/// Painter-drawn pill-shaped toggle switch matching the mockup's on/off
/// control (`docs/images/gui-mockups/settings.png`): a rounded track that
/// fills with the accent color when on, and a circular knob that slides to
/// the matching side - instead of a text button whose whole label changes
/// between "shown"/"hidden" copy. An explicit accessible label is set (like
/// `paint_close_button`'s pattern) since the switch has no text of its own
/// for screen readers or headless-test queries to find.
fn toggle_switch(ui: &mut Ui, value: bool, accessible_label: &str) -> egui::Response {
    let desired_size = egui::vec2(40.0, 22.0);
    let (rect, response) = ui.allocate_exact_size(desired_size, Sense::click());
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Checkbox, true, accessible_label));

    if ui.is_rect_visible(rect) {
        let how_on = ui.ctx().animate_bool(response.id, value);
        let rounding = rect.height() / 2.0;
        let track_fill = theme::SURFACE_TAB_INACTIVE.lerp_to_gamma(theme::ACCENT_PRIMARY, how_on);
        let track_stroke = theme::BORDER_SUBTLE.lerp_to_gamma(theme::ACCENT_PRIMARY, how_on);
        ui.painter().rect_filled(rect, rounding, track_fill);
        ui.painter().rect_stroke(
            rect,
            rounding,
            Stroke::new(1.0, track_stroke),
            egui::StrokeKind::Inside,
        );
        let knob_radius = rounding - 3.0;
        let knob_x = egui::lerp((rect.left() + rounding)..=(rect.right() - rounding), how_on);
        ui.painter().circle_filled(
            egui::pos2(knob_x, rect.center().y),
            knob_radius,
            egui::Color32::WHITE,
        );
    }

    response.on_hover_text(accessible_label)
}

/// One labeled multi-choice preference row: a fixed title/description on the
/// left and a segmented button group on the right
/// (`docs/images/gui-mockups/settings.png`'s "Session chip layout" row),
/// replacing a single text button whose label flipped to name the *other*
/// choice. Returns the index of a newly selected (previously inactive)
/// option; clicking the already-active option is a no-op, matching ordinary
/// segmented-control behavior.
/// The width a row of `selectable_label`s will occupy.
///
/// `egui::Sides` gives its left closure whatever width that closure claims,
/// so a settings row reserves the right-hand control's width up front and
/// lets the description wrap into the remainder. Reserving a fixed guess
/// works only until a control outgrows it, and the failure is not a clipped
/// button: egui grows the enclosing card to fit instead, so the card spills
/// past the window's right edge *and* every later card inherits the wider
/// content width and spills with it. Measuring the control removes the guess.
fn segmented_control_width(ui: &Ui, options: &[(&str, bool)]) -> f32 {
    let font = egui::TextStyle::Button.resolve(ui.style());
    let padding = ui.spacing().button_padding.x * 2.0;
    let labels: f32 = options
        .iter()
        .map(|(label, _)| {
            ui.painter()
                .layout_no_wrap(
                    (*label).to_owned(),
                    font.clone(),
                    egui::Color32::PLACEHOLDER,
                )
                .size()
                .x
                + padding
        })
        .sum();
    labels + ui.spacing().item_spacing.x * options.len().saturating_sub(1) as f32
}

/// The narrowest a settings row's description column is allowed to become
/// while making room for its control, so a wide control cannot squeeze the
/// prose into a one-word-per-line ribbon.
const SETTINGS_MIN_DESCRIPTION_WIDTH: f32 = 180.0;

fn settings_segmented_row(
    ui: &mut Ui,
    title: &str,
    description: &str,
    options: &[(&str, bool)],
) -> Option<usize> {
    let mut clicked = None;
    // Measured before the row is laid out, because the left closure runs
    // first and has to know how much to leave behind.
    let control_width = segmented_control_width(ui, options);
    egui::Sides::new().show(
        ui,
        |ui| {
            // Reserve exactly what the buttons need (plus the `Sides` gap) so
            // the description wraps at measurement time instead of laying out
            // as one long unwrapped line that pushes the segmented buttons
            // off the right edge of the card and out of click range.
            let reserved = control_width + ui.spacing().item_spacing.x;
            ui.set_max_width((ui.available_width() - reserved).max(SETTINGS_MIN_DESCRIPTION_WIDTH));
            ui.vertical(|ui| {
                ui.label(egui::RichText::new(title).color(theme::TEXT_PRIMARY));
                ssh_paragraph(ui, description);
            });
        },
        |ui| {
            ui.horizontal(|ui| {
                for (index, (label, selected)) in options.iter().enumerate() {
                    if ui.selectable_label(*selected, *label).clicked() && !*selected {
                        clicked = Some(index);
                    }
                }
            });
        },
    );
    clicked
}

/// Give a slider a rail that is actually visible inside a `settings_card`.
///
/// egui paints a slider's rail with `widgets.inactive.bg_fill` (see
/// `egui::Slider::slider_ui`), and `theme::default_visuals` sets that to
/// `SURFACE_TAB_INACTIVE` - which is exactly `settings_card`'s own fill. The
/// rail therefore vanished into the card and the only thing left on screen
/// was the handle's one-pixel outline, so the control read as a small empty
/// box floating in whitespace rather than as a slider. Lifting the rail one
/// surface step and filling the travelled portion with the accent color
/// matches `toggle_switch`'s vocabulary (inert track, accent for "how far
/// on") and makes the handle's position unmistakable.
fn style_settings_slider(ui: &mut Ui) {
    let visuals = &mut ui.style_mut().visuals;
    visuals.widgets.inactive.bg_fill = theme::SURFACE_TAB_ACTIVE;
    visuals.selection.bg_fill = theme::ACCENT_PRIMARY;
}

/// A labeled row with a discrete, clickstop-only slider: dragging or
/// clicking only ever lands on one of `options`' exact indices, unlike a
/// continuous `egui::Slider`, since a scroll-speed multiplier is meant to be
/// chosen from a small named set (mirroring `settings_segmented_row`'s
/// discrete-choice intent) rather than fine-tuned to an arbitrary numeric
/// value. Returns the newly selected value when the slider moves to a
/// different clickstop than `selected` this frame.
fn settings_clickstop_row(
    ui: &mut Ui,
    title: &str,
    description: &str,
    options: &[ScrollSpeedPreference],
    selected: ScrollSpeedPreference,
) -> Option<ScrollSpeedPreference> {
    const SLIDER_WIDTH: f32 = 160.0;

    let mut changed = None;
    // `egui::Sides` defaults its row height to a single `interact_size.y`
    // (matching the toggle/segmented rows' one-line right side), but this
    // row's right side stacks a slider *and* a value label underneath it.
    // Reserve enough height for both stacked lines up front.
    let row_height = ui.spacing().interact_size.y * 2.0 + 4.0;
    egui::Sides::new().height(row_height).show(
        ui,
        |ui| {
            // Same defensive width reservation as the other settings rows:
            // without it the description can measure as one long unwrapped
            // line and push the slider off the right edge of the card.
            ui.set_max_width(ui.available_width() - 190.0);
            ui.vertical(|ui| {
                ui.label(egui::RichText::new(title).color(theme::TEXT_PRIMARY));
                ssh_paragraph(ui, description);
            });
        },
        |ui| {
            // Unlike `ui.horizontal`, plain `ui.vertical` always lays out
            // its children with `Layout::top_down(Align::Min)` and does
            // not mirror the enclosing `Sides` right-to-left direction (see
            // `egui::Ui::horizontal`, which explicitly checks
            // `placer.prefer_right_to_left()` and `ui.vertical`, which
            // does not). A bare `ui.vertical(...)` here inherited the
            // *entire* remaining card width as its rect and then
            // left-aligned the slider and label inside it, so on any card
            // wider than description-text-plus-slider, the block rendered
            // immediately after the description paragraph instead of
            // pinned to the card's right edge - squeezing the slider down
            // to a sliver-sized hit target and spilling the value label
            // over the description (reported: "you can't tell it's
            // actually a slider" and "sliding the value doesn't seem to
            // change scroll speed"). Explicitly allocating a
            // `SLIDER_WIDTH`-wide block lets the *outer* right-to-left
            // cursor place it, matching how `toggle_switch` and
            // `settings_segmented_row`'s `ui.horizontal` already anchor to
            // the right edge.
            ui.allocate_ui_with_layout(
                egui::vec2(SLIDER_WIDTH, row_height),
                egui::Layout::top_down(egui::Align::Center),
                |ui| {
                    let max_index = options.len().saturating_sub(1);
                    let mut index = selected.index().min(max_index);
                    ui.style_mut().spacing.slider_width = SLIDER_WIDTH;
                    style_settings_slider(ui);
                    let response = ui.add(
                        egui::Slider::new(&mut index, 0..=max_index)
                            .step_by(1.0)
                            .trailing_fill(true)
                            .show_value(false),
                    );
                    if response.changed() {
                        let new_value = ScrollSpeedPreference::from_index(index);
                        if new_value != selected {
                            changed = Some(new_value);
                        }
                    }
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(selected.label())
                            .size(12.0)
                            .color(theme::TEXT_MUTED),
                    );
                },
            );
        },
    );
    changed
}

/// One row's stable identifying summary in the Profiles list, without
/// exposing credential material.
fn profile_summary(profile: &Profile) -> (&'static str, String, String) {
    match profile {
        Profile::Local(local) => {
            let mut description = local.executable().to_owned();
            if !local.arguments().is_empty() {
                description.push(' ');
                description.push_str(&local.arguments().join(" "));
            }
            ("Local", local.identifier().to_owned(), description)
        }
        Profile::Ssh(ssh) => (
            match ssh.profile_kind() {
                RemoteProfileKind::Ssh => "SSH",
                RemoteProfileKind::Sftp => "SFTP",
            },
            ssh.identifier().to_owned(),
            format!("{}@{}:{}", ssh.username(), ssh.host(), ssh.port()),
        ),
        Profile::Serial(serial) => (
            "Serial",
            serial.identifier().to_owned(),
            format!("{} · {} baud", serial.device(), serial.baud_rate()),
        ),
    }
}

/// Which staged view the Profiles surface is currently showing. Multi-field
/// edits are staged behind Save; Cancel discards them
/// (`docs/gui-design.md` "Profile editing").
#[derive(Clone, Default)]
enum ProfilesScreenMode {
    #[default]
    List,
    EditLocal(LocalProfileDraft),
    EditSsh(SshProfileDraft),
    EditSerial(SerialProfileDraft),
    ConfirmDelete {
        identifier: String,
        references: usize,
    },
}

#[derive(Clone, Default)]
struct ProfilesScreenState {
    mode: ProfilesScreenMode,
}

fn profiles_state_id(tab_id: TabId) -> egui::Id {
    egui::Id::new(("profiles_state", tab_id))
}

#[derive(Clone)]
struct LocalProfileDraft {
    /// `None` while creating a new profile; `Some` while editing an
    /// existing one, so Save always upserts by this original identifier
    /// rather than the (possibly just-edited) name field.
    original_id: Option<String>,
    name: String,
    executable: String,
    arguments: String,
    working_directory: String,
    durable_session: DurableSessionDraft,
    error: Option<String>,
}

impl Default for LocalProfileDraft {
    /// A brand-new Local profile defaults its executable to this
    /// platform's actual default shell (`$SHELL`/`COMSPEC`, matching the
    /// Local Shell launcher card) rather than leaving it empty, and its
    /// durable-session provider to fesTerm native (preserved for callers,
    /// such as tests, that don't yet detect a local default; prefer
    /// [`Self::new`] elsewhere).
    fn default() -> Self {
        Self::new(PersistenceProviderKind::FestermSessiond)
    }
}

impl LocalProfileDraft {
    /// A brand-new Local profile, defaulting its durable-session provider
    /// (once the toggle is switched on) to `local_default_provider` --
    /// normally the result of [`PersistenceProviderKind::default_for_local_session`]
    /// detected once at composition-root time.
    fn new(local_default_provider: PersistenceProviderKind) -> Self {
        Self {
            original_id: None,
            name: String::new(),
            executable: festerm_pty::default_local_profile()
                .map(|profile| profile.executable().display().to_string())
                .unwrap_or_default(),
            arguments: String::new(),
            working_directory: String::new(),
            durable_session: DurableSessionDraft {
                local_default_provider,
                ..DurableSessionDraft::default()
            },
            error: None,
        }
    }

    fn from_profile(local: &festerm_config::LocalProfileConfiguration) -> Self {
        Self {
            original_id: Some(local.identifier().to_owned()),
            name: local.identifier().to_owned(),
            executable: local.executable().to_owned(),
            arguments: local.arguments().join(" "),
            working_directory: local
                .working_directory()
                .map(|path| path.display().to_string())
                .unwrap_or_default(),
            durable_session: DurableSessionDraft::from_persistence(local.persistence()),
            error: None,
        }
    }

    fn build(&self) -> Result<Profile, String> {
        let arguments = self
            .arguments
            .split_whitespace()
            .map(str::to_owned)
            .collect();
        let working_directory = (!self.working_directory.trim().is_empty())
            .then(|| self.working_directory.trim().to_owned());
        let profile = Profile::local(
            self.name.trim(),
            self.executable.trim(),
            arguments,
            working_directory,
        )
        .map_err(|error| error.to_string())?;
        match self.durable_session.persistence()? {
            Some(persistence) => profile
                .with_persistence(persistence.provider(), persistence.session_name())
                .map_err(|error| error.to_string()),
            None => Ok(profile),
        }
    }
}

#[derive(Clone)]
struct SshProfileDraft {
    original_id: Option<String>,
    name: String,
    host: String,
    port: String,
    username: String,
    port_forwards: Vec<SshPortForwardDraft>,
    profile_kind: RemoteProfileKind,
    sftp_gui_mode: bool,
    /// Which credential kind the editor's authentication section is
    /// currently showing entry fields for. Independent of
    /// `stored_credential_kind`, which reflects what is actually saved —
    /// switching this radio only changes which fields are visible/active
    /// until "Save password"/"Save private key" is clicked.
    auth_method: SshAuthenticationMethod,
    /// Transient plaintext password entry for "remember/replace password"
    /// in the profile editor (item 5: relocated here from the launcher's
    /// live-connect form). Never persisted to disk directly — only ever
    /// sent to the composition root's secret store worker via
    /// `AppCommand::StoreProfilePassword` and cleared immediately after.
    password: String,
    /// Transient OpenSSH private-key text for "remember/replace private
    /// key" in the profile editor. Like `password`, never persisted
    /// directly — only ever sent to the composition root's secret store
    /// worker via `AppCommand::StoreProfilePrivateKey` and cleared
    /// immediately after.
    private_key: String,
    /// Transient optional passphrase for `private_key`, cleared alongside it.
    key_passphrase: String,
    has_stored_credential: bool,
    /// Which kind of credential is actually stored for this profile.
    /// Meaningless unless `has_stored_credential` is true.
    stored_credential_kind: CredentialKind,
    durable_session: DurableSessionDraft,
    remote_tmux_probe: Box<RemoteTmuxProbeState>,
    error: Option<String>,
}

impl Default for SshProfileDraft {
    /// A brand-new SSH profile defaults its port to "22" in the text box
    /// (matching Quick Connect's `SshLauncherForm::DEFAULT_PORT`) rather
    /// than leaving it empty.
    fn default() -> Self {
        Self {
            original_id: None,
            name: String::new(),
            host: String::new(),
            port: SshLauncherForm::DEFAULT_PORT.to_string(),
            username: String::new(),
            port_forwards: Vec::new(),
            profile_kind: RemoteProfileKind::Ssh,
            sftp_gui_mode: true,
            auth_method: SshAuthenticationMethod::Password,
            password: String::new(),
            private_key: String::new(),
            key_passphrase: String::new(),
            has_stored_credential: false,
            stored_credential_kind: CredentialKind::Password,
            durable_session: DurableSessionDraft::default(),
            remote_tmux_probe: Box::default(),
            error: None,
        }
    }
}

impl SshProfileDraft {
    fn new_sftp() -> Self {
        Self {
            profile_kind: RemoteProfileKind::Sftp,
            ..Self::default()
        }
    }

    fn from_profile(ssh: &SshProfileConfiguration) -> Self {
        let stored_credential_kind = ssh.credential_kind();
        Self {
            original_id: Some(ssh.identifier().to_owned()),
            name: ssh.identifier().to_owned(),
            host: ssh.host().to_owned(),
            port: ssh.port().to_string(),
            username: ssh.username().to_owned(),
            port_forwards: ssh
                .port_forwards()
                .iter()
                .map(SshPortForwardDraft::from_configuration)
                .collect(),
            profile_kind: ssh.profile_kind(),
            sftp_gui_mode: ssh.sftp_gui_mode(),
            auth_method: match stored_credential_kind {
                CredentialKind::Password => SshAuthenticationMethod::Password,
                CredentialKind::PrivateKey => SshAuthenticationMethod::PrivateKey,
            },
            password: String::new(),
            private_key: String::new(),
            key_passphrase: String::new(),
            has_stored_credential: ssh.credential_reference().is_some(),
            stored_credential_kind,
            durable_session: DurableSessionDraft::from_persistence(ssh.persistence()),
            remote_tmux_probe: Box::default(),
            error: None,
        }
    }

    fn sync_remote_durable_provider_default(
        &mut self,
        context: &egui::Context,
        configuration: &Configuration,
    ) {
        let request = self.remote_tmux_probe_request(configuration);
        self.remote_tmux_probe
            .sync(context, &mut self.durable_session, request);
    }

    fn remote_tmux_probe_request(
        &self,
        configuration: &Configuration,
    ) -> Option<RemoteTmuxProbeRequest> {
        let port: u16 = self.port.trim().parse().ok()?;
        let profile = SshConnectionProfile::new(
            HostIdentity::new(&self.host, port).ok()?,
            self.username.clone(),
            "xterm-256color",
            TerminalSize::new(80, 24).expect("profile-editor probe terminal size is valid"),
        )
        .ok()?;
        let known_host_fingerprint = configuration
            .known_host_fingerprint(profile.identity().host(), profile.identity().port())?
            .to_owned();
        let (authentication, authentication_key) = match self.auth_method {
            SshAuthenticationMethod::Password => {
                if self.password.is_empty() {
                    return None;
                }
                (
                    SshAuthentication::password(self.password.clone()),
                    RemoteTmuxProbeAuthKey::Password(self.password.clone()),
                )
            }
            SshAuthenticationMethod::PrivateKey => {
                if self.private_key.is_empty() {
                    return None;
                }
                (
                    SshLauncherForm::parse_private_key(
                        self.private_key.clone(),
                        self.key_passphrase.clone(),
                    )
                    .ok()?,
                    RemoteTmuxProbeAuthKey::PrivateKey {
                        private_key: self.private_key.clone(),
                        key_passphrase: self.key_passphrase.clone(),
                    },
                )
            }
            // Saved SSH profiles don't yet support storing a certificate
            // credential (deferred alongside certificate auth in #120), so
            // there is nothing to probe with here.
            SshAuthenticationMethod::Certificate => return None,
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

    fn build(&self, existing_profile: Option<&SshProfileConfiguration>) -> Result<Profile, String> {
        let port: u16 = self
            .port
            .trim()
            .parse()
            .map_err(|_| "SSH port must be a number between 1 and 65535".to_owned())?;
        let port_forwards = self
            .port_forwards
            .iter()
            .map(SshPortForwardDraft::build)
            .collect::<Result<Vec<_>, _>>()?;
        let profile = match self.profile_kind {
            RemoteProfileKind::Ssh => Profile::ssh(
                self.name.trim(),
                self.host.trim(),
                port,
                self.username.trim(),
                "xterm-256color",
                80,
                24,
            ),
            RemoteProfileKind::Sftp => Profile::sftp(
                self.name.trim(),
                self.host.trim(),
                port,
                self.username.trim(),
                self.sftp_gui_mode,
            ),
        }
        .map_err(|error| error.to_string())?;
        let profile = match profile {
            Profile::Ssh(ssh) if self.profile_kind == RemoteProfileKind::Ssh => Profile::Ssh(
                ssh.with_port_forwards(port_forwards)
                    .map_err(|error| error.to_string())?,
            ),
            Profile::Ssh(ssh) => Profile::Ssh(ssh),
            Profile::Local(_) | Profile::Serial(_) => unreachable!("Profile::ssh returns SSH"),
        };
        let profile = match (self.profile_kind, self.durable_session.persistence()?) {
            (RemoteProfileKind::Ssh, Some(persistence)) => profile
                .with_persistence(persistence.provider(), persistence.session_name())
                .map_err(|error| error.to_string()),
            (RemoteProfileKind::Ssh, None) | (RemoteProfileKind::Sftp, _) => Ok(profile),
        }?;
        if let Some(existing_profile) = existing_profile {
            if let Some(reference) = existing_profile.credential_reference() {
                return profile
                    .with_credential_reference_kind(
                        reference.duplicate_for_transport(),
                        existing_profile.credential_kind(),
                    )
                    .map_err(|error| error.to_string());
            }
        }
        Ok(profile)
    }

    fn take_initial_credential(&mut self) -> Option<ProfileCredentialToStore> {
        if self.original_id.is_some() {
            return None;
        }
        match self.auth_method {
            SshAuthenticationMethod::Password if !self.password.is_empty() => {
                Some(ProfileCredentialToStore::Password(PasswordToStore::new(
                    std::mem::take(&mut self.password),
                )))
            }
            SshAuthenticationMethod::PrivateKey if !self.private_key.trim().is_empty() => {
                let passphrase = (!self.key_passphrase.is_empty())
                    .then(|| std::mem::take(&mut self.key_passphrase));
                Some(ProfileCredentialToStore::PrivateKey(
                    PrivateKeyToStore::new(std::mem::take(&mut self.private_key), passphrase),
                ))
            }
            SshAuthenticationMethod::Password
            | SshAuthenticationMethod::PrivateKey
            | SshAuthenticationMethod::Certificate => None,
        }
    }
}

fn ssh_profile_name_collides(
    configuration: &festerm_config::Configuration,
    original_id: Option<&str>,
    candidate_name: &str,
) -> bool {
    configuration
        .profile(candidate_name)
        .is_some_and(|profile| Some(profile.identifier()) != original_id)
}

#[derive(Clone)]
struct SerialProfileDraft {
    original_id: Option<String>,
    name: String,
    device: String,
    baud_rate: String,
    data_bits: festerm_config::SerialDataBits,
    parity: festerm_config::SerialParity,
    stop_bits: festerm_config::SerialStopBits,
    flow_control: festerm_config::SerialFlowControl,
    error: Option<String>,
}

impl Default for SerialProfileDraft {
    fn default() -> Self {
        Self {
            original_id: None,
            name: String::new(),
            device: String::new(),
            baud_rate: "115200".to_owned(),
            data_bits: festerm_config::SerialDataBits::Eight,
            parity: festerm_config::SerialParity::None,
            stop_bits: festerm_config::SerialStopBits::One,
            flow_control: festerm_config::SerialFlowControl::None,
            error: None,
        }
    }
}

impl SerialProfileDraft {
    fn from_profile(serial: &festerm_config::SerialProfileConfiguration) -> Self {
        Self {
            original_id: Some(serial.identifier().to_owned()),
            name: serial.identifier().to_owned(),
            device: serial.device().to_owned(),
            baud_rate: serial.baud_rate().to_string(),
            data_bits: serial.data_bits(),
            parity: serial.parity(),
            stop_bits: serial.stop_bits(),
            flow_control: serial.flow_control(),
            error: None,
        }
    }

    fn build_profile(&self) -> Result<Profile, String> {
        let baud_rate: u32 = self
            .baud_rate
            .trim()
            .parse()
            .map_err(|_| "Baud rate must be a positive number".to_owned())?;
        Profile::serial(
            self.name.trim(),
            self.device.trim(),
            baud_rate,
            self.data_bits,
            self.parity,
            self.stop_bits,
            self.flow_control,
        )
        .map_err(|error| error.to_string())
    }
}

/// The standalone Profiles management surface: list, create, edit,
/// duplicate, and delete reusable local/SSH launch definitions
/// (`docs/gui-design.md` "Profile editing").
fn serial_enum_combo<T: Copy + PartialEq + SerialEnumLabels>(
    ui: &mut Ui,
    label: &str,
    current: &mut T,
) {
    ui.horizontal(|ui| {
        ui.label(label);
        egui::ComboBox::from_id_salt(("serial_enum_combo", label))
            .selected_text(current.label())
            .show_ui(ui, |ui| {
                for (variant, variant_label) in T::all() {
                    ui.selectable_value(current, variant, variant_label);
                }
            });
    });
}

trait SerialEnumLabels: Sized {
    fn label(&self) -> &'static str;
    fn all() -> Vec<(Self, &'static str)>;
}

impl SerialEnumLabels for festerm_config::SerialDataBits {
    fn label(&self) -> &'static str {
        match self {
            Self::Five => "5",
            Self::Six => "6",
            Self::Seven => "7",
            Self::Eight => "8",
        }
    }
    fn all() -> Vec<(Self, &'static str)> {
        vec![
            (Self::Five, "5"),
            (Self::Six, "6"),
            (Self::Seven, "7"),
            (Self::Eight, "8"),
        ]
    }
}

impl SerialEnumLabels for festerm_config::SerialParity {
    fn label(&self) -> &'static str {
        match self {
            Self::None => "None",
            Self::Odd => "Odd",
            Self::Even => "Even",
        }
    }
    fn all() -> Vec<(Self, &'static str)> {
        vec![
            (Self::None, "None"),
            (Self::Odd, "Odd"),
            (Self::Even, "Even"),
        ]
    }
}

impl SerialEnumLabels for festerm_config::SerialStopBits {
    fn label(&self) -> &'static str {
        match self {
            Self::One => "1",
            Self::Two => "2",
        }
    }
    fn all() -> Vec<(Self, &'static str)> {
        vec![(Self::One, "1"), (Self::Two, "2")]
    }
}

impl SerialEnumLabels for festerm_config::SerialFlowControl {
    fn label(&self) -> &'static str {
        match self {
            Self::None => "None",
            Self::Software => "Software (XON/XOFF)",
            Self::Hardware => "Hardware (RTS/CTS)",
        }
    }
    fn all() -> Vec<(Self, &'static str)> {
        vec![
            (Self::None, "None"),
            (Self::Software, "Software (XON/XOFF)"),
            (Self::Hardware, "Hardware (RTS/CTS)"),
        ]
    }
}

fn profile_text_edit(
    ui: &mut Ui,
    tab_id: TabId,
    field: &'static str,
    label: &str,
    value: &mut String,
) -> egui::Response {
    profile_text_edit_inner(ui, tab_id, field, label, value, false)
}

fn profile_text_edit_with_id(
    ui: &mut Ui,
    tab_id: TabId,
    field: impl std::hash::Hash + std::fmt::Debug,
    label: &str,
    value: &mut String,
) -> egui::Response {
    profile_text_edit_inner(ui, tab_id, field, label, value, false)
}

fn profile_password_edit(
    ui: &mut Ui,
    tab_id: TabId,
    field: &'static str,
    label: &str,
    value: &mut String,
) -> egui::Response {
    profile_text_edit_inner(ui, tab_id, field, label, value, true)
}

fn profile_text_edit_inner(
    ui: &mut Ui,
    tab_id: TabId,
    field: impl std::hash::Hash + std::fmt::Debug,
    label: &str,
    value: &mut String,
    password: bool,
) -> egui::Response {
    ui.horizontal(|ui| {
        ui.add_space(2.0);
        let label = ui.add(
            egui::Label::new(egui::RichText::new(label).color(theme::TEXT_SECONDARY))
                .selectable(false),
        );
        let field = ui.add(
            TextEdit::singleline(value)
                .id_salt(("profiles_form", tab_id, field))
                .password(password)
                .desired_width(240.0),
        );
        field.labelled_by(label.id)
    })
    .inner
}

/// Maximum number of `PATH` matches offered below the Local profile
/// executable field as the user types.
const EXECUTABLE_SUGGESTION_LIMIT: usize = 6;

/// The Local profile editor's executable field, with a live `PATH`-search
/// dropdown: as the user types a bare command name (e.g. `cmd`), this
/// offers up to [`EXECUTABLE_SUGGESTION_LIMIT`] concrete absolute paths
/// found on `PATH` so they can pin down exactly which one to launch
/// instead of relying on fesTerm's own search order at spawn time.
/// Selecting a suggestion fills in its absolute path; leaving the field as
/// a bare name is equally valid — it is resolved against `PATH` normally
/// when the profile launches.
fn local_executable_field(ui: &mut Ui, autocomplete_id: egui::Id, value: &mut String) {
    let dropdown_rect_id = autocomplete_id.with("suggestions-rect");
    ui.vertical(|ui| {
        let field = ui
            .horizontal(|ui| {
                let label = ui.add(
                    egui::Label::new(
                        egui::RichText::new("Executable").color(theme::TEXT_SECONDARY),
                    )
                    .selectable(false),
                );
                let field = ui.add(TextEdit::singleline(value).desired_width(240.0));
                field.labelled_by(label.id)
            })
            .inner;

        let mut suppress = ui.data(|data| data.get_temp::<bool>(autocomplete_id).unwrap_or(false));
        if field.changed() {
            suppress = false;
        }

        // A real mouse click on a suggestion first lands here as a click
        // "elsewhere" as far as the text field is concerned, so egui drops
        // the field's focus *before* this function runs again this frame.
        // Without this fallback, `field.has_focus()` would already be false
        // by the time we decide whether to show the dropdown, so the
        // suggestion would vanish out from under the click and never
        // receive it. Keep the dropdown alive for this frame if the click
        // that just happened started inside last frame's dropdown rect.
        let last_dropdown_rect: Option<egui::Rect> =
            ui.data(|data| data.get_temp(dropdown_rect_id));
        let click_started_in_dropdown = ui.input(|input| {
            input.pointer.primary_clicked()
                && input
                    .pointer
                    .interact_pos()
                    .zip(last_dropdown_rect)
                    .is_some_and(|(pos, rect)| rect.contains(pos))
        });

        if (field.has_focus() || click_started_in_dropdown) && !suppress && !value.trim().is_empty()
        {
            let suggestions =
                festerm_pty::search_path_executables(value.trim(), EXECUTABLE_SUGGESTION_LIMIT);
            if !suggestions.is_empty() {
                ui.add_space(4.0);
                let dropdown = egui::Frame::new()
                    .fill(theme::SURFACE_TAB_INACTIVE)
                    .stroke(egui::Stroke::new(1.0, theme::BORDER_SUBTLE))
                    .corner_radius(6.0)
                    .inner_margin(6.0)
                    .show(ui, |ui| {
                        for candidate in &suggestions {
                            let text = candidate.display().to_string();
                            // Force a single line and an explicit bright color:
                            // the default inactive-widget text style is dim
                            // (hard to read against the suggestion frame), and
                            // wrapping onto a second line makes long absolute
                            // paths harder to scan at a glance.
                            let response = ui.add(
                                egui::Button::selectable(
                                    false,
                                    egui::RichText::new(&text).color(theme::TEXT_PRIMARY),
                                )
                                .wrap_mode(egui::TextWrapMode::Extend),
                            );
                            if response.clicked() {
                                *value = text;
                                suppress = true;
                            }
                        }
                    });
                ui.data_mut(|data| data.insert_temp(dropdown_rect_id, dropdown.response.rect));
            }
        } else {
            ui.data_mut(|data| data.remove::<egui::Rect>(dropdown_rect_id));
        }
        ui.data_mut(|data| data.insert_temp(autocomplete_id, suppress));
    });
}

pub fn show_profiles(
    ui: &mut Ui,
    tab_id: TabId,
    configuration: &festerm_config::Configuration,
    pending_edit: Option<String>,
    pending_create: Option<NewProfileKind>,
    local_default_provider: PersistenceProviderKind,
) -> Option<AppCommand> {
    let state_id = profiles_state_id(tab_id);
    let mut state = ui.data(|data| {
        data.get_temp::<ProfilesScreenState>(state_id)
            .unwrap_or_default()
    });
    let mut command = None;

    if let Some(identifier) = pending_edit {
        if let Some(profile) = configuration.profile(&identifier) {
            state.mode = match profile {
                Profile::Local(local) => {
                    ProfilesScreenMode::EditLocal(LocalProfileDraft::from_profile(local))
                }
                Profile::Ssh(ssh) => {
                    ProfilesScreenMode::EditSsh(SshProfileDraft::from_profile(ssh))
                }
                Profile::Serial(serial) => {
                    ProfilesScreenMode::EditSerial(SerialProfileDraft::from_profile(serial))
                }
            };
        }
    }

    if let Some(kind) = pending_create {
        state.mode = match kind {
            NewProfileKind::Local => {
                ProfilesScreenMode::EditLocal(LocalProfileDraft::new(local_default_provider))
            }
            NewProfileKind::Ssh => ProfilesScreenMode::EditSsh(SshProfileDraft::default()),
            NewProfileKind::Sftp => ProfilesScreenMode::EditSsh(SshProfileDraft::new_sftp()),
            NewProfileKind::Serial => ProfilesScreenMode::EditSerial(SerialProfileDraft::default()),
        };
    }

    let mut next_mode = None;
    ui.horizontal(|ui| {
        ui.add_space(26.0);
        ui.vertical(|ui| {
    match &mut state.mode {
        ProfilesScreenMode::List => {
            ui.vertical(|ui| {
                ui.add_space(24.0);
                ui.heading("Profiles");
                ui.label("Reusable local shell, SSH, SFTP, and serial launch definitions.");
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui.button("New Local Profile").clicked() {
                        next_mode = Some(ProfilesScreenMode::EditLocal(LocalProfileDraft::new(
                            local_default_provider,
                        )));
                    }
                    if ui.button("New SSH Profile").clicked() {
                        next_mode = Some(ProfilesScreenMode::EditSsh(SshProfileDraft::default()));
                    }
                    if ui.button("New SFTP Profile").clicked() {
                        next_mode =
                            Some(ProfilesScreenMode::EditSsh(SshProfileDraft::new_sftp()));
                    }
                    if ui.button("New Serial Profile").clicked() {
                        next_mode =
                            Some(ProfilesScreenMode::EditSerial(SerialProfileDraft::default()));
                    }
                });
                ui.add_space(12.0);
                ui.separator();
                if configuration.profiles().is_empty() {
                    ui.add_space(12.0);
                    ui.label("No profiles saved yet.");
                }
                for profile in configuration.profiles() {
                    let (kind, name, description) = profile_summary(profile);
                    ui.add_space(8.0);
                    // Drag-and-drop reorder (`Configuration::with_reordered_profiles`),
                    // reflected in the Launcher's own profile ordering too.
                    // Only the chip frame itself is a drag source, matching
                    // the chrome chip row's press-and-hold-anywhere-on-the-
                    // card convention; the Connect/Edit/Duplicate/Delete
                    // buttons sit outside it and are unaffected.
                    let drag_id = egui::Id::new("profile_reorder_source").with(&name);
                    let mut row_rect = None;
                    ui.horizontal(|ui| {
                        let drag_response = ui.dnd_drag_source(drag_id, name.clone(), |ui| {
                            egui::Frame::new()
                                .fill(theme::SURFACE_TAB_INACTIVE)
                                .stroke(Stroke::new(1.0, theme::BORDER_SUBTLE))
                                .corner_radius(8.0)
                                .inner_margin(egui::Margin::symmetric(14, 10))
                                .show(ui, |ui| {
                                    ui.set_width(220.0);
                                    ui.vertical(|ui| {
                                        ui.label(egui::RichText::new(&name).strong());
                                        ui.label(
                                            egui::RichText::new(format!(
                                                "{kind} · {description}"
                                            ))
                                            .size(11.0)
                                            .color(theme::TEXT_SECONDARY),
                                        );
                                    });
                                });
                        });
                        row_rect = Some(drag_response.response.rect);
                        ui.add_space(8.0);
                        if ui.button("Connect").clicked() {
                            command = Some(match profile {
                                Profile::Local(_) => AppCommand::StartConfiguredLocalProfile {
                                    profile_id: name.clone(),
                                },
                                Profile::Ssh(ssh)
                                    if ssh.profile_kind() == RemoteProfileKind::Sftp =>
                                {
                                    AppCommand::StartConfiguredSftpProfile {
                                        profile_id: name.clone(),
                                    }
                                }
                                Profile::Ssh(_) => AppCommand::StartConfiguredSshProfile {
                                    profile_id: name.clone(),
                                },
                                Profile::Serial(_) => AppCommand::StartConfiguredSerialProfile {
                                    profile_id: name.clone(),
                                },
                            });
                        }
                        if matches!(
                            profile,
                            Profile::Ssh(ssh)
                                if ssh.profile_kind() == RemoteProfileKind::Ssh
                        )
                            && ui.button("Open SFTP").clicked()
                        {
                            command = Some(AppCommand::OpenConfiguredSftpFileManagerProfile {
                                profile_id: name.clone(),
                            });
                        }
                        if ui.button("Edit").clicked() {
                            next_mode = Some(match profile {
                                Profile::Local(local) => ProfilesScreenMode::EditLocal(
                                    LocalProfileDraft::from_profile(local),
                                ),
                                Profile::Ssh(ssh) => ProfilesScreenMode::EditSsh(
                                    SshProfileDraft::from_profile(ssh),
                                ),
                                Profile::Serial(serial) => ProfilesScreenMode::EditSerial(
                                    SerialProfileDraft::from_profile(serial),
                                ),
                            });
                        }
                        if ui.button("Duplicate").clicked() {
                            let duplicate_name = format!("{name}-copy");
                            next_mode = Some(match profile {
                                Profile::Local(local) => {
                                    let mut draft = LocalProfileDraft::from_profile(local);
                                    draft.original_id = None;
                                    draft.name = duplicate_name;
                                    ProfilesScreenMode::EditLocal(draft)
                                }
                                Profile::Ssh(ssh) => {
                                    let mut draft = SshProfileDraft::from_profile(ssh);
                                    draft.original_id = None;
                                    draft.name = duplicate_name;
                                    ProfilesScreenMode::EditSsh(draft)
                                }
                                Profile::Serial(serial) => {
                                    let mut draft = SerialProfileDraft::from_profile(serial);
                                    draft.original_id = None;
                                    draft.name = duplicate_name;
                                    ProfilesScreenMode::EditSerial(draft)
                                }
                            });
                        }
                        if ui.button("Delete").clicked() {
                            next_mode = Some(ProfilesScreenMode::ConfirmDelete {
                                identifier: name.clone(),
                                references: configuration.workspace_tab_references(&name),
                            });
                        }
                    });
                    // Only commit the reorder when the drag is released over
                    // this row (not continuously while hovering): each
                    // reorder is persisted to disk immediately
                    // (`ConfigurationReloader::reorder_profiles`), so
                    // dispatching on every hovered frame during a drag would
                    // write the configuration file dozens of times per
                    // second.
                    if let Some(rect) = row_rect {
                        if let Some(dragged) = egui::DragAndDrop::payload::<String>(ui.ctx()) {
                            let released = ui.input(|i| i.pointer.any_released());
                            if let Some(pointer_pos) = ui.ctx().pointer_interact_pos() {
                                if *dragged != name && released && rect.contains(pointer_pos) {
                                    command = Some(AppCommand::ReorderProfiles {
                                        moved: (*dragged).clone(),
                                        before: Some(name.clone()),
                                    });
                                }
                            }
                        }
                    }
                }
                // A trailing drop target lets a drag be released past the
                // last profile row to move it to the end of the list.
                if !configuration.profiles().is_empty() {
                    let (end_rect, _) =
                        ui.allocate_exact_size(vec2(220.0, 12.0), Sense::hover());
                    if let Some(dragged) = egui::DragAndDrop::payload::<String>(ui.ctx()) {
                        let released = ui.input(|i| i.pointer.any_released());
                        if let Some(pointer_pos) = ui.ctx().pointer_interact_pos() {
                            if released
                                && configuration
                                    .profiles()
                                    .last()
                                    .is_some_and(|last| last.identifier() != dragged.as_str())
                                && end_rect.contains(pointer_pos)
                            {
                                command = Some(AppCommand::ReorderProfiles {
                                    moved: (*dragged).clone(),
                                    before: None,
                                });
                            }
                        }
                    }
                }
            });
        }
        ProfilesScreenMode::EditLocal(draft) => {
            show_bounded_content_scroll(ui, (tab_id, "edit_local_profile_scroll"), |ui| {
                ui.vertical(|ui| {
                    ui.add_space(24.0);
                    ui.heading(if draft.original_id.is_some() {
                        "Edit Local Profile"
                    } else {
                        "New Local Profile"
                    });
                    ui.add_space(16.0);
                    egui::Frame::new()
                        .fill(theme::SURFACE_TAB_INACTIVE)
                        .stroke(Stroke::new(1.0, theme::BORDER_SUBTLE))
                        .corner_radius(8.0)
                        .inner_margin(egui::Margin::same(16))
                        .show(ui, |ui| {
                        ui.set_width(340.0);
                        ssh_section_heading(ui, "Profile");
                        if profile_text_edit(ui, tab_id, "name", "Name", &mut draft.name).changed()
                        {
                            draft
                                .durable_session
                                .sync_session_name_from_profile_name(&draft.name);
                        }
                        local_executable_field(
                            ui,
                            profiles_state_id(tab_id).with("executable_autocomplete"),
                            &mut draft.executable,
                        );
                        profile_text_edit(
                            ui,
                            tab_id,
                            "arguments",
                            "Arguments (space-separated)",
                            &mut draft.arguments,
                        );
                        profile_text_edit(
                            ui,
                            tab_id,
                            "working_directory",
                            "Working directory (optional)",
                            &mut draft.working_directory,
                        );
                        ui.add_space(10.0);
                        ssh_section_heading(ui, "Durable session");
                        show_durable_session_controls(
                            ui,
                            tab_id,
                            &mut draft.durable_session,
                            DurableSessionTarget::Local,
                            false,
                        );
                        ssh_paragraph(
                            ui,
                            "Available only on saved Local profiles. The built-in Local Shell always starts a fresh plain shell.",
                        );
                        if let Some(error) = &draft.error {
                            ui.add_space(6.0);
                            ui.colored_label(theme::STATUS_ERROR, error);
                        }
                        ui.add_space(12.0);
                        ui.horizontal(|ui| {
                            if ui.button("Save").clicked() {
                                match draft.build() {
                                    Ok(profile) => {
                                        command = Some(AppCommand::SaveProfile { profile });
                                        next_mode = Some(ProfilesScreenMode::List);
                                    }
                                    Err(_) => {
                                        draft.error = Some(
                                            "Enter a name and a non-empty executable.".to_owned(),
                                        );
                                    }
                                }
                            }
                            if ui.button("Cancel").clicked() {
                                next_mode = Some(ProfilesScreenMode::List);
                            }
                        });
                        });
                });
            });
        }
        ProfilesScreenMode::EditSsh(draft) => {
            ui.vertical(|ui| {
                ui.add_space(24.0);
                ui.heading(match (draft.profile_kind, draft.original_id.is_some()) {
                    (RemoteProfileKind::Ssh, true) => "Edit SSH Profile",
                    (RemoteProfileKind::Ssh, false) => "New SSH Profile",
                    (RemoteProfileKind::Sftp, true) => "Edit SFTP Profile",
                    (RemoteProfileKind::Sftp, false) => "New SFTP Profile",
                });
                ui.add_space(16.0);
                egui::Frame::new()
                    .fill(theme::SURFACE_TAB_INACTIVE)
                    .stroke(Stroke::new(1.0, theme::BORDER_SUBTLE))
                    .corner_radius(8.0)
                    .inner_margin(egui::Margin::same(16))
                    .show(ui, |ui| {
                        ui.set_width(340.0);
                        // Private-key authentication adds a tall multiline
                        // secret field that can otherwise push Save/Cancel
                        // below the window's bottom edge. Rather than
                        // wrapping the whole bordered panel in a scroll area
                        // (which either grows it to fill whatever height the
                        // surrounding layout happens to report as
                        // "available" — collapsing it to a sliver in
                        // practice — or clips the panel's own border), only
                        // the fields scroll, *inside* the panel; the panel
                        // itself keeps its natural (small-form) height and
                        // only grows a scrollbar once content would run past
                        // the actual window height. `available_height()` is
                        // unreliable here: this Frame auto-sizes to its
                        // content, so on the pass that decides how tall to
                        // make itself its own max_rect is degenerate (zero
                        // height) -- a chicken-and-egg problem for any
                        // auto-sizing container in immediate mode. Instead,
                        // measure the *absolute* remaining space in the real
                        // viewport: the current cursor's vertical position
                        // (screen coordinates, valid even when max_rect
                        // isn't) down to the bottom of the window, minus
                        // room for the Save/Cancel row below -- and, if the
                        // bottom status bar is showing, its exact reserved
                        // area (queried directly from its own persisted
                        // panel state rather than guessed) so the panel
                        // never overlaps it.
                        let panel_top = ui.cursor().top();
                        let mut viewport_bottom = ui.ctx().content_rect().bottom();
                        if let Some(status_bar) = egui::containers::panel::PanelState::load(
                            ui.ctx(),
                            egui::Id::new("status_bar"),
                        ) {
                            viewport_bottom = viewport_bottom.min(status_bar.outer_rect.top());
                        }
                        let scroll_max_height = (viewport_bottom - panel_top - 56.0).max(120.0);
                        // `ScrollArea` computes its own available space via
                        // `ui.available_rect_before_wrap()`, which is
                        // degenerate (zero height) here because the
                        // enclosing Frame hasn't settled on its own size
                        // yet -- an auto-sizing container doesn't know its
                        // height until after its content is laid out. Give
                        // the scroll area its own child `Ui` with a real
                        // (non-degenerate) max_rect reflecting the budget we
                        // just computed, so its internal sizing sees actual
                        // numbers instead of zero. Unlike `set_min_height`,
                        // this doesn't force the surrounding Frame to grow:
                        // the child `Ui`'s *allocated* size still comes from
                        // what was actually drawn, so the panel keeps
                        // shrinking to fit short content and only grows a
                        // scrollbar when content would truly overflow.
                        let scroll_rect = egui::Rect::from_min_size(
                            ui.cursor().min,
                            egui::vec2(ui.available_width(), scroll_max_height),
                        );
                        ui.scope_builder(egui::UiBuilder::new().max_rect(scroll_rect), |ui| {
                            configure_content_scrollbar(ui);
                            ScrollArea::vertical()
                            .id_salt((tab_id, "edit_ssh_profile_scroll"))
                            .max_height(scroll_max_height)
                            .show(ui, |ui| {
                                ui.set_max_width(
                                    (ui.available_width() - CONTENT_SCROLLBAR_LANE).max(0.0),
                                );
                                ssh_section_heading(ui, "Connection");
                                if profile_text_edit(ui, tab_id, "name", "Name", &mut draft.name)
                                    .changed()
                                {
                                    draft
                                        .durable_session
                                        .sync_session_name_from_profile_name(&draft.name);
                                }
                                profile_text_edit(
                                    ui,
                                    tab_id,
                                    "username",
                                    "Username",
                                    &mut draft.username,
                                );
                                profile_text_edit(ui, tab_id, "host", "Host", &mut draft.host);
                                profile_text_edit(ui, tab_id, "port", "Port", &mut draft.port);
                                if draft.profile_kind == RemoteProfileKind::Sftp {
                                    ui.add_space(10.0);
                                    ui.checkbox(
                                        &mut draft.sftp_gui_mode,
                                        "Use graphical file manager",
                                    );
                                    ssh_paragraph(
                                        ui,
                                        "On by default. Turn this off to launch the terminal SFTP command surface.",
                                    );
                                } else {
                                    ui.add_space(10.0);
                                    ssh_section_heading(ui, "Durable session");
                                    draft.sync_remote_durable_provider_default(
                                        ui.ctx(),
                                        configuration,
                                    );
                                    show_durable_session_controls(
                                        ui,
                                        tab_id,
                                        &mut draft.durable_session,
                                        DurableSessionTarget::Remote,
                                        false,
                                    );
                                    ui.add_space(10.0);
                                    ssh_section_heading(ui, "Port forwards");
                                    show_port_forward_drafts(
                                        ui,
                                        tab_id,
                                        "ssh_profile_port_forward",
                                        &mut draft.port_forwards,
                                    );
                                }
                                ui.add_space(10.0);
                                ssh_section_heading(ui, "Authentication");
                                ui.horizontal(|ui| {
                                    ui.radio_value(
                                        &mut draft.auth_method,
                                        SshAuthenticationMethod::Password,
                                        "Password authentication",
                                    );
                                    ui.radio_value(
                                        &mut draft.auth_method,
                                        SshAuthenticationMethod::PrivateKey,
                                        "Private-key authentication",
                                    );
                                });
                                ui.add_space(4.0);
                                match draft.auth_method {
                                    SshAuthenticationMethod::Password => {
                                        ssh_paragraph(
                                            ui,
                                            if draft.has_stored_credential
                                                && draft.stored_credential_kind
                                                    == CredentialKind::Password
                                            {
                                                "A password is stored in native secure storage for this profile. Enter a new one below to replace it."
                                            } else if draft.original_id.is_some() {
                                                "Enter a password to remember it in native secure storage, or leave this blank to be prompted at connect time."
                                            } else {
                                                "Enter a password to save it in native secure storage with this profile, or leave this blank to be prompted at connect time."
                                            },
                                        );
                                        ui.add_space(4.0);
                                        profile_password_edit(
                                            ui,
                                            tab_id,
                                            "password",
                                            "Password",
                                            &mut draft.password,
                                        );
                                        if let Some(profile_id) = draft.original_id.clone() {
                                            ui.add_space(4.0);
                                            if ui
                                                .add_enabled(
                                                    !draft.password.is_empty(),
                                                    egui::Button::new("Save password"),
                                                )
                                                .clicked()
                                            {
                                                command =
                                                    Some(AppCommand::StoreProfilePassword {
                                                        profile_id,
                                                        password: PasswordToStore::new(
                                                            std::mem::take(&mut draft.password),
                                                        ),
                                                    });
                                                draft.has_stored_credential = true;
                                                draft.stored_credential_kind =
                                                    CredentialKind::Password;
                                            }
                                        }
                                    }
                                    SshAuthenticationMethod::PrivateKey => {
                                        ssh_paragraph(
                                            ui,
                                            if draft.has_stored_credential
                                                && draft.stored_credential_kind
                                                    == CredentialKind::PrivateKey
                                            {
                                                "A private key is stored in native secure storage for this profile. Enter a new one below to replace it."
                                            } else if draft.original_id.is_some() {
                                                "Enter an OpenSSH private key to remember it in native secure storage."
                                            } else {
                                                "Enter an OpenSSH private key to save it in native secure storage with this profile."
                                            },
                                        );
                                        ui.add_space(4.0);
                                        ssh_multiline_secret_text_edit(
                                            ui,
                                            tab_id,
                                            "private_key",
                                            "OpenSSH private key",
                                            &mut draft.private_key,
                                        );
                                        profile_password_edit(
                                            ui,
                                            tab_id,
                                            "key_passphrase",
                                            "Key passphrase (optional)",
                                            &mut draft.key_passphrase,
                                        );
                                        if let Some(profile_id) = draft.original_id.clone() {
                                            ui.add_space(4.0);
                                            if ui
                                                .add_enabled(
                                                    !draft.private_key.trim().is_empty(),
                                                    egui::Button::new("Save private key"),
                                                )
                                                .clicked()
                                            {
                                                let passphrase =
                                                    if draft.key_passphrase.is_empty() {
                                                        None
                                                    } else {
                                                        Some(std::mem::take(
                                                            &mut draft.key_passphrase,
                                                        ))
                                                    };
                                                command =
                                                    Some(AppCommand::StoreProfilePrivateKey {
                                                        profile_id,
                                                        private_key: PrivateKeyToStore::new(
                                                            std::mem::take(
                                                                &mut draft.private_key,
                                                            ),
                                                            passphrase,
                                                        ),
                                                    });
                                                draft.has_stored_credential = true;
                                                draft.stored_credential_kind =
                                                    CredentialKind::PrivateKey;
                                            }
                                        }
                                    }
                                    SshAuthenticationMethod::Certificate => {
                                        ssh_paragraph(
                                            ui,
                                            "Certificate authentication is available only for one-off SSH and terminal SFTP quick-connect launches. Saved profiles still support storing passwords or private keys only.",
                                        );
                                    }
                                }
                            });
                        });
                        // Kept outside the scroll area (but still inside the
                        // bordered panel) so Save/Cancel — and any error —
                        // stay pinned and reachable without scrolling, even
                        // when the fields above are tall enough to scroll.
                        if let Some(error) = &draft.error {
                            ui.add_space(6.0);
                            ui.colored_label(theme::STATUS_ERROR, error);
                        }
                        ui.add_space(12.0);
                        ui.horizontal(|ui| {
                            if ui.button("Save").clicked() {
                                let trimmed_name = draft.name.trim();
                                if ssh_profile_name_collides(
                                    configuration,
                                    draft.original_id.as_deref(),
                                    trimmed_name,
                                ) {
                                    draft.error = Some(format!(
                                        "A profile named '{}' already exists.",
                                        trimmed_name
                                    ));
                                    return;
                                }
                                let existing_profile = draft
                                    .original_id
                                    .as_deref()
                                    .and_then(|identifier| configuration.profile(identifier))
                                    .and_then(Profile::as_ssh);
                                match draft.build(existing_profile) {
                                    Ok(profile) => {
                                        command =
                                            Some(match draft.take_initial_credential() {
                                                Some(credential) => {
                                                    AppCommand::SaveProfileWithCredential {
                                                        profile,
                                                        credential,
                                                    }
                                                }
                                                None => AppCommand::SaveProfile { profile },
                                            });
                                        next_mode = Some(ProfilesScreenMode::List);
                                    }
                                    Err(error) => draft.error = Some(error),
                                }
                            }
                            if ui.button("Cancel").clicked() {
                                next_mode = Some(ProfilesScreenMode::List);
                            }
                        });
                    });
            });
        }
        ProfilesScreenMode::EditSerial(draft) => {
            show_bounded_content_scroll(ui, (tab_id, "serial_profile_editor"), |ui| {
            ui.vertical(|ui| {
                ui.add_space(24.0);
                let heading = if draft.original_id.is_some() {
                    "Edit Serial Profile"
                } else {
                    "New Serial Profile"
                };
                ui.heading(heading);
                ui.add_space(12.0);
                profile_text_edit(ui, tab_id, "serial_name", "Name", &mut draft.name);
                profile_text_edit(ui, tab_id, "serial_device", "Device", &mut draft.device);
                profile_text_edit(
                    ui,
                    tab_id,
                    "serial_baud_rate",
                    "Baud rate",
                    &mut draft.baud_rate,
                );
                ui.add_space(8.0);
                serial_enum_combo(ui, "Data bits", &mut draft.data_bits);
                serial_enum_combo(ui, "Parity", &mut draft.parity);
                serial_enum_combo(ui, "Stop bits", &mut draft.stop_bits);
                serial_enum_combo(ui, "Flow control", &mut draft.flow_control);
                if let Some(error) = &draft.error {
                    ui.add_space(8.0);
                    ui.colored_label(theme::STATUS_ERROR, error.as_str());
                }
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui.button("Save").clicked() {
                        match draft.build_profile() {
                            Ok(profile) => {
                                command = Some(AppCommand::SaveProfile { profile });
                                next_mode = Some(ProfilesScreenMode::List);
                            }
                            Err(error) => draft.error = Some(error),
                        }
                    }
                    if ui.button("Cancel").clicked() {
                        next_mode = Some(ProfilesScreenMode::List);
                    }
                });
            });
            });
        }
        ProfilesScreenMode::ConfirmDelete {
            identifier,
            references,
        } => {
            ui.vertical(|ui| {
                ui.add_space(24.0);
                ui.heading("Delete profile?");
                ui.label(format!("This will permanently delete \"{identifier}\"."));
                if *references > 0 {
                    ui.colored_label(
                        theme::STATUS_ERROR,
                        format!(
                            "{references} saved workspace tab{} currently launch{} from this profile and will block deletion until removed.",
                            if *references == 1 { "" } else { "s" },
                            if *references == 1 { "s" } else { "" },
                        ),
                    );
                }
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui.button("Delete").clicked() {
                        command = Some(AppCommand::DeleteProfile {
                            identifier: identifier.clone(),
                        });
                        next_mode = Some(ProfilesScreenMode::List);
                    }
                });
            });
        }
    }
        });
    });
    if let Some(mode) = next_mode {
        state.mode = mode;
    }

    ui.data_mut(|data| data.insert_temp(state_id, state));
    command
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tabs::AppState;
    use egui_kittest::{
        kittest::{NodeT, Queryable},
        Harness,
    };

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

    struct SettingsHarnessState {
        command: Option<AppCommand>,
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

    fn settings_harness() -> Harness<'static, SettingsHarnessState> {
        settings_harness_with_width(520.0)
    }

    /// A wider settings harness, matching a typical desktop window rather
    /// than the other settings tests' narrow fixed harness width. Needed to
    /// reproduce the "Scroll speed" slider mispositioning regression (see
    /// `scroll_speed_slider_is_reachable_and_dispatches_the_next_clickstop`):
    /// the bug only appears once the card is wider than the description
    /// text plus the slider's own width, which the narrow harness never is.
    fn wide_settings_harness() -> Harness<'static, SettingsHarnessState> {
        settings_harness_with_width(1400.0)
    }

    fn settings_harness_with_width(width: f32) -> Harness<'static, SettingsHarnessState> {
        Harness::builder()
            .with_size(egui::vec2(width, 2000.0))
            .build_ui_state(
                |ui, state: &mut SettingsHarnessState| {
                    if let Some(command) = show_settings(
                        ui,
                        SettingsViewModel {
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
                            quick_switch_overlay: false,
                            compact_launcher_grid: false,
                            pulse_new_output_dot: false,
                            show_resumable_sessions: false,
                            default_sftp_local_directory: None,
                            sftp_pane_order: SftpPaneOrderPreference::LocalLeft,
                        },
                        "Cmd+Shift+P",
                        "Cmd+Shift+S",
                    ) {
                        state.command = Some(command);
                    }
                },
                SettingsHarnessState { command: None },
            )
    }

    #[test]
    fn settings_has_no_manual_reload_or_save_controls() {
        // Regression test: Settings used to offer explicit "Reload
        // configuration"/"Save workspace" buttons; configuration now
        // save/restores automatically, so neither control (nor their
        // explanatory copy) should be present any more.
        let mut harness = settings_harness();
        harness.run();

        assert!(harness.query_by_label("Reload configuration").is_none());
        assert!(harness.query_by_label("Save workspace").is_none());
        assert!(harness
            .query_by_label("Configuration is never written automatically.")
            .is_none());
        assert!(harness
            .query_by_label("Chip layout and status bar visibility are saved automatically.")
            .is_none());
    }

    #[test]
    fn settings_has_no_configuration_card() {
        // Regression test: the "Configuration" card (startup/save status
        // copy plus native-secure-storage status) was removed from
        // Settings; that status is not shown here any more (secure storage
        // status already surfaces on the Launcher instead).
        let mut harness = settings_harness();
        harness.run();

        assert!(harness.query_by_label("Configuration").is_none());
        assert!(harness.query_by_label("Native secure storage").is_none());
    }

    #[test]
    fn settings_keyboard_card_shows_the_settings_hotkey() {
        let mut harness = settings_harness();
        harness.run();

        assert!(harness.query_by_label("Open Settings").is_some());
        assert!(harness.query_by_label("Cmd+Shift+S").is_some());
    }

    #[test]
    fn settings_toggle_chip_layout_control_returns_the_toggle_command() {
        let mut harness = settings_harness();
        harness.run();

        // The harness starts in `ChipLayout::Wrap`; clicking the *other*,
        // currently-inactive segmented option ("Single row") is what
        // selects a new value. Clicking the already-active "Wrap" option
        // is a no-op, matching ordinary segmented-control behavior.
        harness.get_by_label("Single row").click();
        harness.run();

        assert!(matches!(
            harness.state().command,
            Some(AppCommand::ToggleChipLayout)
        ));
    }

    #[test]
    fn settings_toggle_restore_workspace_control_returns_the_toggle_command() {
        // Regression test for the "Workspace restore" preference: off by
        // default, with its own explicit toggle distinct from the
        // always-autosaving chip-layout/status-bar/session-detail toggles.
        let mut harness = settings_harness();
        harness.run();

        assert!(harness
            .query_by_role_and_label(accesskit::Role::CheckBox, "Workspace restore")
            .is_some());

        harness
            .get_by_role_and_label(accesskit::Role::CheckBox, "Workspace restore")
            .click();
        harness.run();

        assert!(matches!(
            harness.state().command,
            Some(AppCommand::ToggleRestoreWorkspace)
        ));
    }

    #[test]
    fn settings_toggle_compact_launcher_grid_control_returns_the_toggle_command() {
        // Regression test for the "Compact New Session layout" preference
        // (feature request #64): off by default, with its own explicit
        // toggle in the Interface card.
        let mut harness = settings_harness();
        harness.run();

        assert!(harness
            .query_by_role_and_label(accesskit::Role::CheckBox, "Compact New Session layout")
            .is_some());

        harness
            .get_by_role_and_label(accesskit::Role::CheckBox, "Compact New Session layout")
            .click();
        harness.run();

        assert!(matches!(
            harness.state().command,
            Some(AppCommand::ToggleCompactLauncherGrid)
        ));
    }

    #[test]
    fn settings_auto_applies_default_sftp_local_directory_edits() {
        let mut harness = settings_harness();
        let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        harness.run();

        harness.get_by_label("Default local SFTP directory").focus();
        harness
            .get_by_label("Default local SFTP directory")
            .type_text(directory.to_string_lossy().as_ref());
        harness.run();

        assert!(matches!(
            harness.state().command.as_ref(),
            Some(AppCommand::SetDefaultSftpLocalDirectory(Some(path))) if path == &directory
        ));
    }

    #[test]
    fn settings_sftp_pane_order_control_dispatches_the_selected_preference() {
        let mut harness = settings_harness();
        harness.run();

        harness
            .get_by_role_and_label(accesskit::Role::RadioButton, "Remote left · Local right")
            .click();
        harness.run();

        assert!(matches!(
            harness.state().command,
            Some(AppCommand::SetSftpPaneOrder(
                SftpPaneOrderPreference::RemoteLeft
            ))
        ));
    }

    #[test]
    fn settings_accept_missing_default_sftp_local_directory_metadata() {
        let mut harness = settings_harness();
        let missing = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("does-not-exist-default-sftp-local-directory");
        harness.run();

        harness.get_by_label("Default local SFTP directory").focus();
        harness
            .get_by_label("Default local SFTP directory")
            .type_text(missing.to_string_lossy().as_ref());
        harness.run();

        assert!(matches!(
            harness.state().command.as_ref(),
            Some(AppCommand::SetDefaultSftpLocalDirectory(Some(path))) if path == &missing
        ));
        assert!(
            harness
                .query_by_label("Default local SFTP directory must not contain control characters.")
                .is_none(),
            "ordinary path metadata must not show inline validation errors"
        );
    }

    #[test]
    fn settings_toggle_pulse_new_output_dot_control_returns_the_toggle_command() {
        // Regression test for the "Pulse status dot on new background
        // output" preference (feature request #68): off by default, with
        // its own explicit toggle in the Interface card.
        let mut harness = settings_harness();
        harness.run();

        assert!(harness
            .query_by_role_and_label(
                accesskit::Role::CheckBox,
                "Pulse status dot on new background output"
            )
            .is_some());

        harness
            .get_by_role_and_label(
                accesskit::Role::CheckBox,
                "Pulse status dot on new background output",
            )
            .click();
        harness.run();

        assert!(matches!(
            harness.state().command,
            Some(AppCommand::TogglePulseNewOutputDot)
        ));
    }

    /// The default window is `DEFAULT_WINDOW_WIDTH` wide (see `main.rs`), and
    /// every surface has to be usable there. `settings_segmented_row` used to
    /// reserve a fixed 170px for its buttons; "Scrollback limit"'s four
    /// options need roughly 240, and the overflow does not clip the buttons -
    /// egui grows the enclosing card instead. The Scrolling card was offered
    /// 684px and painted 754.5, and because each following card then inherited
    /// that wider content width, the Terminal font dropdown and the
    /// scroll-speed slider were pushed off the right edge of the window.
    #[test]
    fn settings_controls_stay_inside_their_card_at_the_default_window_width() {
        let mut harness = settings_harness_with_width(crate::DEFAULT_WINDOW_WIDTH);
        harness.run();

        // A toggle row's switch is pinned to the card's right edge and its
        // right side is narrow enough that it never forced the card wider,
        // so it marks where every other control should stop.
        let card_right = harness
            .get_by_role_and_label(accesskit::Role::CheckBox, "Workspace restore")
            .rect()
            .right();

        for (what, right) in [
            (
                "the scrollback-limit segmented control",
                harness.get_by_label("Disabled").rect().right(),
            ),
            (
                "the scroll-speed slider",
                harness.get_by_role(accesskit::Role::Slider).rect().right(),
            ),
            (
                "the terminal-font dropdown",
                harness
                    .get_by_role(accesskit::Role::ComboBox)
                    .rect()
                    .right(),
            ),
        ] {
            assert!(
                right <= card_right + 1.0,
                "{what} reaches {right}, past the {card_right} right edge the \
                 toggle rows line up on, so its card is wider than the window"
            );
        }
    }

    /// Regression test for the scroll-speed slider rendering as a small empty
    /// box with no visible track. egui paints a slider rail with
    /// `widgets.inactive.bg_fill`, and `theme::default_visuals` sets that to
    /// `SURFACE_TAB_INACTIVE` - byte-for-byte the fill `settings_card` uses -
    /// so the rail disappeared into the card and left only the handle's
    /// one-pixel outline on screen.
    #[test]
    fn a_settings_slider_rail_is_visible_against_the_card_it_sits_in() {
        let mut observed = None;
        egui::__run_test_ui(|ui| {
            ui.style_mut().visuals = theme::default_visuals();
            style_settings_slider(ui);
            observed = Some((
                ui.visuals().widgets.inactive.bg_fill,
                ui.visuals().selection.bg_fill,
            ));
        });

        let (rail, travelled) = observed.expect("the test ui body should have run");
        assert_ne!(
            rail,
            theme::SURFACE_TAB_INACTIVE,
            "the slider rail is painted in the same color as the settings card \
             around it, so the slider renders as a bare floating handle"
        );
        assert_eq!(
            travelled,
            theme::ACCENT_PRIMARY,
            "the travelled part of the rail should use the same accent the \
             toggle switches use for 'on'"
        );
    }

    #[test]
    fn scroll_speed_slider_is_reachable_and_dispatches_the_next_clickstop() {
        // Regression test for `settings_clickstop_row` rendering the
        // "Scroll speed" slider unusably: unlike `ui.horizontal` (used by
        // `settings_segmented_row`), plain `ui.vertical` does not mirror
        // `egui::Sides`' right-to-left direction (see `egui::Ui::horizontal`,
        // which checks `placer.prefer_right_to_left()`, versus `ui.vertical`,
        // which always lays out `Layout::top_down(Align::Min)`). A bare
        // `ui.vertical(...)` on the right side inherited the *entire*
        // remaining card width and then left-aligned the slider inside it,
        // so on any card wider than description-text-plus-slider the block
        // rendered immediately after the description paragraph instead of
        // pinned to the card's right edge like every other settings row -
        // squeezing the slider down to a tiny hit target and spilling the
        // value label over the description (reported: "you can't tell it's
        // actually a slider" and "sliding the value doesn't seem to change
        // scroll speed"). A width at least as wide as `docs/gui-mockups`'
        // settings card is required to reproduce this: the bug was invisible
        // at the narrow fixed-size harness width used by the other settings
        // tests here.
        let mut harness = wide_settings_harness();
        harness.run();

        let slider = harness.get_by_role(accesskit::Role::Slider);
        let card_right_edge = harness
            .get_by_role_and_label(accesskit::Role::CheckBox, "Workspace restore")
            .rect()
            .right();
        assert!(
            slider.rect().width() >= 100.0,
            "expected the clickstop slider to render at its configured width, got {:?}",
            slider.rect()
        );
        assert!(
            (slider.rect().right() - card_right_edge).abs() <= 40.0,
            "expected the slider to be pinned to the card's right edge like every \
             other settings control, but it rendered at {:?} while the card's \
             right edge is at {card_right_edge}",
            slider.rect()
        );

        slider.focus();
        harness.run();
        harness.key_press(egui::Key::ArrowRight);
        harness.run();

        assert!(matches!(
            harness.state().command,
            Some(AppCommand::SetScrollSpeed(ScrollSpeedPreference::Fast))
        ));
    }

    #[test]
    fn settings_toggle_show_resumable_sessions_control_returns_the_toggle_command() {
        // Regression test for the "Resume unattached local sessions from
        // New Session" preference (feature request #70): off by default,
        // with its own explicit toggle in the Interface card.
        let mut harness = settings_harness();
        harness.run();

        assert!(harness
            .query_by_role_and_label(
                accesskit::Role::CheckBox,
                "Resume unattached local sessions from New Session"
            )
            .is_some());

        harness
            .get_by_role_and_label(
                accesskit::Role::CheckBox,
                "Resume unattached local sessions from New Session",
            )
            .click();
        harness.run();

        assert!(matches!(
            harness.state().command,
            Some(AppCommand::ToggleShowResumableSessions)
        ));
    }

    #[test]
    fn settings_close_confirmation_control_returns_the_toggle_command() {
        let mut harness = settings_harness();
        harness.run();

        let label = "Confirm before closing live sessions";
        assert!(harness
            .query_by_role_and_label(accesskit::Role::CheckBox, label)
            .is_some());

        harness
            .get_by_role_and_label(accesskit::Role::CheckBox, label)
            .click();
        harness.run();

        assert!(matches!(
            harness.state().command,
            Some(AppCommand::ToggleConfirmSessionClose)
        ));
    }

    #[cfg(windows)]
    #[test]
    fn settings_powershell_preference_returns_the_toggle_command() {
        let mut harness = settings_harness();
        harness.run();

        let label = "Prefer PowerShell when available";
        assert!(harness
            .query_by_role_and_label(accesskit::Role::CheckBox, label)
            .is_some());
        harness
            .get_by_role_and_label(accesskit::Role::CheckBox, label)
            .click();
        harness.run();

        assert!(matches!(
            harness.state().command,
            Some(AppCommand::TogglePreferPowershell)
        ));
    }

    #[test]
    fn settings_exposes_terminal_font_ligature_and_emoji_controls() {
        let mut harness = settings_harness();
        harness.run();

        assert!(harness.query_by_label("Terminal font").is_some());
        let ligatures = "Programming ligatures";
        harness
            .get_by_role_and_label(accesskit::Role::CheckBox, ligatures)
            .click();
        harness.run();

        assert!(matches!(
            harness.state().command,
            Some(AppCommand::ToggleTerminalLigatures)
        ));
        assert!(harness.query_by_label("Emoji presentation").is_some());

        harness.get_by_label("Monochrome").click();
        harness.run();

        assert!(matches!(
            harness.state().command,
            Some(AppCommand::SetEmojiPresentation(
                EmojiPresentationPreference::Monochrome
            ))
        ));
    }

    #[test]
    fn settings_exposes_scrollback_limit_for_future_sessions() {
        let mut harness = settings_harness();
        harness.run();

        assert!(harness.query_by_label("Scrollback limit").is_some());
        harness.get_by_label("16 MiB").click();
        harness.run();

        assert!(matches!(
            harness.state().command,
            Some(AppCommand::SetScrollbackLimit(
                ScrollbackLimitPreference::MiB16
            ))
        ));
    }

    #[test]
    fn settings_panel_does_not_overlap_a_visible_bottom_status_bar() {
        // Regression test: the card-based Settings redesign is taller than
        // the old flat button list, so without a status-bar-aware height
        // clamp (mirroring the SSH profile editor panel's), its content
        // could paint into - or past - the bottom status bar strip. The
        // window here is tall enough for all of Settings' content to fit
        // without needing to scroll, so every widget's rect should stay
        // above the status bar; a shorter window would legitimately push
        // some content below the fold (inside the scrollable area) without
        // that being a bug, which a naive per-widget position check can't
        // distinguish from actually overlapping the status bar.
        //
        // The height has to track the content: this fixture is only 520
        // wide, and once `settings_segmented_row` began reserving its
        // buttons' real width the descriptions beside them wrap one line
        // further at that width, making the whole surface taller.
        let mut harness = Harness::builder()
            .with_size(egui::vec2(520.0, 1700.0))
            .build_ui_state(
                |ui, state: &mut SettingsHarnessState| {
                    egui::Panel::bottom("status_bar")
                        .resizable(false)
                        .show_separator_line(false)
                        .show(ui, |ui| {
                            ui.set_min_height(24.0);
                            ui.set_max_height(24.0);
                        });
                    if let Some(command) = show_settings(
                        ui,
                        SettingsViewModel {
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
                            quick_switch_overlay: false,
                            compact_launcher_grid: false,
                            pulse_new_output_dot: false,
                            show_resumable_sessions: false,
                            default_sftp_local_directory: None,
                            sftp_pane_order: SftpPaneOrderPreference::LocalLeft,
                        },
                        "Cmd+Shift+P",
                        "Cmd+Shift+S",
                    ) {
                        state.command = Some(command);
                    }
                },
                SettingsHarnessState { command: None },
            );
        harness.run();
        harness.run();

        let status_bar_top =
            egui::containers::panel::PanelState::load(&harness.ctx, egui::Id::new("status_bar"))
                .expect("status bar panel state should be recorded")
                .outer_rect
                .top();
        let command_palette_rect = harness.get_by_label("Command palette").rect();
        assert!(
            command_palette_rect.max.y <= status_bar_top,
            "Settings content must stay above the status bar rather than overlapping it"
        );
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
        // Quick Connect is the default surface; these tests exercise the
        // full advanced form, so reveal it the same way a user would.
        harness.get_by_label("Show advanced settings").click();
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
    fn opening_the_ssh_form_focuses_the_username_field() {
        let mut harness = harness();
        harness.run();
        open_ssh_form(&mut harness);

        assert!(
            harness.get_by_label("Username").is_focused(),
            "Username must have initial keyboard focus when the SSH form opens"
        );
    }

    #[test]
    fn ssh_form_orders_fields_username_then_host_then_port_prefilled_with_22() {
        // Regression test pinning the requested field order (Username,
        // Host, Port) and that Port is prefilled with the actual default
        // value (not left empty with "(default: 22)"-style wording).
        let mut harness = harness();
        harness.run();
        open_ssh_form(&mut harness);

        let username_top = harness.get_by_label("Username").rect().top();
        let host_top = harness.get_by_label("Host").rect().top();
        let port_top = harness.get_by_label("Port").rect().top();

        assert!(
            username_top < host_top,
            "Username must be positioned above Host"
        );
        assert!(host_top < port_top, "Host must be positioned above Port");

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
        enter_text(&mut harness, "Host", "example.invalid");
        enter_text(&mut harness, "Username", "test-user");
        enter_text(&mut harness, "Password", "transient-test-password");

        harness.get_by_label("Connect with password").click();
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
    fn ssh_launcher_defaults_to_quick_connect_not_the_advanced_form() {
        let mut harness = harness();
        harness.run();
        harness
            .get_by_label("SSH — Connect to a remote host over SSH")
            .click();
        harness.run();

        assert!(
            harness.query_by_label("user@host").is_some(),
            "a freshly opened SSH launcher must show the Quick Connect field"
        );
        assert!(
            harness.query_by_label("Username").is_none(),
            "the advanced form must stay hidden until 'Show advanced settings' is checked"
        );
    }

    #[test]
    fn quick_connect_focuses_its_field_when_the_launcher_opens() {
        let mut harness = harness();
        harness.run();
        harness
            .get_by_label("SSH — Connect to a remote host over SSH")
            .click();
        harness.run();

        assert!(
            harness.get_by_label("user@host").is_focused(),
            "Quick Connect's field must have initial keyboard focus"
        );
    }

    #[test]
    fn quick_connect_with_no_password_opens_the_in_terminal_password_prompt() {
        let mut harness = harness();
        harness.run();
        harness
            .get_by_label("SSH — Connect to a remote host over SSH")
            .click();
        harness.run();

        harness.get_by_label("user@host").type_text("fes@10.1.2.3");
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
            harness.query_by_label("user@host").is_some(),
            "a freshly opened SFTP launcher must show the Quick Connect field"
        );
        assert!(
            harness.query_by_label("Username").is_none(),
            "the advanced form must stay hidden until 'Show advanced settings' is checked"
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
            .get_by_label("user@host")
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
    fn quick_connect_parses_an_explicit_port() {
        let mut harness = harness();
        harness.run();
        harness
            .get_by_label("SSH — Connect to a remote host over SSH")
            .click();
        harness.run();

        harness
            .get_by_label("user@host")
            .type_text("fes@10.1.2.3:2222");
        harness.run();
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
    fn quick_connect_can_attach_to_a_named_tmux_session() {
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
    fn quick_connect_rejects_a_destination_with_no_at_sign() {
        let mut harness = harness();
        harness.run();
        harness
            .get_by_label("SSH — Connect to a remote host over SSH")
            .click();
        harness.run();

        harness.get_by_label("user@host").type_text("10.1.2.3");
        harness.run();
        harness.get_by_label("Connect").click();
        harness.run();

        assert!(
            harness.state().command.is_none(),
            "an invalid quick-connect destination must not dispatch a command"
        );
        assert!(harness
            .query_by_label("Enter a destination as user@host")
            .is_some());
    }

    #[test]
    fn checking_show_advanced_settings_reveals_the_full_form_and_focuses_username() {
        let mut harness = harness();
        harness.run();
        harness
            .get_by_label("SSH — Connect to a remote host over SSH")
            .click();
        harness.run();

        harness.get_by_label("Show advanced settings").click();
        harness.run();

        assert!(
            harness.query_by_label("Username").is_some(),
            "checking 'Show advanced settings' must reveal the full form"
        );
        assert!(
            harness.get_by_label("Username").is_focused(),
            "revealing the advanced form must move focus to Username"
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
    fn toggling_advanced_settings_clears_quick_connect_feedback() {
        let mut harness = harness();
        harness.run();
        harness
            .get_by_label("SSH — Connect to a remote host over SSH")
            .click();
        harness.run();

        harness.get_by_label("Connect").click();
        harness.run();
        assert!(harness
            .query_by_label("Enter a destination, e.g. user@host")
            .is_some());

        harness.get_by_label("Show advanced settings").click();
        harness.run();

        assert!(
            harness
                .query_by_label("Enter a destination, e.g. user@host")
                .is_none(),
            "stale Quick Connect feedback must not survive a toggle to the advanced form"
        );
    }

    #[test]
    fn toggling_quick_connect_clears_advanced_form_feedback() {
        let mut harness = harness();
        harness.run();
        open_ssh_form(&mut harness);
        enter_text(&mut harness, "Host", "invalid host");

        harness.get_by_label("Connect with password").click();
        harness.run();
        assert!(harness
            .query_by_label("SSH host must not contain whitespace")
            .is_some());

        harness.get_by_label("Show advanced settings").click();
        harness.run();

        assert!(
            harness
                .query_by_label("SSH host must not contain whitespace")
                .is_none(),
            "stale advanced-form feedback must not survive a toggle back to Quick Connect"
        );
    }

    #[test]
    fn advanced_form_with_an_empty_password_starts_an_interactive_session_instead_of_connecting() {
        let mut harness = harness();
        harness.run();
        open_ssh_form(&mut harness);
        enter_text(&mut harness, "Host", "example.invalid");
        enter_text(&mut harness, "Username", "test-user");

        harness.get_by_label("Connect with password").click();
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
                    let rect = harness.get_by_label(label).rect();
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
        harness.get_by_label("Connect with password").click();
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

        harness.get_by_label("Private-key authentication").click();
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

        harness.get_by_label("Certificate authentication").click();
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
        enter_text(&mut harness, "Host", "invalid host");

        harness.get_by_label("Connect with password").click();
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

    struct ProfilesHarnessState {
        tab_id: TabId,
        configuration: festerm_config::Configuration,
        command: Option<AppCommand>,
    }

    fn profiles_harness(
        configuration: festerm_config::Configuration,
    ) -> Harness<'static, ProfilesHarnessState> {
        Harness::builder()
            .with_size(egui::vec2(560.0, 640.0))
            .build_ui_state(
                |ui, state: &mut ProfilesHarnessState| {
                    if let Some(command) = show_profiles(
                        ui,
                        state.tab_id,
                        &state.configuration,
                        None,
                        None,
                        PersistenceProviderKind::FestermSessiond,
                    ) {
                        state.command = Some(command);
                    }
                },
                ProfilesHarnessState {
                    tab_id: AppState::for_test().active(),
                    configuration,
                    command: None,
                },
            )
    }

    #[test]
    fn dragging_a_profile_card_onto_another_reorders_it() {
        let profiles = vec![
            Profile::local("alpha", "sh", Vec::new(), None).unwrap(),
            Profile::local("beta", "sh", Vec::new(), None).unwrap(),
            Profile::local("gamma", "sh", Vec::new(), None).unwrap(),
        ];
        let configuration = festerm_config::Configuration::new(profiles).unwrap();
        let mut harness = profiles_harness(configuration);
        harness.run();

        let from = harness.get_by_label("alpha").rect().center();
        let to = harness.get_by_label("gamma").rect().center();

        harness.drag_at(from);
        harness.run();
        let steps = 8;
        for step in 1..=steps {
            let t = step as f32 / steps as f32;
            harness.hover_at(from + (to - from) * t);
            harness.run();
        }
        harness.drop_at(to);
        harness.run();

        assert!(
            matches!(
                harness.state().command,
                Some(AppCommand::ReorderProfiles {
                    ref moved,
                    ref before,
                }) if moved == "alpha" && before.as_deref() == Some("gamma")
            ),
            "observed command: {:?}",
            harness.state().command
        );
    }

    #[test]
    fn dragging_a_profile_card_past_the_last_row_moves_it_to_the_end() {
        let profiles = vec![
            Profile::local("alpha", "sh", Vec::new(), None).unwrap(),
            Profile::local("beta", "sh", Vec::new(), None).unwrap(),
        ];
        let configuration = festerm_config::Configuration::new(profiles).unwrap();
        let mut harness = profiles_harness(configuration);
        harness.run();

        let from = harness.get_by_label("alpha").rect().center();
        let beta_rect = harness.get_by_label("beta").rect();
        let to = beta_rect.center() + egui::vec2(0.0, beta_rect.height() * 3.0);

        harness.drag_at(from);
        harness.run();
        let steps = 8;
        for step in 1..=steps {
            let t = step as f32 / steps as f32;
            harness.hover_at(from + (to - from) * t);
            harness.run();
        }
        harness.drop_at(to);
        harness.run();

        assert!(
            matches!(
                harness.state().command,
                Some(AppCommand::ReorderProfiles {
                    ref moved,
                    ref before,
                }) if moved == "alpha" && before.is_none()
            ),
            "observed command: {:?}",
            harness.state().command
        );
    }

    #[test]
    fn profiles_list_shows_no_profiles_saved_yet_when_empty() {
        let mut harness = profiles_harness(festerm_config::Configuration::new(Vec::new()).unwrap());
        harness.run();

        assert!(harness.query_by_label("No profiles saved yet.").is_some());
    }

    #[test]
    fn profiles_new_local_profile_flow_returns_a_save_profile_command() {
        let mut harness = profiles_harness(festerm_config::Configuration::new(Vec::new()).unwrap());
        harness.run();

        harness.get_by_label("New Local Profile").click();
        harness.run();

        harness.get_by_label("Name").focus();
        harness.get_by_label("Name").type_text("dev-shell");
        harness.run();
        harness.get_by_label("Executable").focus();
        harness.get_by_label("Executable").type_text("/bin/zsh");
        harness.run();

        harness.get_by_label("Save").click();
        harness.run();

        let Some(AppCommand::SaveProfile {
            profile: Profile::Local(local),
        }) = harness.state().command.as_ref()
        else {
            panic!("saving a valid local profile draft must return a SaveProfile command");
        };
        assert_eq!(local.identifier(), "dev-shell");
    }

    #[test]
    fn saved_local_profile_defaults_to_named_native_persistence() {
        let mut harness = profiles_harness(festerm_config::Configuration::new(Vec::new()).unwrap());
        harness.run();

        harness.get_by_label("New Local Profile").click();
        harness.run();
        harness.get_by_label("Name").focus();
        harness.get_by_label("Name").type_text("durable-local");
        harness.run();
        harness.get_by_label("Use a durable local session").click();
        harness.run();
        harness.get_by_label("Save").scroll_to_me();
        harness.run();
        harness.get_by_label("Save").click();
        harness.run();

        let Some(AppCommand::SaveProfile {
            profile: Profile::Local(local),
        }) = harness.state().command.as_ref()
        else {
            panic!("saving a durable local profile must return a SaveProfile command");
        };
        let persistence = local
            .persistence()
            .expect("saved local profile must retain explicit persistence");
        assert_eq!(
            persistence.provider(),
            PersistenceProviderKind::FestermSessiond
        );
        assert_eq!(persistence.session_name(), "durable-local");
    }

    #[test]
    fn a_detected_local_tmux_default_is_applied_when_the_toggle_is_first_enabled() {
        let mut harness = Harness::builder()
            .with_size(egui::vec2(560.0, 640.0))
            .build_ui_state(
                |ui, state: &mut ProfilesHarnessState| {
                    if let Some(command) = show_profiles(
                        ui,
                        state.tab_id,
                        &state.configuration,
                        None,
                        None,
                        PersistenceProviderKind::Tmux,
                    ) {
                        state.command = Some(command);
                    }
                },
                ProfilesHarnessState {
                    tab_id: AppState::for_test().active(),
                    configuration: festerm_config::Configuration::new(Vec::new()).unwrap(),
                    command: None,
                },
            );
        harness.run();

        harness.get_by_label("New Local Profile").click();
        harness.run();
        harness.get_by_label("Name").focus();
        harness.get_by_label("Name").type_text("detected-tmux");
        harness.run();
        harness.get_by_label("Use a durable local session").click();
        harness.run();
        harness.get_by_label("Save").scroll_to_me();
        harness.run();
        harness.get_by_label("Save").click();
        harness.run();

        let Some(AppCommand::SaveProfile {
            profile: Profile::Local(local),
        }) = harness.state().command.as_ref()
        else {
            panic!("saving a durable local profile must return a SaveProfile command");
        };
        let persistence = local
            .persistence()
            .expect("saved local profile must retain explicit persistence");
        assert_eq!(
            persistence.provider(),
            PersistenceProviderKind::Tmux,
            "a locally-detected tmux availability becomes the toggle-on default, \
             not the fesTerm-native fallback"
        );
    }

    #[test]
    fn sanitize_session_name_from_profile_name_normalizes_case_and_separators() {
        assert_eq!(
            sanitize_session_name_from_profile_name("My Prod Server!!"),
            "my-prod-server"
        );
        assert_eq!(
            sanitize_session_name_from_profile_name("  leading and trailing  "),
            "leading-and-trailing"
        );
        assert_eq!(
            sanitize_session_name_from_profile_name("already-valid_name.1"),
            "already-valid_name.1"
        );
        assert_eq!(sanitize_session_name_from_profile_name("***"), "");
        assert_eq!(
            sanitize_session_name_from_profile_name(&"x".repeat(100)),
            "x".repeat(64)
        );
    }

    #[test]
    fn remote_tmux_detection_defaults_to_tmux_or_screen() {
        assert_eq!(
            remote_provider_from_tmux_detection(RemoteTmuxDetectionResult::Detected),
            PersistenceProviderKind::Tmux
        );
        assert_eq!(
            remote_provider_from_tmux_detection(RemoteTmuxDetectionResult::NotDetected),
            PersistenceProviderKind::Screen
        );
    }

    #[test]
    fn detected_remote_provider_default_does_not_override_an_explicit_choice() {
        let mut draft = DurableSessionDraft {
            enabled: true,
            ..Default::default()
        };
        draft.select_provider(PersistenceProviderKind::Screen);
        draft.apply_detected_remote_provider_default(RemoteTmuxDetectionResult::Detected);

        assert_eq!(draft.provider, PersistenceProviderKind::Screen);
    }

    #[test]
    fn new_local_profile_session_name_tracks_the_profile_name_until_manually_edited() {
        let mut harness = profiles_harness(festerm_config::Configuration::new(Vec::new()).unwrap());
        harness.run();

        harness.get_by_label("New Local Profile").click();
        harness.run();
        harness.get_by_label("Use a durable local session").click();
        harness.run();
        harness.get_by_label("Name").focus();
        harness.get_by_label("Name").type_text("Build Box");
        harness.run();

        assert_eq!(
            harness.get_by_label("Session name").value().as_deref(),
            Some("build-box")
        );

        // Once the user edits the session name directly, further profile
        // name edits must not clobber their choice.
        harness.get_by_label("Session name").focus();
        harness.get_by_label("Session name").type_text("-pinned");
        harness.run();
        harness.get_by_label("Name").focus();
        harness.get_by_label("Name").type_text(" Two");
        harness.run();

        assert_eq!(
            harness.get_by_label("Session name").value().as_deref(),
            Some("build-box-pinned")
        );
    }

    #[test]
    fn profiles_new_local_profile_flow_reports_an_error_for_an_empty_name() {
        let mut harness = profiles_harness(festerm_config::Configuration::new(Vec::new()).unwrap());
        harness.run();

        harness.get_by_label("New Local Profile").click();
        harness.run();

        harness.get_by_label("Save").click();
        harness.run();

        assert!(harness.state().command.is_none());
        assert!(harness
            .query_by_label("Enter a name and a non-empty executable.")
            .is_some());
    }

    #[test]
    fn local_profile_executable_field_survives_a_real_pointer_click_on_a_suggestion() {
        // Unlike the sibling test above, this uses a raw `.click()` (a
        // synthetic pointer press/release), matching what a real mouse click
        // does: it first defocuses the text field as a "click elsewhere",
        // which used to hide the dropdown out from under the click before
        // the suggestion ever received it.
        let Some(expected_path) = festerm_pty::search_path_executables("cargo", 1)
            .into_iter()
            .next()
        else {
            panic!("`cargo` must be discoverable on PATH while running under `cargo test`");
        };

        let mut harness = profiles_harness(festerm_config::Configuration::new(Vec::new()).unwrap());
        harness.run();
        harness.get_by_label("New Local Profile").click();
        harness.run();
        harness.get_by_label("Executable").focus();
        harness.run();
        harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
        harness.get_by_label("Executable").type_text("cargo");
        harness.run();

        let expected_label = expected_path.display().to_string();
        harness
            .get_by_role_and_label(accesskit::Role::Button, &expected_label)
            .click();
        harness.run();

        assert_eq!(
            harness.get_by_label("Executable").value().as_deref(),
            Some(expected_label.as_str()),
            "a raw pointer click on a PATH suggestion must fill the field with its absolute path"
        );
    }

    #[test]
    fn local_profile_executable_field_offers_path_matches_and_selecting_one_fills_absolute_path() {
        // `cargo` must be resolvable on `PATH` for `cargo test` itself to be
        // running, so this environment always has at least one real match
        // without this test needing to mutate the process-wide `PATH`.
        let Some(expected_path) = festerm_pty::search_path_executables("cargo", 1)
            .into_iter()
            .next()
        else {
            panic!("`cargo` must be discoverable on PATH while running under `cargo test`");
        };

        let mut harness = profiles_harness(festerm_config::Configuration::new(Vec::new()).unwrap());
        harness.run();
        harness.get_by_label("New Local Profile").click();
        harness.run();
        harness.get_by_label("Executable").focus();
        harness.run();
        harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
        harness.get_by_label("Executable").type_text("cargo");
        harness.run();

        let expected_label = expected_path.display().to_string();
        // `click_accesskit()` dispatches a direct accesskit click action rather
        // than a synthetic pointer press/release, which reliably lands on the
        // suggestion regardless of exact pixel geometry.
        harness
            .get_by_role_and_label(accesskit::Role::Button, &expected_label)
            .click_accesskit();
        harness.run();

        assert_eq!(
            harness.get_by_label("Executable").value().as_deref(),
            Some(expected_label.as_str()),
            "selecting a PATH suggestion must fill the field with its absolute path"
        );
        assert!(
            harness.query_by_label(&expected_label).is_none(),
            "the suggestion dropdown must be hidden immediately after a selection"
        );
    }

    #[test]
    fn profiles_delete_flow_returns_a_delete_profile_command() {
        let profile = Profile::local("dev-shell", "/bin/zsh", Vec::new(), None).unwrap();
        let mut harness =
            profiles_harness(festerm_config::Configuration::new(vec![profile]).unwrap());
        harness.run();

        harness.get_by_label("Delete").click();
        harness.run();
        assert!(harness.query_by_label("Delete profile?").is_some());

        harness.get_by_label("Delete").click();
        harness.run();

        let Some(AppCommand::DeleteProfile { identifier }) = harness.state().command.as_ref()
        else {
            panic!("confirming deletion must return a DeleteProfile command");
        };
        assert_eq!(identifier, "dev-shell");
    }

    #[test]
    fn ssh_profile_editor_panel_stays_compact_instead_of_stretching_to_fill_the_window() {
        // Regression test for a panel that, when its scroll area filled
        // "available height" reported by the surrounding layout, either
        // collapsed to a sliver or stretched to match whatever height that
        // layout reported — instead of sizing to its own (short) content.
        let profile = Profile::ssh(
            "prod",
            "ssh.example.test",
            22,
            "deploy",
            "xterm-256color",
            80,
            24,
        )
        .unwrap();
        let mut harness = Harness::builder()
            .with_size(egui::vec2(900.0, 900.0))
            .build_ui_state(
                |ui, state: &mut ProfilesHarnessState| {
                    if let Some(command) = show_profiles(
                        ui,
                        state.tab_id,
                        &state.configuration,
                        None,
                        None,
                        PersistenceProviderKind::FestermSessiond,
                    ) {
                        state.command = Some(command);
                    }
                },
                ProfilesHarnessState {
                    tab_id: AppState::for_test().active(),
                    configuration: festerm_config::Configuration::new(vec![profile]).unwrap(),
                    command: None,
                },
            );
        harness.run();

        harness.get_by_label("Edit").click();
        harness.run();

        // A short connection-details-only form (password auth by default)
        // should keep "Save" well above a 900px-tall window rather than
        // stretching the panel to fill it.
        assert!(harness.get_by_label("Save").rect().max.y < 500.0);
        // With ample room, the whole form fits without needing to scroll at
        // all -- once content doesn't exceed the available height, egui's
        // default `ScrollBarVisibility::VisibleWhenNeeded` keeps the
        // scrollbar hidden (it may still exist in the accessibility tree,
        // just marked hidden).
        let scroll_bar = harness.query_by_role(accesskit::Role::ScrollBar);
        assert!(
            scroll_bar.is_none_or(|node| node.accesskit_node().is_hidden()),
            "scroll bar should not be visible when the form fits comfortably"
        );
    }

    #[test]
    fn ssh_profile_editor_panel_does_not_overlap_a_visible_bottom_status_bar() {
        // Regression test: the editor's height budget must account for the
        // app's bottom status bar (reserved via `egui::Panel::bottom`), not
        // just the raw window height, or the panel ends up sized as if that
        // strip weren't there and visually runs into/under it.
        let profile = Profile::ssh(
            "prod",
            "ssh.example.test",
            22,
            "deploy",
            "xterm-256color",
            80,
            24,
        )
        .unwrap();
        let mut harness = Harness::builder()
            .with_size(egui::vec2(900.0, 350.0))
            .build_ui_state(
                |ui, state: &mut ProfilesHarnessState| {
                    egui::Panel::bottom("status_bar")
                        .resizable(false)
                        .show_separator_line(false)
                        .show(ui, |ui| {
                            ui.set_min_height(24.0);
                            ui.set_max_height(24.0);
                        });
                    if let Some(command) = show_profiles(
                        ui,
                        state.tab_id,
                        &state.configuration,
                        None,
                        None,
                        PersistenceProviderKind::FestermSessiond,
                    ) {
                        state.command = Some(command);
                    }
                },
                ProfilesHarnessState {
                    tab_id: AppState::for_test().active(),
                    configuration: festerm_config::Configuration::new(vec![profile]).unwrap(),
                    command: None,
                },
            );
        harness.run();

        harness.get_by_label("Edit").click();
        harness.run();

        let status_bar_top =
            egui::containers::panel::PanelState::load(&harness.ctx, egui::Id::new("status_bar"))
                .expect("status bar panel state should be recorded")
                .outer_rect
                .top();
        assert!(
            harness.get_by_label("Save").rect().max.y < status_bar_top,
            "the editor panel must stay above the status bar rather than overlapping it"
        );
    }

    #[test]
    fn ssh_profile_editor_offers_a_password_field_that_dispatches_store_profile_password() {
        let profile = Profile::ssh(
            "prod",
            "ssh.example.test",
            22,
            "deploy",
            "xterm-256color",
            80,
            24,
        )
        .unwrap();
        let mut harness =
            profiles_harness(festerm_config::Configuration::new(vec![profile]).unwrap());
        harness.run();

        harness.get_by_label("Edit").click();
        harness.run();
        assert!(harness.query_by_label("Edit SSH Profile").is_some());

        // No stored credential yet, so the field starts empty and "Save
        // password" is disabled until something is typed.
        assert!(harness
            .query_by_label(
                "Enter a password to remember it in native secure storage, or leave this blank to be prompted at connect time."
            )
            .is_some());

        harness.get_by_label("Password").focus();
        harness.get_by_label("Password").type_text("hunter2");
        harness.run();

        // The password-authentication panel is taller than the harness
        // viewport (matching the private-key panel that motivated wrapping
        // this editor in a `ScrollArea`), so "Save password" starts
        // scrolled out of view; scroll it into view before clicking, same
        // as a real user would.
        harness.get_by_label("Save password").scroll_to_me();
        harness.run();
        harness.get_by_label("Save password").click();
        harness.run();

        let Some(AppCommand::StoreProfilePassword { profile_id, .. }) =
            harness.state().command.as_ref()
        else {
            panic!("clicking Save password must return a StoreProfilePassword command");
        };
        assert_eq!(profile_id, "prod");
    }

    #[test]
    fn ssh_profile_editor_offers_a_private_key_field_that_dispatches_store_profile_private_key() {
        let profile = Profile::ssh(
            "prod",
            "ssh.example.test",
            22,
            "deploy",
            "xterm-256color",
            80,
            24,
        )
        .unwrap();
        let mut harness =
            profiles_harness(festerm_config::Configuration::new(vec![profile]).unwrap());
        harness.run();

        harness.get_by_label("Edit").click();
        harness.run();
        assert!(harness.query_by_label("Edit SSH Profile").is_some());

        harness
            .get_by_label("Private-key authentication")
            .scroll_to_me();
        harness.run();
        harness.get_by_label("Private-key authentication").click();
        harness.run();
        assert!(harness
            .query_by_label("Enter an OpenSSH private key to remember it in native secure storage.")
            .is_some());
        // Switching methods must not surface the password-authentication
        // fields at the same time.
        assert!(harness.query_by_label("Password").is_none());

        harness.get_by_label("OpenSSH private key").focus();
        harness.get_by_label("OpenSSH private key").type_text(
            "-----BEGIN OPENSSH PRIVATE KEY-----\nfake\n-----END OPENSSH PRIVATE KEY-----",
        );
        harness.run();

        // As above: the private-key panel is taller than the harness
        // viewport, so "Save private key" starts scrolled out of view.
        harness.get_by_label("Save private key").scroll_to_me();
        harness.run();
        harness.get_by_label("Save private key").click();
        harness.run();

        let Some(AppCommand::StoreProfilePrivateKey { profile_id, .. }) =
            harness.state().command.as_ref()
        else {
            panic!("clicking Save private key must return a StoreProfilePrivateKey command");
        };
        assert_eq!(profile_id, "prod");
    }

    #[test]
    fn ssh_profile_editor_saves_named_tmux_persistence() {
        let mut harness = profiles_harness(festerm_config::Configuration::new(Vec::new()).unwrap());
        harness.run();

        harness.get_by_label("New SSH Profile").click();
        harness.run();
        for (label, value) in [
            ("Name", "build-host"),
            ("Username", "builder"),
            ("Host", "ssh.example.test"),
        ] {
            harness.get_by_label(label).click();
            harness.run();
            harness.get_by_label(label).type_text(value);
            harness.run();
        }
        harness.get_by_label("Use a durable remote session").click();
        harness.run();
        harness.get_by_label("Save").scroll_to_me();
        harness.run();
        harness.get_by_label("Save").click();
        harness.run();

        let Some(AppCommand::SaveProfile { profile }) = harness.state().command.as_ref() else {
            panic!("saving the SSH profile must return a SaveProfile command");
        };
        let persistence = profile
            .persistence()
            .expect("the profile must retain durable-session settings");
        assert_eq!(persistence.provider(), PersistenceProviderKind::Tmux);
        assert_eq!(persistence.session_name(), "build-host");
    }

    #[test]
    fn editing_durable_session_settings_preserves_the_stored_credential_reference() {
        let reference = festerm_secret_store::SecretReference::generate();
        let expected_reference = reference.to_persisted_string();
        let profile = Profile::ssh(
            "prod",
            "ssh.example.test",
            22,
            "deploy",
            "xterm-256color",
            80,
            24,
        )
        .unwrap()
        .with_credential_reference_kind(reference, CredentialKind::PrivateKey)
        .unwrap();
        let mut harness =
            profiles_harness(festerm_config::Configuration::new(vec![profile]).unwrap());
        harness.run();

        harness.get_by_label("Edit").click();
        harness.run();
        harness.get_by_label("Use a durable remote session").click();
        harness.run();
        harness.get_by_label("Save").scroll_to_me();
        harness.run();
        harness.get_by_label("Save").click();
        harness.run();

        let Some(AppCommand::SaveProfile { profile }) = harness.state().command.as_ref() else {
            panic!("editing the SSH profile must return a SaveProfile command");
        };
        assert_eq!(
            profile
                .credential_reference()
                .expect("the stored credential reference must survive the edit")
                .to_persisted_string(),
            expected_reference
        );
        assert_eq!(
            profile
                .as_ssh()
                .expect("profile remains SSH")
                .credential_kind(),
            CredentialKind::PrivateKey
        );
    }

    #[test]
    fn ssh_profile_editor_adds_a_port_forward_and_saves_it_with_the_profile() {
        let mut harness = profiles_harness(festerm_config::Configuration::new(Vec::new()).unwrap());
        harness.run();

        harness.get_by_label("New SSH Profile").click();
        harness.run();
        for (label, value) in [
            ("Name", "build-host"),
            ("Username", "builder"),
            ("Host", "ssh.example.test"),
        ] {
            harness.get_by_label(label).focus();
            harness.get_by_label(label).type_text(value);
            harness.run();
        }
        harness.get_by_label("Add port forward").click();
        harness.run();

        assert_eq!(
            harness.get_by_label("Bind host").value().as_deref(),
            Some("127.0.0.1")
        );

        for (label, value) in [
            ("Bind port", "8080"),
            ("Destination host", "app.internal"),
            ("Destination port", "80"),
        ] {
            harness.get_by_label(label).focus();
            harness.get_by_label(label).type_text(value);
            harness.run();
        }

        harness.get_by_label("Save").scroll_to_me();
        harness.run();
        harness.get_by_label("Save").click();
        harness.run();

        let Some(AppCommand::SaveProfile { profile }) = harness.state().command.as_ref() else {
            panic!("saving the SSH profile must return a SaveProfile command");
        };
        let ssh = profile.as_ssh().expect("saved profile remains SSH");
        assert_eq!(ssh.port_forwards().len(), 1);
        let forward = &ssh.port_forwards()[0];
        assert_eq!(forward.direction(), SshPortForwardDirection::Local);
        assert_eq!(forward.bind_host(), "127.0.0.1");
        assert_eq!(forward.bind_port(), 8080);
        assert_eq!(forward.destination_host(), "app.internal");
        assert_eq!(forward.destination_port(), 80);
    }

    #[test]
    fn new_sftp_profile_saves_an_initial_password_with_the_profile() {
        let mut harness = profiles_harness(festerm_config::Configuration::empty());
        harness.run();

        harness.get_by_label("New SFTP Profile").click();
        harness.run();
        for (label, value) in [
            ("Name", "files"),
            ("Username", "deploy"),
            ("Host", "sftp.example.test"),
            ("Password", "initial-password"),
        ] {
            harness.get_by_label(label).scroll_to_me();
            harness.run();
            harness.get_by_label(label).focus();
            harness.get_by_label(label).type_text(value);
            harness.run();
        }
        harness.get_by_label("Save").scroll_to_me();
        harness.run();
        harness.get_by_label("Save").click();
        harness.run();

        let Some(AppCommand::SaveProfileWithCredential {
            profile,
            credential,
        }) = harness.state().command.as_ref()
        else {
            panic!("new SFTP profile with a password must save metadata and credential together");
        };
        assert_eq!(
            profile
                .as_ssh()
                .expect("SFTP reuses SSH metadata")
                .profile_kind(),
            RemoteProfileKind::Sftp
        );
        assert!(matches!(credential, ProfileCredentialToStore::Password(_)));
        assert!(!format!("{credential:?}").contains("initial-password"));
    }

    #[test]
    fn creating_a_profile_cannot_replace_an_existing_profiles_secret_reference() {
        let reference = festerm_secret_store::SecretReference::generate();
        let existing = Profile::ssh(
            "production",
            "ssh.example.test",
            22,
            "deploy",
            "xterm-256color",
            80,
            24,
        )
        .unwrap()
        .with_credential_reference(reference)
        .unwrap();
        let mut harness =
            profiles_harness(festerm_config::Configuration::new(vec![existing]).unwrap());
        harness.run();

        harness.get_by_label("New SSH Profile").click();
        harness.run();
        for (label, value) in [
            ("Name", "production"),
            ("Username", "other-user"),
            ("Host", "other.example.test"),
            ("Password", "replacement-password"),
        ] {
            harness.get_by_label(label).scroll_to_me();
            harness.run();
            harness.get_by_label(label).focus();
            harness.get_by_label(label).type_text(value);
            harness.run();
        }
        harness.get_by_label("Save").scroll_to_me();
        harness.run();
        harness.get_by_label("Save").click();
        harness.run();

        assert!(harness.state().command.is_none());
        assert!(harness
            .query_by_label("A profile named 'production' already exists.")
            .is_some());
    }

    #[test]
    fn renaming_a_profile_cannot_replace_another_profiles_secret_reference() {
        let original = Profile::ssh(
            "one",
            "one.example.test",
            22,
            "deploy",
            "xterm-256color",
            80,
            24,
        )
        .unwrap()
        .with_credential_reference(festerm_secret_store::SecretReference::generate())
        .unwrap();
        let preserved_reference = festerm_secret_store::SecretReference::generate();
        let preserved_reference_id = preserved_reference.to_persisted_string();
        let preserved = Profile::ssh(
            "two",
            "two.example.test",
            22,
            "release",
            "xterm-256color",
            80,
            24,
        )
        .unwrap()
        .with_credential_reference(preserved_reference)
        .unwrap();
        let configuration =
            festerm_config::Configuration::new(vec![original.clone(), preserved]).unwrap();
        let mut draft = SshProfileDraft::from_profile(original.as_ssh().unwrap());
        draft.name = "two".to_owned();

        let trimmed_name = draft.name.trim();
        if ssh_profile_name_collides(&configuration, draft.original_id.as_deref(), trimmed_name) {
            draft.error = Some(format!(
                "A profile named '{}' already exists.",
                trimmed_name
            ));
        }

        assert_eq!(
            draft.error.as_deref(),
            Some("A profile named 'two' already exists.")
        );
        let preserved = configuration
            .profile("two")
            .and_then(Profile::as_ssh)
            .expect("the conflicting profile must remain untouched");
        assert_eq!(preserved.host(), "two.example.test");
        assert_eq!(preserved.username(), "release");
        assert_eq!(
            preserved
                .credential_reference()
                .expect("the conflicting profile must keep its stored credential")
                .to_persisted_string(),
            preserved_reference_id
        );
    }

    #[test]
    fn new_ssh_profile_can_stage_an_initial_private_key() {
        let mut draft = SshProfileDraft {
            name: "build-host".to_owned(),
            username: "builder".to_owned(),
            host: "ssh.example.test".to_owned(),
            auth_method: SshAuthenticationMethod::PrivateKey,
            private_key: "private-key-material".to_owned(),
            key_passphrase: "key-passphrase".to_owned(),
            ..Default::default()
        };

        let profile = draft.build(None).expect("profile metadata should validate");
        let credential = draft
            .take_initial_credential()
            .expect("private key should be staged with a new profile");

        assert_eq!(
            profile
                .as_ssh()
                .expect("profile remains SSH")
                .profile_kind(),
            RemoteProfileKind::Ssh
        );
        assert!(matches!(
            credential,
            ProfileCredentialToStore::PrivateKey(_)
        ));
        let debug = format!("{credential:?}");
        assert!(!debug.contains("private-key-material"));
        assert!(!debug.contains("key-passphrase"));
        assert!(draft.private_key.is_empty());
        assert!(draft.key_passphrase.is_empty());
    }

    #[test]
    fn ssh_profile_editor_can_remove_a_saved_port_forward_before_saving() {
        let profile = Profile::Ssh(
            Profile::ssh(
                "prod",
                "ssh.example.test",
                22,
                "deploy",
                "xterm-256color",
                80,
                24,
            )
            .unwrap()
            .as_ssh()
            .unwrap()
            .clone()
            .with_port_forwards(vec![SshPortForwardConfiguration::new(
                SshPortForwardDirection::Local,
                "127.0.0.1",
                8080,
                "app.internal",
                80,
            )
            .unwrap()])
            .unwrap(),
        );
        let mut harness =
            profiles_harness(festerm_config::Configuration::new(vec![profile]).unwrap());
        harness.run();

        harness.get_by_label("Edit").click();
        harness.run();
        assert!(harness.query_by_label("Remove forward 1").is_some());

        harness.get_by_label("Remove forward 1").click();
        harness.run();
        harness.get_by_label("Save").scroll_to_me();
        harness.run();
        harness.get_by_label("Save").click();
        harness.run();

        let Some(AppCommand::SaveProfile { profile }) = harness.state().command.as_ref() else {
            panic!("saving the SSH profile must return a SaveProfile command");
        };
        assert!(profile
            .as_ssh()
            .expect("saved profile remains SSH")
            .port_forwards()
            .is_empty());
    }

    #[test]
    fn ssh_profile_editor_rejects_an_invalid_port_forward_without_saving() {
        let mut harness = profiles_harness(festerm_config::Configuration::new(Vec::new()).unwrap());
        harness.run();

        harness.get_by_label("New SSH Profile").click();
        harness.run();
        for (label, value) in [
            ("Name", "build-host"),
            ("Username", "builder"),
            ("Host", "ssh.example.test"),
        ] {
            harness.get_by_label(label).focus();
            harness.get_by_label(label).type_text(value);
            harness.run();
        }
        harness.get_by_label("Add port forward").click();
        harness.run();
        for (label, value) in [
            ("Bind port", "0"),
            ("Destination host", "app.internal"),
            ("Destination port", "80"),
        ] {
            harness.get_by_label(label).focus();
            harness.get_by_label(label).type_text(value);
            harness.run();
        }

        harness.get_by_label("Save").scroll_to_me();
        harness.run();
        harness.get_by_label("Save").click();
        harness.run();

        assert!(harness.state().command.is_none());
        assert!(harness
            .query_by_label(
                "SSH port forwards must use non-empty, safe bind and destination hosts with nonzero ports"
            )
            .is_some());
    }

    #[test]
    fn sftp_launcher_defaults_to_gui_mode_and_can_use_terminal_mode() {
        let mut form = SshLauncherForm {
            quick_connect: "deploy@sftp.example.test".to_owned(),
            ..SshLauncherForm::default()
        };

        assert!(matches!(
            form.submit_quick_connect_sftp().unwrap(),
            AppCommand::OpenSftpFileManager { .. }
        ));

        form.sftp_gui_mode = false;
        assert!(matches!(
            form.submit_quick_connect_sftp().unwrap(),
            AppCommand::StartSftpSession { .. }
        ));
    }

    #[test]
    fn restored_terminal_sftp_surface_preserves_terminal_mode() {
        let profile = Profile::sftp("files", "sftp.example.test", 22, "deploy", true).unwrap();
        let mut form = SshLauncherForm::default();
        form.prefill_restored_sftp_profile(profile.as_ssh().unwrap());

        assert!(!form.sftp_gui_mode);
        assert!(matches!(
            form.submit_sftp().unwrap(),
            AppCommand::StartSftpSession { .. }
        ));
    }

    #[test]
    fn profiles_surface_creates_reusable_terminal_sftp_profiles() {
        let mut harness = profiles_harness(festerm_config::Configuration::empty());
        harness.run();

        harness.get_by_label("New SFTP Profile").click();
        harness.run();
        assert!(harness.query_by_label("New SFTP Profile").is_some());
        assert!(harness
            .query_by_label("Use graphical file manager")
            .is_some());

        for (label, value) in [
            ("Name", "files"),
            ("Username", "deploy"),
            ("Host", "sftp.example.test"),
        ] {
            harness.get_by_label(label).focus();
            harness.get_by_label(label).type_text(value);
            harness.run();
        }
        harness.get_by_label("Use graphical file manager").click();
        harness.run();
        harness.get_by_label("Save").scroll_to_me();
        harness.run();
        harness.get_by_label("Save").click();
        harness.run();

        let Some(AppCommand::SaveProfile { profile }) = harness.state().command.as_ref() else {
            panic!("saving the SFTP profile must return a SaveProfile command");
        };
        let sftp = profile
            .as_ssh()
            .expect("SFTP reuses SSH transport metadata");
        assert_eq!(sftp.profile_kind(), RemoteProfileKind::Sftp);
        assert!(!sftp.sftp_gui_mode());
    }
}
