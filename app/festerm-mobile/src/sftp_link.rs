//! Opens an SFTP channel for the Files screen with `festerm-ssh`, off the UI
//! thread. Host-key trust and password prompts surface as [`LinkEvent`]s so the
//! same sheets as the terminal are used.

use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use eframe::egui;
use festerm_session::TerminalSize;
use festerm_sftp::{ClientConfig, LocalFs, PaneFs, SftpClient, TransferConfig, TransferManager};
use festerm_ssh::{
    connect_gui_sftp_session, GuiSftpSessionConnectError, GuiSftpSessionConnectOutcome,
    HostIdentity, HostTrustDecision, SshAuthentication, SshConnectionProfile,
};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

use super::files::{Connector, LinkEvent, ManagerTransfers, Ready};
use super::model::{HostDecision, HostVerification};

const MAX_PASSWORD_ATTEMPTS: usize = 3;

pub(crate) struct SftpRequest {
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) username: String,
    pub(crate) authentication: SshAuthentication,
    /// A host key the user already trusted for this host.
    pub(crate) pin: Option<String>,
    pub(crate) start_dir: Option<String>,
    pub(crate) local_root: PathBuf,
}

enum Reply {
    Host(HostDecision),
    Password(String),
}

pub(crate) struct SftpConnector {
    events: mpsc::Receiver<LinkEvent>,
    replies: UnboundedSender<Reply>,
}

/// The app sandbox's Documents directory, which the Files app can show.
pub(crate) fn documents_dir() -> Option<PathBuf> {
    let directory = PathBuf::from(std::env::var_os("HOME")?).join("Documents");
    std::fs::create_dir_all(&directory).ok()?;
    Some(directory)
}

impl SftpConnector {
    pub(crate) fn start(request: SftpRequest, wake: &egui::Context) -> Self {
        let (sender, events) = mpsc::channel();
        let (replies, receiver) = unbounded_channel();
        let wake = wake.clone();
        std::thread::Builder::new()
            .name("festerm-sftp-connect".to_owned())
            .spawn(move || {
                let outcome = run(request, &sender, receiver, &wake);
                let event = match outcome {
                    Ok(ready) => LinkEvent::Ready(Box::new(ready)),
                    Err(message) => LinkEvent::Failed(message),
                };
                let _ = sender.send(event);
                wake.request_repaint();
            })
            .ok();
        Self { events, replies }
    }
}

impl Connector for SftpConnector {
    fn poll(&mut self) -> Vec<LinkEvent> {
        self.events.try_iter().collect()
    }

    fn resolve_host(&mut self, decision: HostDecision) {
        let _ = self.replies.send(Reply::Host(decision));
    }

    fn submit_password(&mut self, password: String) {
        let _ = self.replies.send(Reply::Password(password));
    }

    fn cancel(&mut self) {
        // Closing the reply channel ends a worker that is waiting on the user.
        let (closed, _) = unbounded_channel();
        self.replies = closed;
    }
}

fn describe(error: &GuiSftpSessionConnectError) -> String {
    match error {
        GuiSftpSessionConnectError::HostKeyRejected => "Host key rejected".to_owned(),
        GuiSftpSessionConnectError::Cancelled => "Cancelled".to_owned(),
        GuiSftpSessionConnectError::InteractiveAuthenticationUnsupported => {
            "Interactive authentication is not available for files".to_owned()
        }
        GuiSftpSessionConnectError::ConnectionFailed(detail) => {
            let mut chars = detail.chars();
            chars
                .next()
                .map(|first| first.to_uppercase().collect::<String>() + chars.as_str())
                .unwrap_or_else(|| "Connection failed".to_owned())
        }
    }
}

fn send(sender: &mpsc::Sender<LinkEvent>, wake: &egui::Context, event: LinkEvent) {
    let _ = sender.send(event);
    wake.request_repaint();
}

