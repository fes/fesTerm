use std::{
    ops::Range,
    path::{Path, PathBuf},
};

use festerm_core::{ContentPosition, Terminal};
use festerm_document::{DocumentBounds, RemoteOrigin, RemoteOwner, TextDocument};
use festerm_markdown::{RemoteMarkdownSource, RemoteSourceOwner};
use festerm_ssh::{LiveRemoteFileRequestor, RemoteFileReadError, SftpEntryType, SftpPath};
use festerm_ui_egui::{TerminalContextMenuAction, TerminalContextTarget, TerminalSnapshot};

use crate::overlay_state::OpenRefusalNotice;

const MAX_LOGICAL_ROWS: usize = 16;
const MAX_LOGICAL_BYTES: usize = 8 * 1024;

#[derive(Debug)]
pub(crate) enum TerminalFilesystemOrigin {
    Local(LocalTerminalOrigin),
    Remote(RemoteTerminalOrigin),
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct LocalTerminalOrigin {
    pub home_directory: Option<PathBuf>,
}

#[derive(Debug)]
pub(crate) struct RemoteTerminalOrigin {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub profile_identifier: Option<String>,
    pub lifecycle_generation: u64,
    pub verified_host_key_fingerprint: Option<String>,
    pub live_transport_available: bool,
    pub trusted_working_directory: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct TerminalPathMenuState {
    generation: u64,
    action: TerminalResolvedPathAction,
}

impl TerminalPathMenuState {
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    pub(crate) fn ui_action(&self) -> TerminalContextMenuAction {
        self.action.ui.clone()
    }

