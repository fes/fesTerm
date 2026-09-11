use std::{
    cmp::Ordering,
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering as AtomicOrdering},
        mpsc::{self, Receiver, Sender},
        Arc, Condvar, Mutex,
    },
    thread,
    time::{Duration, SystemTime},
};

use eframe::egui::{
    self, Align, Color32, FontFamily, FontId, Key, Layout, RichText, ScrollArea, Sense, TextEdit,
    Ui, WidgetInfo, WidgetType,
};
use festerm_config::{CredentialKind, SftpPaneOrderPreference};
use festerm_markdown::{MarkdownBounds, RemoteMarkdownSource, RemoteSourceOwner};
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
    chrome::ChipStatus,
    icon::{self, Icon},
    theme,
};

/// How often the browsing session's worker checks that the remote
/// connection is still alive, so a dropped connection (network blip, VPN
/// hiccup, the host machine coming back from sleep, etc.) is detected and
/// silently reconnected in the background rather than only surfacing as a
/// failure the next time the user tries to navigate or run a command.
const SFTP_LIVENESS_CHECK_INTERVAL: Duration = Duration::from_secs(20);

type LocalDirectoryLoadResult = Result<(SftpDirectorySnapshot, Option<SftpPathMetadata>), String>;

struct LocalDirectoryLoadRequest {
    path: SftpPath,
    complete: Box<dyn FnOnce(LocalDirectoryLoadResult) + Send>,
}

struct LocalDirectoryLoadShared {
    pending: Mutex<Option<LocalDirectoryLoadRequest>>,
    wake: Condvar,
    shutdown: AtomicBool,
}

/// One bounded loader per local browser. A request already being read may
/// finish, while repeated navigation coalesces to only the newest pending
/// path instead of spawning an OS thread for every click.
struct LocalDirectoryLoader {
    shared: Arc<LocalDirectoryLoadShared>,
}

impl LocalDirectoryLoader {
    fn new(thread_name: String) -> Self {
        let shared = Arc::new(LocalDirectoryLoadShared {
            pending: Mutex::new(None),
            wake: Condvar::new(),
            shutdown: AtomicBool::new(false),
        });
        let worker_shared = Arc::clone(&shared);
        thread::Builder::new()
            .name(thread_name)
            .spawn(move || loop {
                let request = {
                    let mut pending = worker_shared
                        .pending
                        .lock()
                        .expect("local directory loader lock is not poisoned");
                    while pending.is_none() && !worker_shared.shutdown.load(AtomicOrdering::Acquire)
                    {
                        pending = worker_shared
                            .wake
                            .wait(pending)
                            .expect("local directory loader lock is not poisoned");
                    }
                    if worker_shared.shutdown.load(AtomicOrdering::Acquire) {
                        return;
                    }
                    pending.take().expect("pending request was checked")
                };
                let result = local_snapshot_and_metadata(&request.path);
                (request.complete)(result);
            })
            .expect("could not spawn local directory loader thread");
        Self { shared }
    }

    fn schedule(&self, request: LocalDirectoryLoadRequest) {
        *self
            .shared
            .pending
            .lock()
            .expect("local directory loader lock is not poisoned") = Some(request);
        self.shared.wake.notify_one();
    }

    #[cfg(test)]
    fn paused_for_test() -> Self {
        Self {
            shared: Arc::new(LocalDirectoryLoadShared {
                pending: Mutex::new(None),
                wake: Condvar::new(),
                shutdown: AtomicBool::new(false),
            }),
        }
    }

    #[cfg(test)]
    fn pending_path_for_test(&self) -> Option<SftpPath> {
        self.shared
            .pending
            .lock()
            .expect("local directory loader lock is not poisoned")
            .as_ref()
            .map(|request| request.path.clone())
    }
}

impl Drop for LocalDirectoryLoader {
    fn drop(&mut self) {
        let guard = self
            .shared
            .pending
            .lock()
            .expect("local directory loader lock is not poisoned");
        self.shared.shutdown.store(true, AtomicOrdering::Release);
        drop(guard);
        self.shared.wake.notify_one();
    }
}

const SFTP_SECTION_GAP: f32 = 8.0;
const SFTP_PANE_OUTER_INSET: f32 = 4.0;
/// Width of every hairline rule fesTerm draws inside an SFTP pane: the pane's
/// own border and each internal divider.
///
/// This is load-bearing for layout, not just cosmetics. egui's
/// [`egui::Frame::total_margin`] adds `stroke.width` to a frame's margin, so a
/// bordered pane hands its children `pane_width - 2 * SFTP_HAIRLINE`. Sizing
/// those children to the pane's *outer* width instead is what previously
/// pushed the remote pane past the viewport, so every child width/height here
/// is derived from the constants below rather than sampled from
/// `ui.available_*` inside a frame.
const SFTP_HAIRLINE: f32 = 1.0;
const SFTP_PANE_INNER_PADDING: i8 = 0;
/// Radius of the pane/rail cards. The panes carry zero inner padding, so their
/// header and footer frames sit flush against this arc; both must repeat the
/// matching corners or their square fill paints over it and the card reads as
/// having its corner tips sliced off.
const SFTP_PANE_CORNER_RADIUS: u8 = 8;
const SFTP_PANE_HEADER_HEIGHT: f32 = 35.0;
const SFTP_PANE_TOOLBAR_HEIGHT: f32 = 39.0;
const SFTP_PANE_FILTER_ROW_HEIGHT: f32 = 37.0;
const SFTP_PANE_FOOTER_HEIGHT: f32 = 26.0;
const SFTP_TOOL_BUTTON_SIZE: f32 = 28.0;
const SFTP_BREADCRUMB_HEIGHT: f32 = 28.0;
const SFTP_FILTER_FIELD_HEIGHT: f32 = 26.0;
const SFTP_TABLE_HEADER_HEIGHT: f32 = 27.0;
const SFTP_TABLE_ROW_HEIGHT: f32 = 31.0;
const SFTP_TABLE_CELL_PADDING: f32 = 7.0;
/// Floors for the file table's metadata columns, in addition to the mockup's
/// 53/15/22/10 proportions. Wide enough for "4.0 KiB", "Yesterday" and
/// "Folder" plus each cell's 7px insets.
const SFTP_NAME_COLUMN_MIN_WIDTH: f32 = 120.0;
const SFTP_SIZE_COLUMN_MIN_WIDTH: f32 = 72.0;
const SFTP_MODIFIED_COLUMN_MIN_WIDTH: f32 = 104.0;
const SFTP_TYPE_COLUMN_MIN_WIDTH: f32 = 66.0;
/// Horizontal insets for the pane's fixed chrome rows, taken from the mockup:
/// `.fsftp-pane-head { padding: 0 11px }`, `.fsftp-toolbar { padding: 5px 7px }`
/// and `.fsftp-filterrow { padding: 5px 8px }`. Each row applies its value to
/// *both* edges; the right-hand inset used to be omitted, so the header's
/// status text and the filter field ran into the pane border.
///
/// The toolbar deliberately uses the filter row's 8px rather than the mockup's
/// 7px. The mockup can afford the 1px difference because its filter field is
/// followed by an overflow ("...") button, so the two fields never share an
/// edge. fesTerm has no such button, which leaves the breadcrumb and filter
/// fields stacked directly on top of each other -- where a 3px difference in
/// their right edges reads as a straightforward misalignment bug.
const SFTP_PANE_HEAD_PADDING: f32 = 11.0;
const SFTP_TOOLBAR_PADDING: f32 = 8.0;
const SFTP_FILTER_ROW_PADDING: f32 = 8.0;
/// Gap between the toolbar's navigation buttons and the breadcrumb field.
const SFTP_TOOLBAR_NAV_GAP: f32 = 2.0;
/// Mockup `.fsftp-pane-foot { padding: 0 9px; gap: 8px }`.
const SFTP_PANE_FOOTER_PADDING: f32 = 9.0;
const SFTP_PANE_FOOTER_GAP: f32 = 8.0;
/// Gap between a sortable column's label and its direction arrow.
const SFTP_SORT_INDICATOR_GAP: f32 = 4.0;
/// Fixed chrome above a pane's file table: header, divider, toolbar, divider,
/// filter row. The table gets whatever is left after this and the footer.
const SFTP_PANE_CHROME_HEIGHT: f32 = SFTP_PANE_HEADER_HEIGHT
    + SFTP_HAIRLINE
    + SFTP_PANE_TOOLBAR_HEIGHT
    + SFTP_HAIRLINE
    + SFTP_PANE_FILTER_ROW_HEIGHT;
/// The shortest a pane can render: its fixed chrome, the table's column
/// header, and the footer, with a zero-height listing between them.
const SFTP_PANE_MIN_HEIGHT: f32 = SFTP_HAIRLINE * 2.0
    + SFTP_PANE_CHROME_HEIGHT
    + SFTP_TABLE_HEADER_HEIGHT
    + SFTP_HAIRLINE
    + SFTP_PANE_FOOTER_HEIGHT;
const SFTP_TRANSFER_RAIL_WIDTH: f32 = 76.0;
const SFTP_TRANSFER_RAIL_PADDING: f32 = 8.0;
const SFTP_TRANSFER_RAIL_BUTTON_GAP: f32 = 12.0;
const SFTP_TRANSFER_BUTTON_WIDTH: f32 = 54.0;
const SFTP_TRANSFER_BUTTON_HEIGHT: f32 = 57.0;
/// Height of the stacked (narrow) layout's transfer bar. The two buttons sit
/// side by side there, so one button's height plus the rail's padding and
/// border is all it needs.
const SFTP_NARROW_RAIL_HEIGHT: f32 =
    SFTP_TRANSFER_BUTTON_HEIGHT + SFTP_TRANSFER_RAIL_PADDING * 2.0 + SFTP_HAIRLINE * 2.0;

/// Whether the transfer rail runs down the gap between two side-by-side panes
/// or across the bottom of a single stacked pane.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum TransferRailLayout {
    Vertical,
    Horizontal,
}
const SFTP_STATUS_DOT_SIZE: f32 = 7.0;
/// Left inset applied to the SFTP tab's toolbar heading/connection-state
/// text and the connection-error banner beneath it. Unlike the split-pane
/// body below (which deliberately has no `Frame` margin -- see
/// `split_view_min_width_matches_two_panes_and_rail`), this heading row
/// otherwise renders flush against the window's left edge since fesTerm's
/// tab content area has no ambient `CentralPanel` inner margin.
const SFTP_TOOLBAR_LEFT_PADDING: f32 = 12.0;
/// Breathing room around the "SFTP · <target>" heading, measured to the
/// *glyphs* rather than to the text row's box.
///
/// Two intrinsic offsets make a naive symmetric `add_space` look lopsided, and
/// they were the "padding on the bottom is noticeably larger than above it"
/// defect: the tab strip already leaves `SFTP_HEADING_INHERITED_TOP_GAP` above
/// the ascenders, and `ui.heading` reserves `SFTP_HEADING_TEXT_DESCENT` of
/// unused descent below the baseline. Subtracting each from the target gap is
/// what makes the heading sit optically centred in its band; before this the
/// gaps measured 5.8px above against 12.5px below.
const SFTP_HEADING_GAP: f32 = 9.0;
const SFTP_HEADING_INHERITED_TOP_GAP: f32 = 5.8;
const SFTP_HEADING_TEXT_DESCENT: f32 = 4.5;
/// Leading gap the tab body already places above its first widget (measured
/// at ppp 2.25 with no heading row present). Subtracted from the pane row's
/// top inset so the row is framed by `SFTP_PANE_OUTER_INSET` on all four
/// sides rather than being 1px tighter at the top than at the bottom.
const SFTP_TAB_BODY_LEADING_GAP: f32 = 3.1;
/// Minimum usable width for a single Local/Remote pane before its table
/// columns and breadcrumb start clipping. Matches the mockup's `.fsftp-pane`
/// intent (which itself imposes no hard floor, `min-width: 0`, relying on
/// its grid to distribute space) while keeping fesTerm's columns legible.
const SFTP_PANE_MIN_WIDTH: f32 = 280.0;
/// The narrowest content width at which two panes (each at their minimum
/// width), the transfer rail, and the intentional outer/inter-pane gaps can
/// be shown side by side.
/// Derived directly from [`SFTP_PANE_MIN_WIDTH`] and [`SFTP_TRANSFER_RAIL_WIDTH`]
/// so the "switch to single-pane" breakpoint always stays consistent with
/// the actual space the split-pane layout needs, rather than an arbitrary
/// cutoff that could leave the split view unreachable at common window
/// sizes (see issue #121).
const SFTP_SPLIT_VIEW_MIN_WIDTH: f32 = SFTP_PANE_MIN_WIDTH * 2.0
    + SFTP_TRANSFER_RAIL_WIDTH
    + SFTP_SECTION_GAP * 2.0
    + SFTP_PANE_OUTER_INSET * 2.0;

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
    /// Destination fields are only meaningful (and only shown as editable)
    /// for an ad-hoc, not-saved-profile destination -- see
    /// `show_authentication_required`. Initialized once from `target` the
    /// first time this state is created, then left alone so the user's
    /// edits survive across frames.
    destination_initialized: bool,
    username: String,
    host: String,
    port: String,
    /// One-shot request to focus the credential field for the current
    /// [`AuthMode`], armed whenever this screen is entered (including a
    /// return trip through "Edit connection…" after a failure) and whenever
    /// the mode changes. Arriving here always means "the connection needs a
    /// secret", so the secret box -- not the first widget in tab order -- is
    /// what should be ready to type into. It has to be one-shot: calling
    /// `request_focus` every frame would trap focus and break Tab.
    focus_secret: bool,
    /// Pass number of the most recent frame this screen rendered on, used to
    /// tell "still here" from "just came back". A gap means the tab showed
    /// something else in between (the connecting spinner, a failure notice,
    /// the host-key prompt), which counts as a fresh arrival.
    last_rendered_pass: Option<u64>,
}