fn run(
    request: SftpRequest,
    sender: &mpsc::Sender<LinkEvent>,
    mut replies: UnboundedReceiver<Reply>,
    wake: &egui::Context,
) -> Result<Ready, String> {
    let SftpRequest {
        host,
        port,
        username,
        authentication,
        pin,
        start_dir,
        local_root,
    } = request;
    let identity = HostIdentity::new(&host, port).map_err(|_| "Invalid SSH host or port")?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("Could not start networking: {error}"))?;
    let size = TerminalSize::new(80, 24).map_err(|_| "Invalid terminal dimensions")?;
    let target = format!("{username}@{host}");
    let mut authentication = Some(authentication);
    let mut attempt = 0;
    let session = loop {
        attempt += 1;
        let credential = match authentication.take() {
            Some(SshAuthentication::Interactive) | None => {
                send(
                    sender,
                    wake,
                    LinkEvent::NeedPassword {
                        target: target.clone(),
                        retry: attempt > 1,
                    },
                );
                match runtime.block_on(replies.recv()) {
                    Some(Reply::Password(password)) => SshAuthentication::password(password),
                    _ => return Err("Cancelled".to_owned()),
                }
            }
            Some(other) => other,
        };
        let interactive = matches!(credential, SshAuthentication::Password(_));
        let profile = SshConnectionProfile::new(
            identity.clone(),
            &username,
            SshConnectionProfile::DEFAULT_TERMINAL_TYPE,
            size,
        )
        .map_err(|_| "Invalid SSH username".to_owned())?;
        let pin = pin.clone();
        let result = runtime.block_on(connect_once(
            profile,
            credential,
            pin,
            &identity,
            sender,
            wake,
            &mut replies,
        ));
        match result {
            Ok(session) => break session,
            Err(GuiSftpSessionConnectError::ConnectionFailed(detail))
                if interactive
                    && attempt < MAX_PASSWORD_ATTEMPTS
                    && detail.to_lowercase().contains("authentication") =>
            {
                authentication = None;
            }
            Err(error) => return Err(describe(&error)),
        }
    };
    send(sender, wake, LinkEvent::Status("Opening files".to_owned()));
    let (client, keepalive) = session.into_transport();
    let client = SftpClient::from_russh(
        client,
        keepalive,
        ClientConfig {
            request_timeout: Duration::from_secs(30),
            operation_timeout: Duration::from_secs(60),
        },
    )
    .map_err(|error| super::files::describe_error(&error))?;
    let local = LocalFs::new(&local_root).map_err(|error| super::files::describe_error(&error))?;
    let manager = TransferManager::new(client.clone(), local.clone(), TransferConfig::default());
    let wake_transfers = wake.clone();
    manager.set_event_callback(move |_| wake_transfers.request_repaint());
    let start_dir = start_dir.map(|dir| expand_home(&dir, &client.home()));
    let remote: Arc<dyn PaneFs> = Arc::new(client);
    let local: Arc<dyn PaneFs> = Arc::new(local);
    Ok(Ready {
        remote,
        local,
        local_root,
        transfers: Arc::new(ManagerTransfers::new(manager)),
        start_dir,
    })
}

fn expand_home(dir: &str, home: &str) -> String {
    match dir.strip_prefix('~') {
        Some("") => home.to_owned(),
        Some(rest) if rest.starts_with('/') => format!("{}{rest}", home.trim_end_matches('/')),
        _ => dir.to_owned(),
    }
}

async fn connect_once(
    profile: SshConnectionProfile,
    credential: SshAuthentication,
    pin: Option<String>,
    identity: &HostIdentity,
    sender: &mpsc::Sender<LinkEvent>,
    wake: &egui::Context,
    replies: &mut UnboundedReceiver<Reply>,
) -> Result<festerm_ssh::SftpSession, GuiSftpSessionConnectError> {
    send(sender, wake, LinkEvent::Status("Connecting".to_owned()));
    match connect_gui_sftp_session(profile, credential, None, pin).await? {
        GuiSftpSessionConnectOutcome::Connected(session) => Ok(session),
        GuiSftpSessionConnectOutcome::NeedsHostKeyDecision {
            prompt,
            resolver,
            completion,
        } => {
            send(
                sender,
                wake,
                LinkEvent::HostKey(HostVerification {
                    name: prompt.host().to_owned(),
                    target: format!("{}:{}", prompt.host(), prompt.port()),
                    algorithm: "SHA-256".to_owned(),
                    fingerprint: prompt.sha256_fingerprint().to_owned(),
                    changed_from: prompt.previously_trusted_fingerprint().map(str::to_owned),
                }),
            );
            let trusted = matches!(replies.recv().await, Some(Reply::Host(HostDecision::Trust)))
                && !prompt.is_key_change();
            let decision = if trusted {
                HostTrustDecision::AcceptOnce
            } else {
                HostTrustDecision::Reject
            };
            if resolver.resolve(&prompt, decision).is_err() || !trusted {
                return Err(GuiSftpSessionConnectError::HostKeyRejected);
            }
            send(
                sender,
                wake,
                LinkEvent::Trust(identity.clone(), prompt.sha256_fingerprint().to_owned()),
            );
            completion.wait().await
        }
    }
}