    pub(crate) fn open_request(&self) -> Option<TerminalPathOpenRequest> {
        self.action.request.clone()
    }
}

#[derive(Clone, Debug)]
struct TerminalResolvedPathAction {
    ui: TerminalContextMenuAction,
    request: Option<TerminalPathOpenRequest>,
}

#[derive(Debug)]
pub(crate) enum TerminalPathOpenRequest {
    Local(LocalTerminalPathOpenRequest),
    Remote(RemoteTerminalPathOpenRequest),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LocalTerminalPathOpenRequest {
    pub path: PathBuf,
    pub display_path: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RemoteTerminalPathOpenRequest {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub profile_identifier: Option<String>,
    pub lifecycle_generation: u64,
    pub remote_path: String,
    pub display_path: String,
}

#[derive(Clone, Debug)]
pub(crate) struct LiveRemoteTerminalPathOpenRequest {
    pub requestor: LiveRemoteFileRequestor,
    pub verified_host_key_fingerprint: String,
    pub request: RemoteTerminalPathOpenRequest,
}

#[derive(Debug)]
pub(crate) enum TerminalPathWorkerResult {
    Command(Box<crate::tabs::AppCommand>),
    OpenRefusal(OpenRefusalNotice),
}

impl Clone for TerminalFilesystemOrigin {
    fn clone(&self) -> Self {
        match self {
            Self::Local(local) => Self::Local(local.clone()),
            Self::Remote(remote) => Self::Remote(remote.clone()),
        }
    }
}

impl Clone for RemoteTerminalOrigin {
    fn clone(&self) -> Self {
        Self {
            host: self.host.clone(),
            port: self.port,
            username: self.username.clone(),
            profile_identifier: self.profile_identifier.clone(),
            lifecycle_generation: self.lifecycle_generation,
            verified_host_key_fingerprint: self.verified_host_key_fingerprint.clone(),
            live_transport_available: self.live_transport_available,
            trusted_working_directory: self.trusted_working_directory.clone(),
        }
    }
}

impl PartialEq for TerminalFilesystemOrigin {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Local(left), Self::Local(right)) => left == right,
            (Self::Remote(left), Self::Remote(right)) => {
                left.host == right.host
                    && left.port == right.port
                    && left.username == right.username
                    && left.profile_identifier == right.profile_identifier
                    && left.lifecycle_generation == right.lifecycle_generation
                    && left.verified_host_key_fingerprint == right.verified_host_key_fingerprint
                    && left.live_transport_available == right.live_transport_available
                    && left.trusted_working_directory == right.trusted_working_directory
            }
            _ => false,
        }
    }
}

impl Eq for TerminalFilesystemOrigin {}

impl PartialEq for RemoteTerminalOrigin {
    fn eq(&self, other: &Self) -> bool {
        TerminalFilesystemOrigin::Remote(self.clone())
            == TerminalFilesystemOrigin::Remote(other.clone())
    }
}

impl Eq for RemoteTerminalOrigin {}

impl Clone for TerminalPathOpenRequest {
    fn clone(&self) -> Self {
        match self {
            Self::Local(request) => Self::Local(request.clone()),
            Self::Remote(request) => Self::Remote(request.clone()),
        }
    }
}

impl PartialEq for TerminalPathMenuState {
    fn eq(&self, other: &Self) -> bool {
        self.generation == other.generation && self.action == other.action
    }
}

impl Eq for TerminalPathMenuState {}

impl PartialEq for TerminalResolvedPathAction {
    fn eq(&self, other: &Self) -> bool {
        self.ui == other.ui
    }
}

impl Eq for TerminalResolvedPathAction {}

pub(crate) fn resolve_context_menu_action(
    terminal: &Terminal,
    target: TerminalContextTarget,
    origin: &TerminalFilesystemOrigin,
) -> Option<TerminalPathMenuState> {
    let hit = detect_terminal_path_hit(terminal, target.content_position)?;
    let action = resolve_candidate(hit, origin);
    Some(TerminalPathMenuState {
        generation: target.generation,
        action,
    })
}

pub(crate) fn open_local_command(
    request: &LocalTerminalPathOpenRequest,
) -> crate::tabs::AppCommand {
    if is_markdown_path(request.path.to_string_lossy().as_ref()) {
        crate::tabs::AppCommand::OpenLocalMarkdownFile {
            path: request.path.clone(),
            replacing: None,
        }
    } else {
        crate::tabs::AppCommand::OpenTextEditor {
            path: request.path.clone(),
        }
    }
}

pub(crate) fn bind_live_remote_request(
    requestor: LiveRemoteFileRequestor,
    request: RemoteTerminalPathOpenRequest,
) -> Result<LiveRemoteTerminalPathOpenRequest, OpenRefusalNotice> {
    if requestor.transport_generation() != request.lifecycle_generation {
        return Err(remote_open_refusal(
            &request.remote_path,
            &request.display_path,
            "This remote path cannot be opened",
            "The source transport has changed since the menu opened.",
        ));
    }
    let Some(verified_host_key_fingerprint) = requestor.verified_host_key_fingerprint() else {
        return Err(remote_open_refusal(
            &request.remote_path,
            &request.display_path,
            "This remote path cannot be opened",
            "The live session no longer has a verified host identity. Reconnect it before opening remote files.",
        ));
    };
    Ok(LiveRemoteTerminalPathOpenRequest {
        requestor,
        verified_host_key_fingerprint,
        request,
    })
}

pub(crate) fn open_remote_request(
    request: LiveRemoteTerminalPathOpenRequest,
) -> TerminalPathWorkerResult {
    read_remote_document(request)
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct TerminalPathHit {
    text: String,
}

fn detect_terminal_path_hit(
    terminal: &Terminal,
    position: ContentPosition,
) -> Option<TerminalPathHit> {
    let snapshot = TerminalSnapshot::from_terminal_viewport(terminal, 0);
    let capture = capture_logical_line(snapshot, position)?;
    let candidate = candidate_from_line(&capture.text, capture.clicked_range)?;
    Some(TerminalPathHit {
        text: candidate.text,
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CapturedLogicalLine {
    text: String,
    clicked_range: Range<usize>,
}

fn capture_logical_line(
    snapshot: TerminalSnapshot<'_>,
    position: ContentPosition,
) -> Option<CapturedLogicalLine> {
    if !snapshot.contains_content_row(position.absolute_row) {
        return None;
    }

    let mut first = position.absolute_row;
    let mut wrapped_rows = 1usize;
    while first > 0 && wrapped_rows < MAX_LOGICAL_ROWS {
        let previous = first - 1;
        if !snapshot.contains_content_row(previous)
            || !snapshot.absolute_row_soft_wrapped(previous)?
        {
            break;
        }
        first = previous;
        wrapped_rows += 1;
    }

    let mut rows = Vec::new();
    let mut row = first;
    loop {
        rows.push(row);
        if wrapped_rows >= MAX_LOGICAL_ROWS
            || !snapshot.absolute_row_soft_wrapped(row).unwrap_or(false)
        {
            break;
        }
        let next = snapshot.next_content_row(row)?;
        if next == row || !snapshot.contains_content_row(next) {
            break;
        }
        row = next;
        wrapped_rows += 1;
    }

    let mut text = String::new();
    let mut clicked_range = None;
    let columns = snapshot.dimensions().columns();
    for content_row in rows {
        for column in 0..columns {
            let cell = snapshot.absolute_cell(column, content_row)?;
            if cell.is_continuation() {
                continue;
            }
            let start = text.len();
            let cell_text = cell.text();
            text.push_str(cell_text);
            let end = text.len();
            let width = cell.width().columns().max(1);
            let clicked_column = position.column;
            if content_row == position.absolute_row
                && clicked_column >= column
                && clicked_column < column + width
            {
                clicked_range = Some(start..end);
            }
            if text.len() > MAX_LOGICAL_BYTES {
                return None;
            }
        }
    }

    Some(CapturedLogicalLine {
        text,
        clicked_range: clicked_range?,
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PathCandidate {
    text: String,
}

fn candidate_from_line(line: &str, clicked_range: Range<usize>) -> Option<PathCandidate> {
    markdown_link_candidate(line, clicked_range.start)
        .or_else(|| quoted_candidate(line, clicked_range.start))
        .or_else(|| shell_token_candidate(line, clicked_range.start))
        .and_then(|raw| normalize_candidate_text(&raw))
        .filter(|candidate| looks_like_path(candidate))
        .map(|text| PathCandidate { text })
}

fn markdown_link_candidate(line: &str, clicked: usize) -> Option<String> {
    let before = line.get(..clicked)?;
    let open = before.rfind("](")?;
    let path_start = open + 2;
    let after = line.get(path_start..)?;
    let close_offset = after.find(')')?;
    let path_end = path_start + close_offset;
    (clicked >= path_start && clicked <= path_end).then(|| line[path_start..path_end].to_owned())
}

fn quoted_candidate(line: &str, clicked: usize) -> Option<String> {
    ['"', '\'']
        .into_iter()
        .filter_map(|quote| {
            let start = find_quote_start(line, clicked, quote)?;
            let end = find_quote_end(line, clicked, quote)?;
            (start < end && clicked >= start && clicked <= end)
                .then(|| (end - start, line[start..=end].to_owned()))
        })
        .min_by_key(|(width, _)| *width)
        .map(|(_, text)| text)
}

fn find_quote_start(line: &str, clicked: usize, quote: char) -> Option<usize> {
    line.char_indices()
        .take_while(|(index, _)| *index <= clicked)
        .filter(|(_, ch)| *ch == quote)
        .map(|(index, _)| index)
        .last()
}

fn find_quote_end(line: &str, clicked: usize, quote: char) -> Option<usize> {
    line.char_indices()
        .skip_while(|(index, _)| *index <= clicked)
        .find(|(_, ch)| *ch == quote)
        .map(|(index, _)| index)
}

fn shell_token_candidate(line: &str, clicked: usize) -> Option<String> {
    let mut token_start = None;
    let mut quote = None;
    let mut escaped = false;
    for (index, ch) in line.char_indices() {
        if token_start.is_none() {
            if ch.is_whitespace() || command_separator(ch) {
                continue;
            }
            token_start = Some(index);
        }

        if escaped {
            escaped = false;
            continue;
        }

        match quote {
            Some(active) if ch == active => {
                quote = None;
            }
            Some('"') if ch == '\\' => {
                escaped = true;
            }
            Some(_) => {}
            None if ch == '\\' => {
                escaped = true;
            }
            None if ch == '"' || ch == '\'' => {
                quote = Some(ch);
            }
            None if ch.is_whitespace() || command_separator(ch) => {
                let start = token_start.take()?;
                if clicked >= start && clicked < index {
                    return Some(line[start..index].to_owned());
                }
            }
            None => {}
        }
    }
    token_start.and_then(|start| {
        (clicked >= start && clicked <= line.len()).then(|| line[start..].to_owned())
    })
}

fn command_separator(ch: char) -> bool {
    matches!(ch, '|' | '&' | ';')
}

fn normalize_candidate_text(raw: &str) -> Option<String> {
    let mut candidate = raw.trim().to_owned();
    if candidate.is_empty() {
        return None;
    }

    loop {
        let stripped = strip_balanced_wrapper(&candidate).unwrap_or(candidate.clone());
        if stripped == candidate {
            break;
        }
        candidate = stripped;
    }

    if let Some(unquoted) = unquote(&candidate) {
        let literal =
            candidate.starts_with('\'') || is_windows_absolute(&unquoted) || is_unc_path(&unquoted);
        let text = if literal {
            unquoted
        } else {
            unescape_shellish(&unquoted)
        };
        return (!text.is_empty()).then_some(text);
    }

    candidate = strip_edge_punctuation(candidate);
    if candidate.is_empty() {
        return None;
    }

    if let Some(unquoted) = unquote(&candidate) {
        candidate = unquoted;
    }
    if !is_windows_absolute(&candidate) && !is_unc_path(&candidate) {
        candidate = unescape_shellish(&candidate);
    }
    candidate = strip_edge_punctuation(candidate);

    let candidate = candidate.trim().to_owned();
    (!candidate.is_empty()).then_some(candidate)
}

fn strip_balanced_wrapper(candidate: &str) -> Option<String> {
    let pairs = [('(', ')'), ('[', ']'), ('{', '}'), ('<', '>')];
    pairs.into_iter().find_map(|(open, close)| {
        candidate
            .strip_prefix(open)
            .and_then(|rest| rest.strip_suffix(close))
            .map(str::to_owned)
    })
}

fn strip_edge_punctuation(mut candidate: String) -> String {
    const LEADING: &[char] = &['"', '\'', '(', '[', '{', '<'];
    const TRAILING: &[char] = &['"', '\'', ')', ']', '}', '>', ',', '.', ':', ';', '!', '?'];
    while candidate.starts_with(LEADING) {
        candidate.remove(0);
    }
    while candidate.ends_with(TRAILING) {
        candidate.pop();
    }
    candidate
}

fn unquote(candidate: &str) -> Option<String> {
    if candidate.len() < 2 {
        return None;
    }
    let first = candidate.chars().next()?;
    let last = candidate.chars().last()?;
    ((first == '"' || first == '\'') && first == last)
        .then(|| candidate[first.len_utf8()..candidate.len() - last.len_utf8()].to_owned())
}

fn unescape_shellish(candidate: &str) -> String {
    let mut normalized = String::with_capacity(candidate.len());
    let mut chars = candidate.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            if let Some(next) = chars.next() {
                if matches!(
                    next,
                    ' ' | '\t' | '\\' | '"' | '\'' | '(' | ')' | '[' | ']' | '{' | '}'
                ) {
                    normalized.push(next);
                } else {
                    normalized.push('\\');
                    normalized.push(next);
                }
            }
        } else {
            normalized.push(ch);
        }
    }
    normalized
}

fn looks_like_path(candidate: &str) -> bool {
    if candidate.is_empty() || candidate.starts_with('$') || candidate.starts_with('%') {
        return false;
    }
    if candidate.contains("$(") || candidate.contains('`') || candidate.contains("${") {
        return false;
    }
    candidate.starts_with("file://")
        || candidate.starts_with('/')
        || candidate.starts_with("~/")
        || candidate == "~"
        || candidate.starts_with("./")
        || candidate.starts_with("../")
        || candidate.starts_with(".\\")
        || candidate.starts_with("..\\")
        || is_windows_absolute(candidate)
        || is_unc_path(candidate)
        || candidate.contains('/')
        || candidate.contains('\\')
        || looks_like_relative_filename(candidate)
}

fn looks_like_relative_filename(candidate: &str) -> bool {
    let lower = candidate.to_ascii_lowercase();
    candidate.starts_with('.')
        || candidate.contains('.')
        || matches!(
            lower.as_str(),
            "readme" | "license" | "makefile" | "dockerfile" | "cargo.toml"
        )
}

fn resolve_candidate(
    hit: TerminalPathHit,
    origin: &TerminalFilesystemOrigin,
) -> TerminalResolvedPathAction {
    match origin {
        TerminalFilesystemOrigin::Local(local) => resolve_local_candidate(&hit.text, local),
        TerminalFilesystemOrigin::Remote(remote) => resolve_remote_candidate(&hit.text, remote),
    }
}

fn resolve_local_candidate(
    candidate: &str,
    origin: &LocalTerminalOrigin,
) -> TerminalResolvedPathAction {
    let parsed = match parse_candidate_reference(candidate) {
        Ok(parsed) => parsed,
        Err(error) => return disabled_action(candidate.to_owned(), error.detail()),
    };
    match parsed {
        ParsedPathReference::FileUri(uri) => {
            if let Some(host) = &uri.host {
                if !host.eq_ignore_ascii_case("localhost") {
                    return disabled_action(
                        candidate.to_owned(),
                        "This file URI names another host, so fesTerm will not open it as a local file.",
                    );
                }
            }
            if cfg!(windows) {
                if let Some(path) = uri.path.strip_prefix('/').filter(|path| is_windows_absolute(path)) {
                    return enabled_local_action(PathBuf::from(path));
                }
            }
            resolve_local_candidate(&uri.path, origin)
        }
        ParsedPathReference::HomeRelative(suffix) => {
            let Some(home) = origin.home_directory.as_ref() else {
                return disabled_action(
                    candidate.to_owned(),
                    "fesTerm does not know this machine's home directory, so `~` cannot be expanded here.",
                );
            };
            let path = join_home(home, &suffix);
            enabled_local_action(path)
        }
        ParsedPathReference::AbsolutePosix(path) => enabled_local_action(PathBuf::from(path)),
        ParsedPathReference::AbsoluteWindows(path) => {
            if cfg!(windows) {
                enabled_local_action(PathBuf::from(path))
            } else {
                disabled_action(
                    candidate.to_owned(),
                    "Windows drive and UNC paths can only be opened locally on Windows.",
                )
            }
        }
        ParsedPathReference::Relative(relative) => disabled_action(
            relative,
            "Relative terminal paths stay disabled here because fesTerm does not know the live shell working directory.",
        ),
    }
}

fn resolve_remote_candidate(
    candidate: &str,
    origin: &RemoteTerminalOrigin,
) -> TerminalResolvedPathAction {
    let parsed = match parse_candidate_reference(candidate) {
        Ok(parsed) => parsed,
        Err(error) => return disabled_action(candidate.to_owned(), error.detail()),
    };
    let resolved = match parsed {
        ParsedPathReference::FileUri(uri) => {
            if let Some(host) = &uri.host {
                if !same_remote_host(host, &origin.host) {
                    return disabled_action(
                        format!("{}@{} · {}", origin.username, origin.host, candidate),
                        "This file URI points at a different host, so fesTerm will not reuse this SSH session for it.",
                    );
                }
            }
            uri.path
        }
        ParsedPathReference::HomeRelative(_) => {
            return disabled_action(
                format!("{}@{} · {}", origin.username, origin.host, candidate),
                "Remote `~` paths stay disabled until the session reports a trustworthy remote home directory.",
            );
        }
        ParsedPathReference::AbsolutePosix(path) => path,
        ParsedPathReference::AbsoluteWindows(path) => remote_windows_to_sftp_path(&path),
        ParsedPathReference::Relative(relative) => {
            let Some(base) = origin.trusted_working_directory.as_ref() else {
                return disabled_action(
                    format!("{}@{} · {}", origin.username, origin.host, relative),
                    "Relative remote paths stay disabled until fesTerm has a trustworthy current directory (for example a text-mode SFTP session's own cwd).",
                );
            };
            resolve_remote_path(base, &relative)
        }
    };

    if origin.verified_host_key_fingerprint.is_none() {
        return disabled_action(
            format!("{}@{} · {}", origin.username, origin.host, resolved),
            "Open the host once through SSH or SFTP first so fesTerm can pin a verified host key before reading files.",
        );
    }
    if !origin.live_transport_available {
        return disabled_action(
            format!("{}@{} · {}", origin.username, origin.host, resolved),
            "This remote session is not currently connected, so fesTerm cannot reuse its verified SSH/SFTP transport to read files.",
        );
    }

    let display_path = format!("{}@{} · {}", origin.username, origin.host, resolved);
    TerminalResolvedPathAction {
        ui: TerminalContextMenuAction {
            label: "Open in viewer".to_owned(),
            preview: display_path.clone(),
            enabled: true,
            disabled_reason: None,
        },
        request: Some(TerminalPathOpenRequest::Remote(
            RemoteTerminalPathOpenRequest {
                host: origin.host.clone(),
                port: origin.port,
                username: origin.username.clone(),
                profile_identifier: origin.profile_identifier.clone(),
                lifecycle_generation: origin.lifecycle_generation,
                remote_path: resolved,
                display_path,
            },
        )),
    }
}

fn enabled_local_action(path: PathBuf) -> TerminalResolvedPathAction {
    let display_path = path.display().to_string();
    TerminalResolvedPathAction {
        ui: TerminalContextMenuAction {
            label: "Open in viewer".to_owned(),
            preview: display_path.clone(),
            enabled: true,
            disabled_reason: None,
        },
        request: Some(TerminalPathOpenRequest::Local(
            LocalTerminalPathOpenRequest { path, display_path },
        )),
    }
}

fn disabled_action(preview: String, reason: impl Into<String>) -> TerminalResolvedPathAction {
    TerminalResolvedPathAction {
        ui: TerminalContextMenuAction {
            label: "Open in viewer".to_owned(),
            preview,
            enabled: false,
            disabled_reason: Some(reason.into()),
        },
        request: None,
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ParsedPathReference {
    FileUri(FileUriReference),
    HomeRelative(String),
    AbsolutePosix(String),
    AbsoluteWindows(String),
    Relative(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct FileUriReference {
    host: Option<String>,
    path: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PathReferenceParseError {
    InvalidFileUri,
    InvalidFileUriEncoding,
    FileUriContainsControlCharacters,
}

impl PathReferenceParseError {
    fn detail(self) -> &'static str {
        match self {
            Self::InvalidFileUri => {
                "This file URI is incomplete, so fesTerm cannot resolve it to a safe path."
            }
            Self::InvalidFileUriEncoding => {
                "This file URI contains invalid percent-encoded UTF-8 bytes."
            }
            Self::FileUriContainsControlCharacters => {
                "This file URI decodes to control characters, so fesTerm will not open it."
            }
        }
    }
}

fn parse_candidate_reference(
    candidate: &str,
) -> Result<ParsedPathReference, PathReferenceParseError> {
    if candidate.starts_with("file://") {
        return parse_file_uri(candidate).map(ParsedPathReference::FileUri);
    }
    if candidate == "~" {
        return Ok(ParsedPathReference::HomeRelative(String::new()));
    }
    if let Some(suffix) = candidate
        .strip_prefix("~/")
        .or_else(|| candidate.strip_prefix("~\\"))
    {
        return Ok(ParsedPathReference::HomeRelative(suffix.to_owned()));
    }
    if candidate.starts_with('/') {
        return Ok(ParsedPathReference::AbsolutePosix(candidate.to_owned()));
    }
    if is_windows_absolute(candidate) || is_unc_path(candidate) {
        return Ok(ParsedPathReference::AbsoluteWindows(candidate.to_owned()));
    }
    Ok(ParsedPathReference::Relative(candidate.to_owned()))
}

fn parse_file_uri(candidate: &str) -> Result<FileUriReference, PathReferenceParseError> {
    let rest = candidate
        .strip_prefix("file://")
        .ok_or(PathReferenceParseError::InvalidFileUri)?;
    let (host, path) = if rest.starts_with('/') {
        (None, rest.to_owned())
    } else {
        let (host, path) = rest
            .split_once('/')
            .ok_or(PathReferenceParseError::InvalidFileUri)?;
        (Some(host.to_owned()), format!("/{path}"))
    };
    Ok(FileUriReference {
        host,
        path: percent_decode(&path)?,
    })
}

fn percent_decode(path: &str) -> Result<String, PathReferenceParseError> {
    let mut decoded = Vec::with_capacity(path.len());
    let bytes = path.as_bytes();
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return Err(PathReferenceParseError::InvalidFileUriEncoding);
            }
            let hex = std::str::from_utf8(&bytes[index + 1..index + 3])
                .map_err(|_| PathReferenceParseError::InvalidFileUriEncoding)?;
            match u8::from_str_radix(hex, 16) {
                Ok(value) => {
                    decoded.push(value);
                    index += 3;
                    continue;
                }
                Err(_) => return Err(PathReferenceParseError::InvalidFileUriEncoding),
            }
        }
        decoded.push(bytes[index]);
        index += 1;
    }
    let decoded =
        String::from_utf8(decoded).map_err(|_| PathReferenceParseError::InvalidFileUriEncoding)?;
    if decoded.chars().any(char::is_control) {
        return Err(PathReferenceParseError::FileUriContainsControlCharacters);
    }
    Ok(decoded)
}

fn is_windows_absolute(candidate: &str) -> bool {
    let bytes = candidate.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'/' || bytes[2] == b'\\')
}

fn is_unc_path(candidate: &str) -> bool {
    candidate.starts_with("\\\\") || candidate.starts_with("//")
}

fn normalize_posix_path(candidate: &str) -> String {
    let mut components = Vec::new();
    for component in candidate.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                let _ = components.pop();
            }
            other => components.push(other),
        }
    }
    if components.is_empty() {
        "/".to_owned()
    } else {
        format!("/{}", components.join("/"))
    }
}

fn resolve_remote_path(base: &str, relative: &str) -> String {
    let trimmed = relative.trim();
    if trimmed.starts_with('/') {
        return normalize_posix_path(trimmed);
    }
    if base == "/" {
        normalize_posix_path(&format!("/{trimmed}"))
    } else {
        normalize_posix_path(&format!("{base}/{trimmed}"))
    }
}

fn remote_windows_to_sftp_path(candidate: &str) -> String {
    if let Some(rest) = candidate.strip_prefix("\\\\") {
        format!("/{}", rest.replace('\\', "/"))
    } else if candidate.len() >= 3 && candidate.as_bytes()[1] == b':' {
        format!("/{}", candidate.replace('\\', "/"))
    } else {
        candidate.replace('\\', "/")
    }
}

fn join_home(home: &Path, suffix: &str) -> PathBuf {
    if suffix.is_empty() {
        home.to_path_buf()
    } else {
        home.join(suffix.trim_start_matches(std::path::is_separator))
    }
}

fn same_remote_host(candidate: &str, expected: &str) -> bool {
    candidate.trim().eq_ignore_ascii_case(expected)
        || candidate.trim().eq_ignore_ascii_case("localhost")
            && expected.eq_ignore_ascii_case("localhost")
}

pub(crate) fn is_markdown_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.ends_with(".md") || lower.ends_with(".markdown") || lower.ends_with(".mdown")
}

fn read_remote_document(request: LiveRemoteTerminalPathOpenRequest) -> TerminalPathWorkerResult {
    let snapshot = match request
        .requestor
        .read_remote_file_snapshot(&request.request.remote_path, DocumentBounds::MAX_BYTES)
    {
        Ok(snapshot) => snapshot,
        Err(error) => {
            return TerminalPathWorkerResult::OpenRefusal(remote_read_error_notice(
                &request.request.remote_path,
                &request.request.display_path,
                error,
            ));
        }
    };
    let (metadata, bytes) = snapshot.into_parts();
    let remote_path = match &metadata.path {
        SftpPath::Remote(path) => path.clone(),
        SftpPath::Local(_) => request.request.remote_path.clone(),
    };

    if is_markdown_path(&remote_path) {
        let source = match build_remote_markdown_source(&request, &remote_path) {
            Some(source) => source,
            None => {
                return TerminalPathWorkerResult::OpenRefusal(remote_open_refusal(
                    &remote_path,
                    &request.request.display_path,
                    "This path cannot be opened",
                    "The remote file identity could not be constructed safely.",
                ));
            }
        };
        return TerminalPathWorkerResult::Command(Box::new(
            crate::tabs::AppCommand::OpenRemoteMarkdownSnapshot {
                source,
                display_path: request.request.display_path,
                content: bytes,
            },
        ));
    }

    let text = match TextDocument::from_bytes(&bytes, DocumentBounds::DEFAULT) {
        Ok(text) => text,
        Err(reason) => {
            return TerminalPathWorkerResult::OpenRefusal(remote_open_refusal(
                &remote_path,
                &request.request.display_path,
                reason.headline(),
                reason.detail(),
            ));
        }
    };
    let origin = match build_remote_document_origin(&request, &remote_path) {
        Some(origin) => origin,
        None => {
            return TerminalPathWorkerResult::OpenRefusal(remote_open_refusal(
                &remote_path,
                &request.request.display_path,
                "This path cannot be opened",
                "The remote file identity could not be constructed safely.",
            ));
        }
    };
    TerminalPathWorkerResult::Command(Box::new(crate::tabs::AppCommand::OpenRemoteTextSnapshot {
        origin,
        text,
        read_only: true,
    }))
}

fn build_remote_document_origin(
    request: &LiveRemoteTerminalPathOpenRequest,
    remote_path: &str,
) -> Option<RemoteOrigin> {
    let owner = build_remote_owner(&request.request)?;
    RemoteOrigin::new(
        request.request.host.clone(),
        request.request.port,
        owner,
        request.verified_host_key_fingerprint.clone(),
        remote_path.to_owned(),
        request.request.lifecycle_generation,
    )
    .ok()
}

fn build_remote_markdown_source(
    request: &LiveRemoteTerminalPathOpenRequest,
    remote_path: &str,
) -> Option<RemoteMarkdownSource> {
    let owner = build_remote_markdown_owner(&request.request)?;
    RemoteMarkdownSource::new(
        request.request.host.clone(),
        request.request.port,
        owner,
        request.verified_host_key_fingerprint.clone(),
        remote_path.to_owned(),
        request.request.lifecycle_generation,
    )
    .ok()
}

fn build_remote_owner(request: &RemoteTerminalPathOpenRequest) -> Option<RemoteOwner> {
    match &request.profile_identifier {
        Some(profile_id) => {
            RemoteOwner::username_and_profile(request.username.clone(), profile_id.clone()).ok()
        }
        None => RemoteOwner::username(request.username.clone()).ok(),
    }
}

fn build_remote_markdown_owner(
    request: &RemoteTerminalPathOpenRequest,
) -> Option<RemoteSourceOwner> {
    match &request.profile_identifier {
        Some(profile_id) => {
            RemoteSourceOwner::username_and_profile(request.username.clone(), profile_id.clone())
                .ok()
        }
        None => RemoteSourceOwner::username(request.username.clone()).ok(),
    }
}

fn remote_read_error_notice(
    remote_path: &str,
    display_path: &str,
    error: RemoteFileReadError,
) -> OpenRefusalNotice {
    match error {
        RemoteFileReadError::NotRunning => remote_open_refusal(
            remote_path,
            display_path,
            "This remote session is no longer connected",
            "Reconnect the source SSH or SFTP session before opening remote files from terminal output.",
        ),
        RemoteFileReadError::QueueFull => remote_open_refusal(
            remote_path,
            display_path,
            "This remote session is busy",
            "Try the command again once the source SSH/SFTP session has finished its current work.",
        ),
        RemoteFileReadError::TimedOut => remote_open_refusal(
            remote_path,
            display_path,
            "Opening the remote file timed out",
            "The SFTP operation did not finish within 30 seconds. The terminal remains connected; retry when the host is responsive.",
        ),
        RemoteFileReadError::InvalidRequest => remote_open_refusal(
            remote_path,
            display_path,
            "This remote file request exceeds its limits",
            "Use a supported path and the bounded document loader.",
        ),
        RemoteFileReadError::Closed => remote_open_refusal(
            remote_path,
            display_path,
            "This remote session is no longer available",
            "fesTerm lost the live SSH/SFTP transport before it could read this file.",
        ),
        RemoteFileReadError::Missing { .. } => remote_open_refusal(
            remote_path,
            display_path,
            "This file no longer exists",
            "It may have been moved, renamed, or deleted since it was referenced.",
        ),
        RemoteFileReadError::NotFile {
            file_type: SftpEntryType::Directory,
            ..
        } => remote_open_refusal(
            remote_path,
            display_path,
            "This is a directory",
            "Only regular files can be opened in the text or Markdown viewer.",
        ),
        RemoteFileReadError::NotFile { .. } => remote_open_refusal(
            remote_path,
            display_path,
            "This is not a file",
            "Only regular files can be opened in the text or Markdown viewer.",
        ),
        RemoteFileReadError::Sftp(error) => remote_open_refusal(
            remote_path,
            display_path,
            "This remote file could not be opened",
            format!("fesTerm could not read the remote file: {error}"),
        ),
    }
}

fn remote_open_refusal(
    remote_path: &str,
    display_path: &str,
    headline: impl Into<String>,
    detail: impl Into<String>,
) -> OpenRefusalNotice {
    OpenRefusalNotice {
        name: Path::new(remote_path)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| remote_path.to_owned()),
        path: display_path.to_owned(),
        headline: headline.into(),
        detail: detail.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use festerm_core::Dimensions;

    fn terminal_with(text: &str, columns: usize) -> Terminal {
        let dimensions = Dimensions::new(columns, 24).unwrap();
        let mut terminal = Terminal::new(dimensions).unwrap();
        terminal.ingest(text.as_bytes());
        terminal
    }

    fn local_origin() -> TerminalFilesystemOrigin {
        TerminalFilesystemOrigin::Local(LocalTerminalOrigin {
            home_directory: Some(PathBuf::from("/Users/fes")),
        })
    }

    fn remote_origin(cwd: Option<&str>) -> TerminalFilesystemOrigin {
        TerminalFilesystemOrigin::Remote(RemoteTerminalOrigin {
            host: "ssh.example.test".to_owned(),
            port: 22,
            username: "deploy".to_owned(),
            profile_identifier: Some("staging".to_owned()),
            lifecycle_generation: 1,
            verified_host_key_fingerprint: Some("SHA256:abc123".to_owned()),
            live_transport_available: true,
            trusted_working_directory: cwd.map(str::to_owned),
        })
    }

    fn target(column: usize, row: u64) -> TerminalContextTarget {
        TerminalContextTarget {
            content_position: ContentPosition {
                column,
                absolute_row: row,
            },
            generation: 7,
        }
    }

    #[test]
    fn detects_unquoted_local_absolute_paths() {
        let terminal = terminal_with("cat /tmp/report.md\n", 80);
        let action = resolve_context_menu_action(&terminal, target(6, 0), &local_origin()).unwrap();
        assert!(action.ui_action().enabled);
        assert_eq!(action.ui_action().preview, "/tmp/report.md");
    }

    #[test]
    fn detects_wrapped_unicode_paths_stably() {
        let terminal = terminal_with("/srv/résumé/über-long-directory/notes.md\n", 12);
        let action =
            resolve_context_menu_action(&terminal, target(3, 1), &remote_origin(None)).unwrap();
        assert_eq!(action.generation(), 7);
        assert!(action.ui_action().preview.contains("notes.md"));
    }

    #[test]
    fn detects_shell_escaped_spaces() {
        let terminal = terminal_with("open docs/My\\ File.md\n", 80);
        let action =
            resolve_context_menu_action(&terminal, target(12, 0), &local_origin()).unwrap();
        assert_eq!(action.ui_action().preview, "docs/My File.md");
        assert!(!action.ui_action().enabled);
    }

    #[test]
    fn detects_markdown_link_targets() {
        let terminal = terminal_with("see [guide](../docs/guide.md) now\n", 80);
        let action =
            resolve_context_menu_action(&terminal, target(18, 0), &remote_origin(Some("/srv/app")))
                .unwrap();
        assert!(action.ui_action().enabled);
        assert_eq!(
            action.ui_action().preview,
            "deploy@ssh.example.test · /srv/docs/guide.md"
        );
    }

    #[test]
    fn local_relative_paths_stay_disabled_without_live_cwd() {
        let terminal = terminal_with("tail NOTES.md\n", 80);
        let action = resolve_context_menu_action(&terminal, target(7, 0), &local_origin()).unwrap();
        assert!(!action.ui_action().enabled);
        assert!(action
            .ui_action()
            .disabled_reason
            .unwrap()
            .contains("live shell working directory"));
    }

    #[test]
    fn remote_relative_paths_need_trusted_cwd() {
        let terminal = terminal_with("cat logs/today.txt\n", 80);
        let action =
            resolve_context_menu_action(&terminal, target(6, 0), &remote_origin(None)).unwrap();
        assert!(!action.ui_action().enabled);
        assert!(action
            .ui_action()
            .disabled_reason
            .unwrap()
            .contains("trustworthy current directory"));
    }

    #[test]
    fn remote_relative_paths_resolve_against_trusted_sftp_cwd() {
        let terminal = terminal_with("cat logs/today.txt\n", 80);
        let action =
            resolve_context_menu_action(&terminal, target(6, 0), &remote_origin(Some("/srv/app")))
                .unwrap();
        assert_eq!(
            action.ui_action().preview,
            "deploy@ssh.example.test · /srv/app/logs/today.txt"
        );
    }

    #[test]
    fn remote_home_paths_stay_disabled_when_home_unknown() {
        let terminal = terminal_with("less ~/notes.md\n", 80);
        let action =
            resolve_context_menu_action(&terminal, target(6, 0), &remote_origin(Some("/srv/app")))
                .unwrap();
        assert!(!action.ui_action().enabled);
        assert!(action
            .ui_action()
            .disabled_reason
            .unwrap()
            .contains("Remote `~` paths"));
    }

    #[test]
    fn file_uri_with_other_host_stays_disabled() {
        let terminal = terminal_with("file://other.example.test/etc/motd\n", 80);
        let action =
            resolve_context_menu_action(&terminal, target(8, 0), &remote_origin(None)).unwrap();
        assert!(!action.ui_action().enabled);
        assert!(action
            .ui_action()
            .disabled_reason
            .unwrap()
            .contains("different host"));
    }

    #[test]
    fn same_host_file_uri_routes_remote() {
        let terminal = terminal_with("file://ssh.example.test/etc/motd\n", 80);
        let action =
            resolve_context_menu_action(&terminal, target(12, 0), &remote_origin(None)).unwrap();
        assert!(action.ui_action().enabled);
        assert_eq!(
            action.ui_action().preview,
            "deploy@ssh.example.test · /etc/motd"
        );
    }

    #[test]
    fn percent_encoded_file_uris_decode_utf8_bytes() {
        let terminal = terminal_with("file:///srv/r%C3%A9sum%C3%A9.md\n", 80);
        let action =
            resolve_context_menu_action(&terminal, target(15, 0), &local_origin()).unwrap();
        assert_eq!(action.ui_action().preview, "/srv/résumé.md");
    }

    #[test]
    fn invalid_percent_encoded_file_uris_are_rejected_honestly() {
        let terminal = terminal_with("file:///srv/%FF.md\n", 80);
        let action =
            resolve_context_menu_action(&terminal, target(14, 0), &local_origin()).unwrap();
        assert!(!action.ui_action().enabled);
        assert!(action
            .ui_action()
            .disabled_reason
            .unwrap()
            .contains("invalid percent-encoded UTF-8"));
    }

    #[test]
    fn control_characters_in_file_uris_are_rejected() {
        let terminal = terminal_with("file:///srv/%0A.md\n", 80);
        let action =
            resolve_context_menu_action(&terminal, target(14, 0), &local_origin()).unwrap();
        assert!(!action.ui_action().enabled);
        assert!(action
            .ui_action()
            .disabled_reason
            .unwrap()
            .contains("control characters"));
    }

    #[test]
    fn malformed_percent_encoding_with_multibyte_text_is_refused_without_panicking() {
        for path in ["file:///tmp/%\u{1f916}.md", "file:///tmp/%a\u{03bb}.md"] {
            assert_eq!(
                parse_file_uri(path),
                Err(PathReferenceParseError::InvalidFileUriEncoding)
            );
        }
    }

    #[test]
    fn quoted_and_unc_paths_preserve_literal_filename_characters() {
        assert_eq!(
            normalize_candidate_text("'/tmp/name!.md '").as_deref(),
            Some("/tmp/name!.md ")
        );
        assert_eq!(
            normalize_candidate_text(r"'C:\Users\Name\File.md'").as_deref(),
            Some(r"C:\Users\Name\File.md")
        );
        assert_eq!(
            normalize_candidate_text(r"\\server\share\file.md").as_deref(),
            Some(r"\\server\share\file.md")
        );
        assert_eq!(
            normalize_candidate_text(r"'/tmp/a\\b.md'").as_deref(),
            Some(r"/tmp/a\\b.md")
        );
    }

    #[test]
    fn local_paths_leave_parent_traversal_for_the_filesystem() {
        let TerminalFilesystemOrigin::Local(origin) = local_origin() else {
            panic!("expected local fixture");
        };
        let action = resolve_local_candidate("/tmp/link/../notes.md", &origin);
        let Some(TerminalPathOpenRequest::Local(request)) = action.request else {
            panic!("expected local path action");
        };
        assert_eq!(request.path, PathBuf::from("/tmp/link/../notes.md"));
        assert_eq!(
            join_home(Path::new("/tmp/home"), "link/../notes.md"),
            Path::new("/tmp/home").join("link/../notes.md")
        );
    }

    #[cfg(windows)]
    #[test]
    fn local_file_uris_resolve_windows_drive_paths() {
        let TerminalFilesystemOrigin::Local(origin) = local_origin() else {
            panic!("expected local fixture");
        };
        let action = resolve_local_candidate("file:///C:/Users/Dev/notes.md", &origin);
        let Some(TerminalPathOpenRequest::Local(request)) = action.request else {
            panic!("expected local drive path action");
        };
        assert_eq!(request.path, PathBuf::from("C:/Users/Dev/notes.md"));
    }

    #[test]
    fn local_home_paths_expand() {
        let terminal = terminal_with("vim ~/notes.md\n", 80);
        let action = resolve_context_menu_action(&terminal, target(6, 0), &local_origin()).unwrap();
        assert_eq!(
            PathBuf::from(action.ui_action().preview),
            PathBuf::from("/Users/fes").join("notes.md")
        );
    }

    #[test]
    fn windows_paths_are_parsed_without_shell_expansion() {
        let terminal = terminal_with("type C:\\Users\\Dev\\NOTES.txt\n", 80);
        let action =
            resolve_context_menu_action(&terminal, target(7, 0), &remote_origin(None)).unwrap();
        assert!(action.ui_action().enabled);
        assert!(action
            .ui_action()
            .preview
            .ends_with("/C:/Users/Dev/NOTES.txt"));
    }

    #[test]
    fn hostile_shell_expansions_are_not_treated_as_paths() {
        let terminal = terminal_with("cat $(pwd)/notes.md\n", 80);
        assert!(resolve_context_menu_action(&terminal, target(5, 0), &local_origin()).is_none());
    }
}
