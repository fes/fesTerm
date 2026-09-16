//! Profiles application-surface presentation, split out of `screens.rs`.

use super::*;

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
    profile_search: String,
}

fn profiles_state_id(tab_id: TabId) -> egui::Id {
    egui::Id::new(("profiles_state", tab_id))
}

fn profile_table_item(profile: &Profile, configuration: &Configuration) -> ProfileTableItem {
    let (kind, location, subtitle) = match profile {
        Profile::Local(local) => (
            ProfileTableKind::Local,
            local.executable().to_owned(),
            local
                .working_directory()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "Default working directory".to_owned()),
        ),
        Profile::Ssh(ssh) => (
            match ssh.profile_kind() {
                RemoteProfileKind::Ssh => ProfileTableKind::Ssh,
                RemoteProfileKind::Sftp => ProfileTableKind::Sftp,
            },
            ssh.host().to_owned(),
            ssh.username().to_owned(),
        ),
        Profile::Serial(serial) => {
            let data_bits = match serial.data_bits() {
                festerm_config::SerialDataBits::Five => "5",
                festerm_config::SerialDataBits::Six => "6",
                festerm_config::SerialDataBits::Seven => "7",
                festerm_config::SerialDataBits::Eight => "8",
            };
            let parity = match serial.parity() {
                festerm_config::SerialParity::None => "N",
                festerm_config::SerialParity::Odd => "O",
                festerm_config::SerialParity::Even => "E",
            };
            let stop_bits = match serial.stop_bits() {
                festerm_config::SerialStopBits::One => "1",
                festerm_config::SerialStopBits::Two => "2",
            };
            (
                ProfileTableKind::Serial,
                serial.device().to_owned(),
                format!(
                    "{} baud · {data_bits}{parity}{stop_bits}",
                    serial.baud_rate()
                ),
            )
        }
    };
    ProfileTableItem {
        identifier: profile.identifier().to_owned(),
        label: profile.identifier().to_owned(),
        subtitle: Some(subtitle),
        kind,
        location,
        last_used_unix_seconds: configuration.profile_last_used(profile.identifier()),
    }
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

    fn from_seed(seed: SshProfileDraftSeed) -> Self {
        Self {
            name: seed.name,
            host: seed.host,
            port: seed.port,
            username: seed.username,
            port_forwards: seed
                .port_forwards
                .into_iter()
                .map(|forward| SshPortForwardDraft {
                    direction: forward.direction,
                    bind_host: forward.bind_host,
                    bind_port: forward.bind_port,
                    destination_host: forward.destination_host,
                    destination_port: forward.destination_port,
                })
                .collect(),
            durable_session: DurableSessionDraft {
                enabled: seed.durable_session_enabled,
                provider: seed.durable_session_provider,
                provider_touched: true,
                session_name: seed.durable_session_name,
                session_name_touched: true,
                ..Default::default()
            },
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
pub(super) fn serial_enum_combo<T: Copy + PartialEq + SerialEnumLabels>(
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

pub(super) trait SerialEnumLabels: Sized {
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

pub(super) fn profile_text_edit_with_id(
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

pub(crate) fn show_profiles(
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
            NewProfileKind::SshFromDraft(draft) => {
                ProfilesScreenMode::EditSsh(SshProfileDraft::from_seed(draft))
            }
        };
    }

    let mut next_mode = None;
    ui.horizontal(|ui| {
        const SIDE_MARGIN: f32 = 26.0;
        ui.add_space(SIDE_MARGIN);
        // Match the leading space on the trailing edge so the table and the
        // search row stop short of the window edge instead of running into it.
        let content_width = (ui.available_width() - SIDE_MARGIN).max(0.0);
        ui.vertical(|ui| {
            ui.set_max_width(content_width);
    match &mut state.mode {
        ProfilesScreenMode::List => {
            let now_unix_seconds = unix_now_seconds();
            let query = state.profile_search.trim().to_lowercase();
            let visible_profiles: Vec<&Profile> = configuration
                .profiles()
                .iter()
                .filter(|profile| {
                    let item = profile_table_item(profile, configuration);
                    query.is_empty()
                        || item.label.to_lowercase().contains(&query)
                        || item.location.to_lowercase().contains(&query)
                        || item.kind.type_label().to_lowercase().contains(&query)
                        || item
                            .subtitle
                            .as_deref()
                            .is_some_and(|subtitle| subtitle.to_lowercase().contains(&query))
                })
                .collect();
            ui.vertical(|ui| {
                ui.add_space(24.0);
                ui.heading("Profiles");
                ui.label("Reusable local, SSH, SFTP, and serial launch definitions.");
                ui.add_space(42.0);
                ui.horizontal(|ui| {
                    // Lay the button out from the trailing edge so the search
                    // field absorbs the remainder exactly and the row ends
                    // flush with the table below it, whatever the button
                    // measures.
                    let new_profile = ui
                        .with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let new_profile = launcher_dropdown_button(
                                ui,
                                Icon::NewProfile,
                                "New Profile",
                                Some("New Profile"),
                                true,
                            );
                            ui.add_space(16.0);
                            show_profile_search_field(
                                ui,
                                ui.available_width().max(120.0),
                                &mut state.profile_search,
                            );
                            new_profile
                        })
                        .inner;
                    egui::Popup::menu(&new_profile).show(|ui| {
                        for (label, mode) in [
                            (
                                "Local",
                                ProfilesScreenMode::EditLocal(LocalProfileDraft::new(
                                    local_default_provider,
                                )),
                            ),
                            ("SSH", ProfilesScreenMode::EditSsh(SshProfileDraft::default())),
                            ("SFTP", ProfilesScreenMode::EditSsh(SshProfileDraft::new_sftp())),
                            (
                                "Serial",
                                ProfilesScreenMode::EditSerial(SerialProfileDraft::default()),
                            ),
                        ] {
                            if ui.button(label).clicked() {
                                next_mode = Some(mode.clone());
                                ui.close();
                            }
                        }
                    });
                });
                ui.add_space(28.0);

                if configuration.profiles().is_empty() {
                    ui.add_space(12.0);
                    ui.label("No profiles saved yet.");
                    return;
                }

                let table_width = ui.available_width().max(0.0);
                egui::Frame::new()
                    .fill(theme::SURFACE_PANEL)
                    .stroke(Stroke::new(1.0, theme::BORDER_SUBTLE))
                    .corner_radius(LAUNCHER_PANEL_CORNER)
                    .inner_margin(egui::Margin::symmetric(0, 12))
                    .show(ui, |ui| {
                        ui.set_width(table_width);
                        let options = ProfileTableOptions::profiles(table_width);
                        show_profile_column_headers(ui, table_width, table_width, options);
                        if visible_profiles.is_empty() {
                            ui.add_space(12.0);
                            ui.horizontal(|ui| {
                                ui.add_space(table_width * options.column_origins[0]);
                                ui.label(
                                    egui::RichText::new("No profiles match this search.")
                                        .size(LAUNCHER_BODY_TEXT_SIZE)
                                        .color(theme::TEXT_MUTED),
                                );
                            });
                            return;
                        }
                        for profile in visible_profiles {
                            let item = profile_table_item(profile, configuration);
                            let row = show_profile_row(
                                ui,
                                table_width,
                                &item,
                                false,
                                now_unix_seconds,
                                options,
                                ProfileTableMenu::Profiles,
                            );
                            row.response.dnd_set_drag_payload(item.identifier.clone());
                            if let Some(action) = row.action {
                                match action {
                                    ProfileTableAction::Connect => {
                                        command = Some(item.connect_command());
                                    }
                                    ProfileTableAction::OpenSftpFileManager => {
                                        command = Some(
                                            AppCommand::OpenConfiguredSftpFileManagerProfile {
                                                profile_id: item.identifier.clone(),
                                            },
                                        );
                                    }
                                    ProfileTableAction::Edit => {
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
                                    ProfileTableAction::Duplicate => {
                                        let duplicate_name = format!("{}-copy", item.identifier);
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
                                    ProfileTableAction::Delete => {
                                        next_mode = Some(ProfilesScreenMode::ConfirmDelete {
                                            identifier: item.identifier.clone(),
                                            references: configuration
                                                .workspace_tab_references(&item.identifier),
                                        });
                                    }
                                    ProfileTableAction::LauncherCrossover(_) => unreachable!(
                                        "profiles table does not expose launcher crossover actions"
                                    ),
                                }
                            }
                            let row_rect = row.response.rect;
                            if let Some(dragged) = egui::DragAndDrop::payload::<String>(ui.ctx()) {
                                let released = ui.input(|i| i.pointer.any_released());
                                if let Some(pointer_pos) = ui.ctx().pointer_interact_pos() {
                                    if *dragged != item.identifier
                                        && released
                                        && row_rect.contains(pointer_pos)
                                    {
                                        command = Some(AppCommand::ReorderProfiles {
                                            moved: (*dragged).clone(),
                                            before: Some(item.identifier.clone()),
                                        });
                                    }
                                }
                            }
                        }
                        let (end_rect, _) =
                            ui.allocate_exact_size(vec2(table_width, 12.0), Sense::hover());
                        if let Some(dragged) = egui::DragAndDrop::payload::<String>(ui.ctx()) {
                            let released = ui.input(|i| i.pointer.any_released());
                            if let Some(pointer_pos) = ui.ctx().pointer_interact_pos() {
                                if released
                                    && configuration.profiles().last().is_some_and(|last| {
                                        last.identifier() != dragged.as_str()
                                    })
                                    && end_rect.contains(pointer_pos)
                                {
                                    command = Some(AppCommand::ReorderProfiles {
                                        moved: (*dragged).clone(),
                                        before: None,
                                    });
                                }
                            }
                        }
                    });
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
                            DurableSessionLayout::Inline,
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
                                        DurableSessionLayout::Inline,
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
    fn profiles_list_keeps_a_trailing_margin_and_aligns_its_search_row_with_the_table() {
        const PANEL_WIDTH: f32 = 560.0;
        let configuration = festerm_config::Configuration::new(vec![Profile::local(
            "dev-shell",
            "/bin/zsh",
            Vec::new(),
            None,
        )
        .unwrap()])
        .unwrap();
        let mut harness = profiles_harness(configuration);
        harness.run();

        let search_right = harness.get_by_label("Search profiles…").rect().right();
        let new_profile_right = harness.get_by_label("New Profile").rect().right();
        let search_left = harness.get_by_label("Search profiles…").rect().left();

        assert!(
            new_profile_right < PANEL_WIDTH - 8.0,
            "the profiles list must keep a trailing margin instead of running into the \
             window edge, but the New Profile button reached {new_profile_right} of \
             {PANEL_WIDTH}"
        );
        assert!(
            search_right < new_profile_right,
            "the search field must sit to the left of the New Profile button"
        );
        assert!(
            search_left > 8.0,
            "the list must keep its leading margin too"
        );
    }

    fn open_new_profile(harness: &mut Harness<'static, ProfilesHarnessState>, kind: &str) {
        harness.get_by_label("New Profile").click();
        harness.run();
        harness.get_by_label(kind).click();
        harness.run();
    }

    fn click_profile_action(
        harness: &mut Harness<'static, ProfilesHarnessState>,
        profile: &str,
        action: &str,
    ) {
        harness
            .get_by_label(&format!("More actions for {profile}"))
            .click();
        harness.run();
        harness.get_by_label(action).click();
        harness.run();
    }

    #[test]
    fn ssh_profile_editor_accepts_a_launcher_seed_without_secrets() {
        let seed = SshProfileDraftSeed {
            name: "staging".to_owned(),
            host: "ssh.example.test".to_owned(),
            port: "2222".to_owned(),
            username: "deploy".to_owned(),
            port_forwards: Vec::new(),
            durable_session_enabled: false,
            durable_session_provider: PersistenceProviderKind::Tmux,
            durable_session_name: "main".to_owned(),
        };
        let configuration =
            festerm_config::Configuration::new(Vec::new()).expect("empty configuration is valid");
        let mut harness = Harness::builder()
            .with_size(egui::vec2(560.0, 640.0))
            .build_ui_state(
                move |ui, state: &mut ProfilesHarnessState| {
                    if let Some(command) = show_profiles(
                        ui,
                        state.tab_id,
                        &state.configuration,
                        None,
                        Some(NewProfileKind::SshFromDraft(seed.clone())),
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
            );

        harness.run();
        assert!(harness.query_by_label("New SSH Profile").is_some());
        harness.get_by_label("Save").click();
        harness.run();

        let Some(AppCommand::SaveProfile {
            profile: Profile::Ssh(profile),
        }) = harness.state().command.as_ref()
        else {
            panic!("seeded SSH profile editor must save a populated profile");
        };
        assert_eq!(profile.identifier(), "staging");
        assert_eq!(profile.host(), "ssh.example.test");
        assert_eq!(profile.port(), 2222);
        assert_eq!(profile.username(), "deploy");
        assert!(profile.credential_reference().is_none());
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

        let from = harness.get_by_label("alpha — Local · sh").rect().center();
        let to = harness.get_by_label("gamma — Local · sh").rect().center();

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

        let from = harness.get_by_label("alpha — Local · sh").rect().center();
        let beta_rect = harness.get_by_label("beta — Local · sh").rect();
        let to = egui::pos2(beta_rect.center().x, beta_rect.bottom() + 6.0);

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
    fn profiles_list_filters_by_search_text() {
        let configuration = festerm_config::Configuration::new(vec![
            Profile::local("dev-shell", "/bin/zsh", Vec::new(), None).unwrap(),
            Profile::ssh(
                "build-host",
                "build.example.test",
                22,
                "builder",
                "xterm-256color",
                80,
                24,
            )
            .unwrap(),
        ])
        .unwrap();
        let mut harness = profiles_harness(configuration);
        harness.run();

        harness.get_by_label("Search profiles…").focus();
        harness
            .get_by_label("Search profiles…")
            .type_text("builder");
        harness.run();

        assert!(harness
            .query_by_label("build-host — SSH · build.example.test")
            .is_some());
        assert!(harness
            .query_by_label("dev-shell — Local · /bin/zsh")
            .is_none());
    }

    #[test]
    fn profiles_new_profile_dropdown_offers_every_kind() {
        let mut harness = profiles_harness(festerm_config::Configuration::empty());
        harness.run();

        harness.get_by_label("New Profile").click();
        harness.run();
        for label in ["Local", "SSH", "SFTP", "Serial"] {
            assert!(
                harness.query_by_label(label).is_some(),
                "{label} must be offered by the New Profile menu"
            );
        }
    }

    #[test]
    fn profiles_row_menu_dispatches_profile_actions() {
        let local = Profile::local("dev-shell", "/bin/zsh", Vec::new(), None).unwrap();
        let ssh = Profile::ssh(
            "prod",
            "ssh.example.test",
            22,
            "deploy",
            "xterm-256color",
            80,
            24,
        )
        .unwrap();

        let mut connect =
            profiles_harness(festerm_config::Configuration::new(vec![local.clone()]).unwrap());
        connect.run();
        click_profile_action(&mut connect, "dev-shell", "Connect");
        assert!(matches!(
            connect.state().command,
            Some(AppCommand::StartConfiguredLocalProfile { ref profile_id })
                if profile_id == "dev-shell"
        ));

        let mut edit =
            profiles_harness(festerm_config::Configuration::new(vec![local.clone()]).unwrap());
        edit.run();
        click_profile_action(&mut edit, "dev-shell", "Edit");
        assert!(edit.query_by_label("Edit Local Profile").is_some());

        let mut duplicate =
            profiles_harness(festerm_config::Configuration::new(vec![local.clone()]).unwrap());
        duplicate.run();
        click_profile_action(&mut duplicate, "dev-shell", "Duplicate");
        assert!(duplicate.query_by_label("New Local Profile").is_some());
        assert_eq!(
            duplicate.get_by_label("Name").value().as_deref(),
            Some("dev-shell-copy")
        );

        let mut delete = profiles_harness(festerm_config::Configuration::new(vec![local]).unwrap());
        delete.run();
        click_profile_action(&mut delete, "dev-shell", "Delete");
        assert!(delete.query_by_label("Delete profile?").is_some());

        let mut open_sftp =
            profiles_harness(festerm_config::Configuration::new(vec![ssh]).unwrap());
        open_sftp.run();
        click_profile_action(&mut open_sftp, "prod", "Open SFTP");
        assert!(matches!(
            open_sftp.state().command,
            Some(AppCommand::OpenConfiguredSftpFileManagerProfile { ref profile_id })
                if profile_id == "prod"
        ));
    }

    #[test]
    fn profiles_new_local_profile_flow_returns_a_save_profile_command() {
        let mut harness = profiles_harness(festerm_config::Configuration::new(Vec::new()).unwrap());
        harness.run();

        open_new_profile(&mut harness, "Local");

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

        open_new_profile(&mut harness, "Local");
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

        open_new_profile(&mut harness, "Local");
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

        open_new_profile(&mut harness, "Local");
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

        open_new_profile(&mut harness, "Local");

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
        open_new_profile(&mut harness, "Local");
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
        open_new_profile(&mut harness, "Local");
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

        click_profile_action(&mut harness, "dev-shell", "Delete");
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

        click_profile_action(&mut harness, "prod", "Edit");

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

        click_profile_action(&mut harness, "prod", "Edit");

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

        click_profile_action(&mut harness, "prod", "Edit");
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

        click_profile_action(&mut harness, "prod", "Edit");
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

        open_new_profile(&mut harness, "SSH");
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

        click_profile_action(&mut harness, "prod", "Edit");
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

        open_new_profile(&mut harness, "SSH");
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

        open_new_profile(&mut harness, "SFTP");
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

        open_new_profile(&mut harness, "SSH");
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

        click_profile_action(&mut harness, "prod", "Edit");
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

        open_new_profile(&mut harness, "SSH");
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

        open_new_profile(&mut harness, "SFTP");
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