/// Parses the (possibly user-edited) destination fields back into a
/// [`SftpFileManagerLaunchTarget`]. A saved-profile target's destination
/// isn't editable here (see `show_authentication_required`), so it's passed
/// through unchanged; only an ad-hoc destination's fields are actually
/// read from `state`.
fn resolve_edited_target(
    target: &SftpFileManagerLaunchTarget,
    state: &AuthenticationFormState,
) -> Result<SftpFileManagerLaunchTarget, String> {
    if target.profile_id.is_some() {
        return Ok(target.clone());
    }
    let username = state.username.trim();
    if username.is_empty() {
        return Err("Enter a username.".to_owned());
    }
    let host = state.host.trim();
    if host.is_empty() {
        return Err("Enter a host.".to_owned());
    }
    let port = state
        .port
        .trim()
        .parse::<u16>()
        .map_err(|_| "Port must be a number between 1 and 65535.".to_owned())?;
    // The host or port may have just changed (that's the whole point of
    // "Edit connection…"), so any previously-known host-key trust no
    // longer necessarily applies; only keep it when the destination is
    // unchanged from what it was originally.
    let known_host_persisted =
        target.known_host_persisted && host == target.host && port == target.port;
    Ok(SftpFileManagerLaunchTarget {
        label: format!("{username}@{host}"),
        username: username.to_owned(),
        host: host.to_owned(),
        port,
        profile_id: None,
        stored_credential_kind: None,
        known_host_persisted,
    })
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
    if !state.destination_initialized {
        state.username = target.username.clone();
        state.host = target.host.clone();
        state.port = target.port.to_string();
        state.destination_initialized = true;
    }
    let pass = ui.ctx().cumulative_pass_nr();
    // Consecutive passes mean the user never left; anything else means this
    // screen was just (re)entered.
    if state.last_rendered_pass != Some(pass.saturating_sub(1)) {
        state.focus_secret = true;
    }
    state.last_rendered_pass = Some(pass);
    let mut command = None;
    egui::Frame::new()
        .inner_margin(egui::Margin::same(16))
        .show(ui, |ui| {
            ui.vertical(|ui| {
                ui.add_space(24.0);
                ui.heading("Open GUI SFTP");
                if target.profile_id.is_some() {
                    ui.label(format!(
                        "Destination: {}@{}:{}",
                        target.username, target.host, target.port
                    ));
                } else {
                    // Editable, unlike a saved profile's destination: this
                    // is the surface a failed connection's "Edit
                    // connection…" action sends the user back to, so a
                    // typo'd host/port/username can be fixed here instead
                    // of only ever being able to retry the exact same
                    // (possibly wrong) destination.
                    ui.label("Destination");
                    ui.horizontal(|ui| {
                        ui.label("Username");
                        ui.add(
                            TextEdit::singleline(&mut state.username).desired_width(120.0),
                        );
                        ui.label("Host");
                        ui.add(TextEdit::singleline(&mut state.host).desired_width(160.0));
                        ui.label("Port");
                        ui.add(TextEdit::singleline(&mut state.port).desired_width(60.0));
                    });
                }
                if !target.known_host_persisted {
                    ui.label(
                        "If this host is new or its key changed, fesTerm will pause for host-key verification before opening the file manager.",
                    );
                }
                ui.add_space(10.0);
                let previous_mode = state.mode;
                ui.horizontal(|ui| {
                    ui.radio_value(&mut state.mode, AuthMode::Password, "Password");
                    ui.radio_value(&mut state.mode, AuthMode::PrivateKey, "Private key");
                });
                if state.mode != previous_mode {
                    state.focus_secret = true;
                }
                ui.add_space(6.0);
                // Read-and-clear: the flag is consumed by whichever field
                // this frame's mode renders, so the other mode's field
                // doesn't inherit a stale request when the user switches.
                let focus_secret = std::mem::take(&mut state.focus_secret);
                let mut enter_pressed = false;
                match state.mode {
                    AuthMode::Password => {
                        ui.label("Password");
                        let response =
                            ui.add(TextEdit::singleline(&mut state.password).password(true));
                        if focus_secret {
                            response.request_focus();
                        }
                        enter_pressed |= response.lost_focus()
                            && ui.input(|input| input.key_pressed(egui::Key::Enter));
                    }
                    AuthMode::PrivateKey => {
                        ui.label("OpenSSH private key");
                        // The key box, not the passphrase, is the field the
                        // user still has to fill in this mode.
                        let key_response =
                            ui.add(TextEdit::multiline(&mut state.private_key).desired_rows(8));
                        if focus_secret {
                            key_response.request_focus();
                        }
                        ui.label("Passphrase (optional)");
                        let response =
                            ui.add(TextEdit::singleline(&mut state.passphrase).password(true));
                        enter_pressed |= response.lost_focus()
                            && ui.input(|input| input.key_pressed(egui::Key::Enter));
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
                if connect.clicked() || enter_pressed {
                    let resolved_target = match resolve_edited_target(target, &state) {
                        Ok(resolved_target) => Some(resolved_target),
                        Err(feedback) => {
                            state.feedback = Some(feedback);
                            None
                        }
                    };
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
                    if let (Some(target), Some(authentication)) =
                        (resolved_target, authentication)
                    {
                        state.feedback = None;
                        command = Some(crate::tabs::AppCommand::StartSftpFileManager {
                            target,
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

/// Tracks an in-flight `WorkerCommand::ReadMarkdownSnapshot` so a stale
/// response (from a since-abandoned double-click) can be told apart from
/// the request the user is actually still waiting on -- see
/// `WorkerEvent::MarkdownSnapshotLoaded`/`Failed`.
struct PendingMarkdownRequest {
    request_id: u64,
    #[allow(dead_code)]
    path: String,
}

/// A fetched remote Markdown snapshot waiting to be turned into an
/// `AppCommand::OpenRemoteMarkdownSnapshot` on the next `show()` call --
/// see `SftpFileManagerTab::show`.
struct PendingMarkdownOpen {
    source: RemoteMarkdownSource,
    display_path: String,
    content: Vec<u8>,
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

/// Which recovery action (if any) the user requested from
/// [`show_connection_status_banner_ui`]. Kept as a separate, pure-`Ui`
/// function (rather than a method taking `&mut SftpFileManagerTab`
/// directly) so the banner's rendering/labels stay unit-testable without
/// needing a live worker/tab -- see `failed_connection_banner_*` tests.
#[derive(Default)]
struct ConnectionStatusBannerAction {
    retry: bool,
    edit_connection: bool,
}

fn show_connection_status_banner_ui(
    ui: &mut Ui,
    connection_state: &SftpConnectionState,
) -> ConnectionStatusBannerAction {
    let mut action = ConnectionStatusBannerAction::default();
    if let SftpConnectionState::Failed { summary, details }
    | SftpConnectionState::Disconnected { summary, details } = connection_state
    {
        let is_failed = matches!(connection_state, SftpConnectionState::Failed { .. });
        ui.horizontal(|ui| {
            ui.add_space(SFTP_TOOLBAR_LEFT_PADDING);
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    ui.colored_label(theme::STATUS_ERROR, summary);
                    ui.add_space(8.0);
                    if ui.small_button("Retry").clicked() {
                        action.retry = true;
                    }
                    if is_failed {
                        ui.add_space(4.0);
                        if ui.small_button("Edit connection…").clicked() {
                            action.edit_connection = true;
                        }
                    }
                });
                if !details.is_empty() {
                    ui.label(RichText::new(details).small().color(theme::TEXT_MUTED));
                }
            });
        });
    }
    action
}

/// Shown in place of the local/remote browser panes before the very first
/// successful connect (see `SftpFileManagerTab::has_connected_once`).
/// There's nothing meaningful to browse yet, so we avoid flashing empty or
/// stale-looking panes while connecting or after an initial connect
/// failure -- the toolbar/banner above already show status and, on
/// failure, the Retry/Edit connection… actions.
fn show_pre_connect_placeholder(ui: &mut Ui, connection_state: &SftpConnectionState) {
    let message = match connection_state {
        SftpConnectionState::Connecting => "Connecting…",
        SftpConnectionState::AwaitingHostKey => "Waiting for host key trust decision…",
        SftpConnectionState::Ready => return,
        SftpConnectionState::Failed { .. } => "Connection failed.",
        SftpConnectionState::Disconnected { .. } => "Disconnected.",
    };
    ui.vertical_centered(|ui| {
        ui.add_space(ui.available_height() / 3.0);
        if matches!(connection_state, SftpConnectionState::Connecting) {
            ui.spinner();
            ui.add_space(8.0);
        }
        ui.label(RichText::new(message).color(theme::TEXT_MUTED));
    });
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

/// egui `DragAndDrop` payload for an in-progress pane-to-pane drag (issue
/// #137). Carries only the source pane: the actual items transferred are
/// read from that pane's *current* selection when the drop lands, the same
/// selection `queue_transfer` already uses for the toolbar/rail buttons, so
/// there is no separate snapshot to keep in sync with selection changes
/// during the drag.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SftpPaneDragPayload {
    source: PaneFocus,
}

/// Whether a pane-to-pane drag payload that originated in `source` and was
/// released while hovering `dropped_on` should enqueue a transfer. Extracted
/// as its own pure function -- rather than inlined as a `!=` comparison at
/// the drop site -- so a regression that flips the comparison (re-queueing
/// a same-pane drop as a no-op, or treating a same-pane drop as a transfer)
/// is caught directly by a unit test instead of only by a full render pass.
fn pane_drop_should_transfer(source: PaneFocus, dropped_on: PaneFocus) -> bool {
    source != dropped_on
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
            previous_valid_path: path.clone(),
            snapshot: None,
            directory_metadata: None,
            filter: String::new(),
            sort: SftpSortState::default(),
            selected_paths: BTreeSet::new(),
            selected_anchor: None,
            cursor_path: None,
            // Seed the back/forward history with the pane's starting
            // directory. Without this, the very first navigation away
            // (via a breadcrumb click, opening a folder, or "up") pushes
            // that destination in as the *only* history entry, leaving
            // history_index at 0 with nothing before it - so "Back"
            // silently does nothing even though the user just navigated
            // away from a real previous location.
            history: vec![path.clone()],
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
    /// Whether the bottom status bar is currently on screen. It carries this
    /// tab's endpoints and transport state, so the heading above the panes
    /// only renders when the bar is hidden -- see `show_toolbar`.
    status_bar_visible: bool,
    pub(crate) focused_pane: PaneFocus,
    pub(crate) narrow_focus: PaneFocus,
    pub(crate) connection_state: SftpConnectionState,
    /// Whether the remote session has ever reached `Ready` at least once.
    /// The local/remote browser panes are only rendered once this is true
    /// -- before the first successful connect, there's nothing meaningful
    /// to browse yet, so we show a connecting/failed status placeholder
    /// instead (see `show`). Once true, it stays true even across a later
    /// drop/reconnect, since the previously-loaded panes remain valid to
    /// show (optionally overlaid with the disconnected banner).
    has_connected_once: bool,
    pub(crate) transfer_drawer: TransferDrawerState,
    pub(crate) collision_dialog: Option<SftpCollisionDialogState>,
    command_sender: tokio::sync::mpsc::UnboundedSender<WorkerCommand>,
    event_receiver: Receiver<WorkerEvent>,
    event_sender: Sender<WorkerEvent>,
    local_loader: LocalDirectoryLoader,
    repaint: egui::Context,
    next_local_request_id: u64,
    pending_host_key: Option<PendingHostKeyDecision>,
    /// The host-key fingerprint accepted for this session, cached from
    /// `WorkerEvent::Connected` so a remote Markdown open (issue #133) can
    /// pin its `RemoteMarkdownSource` to the exact verified origin without
    /// re-deriving it.
    verified_host_key_fingerprint: Option<String>,
    next_markdown_request_id: u64,
    pending_markdown_request: Option<PendingMarkdownRequest>,
    pending_markdown_open: Option<PendingMarkdownOpen>,
    /// A dismissible operation failure that does not invalidate the browsing
    /// connection, such as a Markdown fetch or bounded transfer-queue
    /// rejection.
    operation_error: Option<(String, String)>,
    /// Set synchronously by `open_item` when a *local* Markdown file is
    /// double-clicked (no worker roundtrip needed); consumed by `show`'s
    /// tail, same as `pending_markdown_open` for the remote case.
    pending_markdown_command: Option<crate::tabs::AppCommand>,
    /// Screen rects each pane occupied the last time it actually rendered
    /// (split mode renders both every frame; narrow mode renders only
    /// `narrow_focus`'s pane and explicitly clears its sibling's rect --
    /// see `show_narrow`). Used only to resolve an external OS file drop's
    /// target pane (issue #137's Finder-to-remote drag-in):
    /// `handle_dropped_files` runs before this tab's body renders for the
    /// frame the drop event lands in, so it has to consult wherever the
    /// panes were drawn last time, not this frame's (not-yet-known) layout.
    pub(crate) last_local_pane_rect: Option<egui::Rect>,
    pub(crate) last_remote_pane_rect: Option<egui::Rect>,
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
        let local_loader =
            LocalDirectoryLoader::new(format!("festerm-gui-sftp-local-{}", target.label));
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
            status_bar_visible: true,
            focused_pane: PaneFocus::Local,
            narrow_focus: PaneFocus::Local,
            connection_state: SftpConnectionState::Connecting,
            has_connected_once: false,
            transfer_drawer: TransferDrawerState::default(),
            collision_dialog: None,
            command_sender,
            event_receiver,
            event_sender,
            local_loader,
            repaint: context.clone(),
            next_local_request_id: 1,
            pending_host_key: None,
            verified_host_key_fingerprint: None,
            next_markdown_request_id: 1,
            pending_markdown_request: None,
            pending_markdown_open: None,
            operation_error: None,
            pending_markdown_command: None,
            last_local_pane_rect: None,
            last_remote_pane_rect: None,
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

    pub(crate) fn set_status_bar_visible(&mut self, status_bar_visible: bool) {
        self.status_bar_visible = status_bar_visible;
    }

    /// The two endpoints this tab bridges, for the bottom status bar. Mirrors
    /// the mockup's `SFTP · Local + ops@prod-03.example` status line rather
    /// than restating the heading: the heading names the remote target, this
    /// names the pair the panes actually show.
    pub(crate) fn status_bar_endpoints(&self) -> String {
        format!("Local + {}", self.label)
    }

    /// Transport state for the bottom status bar's dot, using the same
    /// vocabulary as a session chip so SFTP and terminal tabs read alike.
    pub(crate) fn chip_status(&self) -> ChipStatus {
        match self.connection_state {
            SftpConnectionState::Ready => ChipStatus::Connected,
            SftpConnectionState::Connecting => ChipStatus::Starting,
            SftpConnectionState::AwaitingHostKey => ChipStatus::AuthRequired,
            SftpConnectionState::Disconnected { .. } => ChipStatus::Disconnected,
            SftpConnectionState::Failed { .. } => ChipStatus::Failed,
        }
    }

    pub(crate) fn status_bar_label(&self) -> &'static str {
        match self.connection_state {
            SftpConnectionState::Ready => "Connected",
            SftpConnectionState::Connecting => "Connecting",
            SftpConnectionState::AwaitingHostKey => "Trust required",
            SftpConnectionState::Disconnected { .. } => "Disconnected",
            SftpConnectionState::Failed { .. } => "Failed",
        }
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
        let narrow = ui.available_width() < SFTP_SPLIT_VIEW_MIN_WIDTH;
        let toolbar_command = self.show_toolbar(ui, narrow, tab_id);
        if let Some(pending) = self.pending_host_key.as_ref() {
            return show_host_key_prompt(ui, tab_id, &self.launch_target, &pending.prompt)
                .or(toolbar_command);
        }
        if !self.has_connected_once {
            // Nothing has ever been browsed yet, so don't show empty/stale
            // local+remote panes underneath the "Connecting…"/"Connection
            // failed" status -- the banner above already carries the
            // Retry/Edit connection… actions.
            show_pre_connect_placeholder(ui, &self.connection_state);
            return toolbar_command;
        }
        if narrow {
            self.show_narrow(ui);
        } else {
            let available_width = ui.available_width();
            // The narrow/split decision above and this allocation share the
            // same width budget. Do not re-apply a minimum here: a child
            // wider than the remaining budget pushes the remote pane beyond
            // the window instead of allowing the responsive narrow layout.
            let pane_width = (available_width
                - SFTP_TRANSFER_RAIL_WIDTH
                - SFTP_SECTION_GAP * 2.0
                - SFTP_PANE_OUTER_INSET * 2.0)
                / 2.0;
            // `show_transfer_drawer` below renders a variable-height block
            // only when transfers are queued/running, so its height isn't
            // known until *after* it's drawn. Reserve space for it up
            // front using its height from the previous frame (egui's
            // standard two-pass-via-cache trick) so the fixed-height pane
            // row doesn't claim the drawer's space and push it past the
            // window into the status bar.
            let drawer_height_id = ui.id().with("sftp_transfer_drawer_height");
            let reserved_drawer_height =
                ui.data(|data| data.get_temp::<f32>(drawer_height_id).unwrap_or(0.0));
            // The available UI is already bounded above the status panel.
            // Respect that bound rather than forcing a 260px minimum that
            // can overlap the status bar in a short window. Reserve the same
            // inset used on the left and right so the pane row sits in a
            // uniform frame instead of butting up against the status bar.
            let pane_height =
                (ui.available_height() - reserved_drawer_height - SFTP_PANE_OUTER_INSET).max(0.0);
            ui.horizontal_top(|ui| {
                // Keep the intended, measured gutters explicit and disable
                // egui's implicit spacing so it cannot silently expand the
                // width budget.
                ui.style_mut().spacing.item_spacing.x = 0.0;
                ui.add_space(SFTP_PANE_OUTER_INSET);
                match self.pane_order {
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
                            |ui| {
                                self.show_transfer_rail(
                                    ui,
                                    pane_height,
                                    TransferRailLayout::Vertical,
                                )
                            },
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
                            |ui| {
                                self.show_transfer_rail(
                                    ui,
                                    pane_height,
                                    TransferRailLayout::Vertical,
                                )
                            },
                        );
                        ui.add_space(SFTP_SECTION_GAP);
                        ui.allocate_ui_with_layout(
                            egui::vec2(pane_width, pane_height),
                            Layout::top_down(Align::Min),
                            |ui| self.show_pane(ui, PaneFocus::Local),
                        );
                    }
                }
            });
        }
        self.show_transfer_drawer(ui);
        self.show_collision_dialog(ui.ctx());
        self.take_pending_markdown_command().or(toolbar_command)
    }

    /// Consumes whichever pending Markdown-open state (issue #133) is set
    /// for this frame -- the synchronous local case from `open_item`, or
    /// the asynchronous remote case filled by `apply_event` earlier in this
    /// same `poll()` call -- and turns it into the `AppCommand` that opens
    /// (or refreshes) the corresponding viewer tab.
    fn take_pending_markdown_command(&mut self) -> Option<crate::tabs::AppCommand> {
        if let Some(command) = self.pending_markdown_command.take() {
            return Some(command);
        }
        let pending = self.pending_markdown_open.take()?;
        Some(crate::tabs::AppCommand::OpenRemoteMarkdownSnapshot {
            source: pending.source,
            display_path: pending.display_path,
            content: pending.content,
        })
    }

    fn show_toolbar(
        &mut self,
        ui: &mut Ui,
        narrow: bool,
        tab_id: crate::tabs::TabId,
    ) -> Option<crate::tabs::AppCommand> {
        // Relocate, don't duplicate. When the bottom status bar is on screen
        // it already names this tab's endpoints and transport state, and the
        // tab chip names the target too, so repeating "SFTP · <target>
        // Connected" here put the same identity on screen three times and the
        // state twice -- and cost the file lists a whole band of height the
        // reference mockup doesn't spend (it has no heading above the panes,
        // only the `.fsftp-statusline` footer). The heading returns when the
        // bar is hidden (focus mode, or View ▸ Status bar off) so the state
        // never simply disappears. This mirrors how the session `detail`
        // relocates into the bar only while chips are compact.
        let show_heading = !self.status_bar_visible;
        if show_heading || narrow {
            ui.add_space(SFTP_HEADING_GAP - SFTP_HEADING_INHERITED_TOP_GAP);
            ui.horizontal(|ui| {
                ui.add_space(SFTP_TOOLBAR_LEFT_PADDING);
                if show_heading {
                    self.show_heading(ui);
                }
                if narrow {
                    if show_heading {
                        ui.add_space(16.0);
                    }
                    ui.selectable_value(&mut self.narrow_focus, PaneFocus::Local, "Local");
                    ui.selectable_value(&mut self.narrow_focus, PaneFocus::Remote, "Remote");
                }
            });
            // A `selectable_value` is its own visual box, so it needs no
            // descent compensation; `ui.heading` does.
            ui.add_space(if show_heading {
                SFTP_HEADING_GAP - SFTP_HEADING_TEXT_DESCENT
            } else {
                SFTP_HEADING_GAP
            });
        } else {
            // No heading row: the tab body's own leading gap already supplies
            // most of the pane row's top inset.
            ui.add_space((SFTP_PANE_OUTER_INSET - SFTP_TAB_BODY_LEADING_GAP).max(0.0));
        }
        self.show_operation_error_banner(ui);
        self.show_connection_status_banner(ui, tab_id)
    }

    /// Shown for a failed operation that leaves the SFTP connection usable.
    fn show_operation_error_banner(&mut self, ui: &mut Ui) {
        let Some((summary, details)) = self.operation_error.clone() else {
            return;
        };
        let mut dismiss = false;
        ui.horizontal(|ui| {
            ui.add_space(SFTP_TOOLBAR_LEFT_PADDING);
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    ui.colored_label(theme::STATUS_ERROR, &summary);
                    ui.add_space(8.0);
                    if ui.small_button("Dismiss").clicked() {
                        dismiss = true;
                    }
                });
                if !details.is_empty() {
                    ui.label(RichText::new(&details).small().color(theme::TEXT_MUTED));
                }
            });
        });
        if dismiss {
            self.operation_error = None;
        }
    }

    fn show_heading(&self, ui: &mut Ui) {
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
                SftpConnectionState::Failed { .. } | SftpConnectionState::Disconnected { .. } => {
                    theme::STATUS_ERROR
                }
            }),
        );
    }

    /// Renders the "Connection failed"/"Disconnected" banner along with its
    /// recovery actions:
    /// - **Retry** re-attempts the connection with the same destination and
    ///   credentials -- useful for a transient failure (network blip,
    ///   momentarily unreachable host) once the underlying problem has
    ///   cleared. Works for both a totally-failed initial connect and a
    ///   later drop, since `run_worker` keeps listening for
    ///   `WorkerCommand::Reconnect` even after the first connect attempt
    ///   fails.
    /// - **Edit connection…** (only offered for an initial connect failure,
    ///   since that's the only case where the destination itself might be
    ///   wrong, e.g. a typo'd host/port) sends the tab back to the
    ///   pre-connect authentication screen with the destination fields
    ///   editable, instead of only ever being able to retry the exact same
    ///   (possibly wrong) destination.
    fn show_connection_status_banner(
        &mut self,
        ui: &mut Ui,
        tab_id: crate::tabs::TabId,
    ) -> Option<crate::tabs::AppCommand> {
        let action = show_connection_status_banner_ui(ui, &self.connection_state);
        if action.retry {
            self.connection_state = SftpConnectionState::Connecting;
            self.remote_pane.loading = true;
            let _ = self.command_sender.send(WorkerCommand::Reconnect);
        }
        if action.edit_connection {
            return Some(crate::tabs::AppCommand::RetrySftpFileManagerConnection {
                tab_id,
                target: self.launch_target.clone(),
            });
        }
        None
    }

    fn show_narrow(&mut self, ui: &mut Ui) {
        // Only `self.narrow_focus`'s pane renders below, so its sibling's
        // cached rect (see `last_local_pane_rect`/`last_remote_pane_rect`)
        // would otherwise keep pointing at wherever split mode last drew it
        // -- stale enough that an external file drop landing inside that
        // leftover rect could be misrouted to a pane that isn't even on
        // screen. Invalidate it up front so a drop is only ever matched
        // against a pane actually visible as of the most recent render.
        match self.narrow_focus {
            PaneFocus::Local => self.last_remote_pane_rect = None,
            PaneFocus::Remote => self.last_local_pane_rect = None,
        }
        // The pane used to be handed `ui.available_height()` -- i.e. all of
        // it -- which left the transfer rail below it nothing to occupy, so
        // the rail rendered past the bottom of the window. Budget the rail
        // first and give the pane only what is left.
        //
        // `min_inner_size` lets the window get shorter than the pane's own
        // fixed chrome plus the rail, and a pane cannot shrink below that, so
        // the leftovers scroll instead of spilling over the window edge.
        let available_height = ui.available_height();
        let width = ui.available_width();
        let rail_height = SFTP_NARROW_RAIL_HEIGHT;
        let pane_height =
            (available_height - rail_height - SFTP_SECTION_GAP).max(SFTP_PANE_MIN_HEIGHT);
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .max_height(available_height)
            .show(ui, |ui| {
                ui.allocate_ui_with_layout(
                    egui::vec2(width, pane_height),
                    Layout::top_down(Align::Min),
                    |ui| self.show_pane(ui, self.narrow_focus),
                );
                ui.add_space(SFTP_SECTION_GAP);
                ui.allocate_ui_with_layout(
                    egui::vec2(width, rail_height),
                    Layout::top_down(Align::Min),
                    |ui| {
                        self.show_transfer_rail(ui, rail_height, TransferRailLayout::Horizontal);
                    },
                );
            });
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
        // `allocate_ui_with_layout` gives this pane an exact column in split
        // mode. Everything below is sized from that column *minus this frame's
        // own border*, because egui folds `Frame::stroke.width` into the
        // frame's margin: children sized to the outer width are 2px too wide
        // and cascade into the transfer rail and the neighbouring pane.
        let outer_width = ui.available_width();
        let outer_height = ui.available_height();
        let pane_width = (outer_width - SFTP_HAIRLINE * 2.0).max(0.0);
        let pane_height = (outer_height - SFTP_HAIRLINE * 2.0).max(0.0);
        // Header, toolbar, filter, and footer are fixed-height pane chrome.
        // The directory listing is the only flexible region, and must never
        // grow past this budget into the application status bar. `list_height`
        // is the ScrollArea's viewport specifically, so it also excludes the
        // table's own column header and the divider beneath it.
        let table_height =
            (pane_height - SFTP_PANE_CHROME_HEIGHT - SFTP_PANE_FOOTER_HEIGHT).max(0.0);
        let available_list_height =
            (table_height - SFTP_TABLE_HEADER_HEIGHT - SFTP_HAIRLINE).max(0.0);
        // Snap the viewport down to a whole number of rows. Otherwise the
        // bottom of the list lands mid-row and the footer's border slices the
        // last entry's text in half, which reads as the listing spilling into
        // the chrome below it. The rounded-off remainder stays as pane
        // background between the list and the footer.
        let list_height =
            (available_list_height / SFTP_TABLE_ROW_HEIGHT).floor() * SFTP_TABLE_ROW_HEIGHT;
        let list_bottom_slack = available_list_height - list_height;
        let frame = egui::Frame::new()
            .fill(theme::SURFACE_WINDOW)
            // The mockup uses the same subtle border for every pane
            // regardless of focus (`.fsftp-pane { border-left: 1px solid
            // var(--fsftp-border); }`); highlighting the focused pane with
            // an accent border made LOCAL/REMOTE look inconsistently
            // outlined depending on which pane last had focus, so both
            // panes now always use the subtle border.
            .stroke(egui::Stroke::new(SFTP_HAIRLINE, theme::BORDER_SUBTLE))
            .corner_radius(egui::CornerRadius::same(SFTP_PANE_CORNER_RADIUS))
            .inner_margin(egui::Margin::same(SFTP_PANE_INNER_PADDING));
        let pane_frame_response = frame
            .show(ui, |ui| {
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
                        .stroke(egui::Stroke::NONE)
                        // Repeat the pane's top corners; see
                        // `SFTP_PANE_CORNER_RADIUS`.
                        .corner_radius(egui::CornerRadius {
                            nw: SFTP_PANE_CORNER_RADIUS,
                            ne: SFTP_PANE_CORNER_RADIUS,
                            sw: 0,
                            se: 0,
                        })
                        .inner_margin(egui::Margin::ZERO)
                        .show(ui, |ui| {
                            pane_chrome_row(ui, pane_width, SFTP_PANE_HEADER_HEIGHT, |ui| {
                                ui.add_space(SFTP_PANE_HEAD_PADDING);
                                let (icon_rect, _) = ui.allocate_exact_size(
                                    egui::vec2(16.0, 16.0),
                                    egui::Sense::hover(),
                                );
                                paint_sftp_glyph(
                                    ui.painter(),
                                    match focus {
                                        PaneFocus::Local => SftpGlyph::LocalPane,
                                        PaneFocus::Remote => SftpGlyph::RemotePane,
                                    },
                                    icon_rect,
                                    if focus == PaneFocus::Remote {
                                        theme::ACCENT_PRIMARY
                                    } else {
                                        theme::TEXT_SECONDARY
                                    },
                                );
                                ui.add_space(7.0);
                                ui.label(
                                    RichText::new(focus.label().to_ascii_uppercase())
                                        .font(font_for_text_role(SftpTextRole::PaneLabel))
                                        .strong(),
                                );
                                // The pane's `item_spacing` is zeroed so the fixed
                                // row heights stay predictable, so the gap that the
                                // mockup shows around the "LOCAL · This computer"
                                // separator has to be added explicitly. Without it
                                // the label ran together as "LOCAL· This computer".
                                ui.add_space(7.0);
                                ui.label(
                                    RichText::new(match focus {
                                        PaneFocus::Local => "· This computer".to_owned(),
                                        PaneFocus::Remote => remote_identity.clone(),
                                    })
                                    .font(font_for_text_role(SftpTextRole::PaneMeta))
                                    .color(theme::TEXT_SECONDARY),
                                );
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    // Mockup `.fsftp-pane-head { padding: 0 11px }`
                                    // -- the right inset has to be added by hand
                                    // because this reversed layout starts at the
                                    // row's right edge.
                                    ui.add_space(SFTP_PANE_HEAD_PADDING);
                                    if let Some((state, color)) = remote_state {
                                        if reconnect_visible
                                            && ui.small_button("Reconnect").clicked()
                                        {
                                            request_reconnect = true;
                                        }
                                        ui.label(
                                            RichText::new(state)
                                                .font(font_for_text_role(SftpTextRole::PaneMeta))
                                                .color(theme::TEXT_SECONDARY),
                                        );
                                        let (dot_rect, _) = ui.allocate_exact_size(
                                            egui::vec2(SFTP_STATUS_DOT_SIZE, SFTP_STATUS_DOT_SIZE),
                                            Sense::hover(),
                                        );
                                        ui.painter().circle_filled(
                                            dot_rect.center(),
                                            SFTP_STATUS_DOT_SIZE / 2.0,
                                            color,
                                        );
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
                        });
                    pane_divider(ui, pane_width);
                    ui.add_enabled_ui(interactions_enabled, |ui| {
                        egui::Frame::new()
                            .fill(theme::SURFACE_WINDOW)
                            .stroke(egui::Stroke::NONE)
                            .corner_radius(0.0)
                            .inner_margin(egui::Margin::ZERO)
                            .show(ui, |ui| {
                                pane_chrome_row(ui, pane_width, SFTP_PANE_TOOLBAR_HEIGHT, |ui| {
                                    ui.add_space(SFTP_TOOLBAR_PADDING);
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
                                    ui.add_space(SFTP_TOOLBAR_NAV_GAP);
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
                                        // Pre-register a click-sensing background BEFORE the
                                        // breadcrumb buttons are painted. egui resolves overlapping
                                        // same-layer widgets by picking whichever was registered
                                        // last, so if this background sense were registered *after*
                                        // the buttons (as it previously was, via
                                        // `bar.response.interact(Sense::click())` following the
                                        // Frame::show call), it would shadow every button
                                        // underneath and permanently break single-click
                                        // breadcrumb navigation. Registering it first lets the
                                        // buttons (added afterwards) win the hit test, while empty
                                        // space in the bar still supports double-click-to-edit.
                                        // The gap between the nav buttons and the
                                        // breadcrumb has to be subtracted too, or
                                        // the bar overshoots the toolbar's right
                                        // inset and stops lining up with the filter
                                        // field directly beneath it.
                                        let breadcrumb_width = (pane_width
                                            - SFTP_TOOL_BUTTON_SIZE * 4.0
                                            - SFTP_TOOLBAR_NAV_GAP
                                            - SFTP_TOOLBAR_PADDING * 2.0)
                                            .max(0.0);
                                        let bar_rect = egui::Rect::from_min_size(
                                            ui.cursor().min,
                                            egui::vec2(breadcrumb_width, SFTP_BREADCRUMB_HEIGHT),
                                        );
                                        let bar_bg_id =
                                            ui.make_persistent_id((focus, "breadcrumb-bg"));
                                        let bar_bg_response =
                                            ui.interact(bar_rect, bar_bg_id, Sense::click());
                                        let bar = egui::Frame::new()
                                            .fill(theme::SURFACE_TAB_INACTIVE)
                                            .stroke(egui::Stroke::new(
                                                SFTP_HAIRLINE,
                                                theme::BORDER_SUBTLE,
                                            ))
                                            .corner_radius(5.0)
                                            .inner_margin(egui::Margin::symmetric(7, 0))
                                            .show(ui, |ui| {
                                                // 7px inner margin per side plus
                                                // this frame's own border, both of
                                                // which egui counts as margin.
                                                let content_width =
                                                    (breadcrumb_width - 14.0 - SFTP_HAIRLINE * 2.0)
                                                        .max(0.0);
                                                ui.set_min_width(content_width);
                                                ui.set_max_width(content_width);
                                                ui.set_min_height(SFTP_BREADCRUMB_HEIGHT);
                                                ui.horizontal_wrapped(|ui| {
                                                    let mut previous_label_was_root_slash = false;
                                                    for (index, segment) in
                                                        breadcrumb_segments(&pane.current_path)
                                                            .into_iter()
                                                            .enumerate()
                                                    {
                                                        // The root segment's own label is already
                                                        // "/" (see `breadcrumb_segments`), so adding
                                                        // another "/" separator right after it would
                                                        // render as "//" before the next segment
                                                        // (e.g. "//config" instead of "/config").
                                                        if index > 0
                                                            && !previous_label_was_root_slash
                                                        {
                                                            ui.label(
                                                                RichText::new("/")
                                                                    .font(font_for_text_role(
                                                                        SftpTextRole::Breadcrumb,
                                                                    ))
                                                                    .color(theme::TEXT_MUTED),
                                                            );
                                                        }
                                                        previous_label_was_root_slash =
                                                            segment.label == "/";
                                                        let text =
                                                            RichText::new(segment.label.clone())
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
                                                                    .min_size(egui::vec2(
                                                                        0.0, 18.0,
                                                                    )),
                                                            )
                                                            .clicked()
                                                        {
                                                            request_breadcrumb = Some(segment.path);
                                                        }
                                                    }
                                                });
                                            });
                                        let _ = bar.response;
                                        bar_bg_response.widget_info(|| {
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
                                        if bar_bg_response.double_clicked() {
                                            pane.editing_path = true;
                                            pane.path_focus_requested = true;
                                        }
                                    }
                                });
                            });
                        pane_divider(ui, pane_width);
                        egui::Frame::new()
                            .fill(theme::SURFACE_WINDOW)
                            .stroke(egui::Stroke::NONE)
                            .corner_radius(0.0)
                            .inner_margin(egui::Margin::ZERO)
                            .show(ui, |ui| {
                                let mut filter_text = pane.filter.clone();
                                let filter_response = pane_chrome_row(
                                    ui,
                                    pane_width,
                                    SFTP_PANE_FILTER_ROW_HEIGHT,
                                    |ui| {
                                        ui.add_space(SFTP_FILTER_ROW_PADDING);
                                        show_filter_field(
                                            ui,
                                            &mut filter_text,
                                            focus,
                                            (pane_width - SFTP_FILTER_ROW_PADDING * 2.0).max(0.0),
                                        )
                                    },
                                );
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
                        // No border here. The pane's own frame already outlines
                        // this region, so a second stroke painted a doubled
                        // hairline down the pane's left/right edges *and* stole
                        // 2px from the table's width budget (egui folds
                        // `stroke.width` into `Frame::total_margin`).
                        .stroke(egui::Stroke::NONE)
                        .corner_radius(0.0)
                        .inner_margin(egui::Margin::ZERO)
                        .show(ui, |ui| {
                            ui.set_min_width(pane_width);
                            ui.set_max_width(pane_width);
                            let columns = sftp_table_columns(pane_width);
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
                            pane_divider(ui, pane_width);
                            ScrollArea::vertical()
                                .id_salt(("sftp-pane", focus))
                                .max_height(list_height)
                                // Claim the whole listing viewport even when the
                                // directory is short, so the footer stays pinned
                                // to the pane's bottom edge instead of floating
                                // up under a half-empty table.
                                .auto_shrink([false, false])
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
                                                    .font(font_for_text_role(
                                                        SftpTextRole::TableBody,
                                                    ))
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
                                            // The selection outline is painted
                                            // *inside* the row rect below rather
                                            // than set as a `Frame` stroke: a
                                            // stroke (even a transparent one)
                                            // widens every row by 2px, which made
                                            // the listing wider than its pane and
                                            // forced a horizontal scrollbar.
                                            .stroke(egui::Stroke::NONE)
                                            .corner_radius(0.0)
                                            .inner_margin(egui::Margin::symmetric(0, 0))
                                            .show(ui, |ui| {
                                                ui.set_min_height(SFTP_TABLE_ROW_HEIGHT);
                                                ui.set_max_height(SFTP_TABLE_ROW_HEIGHT);
                                                ui.horizontal(|ui| {
                                                    ui.allocate_ui_with_layout(
                                                        egui::vec2(
                                                            columns[0],
                                                            SFTP_TABLE_ROW_HEIGHT,
                                                        ),
                                                        Layout::left_to_right(Align::Center),
                                                        |ui| {
                                                            // See show_table_text_cell: force the
                                                            // full column width so the following
                                                            // Size/Modified/Type cells line up under
                                                            // their headers instead of collapsing
                                                            // against the (usually much shorter) name.
                                                            ui.set_min_width(columns[0]);
                                                            ui.set_max_width(
                                                                (columns[0]
                                                                    - SFTP_TABLE_CELL_PADDING)
                                                                    .max(0.0),
                                                            );
                                                            ui.add_space(SFTP_TABLE_CELL_PADDING);
                                                            // See `show_filter_field`: paint the
                                                            // icon through an allocated rect (not
                                                            // directly at `ui.cursor().min`) so this
                                                            // row's `Align::Center` layout actually
                                                            // vertically centers it against the name
                                                            // label beside it.
                                                            let (icon_rect, _) = ui
                                                                .allocate_exact_size(
                                                                    egui::vec2(15.0, 15.0),
                                                                    egui::Sense::hover(),
                                                                );
                                                            paint_sftp_glyph(
                                                                ui.painter(),
                                                                item_glyph(&item),
                                                                icon_rect,
                                                                if selected {
                                                                    theme::TEXT_PRIMARY
                                                                } else {
                                                                    theme::TEXT_SECONDARY
                                                                },
                                                            );
                                                            ui.add_space(5.0);
                                                            ui.add(
                                                                egui::Label::new(
                                                                    RichText::new(&item.name)
                                                                        .font(font_for_text_role(
                                                                            SftpTextRole::TableBody,
                                                                        ))
                                                                        .color(theme::TEXT_PRIMARY),
                                                                )
                                                                .truncate(),
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
                                        let response =
                                            row.response.interact(Sense::click_and_drag());
                                        if interactions_enabled {
                                            // Dragging an item that isn't part of
                                            // the current selection starts a
                                            // fresh single-item drag (matching
                                            // Finder/Explorer), rather than
                                            // silently dragging whatever was
                                            // selected before.
                                            if response.drag_started() && !selected {
                                                pane.select_single(&item.path);
                                                pane.cursor_path = Some(key.clone());
                                            }
                                            response.dnd_set_drag_payload(SftpPaneDragPayload {
                                                source: focus,
                                            });
                                        }
                                        // Reveal in Finder/Explorer (issue #137's
                                        // follow-up comment): local-pane-only,
                                        // matches Finder/Explorer's own
                                        // single-item-or-first-of-selection
                                        // semantics rather than acting on every
                                        // selected item at once.
                                        if interactions_enabled && focus == PaneFocus::Local {
                                            let reveal_target = if selected
                                                && pane.selected_paths.len() > 1
                                            {
                                                table_entries
                                                    .iter()
                                                    .find(|candidate| {
                                                        pane.selected_paths
                                                            .contains(&path_key(&candidate.path))
                                                    })
                                                    .map(|candidate| candidate.path.clone())
                                                    .unwrap_or_else(|| item.path.clone())
                                            } else {
                                                item.path.clone()
                                            };
                                            response.context_menu(|ui| {
                                                if ui
                                                    .button(reveal_in_file_manager_label())
                                                    .clicked()
                                                {
                                                    if let SftpPath::Local(path) = &reveal_target {
                                                        if let Err(error) =
                                                            reveal_in_file_manager(path)
                                                        {
                                                            pane.set_error(
                                                                "Couldn't reveal the item."
                                                                    .to_owned(),
                                                                error,
                                                            );
                                                        }
                                                    }
                                                    ui.close();
                                                }
                                            });
                                        }
                                        // Mockup `.fsftp-table td { border-bottom:
                                        // 1px solid rgba(53,65,78,.46) }` -- the
                                        // row rules the mockup uses to keep long
                                        // listings scannable.
                                        ui.painter().hline(
                                            response.rect.x_range(),
                                            response.rect.bottom() - 0.5,
                                            egui::Stroke::new(
                                                SFTP_HAIRLINE,
                                                theme::BORDER_SUBTLE.gamma_multiply(0.46),
                                            ),
                                        );
                                        if selected && pane_focused {
                                            ui.painter().rect_stroke(
                                                response.rect,
                                                0.0,
                                                egui::Stroke::new(
                                                    SFTP_HAIRLINE,
                                                    theme::ACCENT_PRIMARY,
                                                ),
                                                egui::StrokeKind::Inside,
                                            );
                                        }
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
                            // ScrollArea only claims the height its content
                            // needs. Reserve its remaining viewport explicitly
                            // so the footer is anchored to the pane bottom,
                            // instead of floating beneath a short listing.
                            // ScrollArea now claims its full viewport via
                            // `auto_shrink([false, false])`, so no residual space
                            // needs reserving here.
                        });
                    ui.add_space(list_bottom_slack);
                    // A full bordered "chip" here (as previously drawn with
                    // `.stroke(...)` on all sides plus rounded corners) reads as
                    // an unrelated boxed-in outline sitting awkwardly below the
                    // file list. The mockup instead uses a flat `border-top`
                    // divider, so match that: no corner radius/side borders, and
                    // a single hairline painted along the top edge only.
                    let footer_response = egui::Frame::new()
                        .fill(theme::SURFACE_WINDOW)
                        .stroke(egui::Stroke::NONE)
                        // Repeat the pane's bottom corners; see
                        // `SFTP_PANE_CORNER_RADIUS`.
                        .corner_radius(egui::CornerRadius {
                            nw: 0,
                            ne: 0,
                            sw: SFTP_PANE_CORNER_RADIUS,
                            se: SFTP_PANE_CORNER_RADIUS,
                        })
                        .inner_margin(egui::Margin::ZERO)
                        .show(ui, |ui| {
                            pane_chrome_row(ui, pane_width, SFTP_PANE_FOOTER_HEIGHT, |ui| {
                                ui.add_space(SFTP_PANE_FOOTER_PADDING);
                                ui.label(
                                    RichText::new(format!("{footer_items} items"))
                                        .font(font_for_text_role(SftpTextRole::Footer))
                                        .color(theme::TEXT_MUTED),
                                );
                                // Mockup `.fsftp-pane-foot { gap: 8px }`. The
                                // pane zeroes `item_spacing`, so without these the
                                // footer ran together as "35 items\u{b7}0 selected".
                                ui.add_space(SFTP_PANE_FOOTER_GAP);
                                ui.label(
                                    RichText::new("·")
                                        .font(font_for_text_role(SftpTextRole::Footer))
                                        .color(theme::TEXT_MUTED),
                                );
                                ui.add_space(SFTP_PANE_FOOTER_GAP);
                                ui.label(
                                    RichText::new(footer_selection)
                                        .font(font_for_text_role(SftpTextRole::Footer))
                                        .color(theme::TEXT_MUTED),
                                );
                            });
                        })
                        .response;
                    ui.painter().hline(
                        footer_response.rect.x_range(),
                        footer_response.rect.top(),
                        egui::Stroke::new(1.0, theme::BORDER_SUBTLE),
                    );
                });
            })
            .response;
        // A pane-to-pane drag (issue #137) lands here: any item drag started
        // in `focus`'s row loop above sets an `SftpPaneDragPayload` via
        // `dnd_set_drag_payload`; releasing it anywhere inside the *other*
        // pane's frame reads it back via `dnd_release_payload` (which
        // already gates on `contains_pointer()` and the mouse actually
        // having been released this frame) and reuses `queue_transfer`'s
        // existing selection-based transfer pipeline -- the same one the
        // toolbar/rail buttons use -- so a drag enqueues exactly what a
        // click on those buttons would have.
        if let Some(payload) = pane_frame_response.dnd_release_payload::<SftpPaneDragPayload>() {
            if pane_drop_should_transfer(payload.source, focus) {
                self.queue_transfer(payload.source);
            }
        }
        match focus {
            PaneFocus::Local => self.last_local_pane_rect = Some(pane_frame_response.rect),
            PaneFocus::Remote => self.last_remote_pane_rect = Some(pane_frame_response.rect),
        }
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

    fn show_transfer_rail(&mut self, ui: &mut Ui, rail_height: f32, layout: TransferRailLayout) {
        // In split mode this is rendered inside an explicitly allocated
        // `SFTP_TRANSFER_RAIL_WIDTH` column. `vertical_centered` can
        // otherwise inherit the parent window's unconstrained width and
        // cause its Frame to grow, pushing the remote pane off-screen.
        let rail_width = match layout {
            TransferRailLayout::Vertical => SFTP_TRANSFER_RAIL_WIDTH,
            TransferRailLayout::Horizontal => ui.available_width(),
        };
        ui.set_min_width(rail_width);
        ui.set_max_width(rail_width);
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
        // Mockup `.fsftp-transferrail` is a *full-height* column with
        // `justify-content: center`, so the two buttons sit as a centred
        // group. The previous fixed 24px top pad left visibly more space
        // above the buttons than below them.
        let rail_padding = SFTP_TRANSFER_RAIL_PADDING;
        let inner_width = (rail_width - rail_padding * 2.0 - SFTP_HAIRLINE * 2.0).max(0.0);
        let content_height = match layout {
            TransferRailLayout::Vertical => {
                SFTP_TRANSFER_BUTTON_HEIGHT * 2.0 + SFTP_TRANSFER_RAIL_BUTTON_GAP
            }
            TransferRailLayout::Horizontal => SFTP_TRANSFER_BUTTON_HEIGHT,
        };
        let content_width = SFTP_TRANSFER_BUTTON_WIDTH * 2.0 + SFTP_TRANSFER_RAIL_BUTTON_GAP;
        let inner_height =
            (rail_height - rail_padding * 2.0 - SFTP_HAIRLINE * 2.0).max(content_height);
        egui::Frame::new()
            .fill(theme::SURFACE_TERMINAL)
            .stroke(egui::Stroke::new(SFTP_HAIRLINE, theme::BORDER_SUBTLE))
            .corner_radius(egui::CornerRadius::same(SFTP_PANE_CORNER_RADIUS))
            .inner_margin(egui::Margin::same(rail_padding as i8))
            .show(ui, |ui| {
                // The 8px inner margin per side *plus* this frame's own
                // border, which egui counts as margin too. Without the
                // border term the rail rendered 78px wide inside its 76px
                // column and nudged both panes outward.
                ui.set_min_width(inner_width);
                ui.set_max_width(inner_width);
                ui.set_min_height(inner_height);
                let mut add_buttons = |ui: &mut Ui, gap: f32| {
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
                    ui.add_space(gap);
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
                };
                match layout {
                    TransferRailLayout::Vertical => {
                        ui.add_space(((inner_height - content_height) / 2.0).max(0.0));
                        ui.vertical_centered(|ui| {
                            add_buttons(ui, SFTP_TRANSFER_RAIL_BUTTON_GAP);
                        });
                    }
                    // Stacked (narrow) mode puts the rail under the pane as a
                    // short bar, so the buttons run across it instead of down
                    // it and are centred horizontally as a group.
                    TransferRailLayout::Horizontal => {
                        ui.horizontal(|ui| {
                            ui.add_space(((inner_width - content_width) / 2.0).max(0.0));
                            add_buttons(ui, SFTP_TRANSFER_RAIL_BUTTON_GAP);
                        });
                    }
                }
            });
    }

    fn show_transfer_drawer(&mut self, ui: &mut Ui) {
        let drawer_height_id = ui.id().with("sftp_transfer_drawer_height");
        if !self.transfer_drawer.has_work() {
            ui.data_mut(|data| data.remove::<f32>(drawer_height_id));
            return;
        }
        let top = ui.cursor().top();
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
        let height = ui.cursor().top() - top;
        ui.data_mut(|data| data.insert_temp(drawer_height_id, height));
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
                load_path(self, focus, SftpPath::local(local_home_directory()), true);
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
            return;
        }
        if !is_markdown_file(item) {
            return;
        }
        match &item.path {
            SftpPath::Local(path) => {
                self.pending_markdown_command =
                    Some(crate::tabs::AppCommand::OpenLocalMarkdownFile {
                        path: path.clone(),
                        replacing: None,
                    });
            }
            SftpPath::Remote(path) => {
                self.request_remote_markdown_snapshot(path.clone());
            }
        }
    }

    /// Kicks off fetching a remote Markdown file's full bytes so it can be
    /// opened in the Markdown viewer (issue #133). The result comes back
    /// asynchronously as `WorkerEvent::MarkdownSnapshotLoaded`/`Failed`,
    /// consumed by `apply_event` and then surfaced as an `AppCommand` from
    /// `show`'s tail.
    fn request_remote_markdown_snapshot(&mut self, path: String) {
        let request_id = self.next_markdown_request_id;
        self.next_markdown_request_id += 1;
        self.operation_error = None;
        self.pending_markdown_request = Some(PendingMarkdownRequest {
            request_id,
            path: path.clone(),
        });
        let _ = self
            .command_sender
            .send(WorkerCommand::ReadMarkdownSnapshot {
                request_id,
                path,
                max_bytes: MarkdownBounds::default().max_source_bytes(),
            });
    }

    /// Builds the `RemoteMarkdownSource` identity pinning a freshly fetched
    /// snapshot to this tab's verified connection. Returns `None` if the
    /// host-key fingerprint hasn't been cached yet (shouldn't happen in
    /// practice -- a Markdown double-click can only reach a connected
    /// remote pane -- but this keeps the identity's validated invariants
    /// honest rather than fabricating a fingerprint).
    fn remote_markdown_identity(&self, remote_path: &str) -> Option<RemoteMarkdownSource> {
        let fingerprint = self.verified_host_key_fingerprint.as_deref()?;
        build_remote_markdown_source(&self.launch_target, fingerprint, remote_path)
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

    /// Enqueue an OS-native file drop (issue #137's Finder/Explorer →
    /// remote pane drag-in) as uploads into the remote pane's current
    /// directory. Unlike `queue_transfer`, the source items never touch a
    /// pane's own selection state -- they come from outside the app
    /// entirely -- so this only reuses `transfer_action`'s enable/reject
    /// rules (connection readiness, destination writability) rather than
    /// its selection check.
    pub(crate) fn enqueue_external_drop_upload(
        &mut self,
        paths: Vec<PathBuf>,
    ) -> Result<usize, String> {
        if !matches!(self.connection_state, SftpConnectionState::Ready) {
            return Err("The remote SFTP connection is unavailable.".to_owned());
        }
        if !self.remote_pane.is_writable() {
            return Err("The remote destination is read-only.".to_owned());
        }
        let destination_path = self.remote_pane.current_path.clone();
        let requests = paths
            .into_iter()
            .filter_map(|path| {
                SftpTransferRequest::new(SftpPath::local(path), destination_path.clone()).ok()
            })
            .collect::<Vec<_>>();
        let count = requests.len();
        if !requests.is_empty() {
            let _ = self.command_sender.send(WorkerCommand::Enqueue(requests));
        }
        Ok(count)
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
                verified_host_key_fingerprint,
            } => {
                self.connection_state = SftpConnectionState::Ready;
                self.has_connected_once = true;
                self.pending_host_key = None;
                self.verified_host_key_fingerprint = Some(verified_host_key_fingerprint);
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
            WorkerEvent::RemoteDirectoryLoaded {
                snapshot,
                metadata,
                verified_host_key_fingerprint,
            } => {
                self.connection_state = SftpConnectionState::Ready;
                self.pending_host_key = None;
                self.verified_host_key_fingerprint = Some(verified_host_key_fingerprint);
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
            WorkerEvent::MarkdownSnapshotLoaded {
                request_id,
                path,
                content,
                verified_host_key_fingerprint,
            } => {
                let Some(pending) = self.pending_markdown_request.as_ref() else {
                    return;
                };
                if pending.request_id != request_id {
                    return;
                }
                self.pending_markdown_request = None;
                self.verified_host_key_fingerprint = Some(verified_host_key_fingerprint);
                match self.remote_markdown_identity(&path) {
                    Some(source) => {
                        self.pending_markdown_open = Some(PendingMarkdownOpen {
                            source,
                            display_path: path,
                            content,
                        });
                    }
                    None => {
                        self.operation_error = Some((
                            "Could not open this file in the Markdown viewer.".to_owned(),
                            "The remote connection's verified identity is not available yet."
                                .to_owned(),
                        ));
                    }
                }
            }
            WorkerEvent::MarkdownSnapshotFailed {
                request_id,
                summary,
                details,
            } => {
                let Some(pending) = self.pending_markdown_request.as_ref() else {
                    return;
                };
                if pending.request_id != request_id {
                    return;
                }
                self.pending_markdown_request = None;
                self.operation_error = Some((summary, details));
            }
            WorkerEvent::TransferCommandFailed { action, details } => {
                self.operation_error = Some((format!("Could not {action}."), details));
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
    LoadRemote {
        path: String,
    },
    /// Fetches a remote file's full bytes for the Markdown viewer
    /// (issue #133), bounded by `max_bytes` so a huge file can't be pulled
    /// into memory whole. `request_id` lets the tab discard a stale
    /// response if the user double-clicked a different file before this
    /// one came back.
    ReadMarkdownSnapshot {
        request_id: u64,
        path: String,
        max_bytes: usize,
    },
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
        /// The host-key fingerprint accepted for this session, cached so a
        /// later remote Markdown open (issue #133) can pin its
        /// `RemoteMarkdownSource` identity to the exact origin the user
        /// verified, without re-deriving or re-prompting for it.
        verified_host_key_fingerprint: String,
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
        /// The fingerprint currently accepted for this session. A
        /// reconnect (transparent or explicit) can legitimately update
        /// this if the host key rotated and the user approved the new
        /// key, so every load carries the current value rather than
        /// relying solely on the one-time `Connected` event (see #135).
        verified_host_key_fingerprint: String,
    },
    RemoteDirectoryFailed {
        summary: String,
        details: String,
    },
    ConnectionFailed {
        summary: String,
        details: String,
    },
    MarkdownSnapshotLoaded {
        request_id: u64,
        path: String,
        content: Vec<u8>,
        /// See `RemoteDirectoryLoaded::verified_host_key_fingerprint`;
        /// kept current across reconnects for the same reason (#135).
        verified_host_key_fingerprint: String,
    },
    MarkdownSnapshotFailed {
        request_id: u64,
        summary: String,
        details: String,
    },
    TransferCommandFailed {
        action: &'static str,
        details: String,
    },
    Transfer(SftpTransferEvent),
}

/// Waits for the next `WorkerCommand`, ignoring anything except
/// `Reconnect` (nothing else is meaningful before the very first connect
/// attempt has ever succeeded -- there's no session yet to load a
/// directory into or enqueue a transfer on). Returns `true` once a retry is
/// requested, or `false` if the command channel closed (the tab was
/// dropped/closed), so the caller can give up instead of waiting forever.
async fn wait_for_initial_connect_retry(
    command_receiver: &mut tokio::sync::mpsc::UnboundedReceiver<WorkerCommand>,
) -> bool {
    loop {
        match command_receiver.recv().await {
            Some(WorkerCommand::Reconnect) => return true,
            Some(_) => continue,
            None => return false,
        }
    }
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
    // Retry the initial connect (both the browsing session and the
    // dedicated transfer session) in place, instead of giving up and
    // letting the whole worker task -- and with it the command channel --
    // end the moment the first attempt fails. This is what lets a
    // `WorkerCommand::Reconnect` (sent when the user clicks "Retry" on the
    // connection-failed banner) recover from a failed first attempt (a
    // momentarily unreachable host, a typo the user just fixed via "Edit
    // connection", etc.) without needing to spin up a brand new worker/tab.
    let (mut browsing, transfer_session) = loop {
        let browsing = match connect_remote_session(
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
                session
            }
            Err(error) => {
                let _ = event_sender.send(WorkerEvent::ConnectionFailed {
                    summary: "Could not connect the SFTP file manager.".to_owned(),
                    details: error,
                });
                repaint.request_repaint();
                if !wait_for_initial_connect_retry(&mut command_receiver).await {
                    return;
                }
                continue;
            }
        };
        let transfer_session = match connect_remote_session(
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
                session
            }
            Err(error) => {
                let _ = event_sender.send(WorkerEvent::ConnectionFailed {
                    summary: "Could not start the SFTP transfer worker.".to_owned(),
                    details: error,
                });
                repaint.request_repaint();
                if !wait_for_initial_connect_retry(&mut command_receiver).await {
                    return;
                }
                continue;
            }
        };
        break (browsing, transfer_session);
    };
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
            verified_host_key_fingerprint: session_known_host_fingerprint
                .clone()
                .unwrap_or_default(),
        });
        repaint.request_repaint();
    }

    // Fires periodically so a connection that silently died (dropped
    // network, VPN hiccup, or the whole machine coming back from sleep) is
    // noticed and reconnected in the background, instead of only being
    // discovered the next time the user happens to navigate or run a
    // command. `Delay` (rather than the default `Burst`) means a long gap
    // -- e.g. the laptop sleeping for hours -- produces one immediate tick
    // on wake instead of a burst of queued-up ticks firing back to back.
    let mut liveness_interval = tokio::time::interval(SFTP_LIVENESS_CHECK_INTERVAL);
    liveness_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    liveness_interval.reset();

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
                                    verified_host_key_fingerprint: session_known_host_fingerprint
                                        .clone()
                                        .unwrap_or_default(),
                                });
                            }
                            Err(error) => {
                                // The session may have died silently (e.g.
                                // the machine slept, or the network blipped)
                                // since the last successful operation.
                                // Transparently try one reconnect-and-retry
                                // before bothering the user with a failure:
                                // if the underlying connection really is
                                // gone, this recovers it without the user
                                // having to click "Reconnect" themselves; if
                                // the error was something else entirely
                                // (e.g. the path really doesn't exist), the
                                // retry fails the same way and the original
                                // failure is still reported below.
                                match reconnect_browsing_session(
                                    &target,
                                    &authentication,
                                    &mut session_known_host_fingerprint,
                                    &event_sender,
                                    &repaint,
                                )
                                .await
                                {
                                    Ok(session) => {
                                        browsing = session;
                                        liveness_interval.reset();
                                        match browsing.remote_directory_snapshot(Some(&path)).await {
                                            Ok(snapshot) => {
                                                current_remote = path;
                                                let metadata = browsing.remote_path_metadata(&current_remote).await.ok().flatten();
                                                let _ = event_sender.send(WorkerEvent::RemoteDirectoryLoaded {
                                                    snapshot,
                                                    metadata,
                                                    verified_host_key_fingerprint: session_known_host_fingerprint
                                                        .clone()
                                                        .unwrap_or_default(),
                                                });
                                            }
                                            Err(retry_error) => {
                                                let _ = event_sender.send(WorkerEvent::RemoteDirectoryFailed {
                                                    summary: "Could not load the remote folder.".to_owned(),
                                                    details: retry_error.to_string(),
                                                });
                                            }
                                        }
                                    }
                                    Err(_) => {
                                        let _ = event_sender.send(WorkerEvent::RemoteDirectoryFailed {
                                            summary: "Could not load the remote folder.".to_owned(),
                                            details: error.to_string(),
                                        });
                                    }
                                }
                            }
                        }
                        repaint.request_repaint();
                    }
                    WorkerCommand::ReadMarkdownSnapshot { request_id, path, max_bytes } => {
                        match browsing.read_markdown_snapshot(&path, max_bytes).await {
                            Ok(content) => {
                                let _ = event_sender.send(WorkerEvent::MarkdownSnapshotLoaded {
                                    request_id,
                                    path,
                                    content,
                                    verified_host_key_fingerprint: session_known_host_fingerprint
                                        .clone()
                                        .unwrap_or_default(),
                                });
                            }
                            Err(error) => {
                                // Same resilience pattern as `LoadRemote`
                                // above: the session may have silently died
                                // since the last operation, so try one
                                // reconnect-and-retry before reporting a
                                // failure to the user.
                                match reconnect_browsing_session(
                                    &target,
                                    &authentication,
                                    &mut session_known_host_fingerprint,
                                    &event_sender,
                                    &repaint,
                                )
                                .await
                                {
                                    Ok(session) => {
                                        browsing = session;
                                        liveness_interval.reset();
                                        match browsing.read_markdown_snapshot(&path, max_bytes).await {
                                            Ok(content) => {
                                                let _ = event_sender.send(WorkerEvent::MarkdownSnapshotLoaded {
                                                    request_id,
                                                    path,
                                                    content,
                                                    verified_host_key_fingerprint: session_known_host_fingerprint
                                                        .clone()
                                                        .unwrap_or_default(),
                                                });
                                            }
                                            Err(retry_error) => {
                                                let _ = event_sender.send(WorkerEvent::MarkdownSnapshotFailed {
                                                    request_id,
                                                    summary: "Could not open this file in the Markdown viewer.".to_owned(),
                                                    details: retry_error.to_string(),
                                                });
                                            }
                                        }
                                    }
                                    Err(_) => {
                                        let _ = event_sender.send(WorkerEvent::MarkdownSnapshotFailed {
                                            request_id,
                                            summary: "Could not open this file in the Markdown viewer.".to_owned(),
                                            details: error.to_string(),
                                        });
                                    }
                                }
                            }
                        }
                        repaint.request_repaint();
                    }
                    WorkerCommand::Enqueue(requests) => {
                        if let Err(error) = transfer_manager.enqueue_batch(requests) {
                            let _ = event_sender.send(WorkerEvent::TransferCommandFailed {
                                action: "queue the transfer",
                                details: error.to_string(),
                            });
                            repaint.request_repaint();
                        }
                    }
                    WorkerCommand::CancelTransfer(transfer_id) => {
                        if let Err(error) = transfer_manager.cancel_transfer(transfer_id) {
                            let _ = event_sender.send(WorkerEvent::TransferCommandFailed {
                                action: "cancel the transfer",
                                details: error.to_string(),
                            });
                            repaint.request_repaint();
                        }
                    }
                    WorkerCommand::ResolveCollision(resolution) => {
                        if let Err(error) = transfer_manager.resolve_collision(resolution) {
                            let _ = event_sender.send(WorkerEvent::TransferCommandFailed {
                                action: "resolve the transfer conflict",
                                details: error.to_string(),
                            });
                            repaint.request_repaint();
                        }
                    }
                    WorkerCommand::Reconnect => {
                        match reconnect_browsing_session(
                            &target,
                            &authentication,
                            &mut session_known_host_fingerprint,
                            &event_sender,
                            &repaint,
                        )
                        .await
                        {
                            Ok(session) => {
                                browsing = session;
                                liveness_interval.reset();
                                match browsing.remote_directory_snapshot(Some(&current_remote)).await {
                                    Ok(snapshot) => {
                                        let metadata = browsing.remote_path_metadata(&current_remote).await.ok().flatten();
                                        let _ = event_sender.send(WorkerEvent::RemoteDirectoryLoaded {
                                            snapshot,
                                            metadata,
                                            verified_host_key_fingerprint: session_known_host_fingerprint
                                                .clone()
                                                .unwrap_or_default(),
                                        });
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
            _ = liveness_interval.tick() => {
                if browsing.remote_path_metadata(&current_remote).await.is_err() {
                    // Reconnect silently in the background: this path is
                    // meant to catch problems (dropped network, sleep/wake)
                    // before the user notices, not to interrupt them with a
                    // failure banner every 20 seconds while offline. If the
                    // reconnect itself fails, just leave `browsing` as-is
                    // and try again on the next tick or the next time the
                    // user issues a command.
                    if let Ok(session) = reconnect_browsing_session(
                        &target,
                        &authentication,
                        &mut session_known_host_fingerprint,
                        &event_sender,
                        &repaint,
                    )
                    .await
                    {
                        browsing = session;
                        if let Ok(snapshot) = browsing.remote_directory_snapshot(Some(&current_remote)).await {
                            let metadata = browsing.remote_path_metadata(&current_remote).await.ok().flatten();
                            let _ = event_sender.send(WorkerEvent::RemoteDirectoryLoaded {
                                snapshot,
                                metadata,
                                verified_host_key_fingerprint: session_known_host_fingerprint
                                    .clone()
                                    .unwrap_or_default(),
                            });
                            repaint.request_repaint();
                        }
                    }
                }
            }
        }
    }
}

/// Reconnects the long-lived browsing session used for directory listings
/// and metadata lookups, updating the accepted host-key fingerprint (if the
/// host key changed since the last connect) so subsequent reconnects don't
/// re-prompt unnecessarily.
async fn reconnect_browsing_session(
    target: &SftpFileManagerLaunchTarget,
    authentication: &SftpFileManagerAuthentication,
    known_host_fingerprint: &mut Option<String>,
    event_sender: &mpsc::Sender<WorkerEvent>,
    repaint: &egui::Context,
) -> Result<festerm_ssh::SftpSession, String> {
    let (session, accepted_fingerprint) = connect_remote_session(
        target,
        authentication,
        known_host_fingerprint.as_deref(),
        event_sender,
        repaint,
    )
    .await?;
    if let Some(fingerprint) = accepted_fingerprint {
        *known_host_fingerprint = Some(fingerprint);
    }
    Ok(session)
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
        GuiSftpSessionConnectError::ConnectionFailed(detail) => {
            format!("The SSH/SFTP connection could not be established: {detail}")
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
                    GuiSftpSessionConnectError::ConnectionFailed(detail) => {
                        format!("The SSH/SFTP connection could not be established: {detail}")
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
    // Build the button as an explicit icon-above-text vertical stack instead
    // of an `egui::Button` with a manually painted icon overlaid on top:
    // `egui::Button` centers its (two-line) label across the *entire*
    // button height, so a fixed-position icon painted near the top ended up
    // drawn directly over the vertically-centered label text.
    let size = egui::vec2(SFTP_TRANSFER_BUTTON_WIDTH, SFTP_TRANSFER_BUTTON_HEIGHT);
    ui.add_enabled_ui(enabled, |ui| {
        let (rect, response) = ui.allocate_exact_size(size, Sense::click());
        let is_hovered = response.hovered() && enabled;
        let fill = if enabled {
            if is_hovered {
                theme::SURFACE_TAB_ACTIVE.gamma_multiply(1.08)
            } else {
                theme::SURFACE_TAB_ACTIVE
            }
        } else {
            ready_fill
        };
        let stroke_color = if enabled {
            theme::ACCENT_PRIMARY
        } else {
            theme::BORDER_SUBTLE
        };
        let text_color = if enabled {
            theme::TEXT_PRIMARY
        } else {
            theme::TEXT_SECONDARY
        };
        ui.painter().rect(
            rect,
            7.0,
            fill,
            egui::Stroke::new(1.0, stroke_color),
            egui::StrokeKind::Inside,
        );
        let icon_size = egui::vec2(16.0, 16.0);
        let icon_rect =
            egui::Rect::from_center_size(egui::pos2(rect.center().x, rect.top() + 16.0), icon_size);
        paint_sftp_glyph(ui.painter(), glyph, icon_rect, text_color);
        let text_pos = egui::pos2(rect.center().x, icon_rect.bottom() + 4.0);
        ui.painter().text(
            text_pos,
            egui::Align2::CENTER_TOP,
            label,
            font_for_text_role(SftpTextRole::TransferButton),
            text_color,
        );
        response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, accessible_label));
        response
    })
    .inner
}

/// Draws a full-width hairline rule of exactly [`SFTP_HAIRLINE`] height.
///
/// `ui.separator()` is deliberately avoided inside SFTP panes: it claims
/// `Spacing::item_spacing` plus its own 6px extent, so the pane's fixed height
/// budget could not be computed up front and the file table overflowed into
/// the application status bar.
fn pane_divider(ui: &mut Ui, width: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, SFTP_HAIRLINE), egui::Sense::hover());
    ui.painter().hline(
        rect.x_range(),
        rect.center().y,
        egui::Stroke::new(SFTP_HAIRLINE, theme::BORDER_SUBTLE),
    );
}

/// Lays out one of a pane's fixed-height chrome rows (header, toolbar, filter)
/// with its contents vertically centred inside the row box.
///
/// `ui.horizontal` inside a frame with `set_min_height` does *not* do this:
/// the horizontal row only ever knows its own natural height, so it centres
/// children against each other and then the frame grows underneath them. That
/// left every chrome row's text flush with its top edge and dumped all of the
/// slack below it -- the "not vertically centered / more padding below than
/// above" defect. Allocating the row's exact rect up front and running the
/// contents in a child `Ui` whose `max_rect` *is* that rect gives
/// `Align::Center` the full row height to centre against.
fn pane_chrome_row<R>(
    ui: &mut Ui,
    width: f32,
    height: f32,
    add_contents: impl FnOnce(&mut Ui) -> R,
) -> R {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
    let mut row = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(Layout::left_to_right(Align::Center)),
    );
    add_contents(&mut row)
}

fn show_filter_field(
    ui: &mut Ui,
    filter_text: &mut String,
    focus: PaneFocus,
    outer_width: f32,
) -> egui::Response {
    let frame = egui::Frame::new()
        .fill(theme::SURFACE_TERMINAL)
        .stroke(egui::Stroke::new(SFTP_HAIRLINE, theme::BORDER_SUBTLE))
        .corner_radius(5.0)
        .inner_margin(egui::Margin::symmetric(8, 0));
    let inner = frame.show(ui, |ui| {
        // The field is sized explicitly rather than via
        // `desired_width(INFINITY)`: letting the inner `TextEdit` expand made
        // it swallow the filter row's right-hand inset, so its right edge sat
        // ~5px past the breadcrumb field directly above it.
        let content_width = (outer_width - 16.0 - SFTP_HAIRLINE * 2.0).max(0.0);
        ui.set_min_width(content_width);
        ui.set_max_width(content_width);
        ui.set_min_height(SFTP_FILTER_FIELD_HEIGHT);
        ui.set_max_height(SFTP_FILTER_FIELD_HEIGHT);
        ui.horizontal(|ui| {
            // `ui.horizontal` vertically centers each child it lays out,
            // but only for widgets that go through its layout allocator.
            // Painting the search glyph directly at `ui.cursor().min`
            // bypassed that centering and always anchored it to the row's
            // top, leaving it visibly misaligned with the (correctly
            // centered) text beside it. Allocating the icon's rect through
            // the horizontal layout fixes that.
            let (icon_rect, _) =
                ui.allocate_exact_size(egui::vec2(13.0, 13.0), egui::Sense::hover());
            paint_sftp_glyph(
                ui.painter(),
                SftpGlyph::Search,
                icon_rect,
                theme::TEXT_MUTED,
            );
            ui.add_space(6.0);
            ui.scope(|ui| {
                ui.style_mut().visuals.extreme_bg_color = Color32::TRANSPARENT;
                ui.style_mut().visuals.widgets.inactive.bg_fill = Color32::TRANSPARENT;
                // The enclosing `Frame` already draws this field's border;
                // egui's default focus ring on the embedded `TextEdit`
                // otherwise draws a second, slightly different outline
                // around the field whenever it has focus.
                ui.style_mut().visuals.selection.stroke = egui::Stroke::NONE;
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
    let width = available_width.max(0.0);
    let mut columns = [width * 0.53, width * 0.15, width * 0.22, width * 0.10];
    // The mockup's percentages assume a wide pane. In a split view on a
    // smaller window they shrink the metadata columns until "4.0 KiB" and
    // "Folder" ellipsise into "4.0 K..." and "Fo...", which is exactly the
    // data those columns exist to show. Hold them at a legible minimum and
    // let Name -- the one column whose entries are expected to be truncated
    // -- absorb the difference instead.
    let minimums = [
        SFTP_NAME_COLUMN_MIN_WIDTH,
        SFTP_SIZE_COLUMN_MIN_WIDTH,
        SFTP_MODIFIED_COLUMN_MIN_WIDTH,
        SFTP_TYPE_COLUMN_MIN_WIDTH,
    ];
    if width >= minimums.iter().sum::<f32>() {
        for index in 1..columns.len() {
            columns[index] = columns[index].max(minimums[index]);
        }
    }
    let allocated = columns[1..].iter().sum::<f32>();
    columns[0] = (width - allocated).max(0.0);
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
    let text = RichText::new(title)
        .font(font_for_text_role(SftpTextRole::TableHeader))
        .color(theme::TEXT_MUTED);
    // `egui::Button` always centers its own label text regardless of the
    // parent `Ui`'s layout alignment, so a Button-based header cell ends up
    // centered even though the data-row cells below it (built with
    // `show_table_text_cell`, which manually lays out a `Label` inside a
    // left/right-aligned layout) are left- or right-aligned. Use the same
    // manual layout here, wrapped in a clickable `Frame`, so header text
    // lines up with the column values beneath it.
    ui.allocate_ui_with_layout(
        egui::vec2(width, SFTP_TABLE_HEADER_HEIGHT),
        match align {
            CellAlign::Left => Layout::left_to_right(Align::Center),
            CellAlign::Right => Layout::right_to_left(Align::Center),
        },
        |ui| {
            let response = egui::Frame::new()
                .fill(theme::SURFACE_TERMINAL)
                .stroke(egui::Stroke::NONE)
                .corner_radius(0.0)
                .show(ui, |ui| {
                    ui.set_min_width(width);
                    ui.set_max_width(width);
                    ui.set_min_height(SFTP_TABLE_HEADER_HEIGHT);
                    ui.set_max_height(SFTP_TABLE_HEADER_HEIGHT);
                    // Add the label and the (optional) sort-direction
                    // indicator directly to this frame's content `Ui`
                    // rather than wrapping them in a nested
                    // `ui.horizontal`, which always lays out left-to-right
                    // and would silently break right alignment for the
                    // "Size" column (whose content `Ui` inherits a
                    // right-to-left layout from the `allocate_ui_with_layout`
                    // call above). In a right-to-left layout the *first*
                    // widget added ends up at the rightmost position, so
                    // the add-order is flipped for `CellAlign::Right` to
                    // keep the arrow trailing the text on screen either way.
                    let indicator_size = egui::vec2(8.0, 12.0);
                    let inner_width = (width - SFTP_TABLE_CELL_PADDING * 2.0).max(0.0);
                    ui.add_space(SFTP_TABLE_CELL_PADDING);
                    ui.allocate_ui_with_layout(
                        egui::vec2(inner_width, SFTP_TABLE_HEADER_HEIGHT),
                        match align {
                            CellAlign::Left => Layout::left_to_right(Align::Center),
                            CellAlign::Right => Layout::right_to_left(Align::Center),
                        },
                        |ui| {
                            ui.set_min_width(inner_width);
                            ui.set_max_width(inner_width);
                            match align {
                                CellAlign::Left => {
                                    ui.add(egui::Label::new(text).truncate());
                                    if active {
                                        ui.add_space(SFTP_SORT_INDICATOR_GAP);
                                        let (rect, _) =
                                            ui.allocate_exact_size(indicator_size, Sense::hover());
                                        paint_sort_indicator(
                                            ui.painter(),
                                            rect,
                                            descending,
                                            theme::TEXT_MUTED,
                                        );
                                    }
                                }
                                CellAlign::Right => {
                                    if active {
                                        let (rect, _) =
                                            ui.allocate_exact_size(indicator_size, Sense::hover());
                                        paint_sort_indicator(
                                            ui.painter(),
                                            rect,
                                            descending,
                                            theme::TEXT_MUTED,
                                        );
                                        ui.add_space(SFTP_SORT_INDICATOR_GAP);
                                    }
                                    ui.add(egui::Label::new(text).truncate());
                                }
                            }
                        },
                    );
                })
                .response;
            response.interact(Sense::click())
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
            // Force the cell to claim its full column width even though the
            // label content is narrower; otherwise egui shrinks the
            // allocated rect to fit the label, and the parent `horizontal`
            // layout packs the next column right up against this one
            // instead of at its intended fixed offset. `set_max_width` plus
            // `Label::truncate()` is the other half of that contract: a
            // *wider* label (e.g. a long file name) must never grow past
            // its column either, or it visually overflows into (and
            // overlaps) the next column instead of eliding with "…".
            ui.set_min_width(width);
            ui.set_max_width(width);
            // Mockup `.fsftp-table td { padding: 0 7px }` -- symmetric on both
            // edges. The inner region is sized explicitly so the trailing
            // inset survives in a right-to-left (Size) column too, where a
            // bare `add_space` would land on the wrong side.
            ui.add_space(SFTP_TABLE_CELL_PADDING);
            let text_width = (width - SFTP_TABLE_CELL_PADDING * 2.0).max(0.0);
            ui.allocate_ui_with_layout(
                egui::vec2(text_width, SFTP_TABLE_ROW_HEIGHT),
                match align {
                    CellAlign::Left => Layout::left_to_right(Align::Center),
                    CellAlign::Right => Layout::right_to_left(Align::Center),
                },
                |ui| {
                    ui.set_min_width(text_width);
                    ui.set_max_width(text_width);
                    ui.add(egui::Label::new(text).truncate());
                },
            );
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

/// Whether double-clicking this item should open the Markdown viewer
/// (issue #133), rather than being a no-op (or, for directories, handled
/// separately in `open_item`).
fn is_markdown_file(item: &SftpDirectoryItem) -> bool {
    if item.file_type != SftpEntryType::File {
        return false;
    }
    let extension = Path::new(item.name.as_str())
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    matches!(extension.as_str(), "md" | "markdown")
}

/// Builds the `RemoteMarkdownSource` identity for a fetched remote Markdown
/// snapshot, pinning it to `target`'s host/port/owner and the given
/// already-verified `fingerprint` (issue #133). Split out from
/// `SftpFileManagerTab::remote_markdown_identity` as a free function purely
/// so it's unit-testable without spinning up a tab's worker thread.
fn build_remote_markdown_source(
    target: &SftpFileManagerLaunchTarget,
    fingerprint: &str,
    remote_path: &str,
) -> Option<RemoteMarkdownSource> {
    let owner = match &target.profile_id {
        Some(profile_id) => {
            RemoteSourceOwner::username_and_profile(target.username.clone(), profile_id.clone())
        }
        None => RemoteSourceOwner::username(target.username.clone()),
    }
    .ok()?;
    // `lifecycle_generation` is currently inert: this implementation
    // deliberately does not support reloading a remote-opened Markdown tab
    // (see `MarkdownViewerLoadFailure::RemoteReloadUnsupported`), so there
    // is nothing yet that needs to distinguish one transport episode from
    // another. A future reload feature should replace this with a real
    // counter (see ADR-0030).
    const INERT_LIFECYCLE_GENERATION: u64 = 1;
    RemoteMarkdownSource::new(
        target.host.clone(),
        target.port,
        owner,
        fingerprint.to_owned(),
        remote_path.to_owned(),
        INERT_LIFECYCLE_GENERATION,
    )
    .ok()
}

/// The directory the Home navigation and a freshly opened picker start from.
///
/// Windows sets `USERPROFILE` rather than `HOME`, so consulting only `HOME`
/// sent every Home navigation there to the filesystem root instead of the
/// user's own folder.
pub(crate) fn local_home_directory() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .filter(|path| path.is_dir())
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from(std::path::MAIN_SEPARATOR_STR))
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

/// Paints a small filled triangle sort-direction indicator (point up for
/// ascending, down for descending) inside `rect`. Used in table column
/// headers instead of a literal "↑"/"↓" character, since the proportional
/// font used for table headers doesn't include those glyphs and rendered
/// them as a missing-glyph box.
fn paint_sort_indicator(
    painter: &egui::Painter,
    rect: egui::Rect,
    descending: bool,
    color: Color32,
) {
    // The mockup writes the sort direction as a text arrow ("Name ↑"), so this
    // draws a thin stemmed arrow rather than the solid triangle it used to.
    // At 8x12 a filled triangle read as a featureless block -- the "sort order
    // icon is just a box, not an arrow" defect.
    let stroke = egui::Stroke::new(1.0, color);
    let center_x = rect.center().x;
    let top = rect.center().y - 4.0;
    let bottom = rect.center().y + 4.0;
    let (tip, tail, head_y) = if descending {
        (bottom, top, bottom - 3.2)
    } else {
        (top, bottom, top + 3.2)
    };
    painter.line_segment(
        [egui::pos2(center_x, tail), egui::pos2(center_x, tip)],
        stroke,
    );
    painter.add(egui::Shape::line(
        vec![
            egui::pos2(center_x - 2.4, head_y),
            egui::pos2(center_x, tip),
            egui::pos2(center_x + 2.4, head_y),
        ],
        stroke,
    ));
}

fn paint_sftp_glyph(painter: &egui::Painter, glyph: SftpGlyph, rect: egui::Rect, color: Color32) {
    match glyph {
        SftpGlyph::Back => icon::paint(painter, Icon::Back, rect, color),
        SftpGlyph::Search => icon::paint(painter, Icon::Search, rect, color),
        SftpGlyph::LocalPane => icon::paint(painter, Icon::LocalTerminal, rect, color),
        SftpGlyph::RemotePane => icon::paint(painter, Icon::SshRemote, rect, color),
        SftpGlyph::Refresh => icon::paint(painter, Icon::Refresh, rect, color),
        SftpGlyph::Up => icon::paint(painter, Icon::ParentDirectory, rect, color),
        SftpGlyph::Home => icon::paint(painter, Icon::HomeDirectory, rect, color),
        SftpGlyph::Folder => {
            // Mirrors the mockup's folder glyph path (`M3 7h7l2 2h9v10H3z` in a
            // 24x24 viewBox): a small tab notch above a boxy, mostly-square
            // body. Using straight corners (rather than a heavily rounded
            // rect) keeps the body from collapsing into a pill/capsule shape
            // at this icon's small (15x15) render size.
            let stroke = egui::Stroke::new(1.5, color);
            let scale = rect.width() / 24.0;
            let pt = |x: f32, y: f32| egui::pos2(rect.left() + x * scale, rect.top() + y * scale);
            let points = vec![
                pt(3.0, 7.0),
                pt(10.0, 7.0),
                pt(12.0, 9.0),
                pt(21.0, 9.0),
                pt(21.0, 19.0),
                pt(3.0, 19.0),
            ];
            painter.add(egui::Shape::closed_line(points, stroke));
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
            let event_sender = tab.event_sender.clone();
            let repaint = tab.repaint.clone();
            tab.local_loader.schedule(LocalDirectoryLoadRequest {
                path,
                complete: Box::new(move |result| {
                    let event = match result {
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
                }),
            });
        }
        PaneFocus::Remote => {
            if let SftpPath::Remote(path) = path {
                let _ = tab.command_sender.send(WorkerCommand::LoadRemote { path });
            }
        }
    }
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

/// A per-platform "reveal this path in the OS file manager" invocation
/// (issue #137's follow-up comment), split from `reveal_in_file_manager` so
/// the argument construction is unit-testable without actually spawning a
/// GUI process.
struct RevealCommand {
    program: &'static str,
    args: Vec<std::ffi::OsString>,
}

fn build_reveal_command(path: &std::path::Path) -> RevealCommand {
    #[cfg(target_os = "macos")]
    {
        RevealCommand {
            program: "open",
            args: vec!["-R".into(), path.as_os_str().to_owned()],
        }
    }
    #[cfg(target_os = "windows")]
    {
        let mut select_arg = std::ffi::OsString::from("/select,");
        select_arg.push(path.as_os_str());
        RevealCommand {
            program: "explorer.exe",
            args: vec![select_arg],
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        // `xdg-open` has no cross-desktop "select this file in its parent
        // folder" equivalent to `open -R`/`explorer.exe /select,`, so this
        // opens the containing folder itself for a file, or the item
        // directly if it's already a directory.
        let target = if path.is_dir() {
            path.to_path_buf()
        } else {
            path.parent()
                .map(std::path::Path::to_path_buf)
                .unwrap_or_else(|| path.to_path_buf())
        };
        RevealCommand {
            program: "xdg-open",
            args: vec![target.into_os_string()],
        }
    }
}

/// The context-menu label naming the platform-specific reveal action, so
/// the same code path reads "Reveal in Finder" on macOS, "Show in File
/// Explorer" on Windows, and "Open Containing Folder" elsewhere.
fn reveal_in_file_manager_label() -> &'static str {
    if cfg!(target_os = "macos") {
        "Reveal in Finder"
    } else if cfg!(target_os = "windows") {
        "Show in File Explorer"
    } else {
        "Open Containing Folder"
    }
}

fn reveal_in_file_manager(path: &std::path::Path) -> Result<(), String> {
    let command = build_reveal_command(path);
    std::process::Command::new(command.program)
        .args(&command.args)
        .spawn()
        .map(|_| ())
        .map_err(|error| error.to_string())
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

/// Formats a modification timestamp the way the mockup's Modified column does:
/// `Today HH:MM` for the current UTC day, `Yesterday` for the previous one,
/// and `Mon D` (with a trailing year when it differs from the current one)
/// otherwise. All comparisons use UTC so the result is deterministic and
/// doesn't require pulling in a timezone-aware date/time dependency.
fn format_modified(timestamp: Option<SystemTime>) -> String {
    let Some(timestamp) = timestamp else {
        return "—".to_owned();
    };
    let Ok(duration) = timestamp.duration_since(SystemTime::UNIX_EPOCH) else {
        return "—".to_owned();
    };
    let Ok(now) = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH) else {
        return "—".to_owned();
    };
    format_modified_from_epoch_seconds(duration.as_secs(), now.as_secs())
}

const SECONDS_PER_DAY: i64 = 86_400;
const MONTH_NAMES: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

fn format_modified_from_epoch_seconds(modified_secs: u64, now_secs: u64) -> String {
    let modified_secs = modified_secs as i64;
    let now_secs = now_secs as i64;
    let modified_day = modified_secs.div_euclid(SECONDS_PER_DAY);
    let now_day = now_secs.div_euclid(SECONDS_PER_DAY);
    let time_of_day = modified_secs.rem_euclid(SECONDS_PER_DAY);
    let hour = time_of_day / 3600;
    let minute = (time_of_day % 3600) / 60;

    match now_day - modified_day {
        0 => format!("Today {hour:02}:{minute:02}"),
        1 => "Yesterday".to_owned(),
        _ => {
            let (year, month, day) = civil_from_days(modified_day);
            let (now_year, _, _) = civil_from_days(now_day);
            let month_name = MONTH_NAMES[(month - 1) as usize];
            if year == now_year {
                format!("{month_name} {day}")
            } else {
                format!("{month_name} {day}, {year}")
            }
        }
    }
}

/// Converts a day count since the Unix epoch (1970-01-01) into a proleptic
/// Gregorian civil (year, month, day) triple. This is Howard Hinnant's
/// widely-used `civil_from_days` algorithm
/// (<https://howardhinnant.github.io/date_algorithms.html#civil_from_days>),
/// reproduced here to avoid adding a timezone/date dependency for a single
/// display-formatting need.
fn civil_from_days(days_since_epoch: i64) -> (i64, u32, u32) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = (z - era * 146_097) as u64;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era as i64 + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let mp = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = if month <= 2 { year + 1 } else { year };
    (year, month, day)
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
            let mut last_was_prefix = false;
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
                        last_was_prefix = true;
                    }
                    Component::RootDir => {
                        current.push(Path::new("/"));
                        // On Windows a drive prefix ("C:") is immediately
                        // followed by a RootDir component; the prefix
                        // segment already implies the root, so emitting a
                        // separate "/" segment here would render as a
                        // spurious extra slash in the breadcrumb (e.g.
                        // "C: / / / Users"). Only Unix-style paths, which
                        // have no preceding prefix, need their own "/"
                        // segment.
                        if !last_was_prefix {
                            segments.push(BreadcrumbSegment {
                                label: "/".to_owned(),
                                path: SftpPath::local(current.clone()),
                                current: path.parent().is_none(),
                            });
                        } else if let Some(last) = segments.last_mut() {
                            last.path = SftpPath::local(current.clone());
                            last.current = path.parent().is_none();
                        }
                        last_was_prefix = false;
                    }
                    Component::Normal(part) => {
                        current.push(part);
                        segments.push(BreadcrumbSegment {
                            label: part.to_string_lossy().into_owned(),
                            path: SftpPath::local(current.clone()),
                            current: current == *path,
                        });
                        last_was_prefix = false;
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

/// What the user did in a [`MarkdownFilePicker`] this frame.
pub(crate) enum MarkdownPickerOutcome {
    /// Nothing decided yet; the picker stays open.
    Pending,
    /// A Markdown file was picked (double-click or Enter); the caller
    /// should dispatch `AppCommand::OpenLocalMarkdownFile` with this path
    /// and close the picker.
    Open(PathBuf),
    /// The user dismissed the picker (Cancel or Escape) without picking a
    /// file.
    Cancelled,
}

enum MarkdownPickerEvent {
    Loaded {
        request_id: u64,
        snapshot: SftpDirectorySnapshot,
        metadata: Option<SftpPathMetadata>,
    },
    Failed {
        request_id: u64,
        summary: String,
        details: String,
    },
}

/// Local-filesystem-only file picker for "Open Markdown File…" (#132),
/// reusing the SFTP file manager's local-pane browsing model (breadcrumbs,
/// up/home/refresh navigation, sortable columns, item icons, single
/// selection) instead of the OS-native `rfd::FileDialog` previously used.
/// Deliberately has no remote pane or transfer machinery: this widget only
/// ever browses the local filesystem, so it owns its own directory-listing
/// thread and event channel rather than reaching into a full
/// `SftpFileManagerTab`/`WorkerEvent` pipeline built for a live SSH
/// connection.
pub(crate) struct MarkdownFilePicker {
    pane: SftpPaneState,
    event_sender: Sender<MarkdownPickerEvent>,
    event_receiver: Receiver<MarkdownPickerEvent>,
    repaint: egui::Context,
    local_loader: LocalDirectoryLoader,
    next_request_id: u64,
}

impl MarkdownFilePicker {
    /// Opens the picker rooted at `start_dir`.
    pub(crate) fn new(start_dir: PathBuf, repaint: egui::Context) -> Self {
        let (event_sender, event_receiver) = mpsc::channel();
        let local_loader =
            LocalDirectoryLoader::new("festerm-gui-markdown-picker-local".to_owned());
        let mut picker = Self {
            pane: SftpPaneState::new(SftpPath::local(start_dir)),
            event_sender,
            event_receiver,
            repaint,
            local_loader,
            next_request_id: 0,
        };
        let start = picker.pane.current_path.clone();
        picker.load(start, false);
        picker
    }

    fn load(&mut self, path: SftpPath, push_history: bool) {
        self.pane.loading = true;
        self.pane.error = None;
        self.pane.details = None;
        self.pane.path_text = path.display();
        self.pane.current_path = path.clone();
        if push_history {
            self.pane.push_history(path.clone());
        }
        self.next_request_id += 1;
        let request_id = self.next_request_id;
        self.pane.pending_request_id = request_id;
        let event_sender = self.event_sender.clone();
        let repaint = self.repaint.clone();
        self.local_loader.schedule(LocalDirectoryLoadRequest {
            path,
            complete: Box::new(move |result| {
                let event = match result {
                    Ok((snapshot, metadata)) => MarkdownPickerEvent::Loaded {
                        request_id,
                        snapshot,
                        metadata,
                    },
                    Err(error) => MarkdownPickerEvent::Failed {
                        request_id,
                        summary: "Could not load the folder.".to_owned(),
                        details: error,
                    },
                };
                let _ = event_sender.send(event);
                repaint.request_repaint();
            }),
        });
    }

    /// Applies any directory-listing results that arrived since the last
    /// frame. Must be called once per frame before `ui`.
    pub(crate) fn poll(&mut self) {
        while let Ok(event) = self.event_receiver.try_recv() {
            match event {
                MarkdownPickerEvent::Loaded {
                    request_id,
                    snapshot,
                    metadata,
                } => {
                    if request_id == self.pane.pending_request_id {
                        self.pane.set_snapshot(snapshot, metadata);
                    }
                }
                MarkdownPickerEvent::Failed {
                    request_id,
                    summary,
                    details,
                } => {
                    if request_id == self.pane.pending_request_id {
                        self.pane.set_error(summary, details);
                    }
                }
            }
        }
    }

    fn navigate_back(&mut self) {
        if self.pane.history_index == 0 {
            return;
        }
        let next = self.pane.history_index - 1;
        self.pane.history_index = next;
        let target = self.pane.history[next].clone();
        self.load(target, false);
    }

    fn navigate_up(&mut self) {
        let path = self.pane.current_path.parent_directory();
        self.load(path, true);
    }

    fn navigate_home(&mut self) {
        // Matches `SftpFileManagerTab::navigate_home`'s Local branch.
        self.load(SftpPath::local(local_home_directory()), true);
    }

    fn navigate_to_breadcrumb(&mut self, path: SftpPath) {
        self.load(path, true);
    }

    /// The directory currently being browsed, so the next picker can resume
    /// where this one left off rather than starting over at the home folder.
    pub(crate) fn current_directory(&self) -> Option<PathBuf> {
        match &self.pane.current_path {
            SftpPath::Local(path) => Some(path.clone()),
            SftpPath::Remote(_) => None,
        }
    }

    fn refresh(&mut self) {
        let path = self.pane.current_path.clone();
        self.load(path, false);
    }

    /// Opens `item`: navigates into it if it's a directory, or reports it as
    /// picked if it's a recognized Markdown file. Any other file is a no-op,
    /// same as the SFTP local pane's own double-click handling (#133).
    fn open_item(&mut self, item: &SftpDirectoryItem) -> MarkdownPickerOutcome {
        if item.file_type == SftpEntryType::Directory {
            self.navigate_to_breadcrumb(item.path.clone());
            return MarkdownPickerOutcome::Pending;
        }
        if !is_markdown_file(item) {
            return MarkdownPickerOutcome::Pending;
        }
        match &item.path {
            SftpPath::Local(path) => MarkdownPickerOutcome::Open(path.clone()),
            SftpPath::Remote(_) => MarkdownPickerOutcome::Pending,
        }
    }

    /// Renders the picker's content (toolbar, breadcrumbs, filter, table,
    /// footer) into `ui`, which the caller wraps in an `egui::Modal`/`Frame`
    /// of its own (see `FesTermApp::show_markdown_file_picker`).
    pub(crate) fn ui(&mut self, ui: &mut Ui) -> MarkdownPickerOutcome {
        let mut outcome = MarkdownPickerOutcome::Pending;
        let width = ui.available_width();

        ui.horizontal(|ui| {
            if toolbar_icon_button(ui, SftpGlyph::Back, "Back").clicked() {
                self.navigate_back();
            }
            if toolbar_icon_button(ui, SftpGlyph::Up, "Up one level").clicked() {
                self.navigate_up();
            }
            if toolbar_icon_button(ui, SftpGlyph::Home, "Home").clicked() {
                self.navigate_home();
            }
            if toolbar_icon_button(ui, SftpGlyph::Refresh, "Refresh folder").clicked() {
                self.refresh();
            }
            ui.add_space(SFTP_TOOLBAR_NAV_GAP);
            let mut breadcrumb_target = None;
            ui.horizontal_wrapped(|ui| {
                for (index, segment) in breadcrumb_segments(&self.pane.current_path)
                    .into_iter()
                    .enumerate()
                {
                    if index > 0 && segment.label != "/" {
                        ui.label(
                            RichText::new("/")
                                .font(font_for_text_role(SftpTextRole::Breadcrumb))
                                .color(theme::TEXT_MUTED),
                        );
                    }
                    let text = RichText::new(segment.label.clone())
                        .font(font_for_text_role(SftpTextRole::Breadcrumb))
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
                        breadcrumb_target = Some(segment.path);
                    }
                }
            });
            if let Some(path) = breadcrumb_target {
                self.navigate_to_breadcrumb(path);
            }
        });
        ui.add_space(6.0);

        let mut filter_text = self.pane.filter.clone();
        let filter_response = show_filter_field(ui, &mut filter_text, PaneFocus::Local, width);
        if filter_response.changed() {
            self.pane.set_filter(filter_text);
        }
        ui.add_space(6.0);

        if let Some(summary) = self.pane.error.clone() {
            ui.colored_label(theme::STATUS_ERROR, summary);
            ui.add_space(6.0);
        }

        let columns = sftp_table_columns(width);
        let mut sort_clicked = None;
        ui.horizontal(|ui| {
            if show_table_header_cell(
                ui,
                columns[0],
                CellAlign::Left,
                "Name",
                self.pane.sort.column == SftpSortColumn::Name,
                self.pane.sort.descending,
            )
            .clicked()
            {
                sort_clicked = Some(SftpSortColumn::Name);
            }
            if show_table_header_cell(
                ui,
                columns[1],
                CellAlign::Right,
                "Size",
                self.pane.sort.column == SftpSortColumn::Size,
                self.pane.sort.descending,
            )
            .clicked()
            {
                sort_clicked = Some(SftpSortColumn::Size);
            }
            if show_table_header_cell(
                ui,
                columns[2],
                CellAlign::Left,
                "Modified",
                self.pane.sort.column == SftpSortColumn::Modified,
                self.pane.sort.descending,
            )
            .clicked()
            {
                sort_clicked = Some(SftpSortColumn::Modified);
            }
            if show_table_header_cell(
                ui,
                columns[3],
                CellAlign::Left,
                "Type",
                self.pane.sort.column == SftpSortColumn::Type,
                self.pane.sort.descending,
            )
            .clicked()
            {
                sort_clicked = Some(SftpSortColumn::Type);
            }
        });
        if let Some(column) = sort_clicked {
            self.pane.set_sort(column);
        }

        let mut picked_item: Option<SftpDirectoryItem> = None;
        let entries = self.pane.visible_entries().to_vec();
        ScrollArea::vertical()
            .id_salt("markdown_file_picker_rows")
            .max_height(280.0)
            .show(ui, |ui| {
                for item in &entries {
                    let key = path_key(&item.path);
                    let selected = self.pane.selected_paths.contains(&key);
                    let markdown_or_dir =
                        item.file_type == SftpEntryType::Directory || is_markdown_file(item);
                    let row = ui
                        .horizontal(|ui| {
                            ui.set_min_height(SFTP_TABLE_ROW_HEIGHT);
                            ui.set_max_height(SFTP_TABLE_ROW_HEIGHT);
                            let name_color = if !markdown_or_dir {
                                theme::TEXT_MUTED
                            } else if selected {
                                theme::TEXT_PRIMARY
                            } else {
                                theme::TEXT_SECONDARY
                            };
                            // The icon has to be allocated inside a cell that
                            // already carries the row's full height. Allocating
                            // it straight into the row (which is only as tall as
                            // the icon until the taller text cells are added)
                            // centered it against its own 16px band, leaving
                            // every glyph floating above the name beside it.
                            // This mirrors `SftpFileManagerTab`'s name cell.
                            ui.allocate_ui_with_layout(
                                egui::vec2(columns[0], SFTP_TABLE_ROW_HEIGHT),
                                Layout::left_to_right(Align::Center),
                                |ui| {
                                    ui.set_min_width(columns[0]);
                                    ui.set_max_width(
                                        (columns[0] - SFTP_TABLE_CELL_PADDING).max(0.0),
                                    );
                                    ui.add_space(SFTP_TABLE_CELL_PADDING);
                                    let (icon_rect, _) = ui.allocate_exact_size(
                                        egui::vec2(15.0, 15.0),
                                        Sense::hover(),
                                    );
                                    paint_sftp_glyph(
                                        ui.painter(),
                                        item_glyph(item),
                                        icon_rect,
                                        if markdown_or_dir {
                                            theme::TEXT_SECONDARY
                                        } else {
                                            theme::TEXT_MUTED
                                        },
                                    );
                                    ui.add_space(5.0);
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(item.name.clone()).color(name_color),
                                        )
                                        .truncate(),
                                    );
                                },
                            );
                            show_table_text_cell(
                                ui,
                                columns[1],
                                CellAlign::Right,
                                RichText::new(format_size(item.size)).color(theme::TEXT_MUTED),
                            );
                            show_table_text_cell(
                                ui,
                                columns[2],
                                CellAlign::Left,
                                RichText::new(format_modified(item.modified_at))
                                    .color(theme::TEXT_MUTED),
                            );
                            show_table_text_cell(
                                ui,
                                columns[3],
                                CellAlign::Left,
                                RichText::new(item_type_label(item)).color(theme::TEXT_MUTED),
                            );
                        })
                        .response;
                    let row_rect = row.rect;
                    let row_response = ui.interact(
                        row_rect,
                        ui.make_persistent_id(("markdown_file_picker_row", &key)),
                        Sense::click(),
                    );
                    if selected {
                        ui.painter().rect_filled(
                            row_rect,
                            0.0,
                            theme::SURFACE_TAB_ACTIVE.gamma_multiply(0.6),
                        );
                    }
                    if row_response.clicked() {
                        self.pane.select_single(&item.path);
                    }
                    if row_response.double_clicked() {
                        picked_item = Some(item.clone());
                    }
                }
            });

        if ui.input(|input| input.key_pressed(Key::Enter)) {
            if let Some(item) = self.pane.activate_cursor() {
                picked_item = Some(item);
            }
        }
        if ui.input(|input| input.key_pressed(Key::ArrowDown)) {
            self.pane.move_cursor(1, false);
        }
        if ui.input(|input| input.key_pressed(Key::ArrowUp)) {
            self.pane.move_cursor(-1, false);
        }

        if let Some(item) = picked_item {
            outcome = self.open_item(&item);
        }

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!("{} items", entries.len()))
                    .font(font_for_text_role(SftpTextRole::Footer))
                    .color(theme::TEXT_MUTED),
            );
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                if ui.button("Cancel").clicked() {
                    outcome = MarkdownPickerOutcome::Cancelled;
                }
            });
        });

        outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::{kittest::Queryable, Harness};
    use std::ffi::OsString;

    /// Builds a `SftpFileManagerTab` for unit tests without going through
    /// `SftpFileManagerTab::new`, which spawns a real background worker
    /// thread that attempts a live SSH connection -- impractical (and
    /// network-dependent/flaky) in a unit test. Tests that need to feed
    /// synthetic `WorkerEvent`s directly into `apply_event` construct the
    /// struct literal here instead, with disconnected channels that no
    /// worker thread will ever touch.
    fn test_tab() -> SftpFileManagerTab {
        let target = test_launch_target(None);
        let (command_sender, _command_receiver) = tokio::sync::mpsc::unbounded_channel();
        let (event_sender, event_receiver) = mpsc::channel();
        let context = egui::Context::default();
        SftpFileManagerTab {
            label: target.label.clone(),
            profile_identifier: target.profile_id.clone(),
            launch_target: target,
            local_pane: SftpPaneState::new(SftpPath::local(PathBuf::from("/tmp"))),
            remote_pane: SftpPaneState::new(SftpPath::remote("/")),
            pane_order: SftpPaneOrderPreference::default(),
            status_bar_visible: true,
            focused_pane: PaneFocus::Local,
            narrow_focus: PaneFocus::Local,
            connection_state: SftpConnectionState::Connecting,
            has_connected_once: false,
            transfer_drawer: TransferDrawerState::default(),
            collision_dialog: None,
            command_sender,
            event_receiver,
            event_sender,
            local_loader: LocalDirectoryLoader::paused_for_test(),
            repaint: context,
            next_local_request_id: 1,
            pending_host_key: None,
            verified_host_key_fingerprint: None,
            next_markdown_request_id: 1,
            pending_markdown_request: None,
            pending_markdown_open: None,
            operation_error: None,
            pending_markdown_command: None,
            last_local_pane_rect: None,
            last_remote_pane_rect: None,
        }
    }

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
    fn format_modified_matches_mockup_relative_and_absolute_conventions() {
        // 2026-09-04 09:27:00 UTC, used as a fixed "now" so the test doesn't
        // depend on the wall clock.
        let now = 1_788_513_620u64;
        // Same UTC calendar day, earlier in the morning.
        let today_early = now - 3 * 3600 - 47 * 60; // 05:40:20 UTC same day
        assert_eq!(
            format_modified_from_epoch_seconds(today_early, now),
            "Today 05:33"
        );
        // Previous UTC calendar day.
        let yesterday = now - SECONDS_PER_DAY as u64;
        assert_eq!(
            format_modified_from_epoch_seconds(yesterday, now),
            "Yesterday"
        );
        // Several days earlier, same year -> "Mon D" without a year suffix.
        let last_week = now - 5 * SECONDS_PER_DAY as u64;
        assert_eq!(format_modified_from_epoch_seconds(last_week, now), "Aug 30");
        // A prior year -> "Mon D, YYYY".
        let last_year = now - 400 * SECONDS_PER_DAY as u64;
        assert_eq!(
            format_modified_from_epoch_seconds(last_year, now),
            "Jul 31, 2025"
        );
    }

    #[test]
    fn format_modified_handles_missing_timestamps() {
        assert_eq!(format_modified(None), "—");
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
    fn authentication_surface_focuses_the_password_field_on_arrival() {
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
            harness
                .get_by_role(egui::accesskit::Role::PasswordInput)
                .is_focused(),
            "arriving from the connection form should put the caret in the password box"
        );

        // One-shot: focus must be releasable, otherwise Tab and clicking
        // another field would be permanently overridden.
        harness
            .get_all_by_role(egui::accesskit::Role::TextInput)
            .next()
            .expect("the ad-hoc destination row renders editable fields")
            .focus();
        harness.run();
        assert!(
            !harness
                .get_by_role(egui::accesskit::Role::PasswordInput)
                .is_focused(),
            "the password focus request must not be re-issued every frame"
        );
    }

    #[test]
    fn authentication_surface_refocuses_the_password_after_returning_from_a_failure() {
        struct ReentryState {
            visible: bool,
        }

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
            move |ui, state: &mut ReentryState| {
                if state.visible {
                    show_authentication_required(ui, tab_id, &target);
                } else {
                    // Stands in for the connecting/failed surface the tab
                    // swaps in while the form is away.
                    ui.label("Connection failed.");
                }
            },
            ReentryState { visible: true },
        );

        harness.run();
        harness
            .get_all_by_role(egui::accesskit::Role::TextInput)
            .next()
            .expect("the ad-hoc destination row renders editable fields")
            .focus();
        harness.run();
        assert!(!harness
            .get_by_role(egui::accesskit::Role::PasswordInput)
            .is_focused());

        // Leave for the failure surface, then come back via "Edit
        // connection…": the password should be ready to retype.
        harness.state_mut().visible = false;
        harness.run();
        harness.state_mut().visible = true;
        harness.run();

        assert!(
            harness
                .get_by_role(egui::accesskit::Role::PasswordInput)
                .is_focused(),
            "returning to the credential form should refocus the password box"
        );
    }

    #[test]
    fn authentication_surface_focuses_the_key_box_when_switching_to_private_key() {
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
        harness.get_by_label("Private key").click();
        harness.run();

        assert!(
            harness
                .get_by_role(egui::accesskit::Role::MultilineTextInput)
                .is_focused(),
            "switching auth mode should move focus to that mode's first empty field"
        );
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
                show_connection_status_banner_ui(ui, &state);
                *command = None;
            },
            None,
        );

        harness.run();

        assert!(harness.query_by_label("Connection failed").is_some());
        assert!(harness
            .query_by_label("The SSH host key was rejected or the trust prompt expired.")
            .is_some());
        assert!(
            harness.query_by_label("Retry").is_some(),
            "a failed connection must offer a Retry action"
        );
        assert!(
            harness.query_by_label("Edit connection…").is_some(),
            "a failed (never-connected) attempt must offer a way to fix the destination"
        );
    }

    #[test]
    fn disconnected_connection_banner_shows_diagnostic_details() {
        let state = SftpConnectionState::Disconnected {
            summary: "Disconnected".to_owned(),
            details: "The SSH/SFTP connection could not be established.".to_owned(),
        };
        let mut harness = Harness::builder().build_ui_state(
            move |ui, command: &mut Option<crate::tabs::AppCommand>| {
                show_connection_status_banner_ui(ui, &state);
                *command = None;
            },
            None,
        );

        harness.run();

        assert!(harness.query_by_label("Disconnected").is_some());
        assert!(
            harness.query_by_label("Retry").is_some(),
            "a dropped connection must offer a Retry action"
        );
        assert!(
            harness.query_by_label("Edit connection…").is_none(),
            "the destination is already known-good once connected once, so editing it isn't offered"
        );
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

    #[cfg(windows)]
    #[test]
    fn breadcrumb_segments_do_not_duplicate_slash_after_drive_prefix() {
        let local = breadcrumb_segments(&SftpPath::local(r"C:\Users\fes\src\fesTerm"));
        assert_eq!(
            local
                .iter()
                .map(|segment| segment.label.as_str())
                .collect::<Vec<_>>(),
            vec!["C:", "Users", "fes", "src", "fesTerm"]
        );
        assert!(local.last().is_some_and(|segment| segment.current));
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
    fn split_view_min_width_matches_two_panes_and_rail() {
        // fesTerm's default application window (80 columns at the approximate
        // monospace cell metrics used in `main.rs`) must be wide enough to
        // show the split-pane SFTP layout without requiring the user to
        // manually resize the window first. This is deliberately the raw
        // default window width with no margin subtracted: the SFTP tab's
        // render path (`FesTermApp::ui` -> `chrome::show` -> `tab.show`)
        // passes the egui `Ui` straight through with no intervening
        // `CentralPanel`/`Frame` inner margin, so no such margin exists to
        // subtract here. `SFTP_SPLIT_VIEW_MIN_WIDTH` itself is derived from
        // (not independent of) `SFTP_PANE_MIN_WIDTH`/`SFTP_TRANSFER_RAIL_WIDTH`/
        // `SFTP_SECTION_GAP`, so this assertion is what actually guards
        // against the split-pane layout becoming unreachable again at
        // fesTerm's default window size (see issue #121, which this
        // threshold was previously too large to satisfy).
        //
        // Both sides are compile-time constants, so this is enforced as a
        // `const` assertion: a future edit that pushes the threshold past
        // the default window width fails the build itself, not just this
        // test.
        const APPROX_DEFAULT_WINDOW_WIDTH: f32 = 80.0 * 9.0 + 16.0 * 2.0;
        const _: () = assert!(APPROX_DEFAULT_WINDOW_WIDTH >= SFTP_SPLIT_VIEW_MIN_WIDTH);
    }

    #[test]
    fn table_columns_follow_mockup_proportions() {
        let columns = sftp_table_columns(1000.0);
        assert_eq!(columns, [530.0, 150.0, 220.0, 100.0]);
    }

    #[test]
    fn narrow_table_columns_keep_metadata_legible() {
        let columns = sftp_table_columns(372.0);
        assert_eq!(columns[1], SFTP_SIZE_COLUMN_MIN_WIDTH);
        assert_eq!(columns[2], SFTP_MODIFIED_COLUMN_MIN_WIDTH);
        assert_eq!(columns[3], SFTP_TYPE_COLUMN_MIN_WIDTH);
        assert!(columns[0] >= SFTP_NAME_COLUMN_MIN_WIDTH);
        assert!((columns.iter().sum::<f32>() - 372.0).abs() < 0.01);
    }

    #[test]
    fn table_columns_never_exceed_available_width() {
        for width in [0.0, 40.0, 120.0, 361.0, 362.0, 800.0, 1600.0] {
            let columns = sftp_table_columns(width);
            let total = columns.iter().sum::<f32>();
            assert!(
                total <= width + 0.01,
                "columns {columns:?} overflow width {width}"
            );
        }
    }

    #[test]
    fn toolbar_and_filter_rows_share_horizontal_padding() {
        // Their boxes stack directly on top of each other, so unequal padding
        // reads as a misaligned right edge.
        assert_eq!(SFTP_TOOLBAR_PADDING, SFTP_FILTER_ROW_PADDING);
    }

    fn remote_item(name: &str, path: &str, file_type: SftpEntryType) -> SftpDirectoryItem {
        SftpDirectoryItem {
            name: name.to_owned(),
            path: SftpPath::remote(path),
            file_type,
            size: Some(1),
            modified_at: None,
            permissions: None,
        }
    }

    #[test]
    fn is_markdown_file_recognizes_md_and_markdown_extensions_case_insensitively() {
        assert!(is_markdown_file(&item(
            "README.md",
            SftpEntryType::File,
            Some(1)
        )));
        assert!(is_markdown_file(&item(
            "NOTES.MARKDOWN",
            SftpEntryType::File,
            Some(1)
        )));
        assert!(is_markdown_file(&remote_item(
            "guide.md",
            "/srv/docs/guide.md",
            SftpEntryType::File
        )));
        assert!(!is_markdown_file(&item(
            "README.txt",
            SftpEntryType::File,
            Some(1)
        )));
        assert!(!is_markdown_file(&item(
            "docs",
            SftpEntryType::Directory,
            None
        )));
    }

    fn test_launch_target(profile_id: Option<&str>) -> SftpFileManagerLaunchTarget {
        SftpFileManagerLaunchTarget {
            label: "production".to_owned(),
            username: "deploy".to_owned(),
            host: "sftp.example.test".to_owned(),
            port: 22,
            profile_id: profile_id.map(str::to_owned),
            stored_credential_kind: None,
            known_host_persisted: true,
        }
    }

    #[test]
    fn build_remote_markdown_source_pins_host_port_and_verified_fingerprint() {
        let target = test_launch_target(None);
        let source = build_remote_markdown_source(&target, "SHA256:abc123", "/srv/docs/guide.md")
            .expect("valid launch target and fingerprint produce a source");
        assert_eq!(source.host(), "sftp.example.test");
        assert_eq!(source.port(), 22);
        assert_eq!(source.verified_host_key_fingerprint(), "SHA256:abc123");
        assert_eq!(source.remote_path(), "/srv/docs/guide.md");
    }

    #[test]
    fn build_remote_markdown_source_uses_profile_owner_when_a_profile_is_saved() {
        let target = test_launch_target(Some("production"));
        let source = build_remote_markdown_source(&target, "SHA256:abc123", "/etc/motd")
            .expect("valid launch target and fingerprint produce a source");
        assert_eq!(
            source.owner(),
            &RemoteSourceOwner::username_and_profile("deploy", "production")
                .expect("valid owner fields")
        );
    }

    #[test]
    fn build_remote_markdown_source_uses_username_owner_without_a_saved_profile() {
        let target = test_launch_target(None);
        let source = build_remote_markdown_source(&target, "SHA256:abc123", "/etc/motd")
            .expect("valid launch target and fingerprint produce a source");
        assert_eq!(
            source.owner(),
            &RemoteSourceOwner::username("deploy").expect("valid owner fields")
        );
    }

    #[test]
    fn remote_directory_loaded_refreshes_the_cached_verified_fingerprint() {
        let mut tab = test_tab();
        tab.verified_host_key_fingerprint = Some("SHA256:original".to_owned());
        tab.apply_event(WorkerEvent::RemoteDirectoryLoaded {
            snapshot: SftpDirectorySnapshot {
                location: SftpLocation::Remote,
                path: SftpPath::remote("/"),
                loaded_at: SystemTime::now(),
                entries: Vec::new(),
            },
            metadata: None,
            // Simulates a mid-session host-key rotation: a transparent
            // reconnect approved a new key, and the fingerprint that comes
            // back with the next directory load reflects it.
            verified_host_key_fingerprint: "SHA256:rotated".to_owned(),
        });
        assert_eq!(
            tab.verified_host_key_fingerprint.as_deref(),
            Some("SHA256:rotated"),
            "a later RemoteDirectoryLoaded must update the cached fingerprint, \
             not just the one-time Connected event (issue #135)"
        );
    }

    #[test]
    fn markdown_snapshot_loaded_refreshes_the_cached_verified_fingerprint() {
        let mut tab = test_tab();
        tab.verified_host_key_fingerprint = Some("SHA256:original".to_owned());
        tab.pending_markdown_request = Some(PendingMarkdownRequest {
            request_id: 7,
            path: "/srv/docs/guide.md".to_owned(),
        });
        tab.apply_event(WorkerEvent::MarkdownSnapshotLoaded {
            request_id: 7,
            path: "/srv/docs/guide.md".to_owned(),
            content: b"# Guide".to_vec(),
            verified_host_key_fingerprint: "SHA256:rotated".to_owned(),
        });
        assert_eq!(
            tab.verified_host_key_fingerprint.as_deref(),
            Some("SHA256:rotated"),
            "a Markdown snapshot fetched after a reconnect must refresh the \
             cached fingerprint used to pin RemoteMarkdownSource (issue #135)"
        );
        // The refreshed fingerprint is what gets pinned into the resulting
        // RemoteMarkdownSource for this open, not the stale one.
        let opened = tab
            .pending_markdown_open
            .as_ref()
            .expect("a successful snapshot with a cached fingerprint opens the file");
        assert_eq!(
            opened.source.verified_host_key_fingerprint(),
            "SHA256:rotated"
        );
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

    /// Polls `picker` until its pane finishes loading (or a bounded number
    /// of attempts elapses), since directory listing happens on a spawned
    /// background thread.
    fn wait_for_picker_load(picker: &mut MarkdownFilePicker) {
        for _ in 0..200 {
            picker.poll();
            if !picker.pane.loading {
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
        panic!("markdown file picker did not finish loading in time");
    }

    #[test]
    fn markdown_file_picker_loads_its_starting_directory_and_identifies_markdown_files() {
        let dir = std::env::temp_dir().join(format!(
            "festerm-markdown-picker-load-{}",
            std::process::id()
        ));
        fs::create_dir_all(dir.join("subdir")).expect("temp directories should be creatable");
        fs::write(dir.join("readme.md"), b"# Title\n").expect("markdown file should be writable");
        fs::write(dir.join("notes.txt"), b"plain text\n").expect("text file should be writable");

        let mut picker = MarkdownFilePicker::new(dir.clone(), egui::Context::default());
        wait_for_picker_load(&mut picker);

        let entries = picker.pane.visible_entries().to_vec();
        let readme = entries
            .iter()
            .find(|item| item.name == "readme.md")
            .expect("readme.md should be listed")
            .clone();
        assert!(is_markdown_file(&readme));
        let notes = entries
            .iter()
            .find(|item| item.name == "notes.txt")
            .expect("notes.txt should be listed")
            .clone();
        assert!(!is_markdown_file(&notes));
        let subdir = entries
            .iter()
            .find(|item| item.name == "subdir")
            .expect("subdir should be listed")
            .clone();

        // Picking the markdown file reports it as the open outcome...
        match picker.open_item(&readme) {
            MarkdownPickerOutcome::Open(path) => assert_eq!(path, dir.join("readme.md")),
            _ => panic!("expected picking a Markdown file to report MarkdownPickerOutcome::Open"),
        }

        // ...a non-Markdown file is a no-op, matching the SFTP local pane's
        // own double-click handling (issue #133)...
        assert!(matches!(
            picker.open_item(&notes),
            MarkdownPickerOutcome::Pending
        ));
        // ...and a directory navigates into it instead of picking a file.
        assert!(matches!(
            picker.open_item(&subdir),
            MarkdownPickerOutcome::Pending
        ));
        wait_for_picker_load(&mut picker);
        assert_eq!(
            picker.pane.current_path,
            SftpPath::local(dir.join("subdir"))
        );

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn local_directory_loader_coalesces_navigation_to_the_newest_pending_path() {
        let loader = LocalDirectoryLoader::paused_for_test();
        for path in ["/first", "/second", "/newest"] {
            loader.schedule(LocalDirectoryLoadRequest {
                path: SftpPath::local(path),
                complete: Box::new(|_| {}),
            });
        }

        assert_eq!(
            loader.pending_path_for_test(),
            Some(SftpPath::local("/newest"))
        );
    }

    #[test]
    fn bounded_transfer_rejection_is_exposed_without_disconnect() {
        let mut tab = test_tab();
        tab.connection_state = SftpConnectionState::Ready;

        tab.apply_event(WorkerEvent::TransferCommandFailed {
            action: "queue the transfer",
            details: "SFTP transfer queue is saturated".to_owned(),
        });

        assert!(matches!(tab.connection_state, SftpConnectionState::Ready));
        assert_eq!(
            tab.operation_error,
            Some((
                "Could not queue the transfer.".to_owned(),
                "SFTP transfer queue is saturated".to_owned(),
            ))
        );
    }

    #[test]
    fn markdown_file_picker_navigation_moves_up_home_and_back() {
        let dir = std::env::temp_dir().join(format!(
            "festerm-markdown-picker-nav-{}",
            std::process::id()
        ));
        fs::create_dir_all(dir.join("child")).expect("temp directories should be creatable");

        let mut picker = MarkdownFilePicker::new(dir.join("child"), egui::Context::default());
        wait_for_picker_load(&mut picker);
        assert_eq!(picker.pane.current_path, SftpPath::local(dir.join("child")));

        picker.navigate_up();
        wait_for_picker_load(&mut picker);
        assert_eq!(picker.pane.current_path, SftpPath::local(dir.clone()));

        picker.navigate_home();
        wait_for_picker_load(&mut picker);
        assert_eq!(
            picker.pane.current_path,
            SftpPath::local(local_home_directory())
        );

        picker.navigate_back();
        wait_for_picker_load(&mut picker);
        assert_eq!(picker.pane.current_path, SftpPath::local(dir.clone()));

        fs::remove_dir_all(&dir).ok();
    }

    /// Home has to be a real directory on every platform. Windows sets
    /// `USERPROFILE` and not `HOME`, so consulting only `HOME` (as this did)
    /// sent every Home navigation to the filesystem root.
    #[test]
    fn local_home_directory_resolves_to_a_real_directory() {
        let home = local_home_directory();
        assert!(
            home.is_dir(),
            "home directory should exist on this platform: {home:?}"
        );
        assert_ne!(home, PathBuf::from(std::path::MAIN_SEPARATOR_STR));
    }

    /// The picker exposes where it is browsing so the next picker can resume
    /// there instead of starting over at Home.
    #[test]
    fn markdown_file_picker_reports_the_directory_it_is_browsing() {
        let dir = std::env::temp_dir().join(format!(
            "festerm-markdown-picker-dir-{}",
            std::process::id()
        ));
        fs::create_dir_all(dir.join("child")).expect("temp directories should be creatable");

        let mut picker = MarkdownFilePicker::new(dir.join("child"), egui::Context::default());
        wait_for_picker_load(&mut picker);
        assert_eq!(picker.current_directory(), Some(dir.join("child")));

        picker.navigate_up();
        wait_for_picker_load(&mut picker);
        assert_eq!(picker.current_directory(), Some(dir.clone()));

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn enqueue_external_drop_upload_rejects_when_remote_connection_is_not_ready() {
        let mut tab = test_tab();
        tab.connection_state = SftpConnectionState::Connecting;
        let result = tab.enqueue_external_drop_upload(vec![PathBuf::from("/tmp/example.txt")]);
        assert_eq!(
            result,
            Err("The remote SFTP connection is unavailable.".to_owned())
        );
    }

    #[test]
    fn enqueue_external_drop_upload_rejects_a_read_only_remote_destination() {
        let mut tab = test_tab();
        tab.connection_state = SftpConnectionState::Ready;
        // A stale remote pane without directory metadata reports itself as
        // read-only -- see `SftpPaneState::is_writable`.
        tab.remote_pane.stale = true;
        let result = tab.enqueue_external_drop_upload(vec![PathBuf::from("/tmp/example.txt")]);
        assert_eq!(
            result,
            Err("The remote destination is read-only.".to_owned())
        );
    }

    #[test]
    fn enqueue_external_drop_upload_enqueues_one_request_per_dropped_path() {
        let (command_sender, mut command_receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut tab = test_tab();
        tab.command_sender = command_sender;
        tab.connection_state = SftpConnectionState::Ready;
        tab.remote_pane.current_path = SftpPath::remote("/uploads");

        let result = tab.enqueue_external_drop_upload(vec![
            PathBuf::from("/tmp/a.txt"),
            PathBuf::from("/tmp/b.txt"),
        ]);
        assert_eq!(result, Ok(2));

        let WorkerCommand::Enqueue(requests) = command_receiver
            .try_recv()
            .expect("a transfer batch should have been queued")
        else {
            panic!("expected a WorkerCommand::Enqueue");
        };
        assert_eq!(requests.len(), 2);
        assert!(requests
            .iter()
            .all(|request| request.destination == SftpPath::remote("/uploads")));
        assert_eq!(requests[0].source, SftpPath::local("/tmp/a.txt"));
        assert_eq!(requests[1].source, SftpPath::local("/tmp/b.txt"));
    }

    #[test]
    fn pane_drop_should_transfer_only_between_different_panes() {
        // This is exactly the decision `show_pane` makes at the drop site
        // (`if pane_drop_should_transfer(payload.source, focus) { queue_transfer(...) }`);
        // asserting it directly (rather than only via a full render pass)
        // catches a flipped comparison regardless of whether a future
        // refactor still routes through egui's `DragAndDrop` plugin.
        assert!(pane_drop_should_transfer(
            PaneFocus::Local,
            PaneFocus::Remote
        ));
        assert!(pane_drop_should_transfer(
            PaneFocus::Remote,
            PaneFocus::Local
        ));
        assert!(!pane_drop_should_transfer(
            PaneFocus::Local,
            PaneFocus::Local
        ));
        assert!(!pane_drop_should_transfer(
            PaneFocus::Remote,
            PaneFocus::Remote
        ));
    }

    #[test]
    fn cross_pane_drag_drop_enqueues_the_dragged_selection_but_same_pane_does_not() {
        let (command_sender, mut command_receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut tab = test_tab();
        tab.command_sender = command_sender;
        tab.connection_state = SftpConnectionState::Ready;
        tab.local_pane.set_snapshot(
            SftpDirectorySnapshot {
                location: SftpLocation::Local,
                path: SftpPath::local("/local/dir"),
                loaded_at: SystemTime::now(),
                entries: vec![item("file.txt", SftpEntryType::File, Some(4))],
            },
            None,
        );
        tab.remote_pane.current_path = SftpPath::remote("/remote/dir");
        tab.local_pane.select_single(&SftpPath::local("file.txt"));

        // A drop that lands back on its own source pane (Local -> Local) is
        // a deliberate no-op: nothing should be enqueued.
        if pane_drop_should_transfer(PaneFocus::Local, PaneFocus::Local) {
            tab.queue_transfer(PaneFocus::Local);
        }
        assert!(
            command_receiver.try_recv().is_err(),
            "a same-pane drop must not enqueue a transfer"
        );

        // A drop on the *other* pane (Local -> Remote) enqueues exactly what
        // `queue_transfer` would for the current selection.
        if pane_drop_should_transfer(PaneFocus::Local, PaneFocus::Remote) {
            tab.queue_transfer(PaneFocus::Local);
        }
        let WorkerCommand::Enqueue(requests) = command_receiver
            .try_recv()
            .expect("a cross-pane drop should enqueue a transfer")
        else {
            panic!("expected a WorkerCommand::Enqueue");
        };
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].source, SftpPath::local("file.txt"));
        assert_eq!(requests[0].destination, SftpPath::remote("/remote/dir"));
    }

    #[test]
    fn build_reveal_command_targets_the_given_path_on_this_platform() {
        let path = PathBuf::from("/tmp/reveal-me.txt");
        let command = build_reveal_command(&path);
        #[cfg(target_os = "macos")]
        {
            assert_eq!(command.program, "open");
            assert_eq!(
                command.args,
                vec![OsString::from("-R"), path.into_os_string()]
            );
        }
        #[cfg(target_os = "windows")]
        {
            assert_eq!(command.program, "explorer.exe");
            let mut expected = OsString::from("/select,");
            expected.push(path.as_os_str());
            assert_eq!(command.args, vec![expected]);
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            assert_eq!(command.program, "xdg-open");
            // `/tmp/reveal-me.txt` doesn't exist, so `Path::is_dir` is false
            // and the fallback opens its parent folder.
            assert_eq!(command.args, vec![OsString::from("/tmp")]);
        }
    }

    #[test]
    fn reveal_in_file_manager_label_names_the_current_platform_action() {
        let label = reveal_in_file_manager_label();
        #[cfg(target_os = "macos")]
        assert_eq!(label, "Reveal in Finder");
        #[cfg(target_os = "windows")]
        assert_eq!(label, "Show in File Explorer");
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        assert_eq!(label, "Open Containing Folder");
    }
}
