//! Opt-in transient SSH composition. The mobile app remains the terminal writer.

use std::sync::Arc;
use std::time::{Duration, Instant};

use super::model::{HostDecision, HostVerification};
use super::password_buffer::PasswordBuffer;
use eframe::egui;
use festerm_core::{Dimensions, Terminal};
use festerm_session::{
    HostKeyPrompt, PasswordPrompt, Session, SessionEvent, SessionEventNotifier, SessionLifecycle,
    SessionTryReceiveError, TerminalSize,
};
use festerm_ssh::{
    HostIdentity, HostKeyDecisionResolver, HostTrustDecision, PasswordDecisionResolver,
    SessionStrategy, SshAuthentication, SshConnectionProfile, SshSession, SshSessionOptions,
};
use zeroize::Zeroizing;

#[cfg(test)]
mod loopback_tests;

const EVENT_BUDGET: usize = 32;
const TUNNEL_LOST: &str = "Tunnel connection lost. The remote session may still be running.";
const PROBE_FAILED: &str = "Tunnel liveness check failed. Reconnect to continue.";

const TAKEN_OVER_LOCAL: &str =
    "Taken over on the host computer. Attach again to take the session back.";
const TAKEN_OVER_REMOTE: &str =
    "Taken over by another device. Attach again to take the session back.";
const TAKEN_OVER_UNKNOWN: &str =
    "Taken over by another client. Attach again to take the session back.";

fn session_size(dimensions: Dimensions) -> Result<TerminalSize, &'static str> {
    let columns = u16::try_from(dimensions.columns()).map_err(|_| "Terminal too wide")?;
    let rows = u16::try_from(dimensions.rows()).map_err(|_| "Terminal too tall")?;
    TerminalSize::new(columns, rows).map_err(|_| "Invalid terminal dimensions")
}

pub(super) struct Repaint(pub(super) egui::Context);

impl SessionEventNotifier for Repaint {
    fn notify(&self) {
        self.0.request_repaint();
    }
}

pub(super) struct LiveSsh {
    identity: Option<HostIdentity>,
    context: egui::Context,
    session: Option<Arc<dyn Session>>,
    pub(super) recording_stream: Option<festerm_recording::RecordingStream>,
    host_resolver: Option<HostKeyDecisionResolver>,
    password_resolver: Option<PasswordDecisionResolver>,
    host_prompt: Option<HostKeyPrompt>,
    password_prompt: Option<PasswordPrompt>,
    password: PasswordBuffer,
    pending_resize: Option<(Dimensions, Instant)>,
    running: bool,
    resume_check: bool,
    probe: Option<super::tunnel_link::Job<Result<(), festerm_mobile_tunnels::TunnelError>>>,
    pub(super) status: &'static str,
    pub(super) accepted_pin: Option<String>,
    /// A fingerprint the user just trusted that the app has yet to persist.
    new_trust: Option<String>,
    /// Set for Dev Tunnel sessions: the bridge owns terminal replies and the
    /// recovery stream, and a lost attachment is never re-established here.
    tunnel: Option<festerm_mobile_tunnels::Attachment>,
}

#[cfg(test)]
pub(super) fn prompt_fixture(changed_key: bool, authentication: bool) -> LiveSsh {
    let mut live = tests::connection(Arc::new(tests::FakeSession::default()));
    live.running = false;
    if authentication {
        live.status = "SSH password required";
        let prompt = PasswordPrompt::new("fixture-user", "example.test", 1, false);
        live.password_prompt = Some(prompt);
    } else {
        live.status = "Review SSH host key";
        let prompt = HostKeyPrompt::new(
            "example.test",
            22,
            "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        );
        live.host_prompt = Some(if changed_key {
            prompt.with_previously_trusted_fingerprint(
                "SHA256:BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB",
            )
        } else {
            prompt
        });
    }
    live
}

#[cfg(test)]
pub(super) fn disconnected_fixture() -> LiveSsh {
    LiveSsh {
        identity: Some(HostIdentity::new("example.test", 22).expect("reserved fixture host")),
        context: egui::Context::default(),
        session: None,
        recording_stream: None,
        host_resolver: None,
        password_resolver: None,
        host_prompt: None,
        password_prompt: None,
        password: PasswordBuffer::default(),
        pending_resize: None,
        running: false,
        resume_check: false,
        probe: None,
        status: "Connection lost. Connect again for a fresh shell.",
        accepted_pin: None,
        new_trust: None,
        tunnel: None,
    }
}

#[cfg(test)]
pub(super) fn taken_over_fixture() -> LiveSsh {
    let mut live = disconnected_fixture();
    live.status = TAKEN_OVER_REMOTE;
    live
}

impl LiveSsh {
    pub(super) fn input_destination(&self) -> Option<std::sync::Weak<dyn Session>> {
        self.session.as_ref().map(Arc::downgrade)
    }

    /// Wraps an attached Dev Tunnel session. The host has already approved the
    /// adoption, so input is accepted at once.
    pub(super) fn from_tunnel(
        attachment: festerm_mobile_tunnels::Attachment,
        context: &egui::Context,
    ) -> Self {
        let session = attachment.session();
        Self {
            identity: None,
            context: context.clone(),
            session: Some(session),
            recording_stream: None,
            host_resolver: None,
            password_resolver: None,
            host_prompt: None,
            password_prompt: None,
            password: PasswordBuffer::default(),
            pending_resize: None,
            running: true,
            resume_check: false,
            probe: None,
            status: "Attached",
            accepted_pin: None,
            new_trust: None,
            tunnel: Some(attachment),
        }
    }

    /// Whether this Dev Tunnel tab ended through no choice of anyone (network
    /// loss or app suspension), so reconnecting automatically is appropriate.
    /// Displacement, a shell exit and an explicit disconnect never qualify.
    pub(super) fn tunnel_lost(&self) -> bool {
        use festerm_mobile_tunnels::TunnelError;
        let Some(tunnel) = self.tunnel.as_ref() else {
            return false;
        };
        if !self.can_reset() || !matches!(self.status, PROBE_FAILED | TUNNEL_LOST) {
            return false;
        }
        match tunnel.end_error() {
            Some(TunnelError::ConnectionLost) => true,
            Some(_) => false,
            None => self.status == PROBE_FAILED,
        }
    }

    pub(super) fn tunnel_target(&self) -> Option<festerm_mobile_tunnels::Target> {
        self.tunnel.as_ref().map(|t| t.target())
    }

    /// Why a Dev Tunnel attachment ended, in words that name who is responsible.
    fn tunnel_end_status(&self) -> &'static str {
        use festerm_mobile_tunnels::{Displacer, TunnelError};
        match self.tunnel.as_ref().and_then(|tunnel| tunnel.end_error()) {
            Some(TunnelError::Displaced(Displacer::Local)) => TAKEN_OVER_LOCAL,
            Some(TunnelError::Displaced(Displacer::Remote)) => TAKEN_OVER_REMOTE,
            Some(TunnelError::Displaced(_)) => TAKEN_OVER_UNKNOWN,
            Some(TunnelError::SessionExited) => "The remote shell exited.",
            _ => TUNNEL_LOST,
        }
    }

    pub(super) fn connect(
        request: ConnectRequest,
        dimensions: Dimensions,
        context: &egui::Context,
    ) -> Result<Self, &'static str> {
        let ConnectRequest {
            host,
            port,
            username,
            authentication,
            strategy,
            pin,
        } = request;
        let (host, username, pin) = (host.as_str(), username.as_str(), pin.as_deref());
        let identity = HostIdentity::new(host, port).map_err(|_| "Invalid SSH host or port")?;
        let size = session_size(dimensions)?;
        let profile = SshConnectionProfile::new(
            identity.clone(),
            username,
            SshConnectionProfile::DEFAULT_TERMINAL_TYPE,
            size,
        )
        .map_err(|_| "Invalid SSH username")?;
        let mut options = SshSessionOptions::manual_recovery(strategy);
        if let Some(pin) = pin {
            options = options.with_known_host_fingerprint(pin);
        }
        let session = SshSession::start_with_notifier_and_options(
            profile,
            authentication,
            options,
            Arc::new(Repaint(context.clone())),
        )
        .map_err(|_| "Could not start SSH worker")?;
        let host_resolver = Some(session.host_key_decision_resolver());
        let password_resolver = Some(session.password_decision_resolver());
        Ok(Self {
            identity: Some(identity),
            context: context.clone(),
            session: Some(Arc::new(session)),
            recording_stream: None,
            host_resolver,
            password_resolver,
            host_prompt: None,
            password_prompt: None,
            password: PasswordBuffer::default(),
            pending_resize: None,
            running: false,
            resume_check: false,
            probe: None,
            status: "Connecting",
            accepted_pin: pin.map(str::to_owned),
            new_trust: None,
            tunnel: None,
        })
    }

    pub(super) fn input_enabled(&self) -> bool {
        self.running
            && !self.resume_check
            && self.probe.is_none()
            && self.host_prompt.is_none()
            && self.password_prompt.is_none()
    }

    pub(super) fn can_reset(&self) -> bool {
        self.session.is_none()
    }

    /// Native callback: retain established tunnels, cancel any old check and gate input.
    pub(super) fn suspend(&mut self) {
        if self.tunnel.is_some() && self.session.is_some() {
            self.probe = None;
            self.resume_check = true;
        } else if self.tunnel.is_none() {
            self.stop(super::SUSPENDED);
        }
    }

    pub(super) fn stop(&mut self, status: &'static str) {
        drop(self.recording_stream.take());
        self.probe = None;
        self.resume_check = false;
        self.running = false;
        self.status = status;
        self.password.clear();
        self.host_prompt = None;
        self.password_prompt = None;
        self.pending_resize = None;
        self.host_resolver = None;
        self.password_resolver = None;
        if let Some(session) = self.session.take() {
            let _ = session.try_shutdown();
        }
    }

    fn stop_failed(&mut self, status: &'static str) {
        if let Some(stream) = &self.recording_stream {
            if let Err(error) =
                stream.fail_integrity("Recording stopped because a terminal session failed")
            {
                log::error!("Could not stop recording after a session failure: {error}");
            }
        }
        self.stop(status);
    }

    pub(super) fn send(&mut self, bytes: &[u8]) {
        if !self.input_enabled() || bytes.is_empty() {
            return;
        }
        if let Some(session) = &self.session {
            if session.try_send_input(bytes).is_err() {
                // Never drop part of a command and then accept its continuation.
                // The backend admission is atomic; make refusal explicit and
                // retire this session rather than replay input on a new shell.
                self.stop_failed(
                    "Input rejected; disconnected. Earlier accepted input may have executed.",
                );
            }
        }
    }

    pub(super) fn resize(&mut self, dimensions: Dimensions) {
        if self.pending_resize.map(|(size, _)| size) != Some(dimensions) {
            self.pending_resize = Some((dimensions, Instant::now()));
            if self.running {
                // Geometry is reported after this frame's event pump. Schedule
                // the debounce even if no output or further input wakes the UI.
                self.context
                    .request_repaint_after(Duration::from_millis(100));
            }
        }
    }

    /// Pumps worker events into `terminal`; returns whether output arrived.
    pub(super) fn poll(&mut self, terminal: &mut Terminal, context: &egui::Context) -> bool {
        let mut output = false;
        let Some(session) = self.session.clone() else {
            return output;
        };
        for _ in 0..EVENT_BUDGET {
            match session.try_recv_event() {
                Ok(SessionEvent::Output(bytes)) => {
                    output |= !bytes.is_empty();
                    if let Some(stream) = &self.recording_stream {
                        // SharedRecording retains any error and wakes its owner.
                        let _ = stream.ingest_output(terminal, &bytes);
                    } else {
                        terminal.ingest(&bytes);
                    }
                    let replies = terminal.drain_replies();
                    // A tunnel host answers terminal queries itself.
                    let own_replies = !session.terminal_replies_owned_by_backend();
                    if own_replies
                        && !replies.is_empty()
                        && session.try_send_input(&replies).is_err()
                    {
                        self.stop_failed("Terminal reply rejected; disconnected");
                        return output;
                    }
                }
                Ok(SessionEvent::HostKeyVerification(prompt)) => {
                    self.status = "Review SSH host key";
                    self.host_prompt = Some(prompt);
                }
                Ok(SessionEvent::PasswordRequested(prompt)) => {
                    self.status = "SSH password required";
                    self.password.clear();
                    self.password_prompt = Some(prompt);
                }
                Ok(SessionEvent::Lifecycle(SessionLifecycle::Running)) => {
                    self.running = true;
                    self.status = "Connected";
                }
                Ok(SessionEvent::Lifecycle(SessionLifecycle::Starting)) => {
                    self.running = false;
                    self.status = "Connecting";
                }
                Ok(event @ (SessionEvent::Lifecycle(_) | SessionEvent::Error(_))) => {
                    let status = if self.tunnel.is_some() {
                        self.tunnel_end_status()
                    } else {
                        "Session ended or connection lost. Connect again for a fresh shell."
                    };
                    if matches!(
                        event,
                        SessionEvent::Error(_)
                            | SessionEvent::Lifecycle(
                                SessionLifecycle::Failed(_) | SessionLifecycle::Disconnected(_)
                            )
                    ) {
                        self.stop_failed(status);
                    } else {
                        self.stop(status);
                    }
                    return output;
                }
                Ok(SessionEvent::RecoverySync(bytes)) if self.tunnel.is_some() => {
                    let command = match festerm_mobile_tunnels::decode_recovery_sync(&bytes) {
                        Ok(command) => command,
                        Err(_) => {
                            if let Some(stream) = &self.recording_stream {
                                let _ = stream.fail_integrity(
                                    "Recording stopped because terminal recovery data was invalid",
                                );
                            }
                            self.stop("Invalid host sync; disconnected");
                            return output;
                        }
                    };
                    self.apply_tunnel_recovery(terminal, command);
                    output = true;
                }

                Ok(SessionEvent::RecoverySync(_)) => {
                    if let Some(stream) = &self.recording_stream {
                        let _ = stream.fail_integrity(
                            "Recording stopped because terminal recovery data was unsupported",
                        );
                    }
                    self.stop("Unsupported recovery event; disconnected");
                    return output;
                }
                Ok(_) => {}
                Err(SessionTryReceiveError::Empty) => break,
                Err(SessionTryReceiveError::Closed) => {
                    let status = if self.tunnel.is_some() {
                        self.tunnel_end_status()
                    } else {
                        "SSH worker closed"
                    };
                    self.stop_failed(status);
                    return output;
                }
            }
        }
        if self.resume_check && self.probe.is_none() {
            self.resume_check = false;
            if let Some(attachment) = self.tunnel.clone() {
                self.status = "Checking tunnel connection";
                self.probe = Some(super::tunnel_link::Job::spawn(context, move |cancel| {
                    attachment.check_liveness(cancel)
                }));
            }
        }
        if let Some(probe) = self.probe.as_ref() {
            use super::tunnel_link::Poll;
            match probe.poll() {
                Poll::Pending => {}
                Poll::Done(Ok(())) => {
                    self.probe = None;
                    self.status = "Attached";
                }
                Poll::Done(Err(_)) | Poll::Lost => {
                    // Preserve typed exit/displacement reasons if the worker ended in flight.
                    let status = if self.tunnel.as_ref().and_then(|t| t.end_error()).is_some() {
                        self.tunnel_end_status()
                    } else {
                        PROBE_FAILED
                    };
                    self.stop_failed(status);
                    return output;
                }
            }
        }
        if session.metrics().event_queue_depth > 0 {
            context.request_repaint();
        }
        if let Some((dimensions, changed)) = self.pending_resize {
            if self.running {
                if changed.elapsed() >= Duration::from_millis(100) {
                    let Ok(size) = session_size(dimensions) else {
                        self.stop_failed(
                            "Terminal size cannot be represented by SSH; disconnected",
                        );
                        return output;
                    };
                    if session.try_resize(size).is_ok() {
                        self.pending_resize = None;
                    }
                }
                // One coalesced pending size; no unbounded resize queue or busy loop.
                if self.pending_resize.is_some() {
                    context.request_repaint_after(Duration::from_millis(100));
                }
            }
        }
        output
    }

    fn apply_tunnel_recovery(
        &self,
        terminal: &mut Terminal,
        command: festerm_mobile_tunnels::RecoverySync,
    ) {
        match command {
            festerm_mobile_tunnels::RecoverySync::Scrollback(limit) => {
                terminal.set_scrollback_limit(limit);
                if let Some(stream) = &self.recording_stream {
                    let _ = stream.observe_configuration(terminal);
                }
            }
            festerm_mobile_tunnels::RecoverySync::Colors(colors) => {
                terminal.set_color_scheme(colors);
                if let Some(stream) = &self.recording_stream {
                    if terminal.color_scheme() != &festerm_ui_egui::terminal_color_scheme() {
                        let _ = stream.fail_integrity(
                            "Recording stopped because terminal colors do not match the visible view",
                        );
                    } else {
                        let _ = stream.observe_configuration(terminal);
                    }
                }
            }
            festerm_mobile_tunnels::RecoverySync::Bytes(bytes) => {
                if let Some(stream) = &self.recording_stream {
                    let _ = stream.ingest_output(terminal, &bytes);
                } else {
                    terminal.ingest(&bytes);
                }
            }
            festerm_mobile_tunnels::RecoverySync::Reset => {
                terminal.reset_to_initial_state();
                if let Some(stream) = &self.recording_stream {
                    let _ = stream.observe_reset();
                }
            }
        }
        let _ = terminal.drain_replies();
        let _ = terminal.take_reply_queue_overflowed();
    }

    pub(super) fn host_verification(&self) -> Option<HostVerification> {
        let prompt = self.host_prompt.as_ref()?;
        Some(HostVerification {
            name: prompt.host().to_owned(),
            target: format!("{}:{}", prompt.host(), prompt.port()),
            algorithm: "SHA-256".to_owned(),
            fingerprint: prompt.sha256_fingerprint().to_owned(),
            changed_from: prompt.previously_trusted_fingerprint().map(str::to_owned),
        })
    }

    pub(super) fn resolve_host(&mut self, decision: HostDecision) {
        let Some(prompt) = self.host_prompt.clone() else {
            return;
        };
        match decision {
            HostDecision::Trust if !prompt.is_key_change() => {
                let resolved = self.host_resolver.as_ref().is_some_and(|resolver| {
                    resolver
                        .resolve(&prompt, HostTrustDecision::AcceptOnce)
                        .is_ok()
                });
                if resolved {
                    self.accepted_pin = Some(prompt.sha256_fingerprint().to_owned());
                    self.new_trust = self.accepted_pin.clone();
                    self.host_prompt = None;
                    self.status = "Authenticating";
                } else {
                    self.stop("Host-key request expired; connect again");
                }
            }
            _ => self.stop("Host key rejected"),
        }
    }

    /// `(username@host, previous attempt failed)` while a password is requested.
    pub(super) fn password_request(&self) -> Option<(String, bool)> {
        let prompt = self.password_prompt.as_ref()?;
        Some((
            format!("{}@{}", prompt.username(), prompt.host()),
            prompt.previous_attempt_failed(),
        ))
    }

    pub(super) fn submit_password(&mut self) {
        let Some(prompt) = self.password_prompt.clone() else {
            return;
        };
        let mut password = Zeroizing::new(self.password.take());
        let resolved = self.password_resolver.as_ref().is_some_and(|resolver| {
            resolver
                .resolve(&prompt, std::mem::take(&mut *password))
                .is_ok()
        });
        if resolved {
            self.password_prompt = None;
            self.status = "Authenticating";
        } else {
            self.stop("Password request expired; connect again");
        }
    }

    pub(super) fn status(&self) -> &'static str {
        self.status
    }

    pub(super) fn password_buffer(&mut self) -> &mut dyn egui::TextBuffer {
        &mut self.password
    }

    /// The host key the user just trusted, to be persisted once.
    pub(super) fn take_new_trust(&mut self) -> Option<(HostIdentity, String)> {
        let pin = self.new_trust.take()?;
        Some((self.identity.clone()?, pin))
    }
}

impl Drop for LiveSsh {
    fn drop(&mut self) {
        self.stop("Disconnected");
    }
}

/// Everything the worker needs to open one SSH session.
pub(super) struct ConnectRequest {
    pub(super) host: String,
    pub(super) port: u16,
    pub(super) username: String,
    pub(super) authentication: SshAuthentication,
    pub(super) strategy: SessionStrategy,
    /// Fingerprint the user already trusted for this host, if any.
    pub(super) pin: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use festerm_session::{
        SessionId, SessionMetrics, SessionOperation, SessionSendError, ShutdownError,
        ShutdownResult,
    };
    use std::collections::VecDeque;
    use std::sync::Mutex;

    fn private_recording_dir() -> tempfile::TempDir {
        let directory = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        festerm_unix_security::make_private_directory(
            &std::fs::File::open(directory.path()).unwrap(),
        )
        .unwrap();
        directory
    }

    #[derive(Default)]
    pub(super) struct FakeSession {
        events: Mutex<VecDeque<SessionEvent>>,
        input: Mutex<Vec<Vec<u8>>>,
        resizes: Mutex<Vec<TerminalSize>>,
        rejected: bool,
        closed: bool,
        shutdowns: Mutex<usize>,
    }

    impl Session for FakeSession {
        fn id(&self) -> SessionId {
            SessionId::next()
        }
        fn lifecycle(&self) -> SessionLifecycle {
            SessionLifecycle::Running
        }
        fn metrics(&self) -> SessionMetrics {
            SessionMetrics {
                event_queue_depth: self.events.lock().unwrap().len(),
                ..Default::default()
            }
        }
        fn try_send_input(&self, bytes: &[u8]) -> Result<(), SessionSendError> {
            if self.rejected {
                return Err(SessionSendError::Full {
                    operation: SessionOperation::Input,
                    capacity: 1,
                });
            }
            self.input.lock().unwrap().push(bytes.to_vec());
            Ok(())
        }
        fn try_resize(&self, size: TerminalSize) -> Result<(), SessionSendError> {
            self.resizes.lock().unwrap().push(size);
            Ok(())
        }
        fn try_shutdown(&self) -> Result<(), SessionSendError> {
            *self.shutdowns.lock().unwrap() += 1;
            Ok(())
        }
        fn try_recv_event(&self) -> Result<SessionEvent, SessionTryReceiveError> {
            self.events
                .lock()
                .unwrap()
                .pop_front()
                .ok_or(if self.closed {
                    SessionTryReceiveError::Closed
                } else {
                    SessionTryReceiveError::Empty
                })
        }
        fn shutdown(&self, _timeout: Duration) -> Result<ShutdownResult, ShutdownError> {
            panic!("UI must never synchronously wait for shutdown")
        }
    }

    pub(super) fn connection(session: Arc<FakeSession>) -> LiveSsh {
        LiveSsh {
            identity: Some(HostIdentity::new("example.test", 22).unwrap()),
            context: egui::Context::default(),
            session: Some(session),
            recording_stream: None,
            host_resolver: None,
            password_resolver: None,
            host_prompt: None,
            password_prompt: None,
            password: PasswordBuffer::default(),
            pending_resize: None,
            running: true,
            resume_check: false,
            probe: None,
            status: "Connected",
            accepted_pin: None,
            new_trust: None,
            tunnel: None,
        }
    }

    fn terminal() -> Terminal {
        Terminal::with_scrollback_limit(Dimensions::new(80, 24).unwrap(), 4096).unwrap()
    }

    #[test]
    fn recording_start_refuses_a_privacy_cut_before_the_recorder_exists() {
        use std::{cell::Cell, sync::mpsc};

        let directory = private_recording_dir();
        let terminal = terminal();
        let suspension = crate::SuspensionHandle::default();
        let calls = Cell::new(0);
        let (wake, _ready) = mpsc::channel();
        let result = suspension.start_recording(
            directory.path(),
            &[&terminal],
            || {
                calls.set(calls.get() + 1);
                if calls.get() == 2 {
                    suspension.stop_recording();
                }
                Ok(1)
            },
            move || {
                let _ = wake.send(());
            },
        );
        assert!(result.is_err());
        assert!(suspension.current_recording().unwrap().is_none());
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
    }

    #[test]
    fn recording_start_rejects_stale_consent_before_and_during_binding() {
        use std::cell::Cell;

        for change_during_binding in [false, true] {
            let suspension = crate::SuspensionHandle::default();
            let calls = Cell::new(0);
            let result = suspension.start_recording_using(
                Some(7),
                || {
                    calls.set(calls.get() + 1);
                    Ok(if change_during_binding && calls.get() == 1 {
                        7
                    } else {
                        8
                    })
                },
                |_| panic!("stale consent must not start a recorder"),
            );
            assert!(matches!(result, Err(error) if error.contains("privacy")));
            assert!(suspension.current_recording().unwrap().is_none());
            assert_eq!(calls.get(), if change_during_binding { 2 } else { 1 });
        }
    }

    #[test]
    fn suspension_and_content_lock_cancel_export_without_a_ui_frame() {
        use std::sync::atomic::Ordering;

        for use_content_lock in [false, true] {
            let suspension = crate::SuspensionHandle::default();
            let cancellation = suspension.foreground_export_cancellation(|| Ok(1)).unwrap();
            if use_content_lock {
                suspension.content_lock_callback()();
            } else {
                suspension.suspend();
            }
            assert!(cancellation.flag().load(Ordering::Acquire));
            assert!(!cancellation.commit_retention());
        }
    }

    #[test]
    fn foreground_export_rejects_a_privacy_cut_during_registration() {
        use std::cell::Cell;

        let suspension = crate::SuspensionHandle::default();
        let calls = Cell::new(0);
        let result = suspension.foreground_export_cancellation(|| {
            calls.set(calls.get() + 1);
            Ok(if calls.get() == 1 { 1 } else { 2 })
        });
        assert!(matches!(result, Err(error) if error.contains("active, unlocked")));
        assert!(suspension.foreground_export_cancellation(|| Ok(2)).is_ok());

        let suspension = crate::SuspensionHandle::default();
        let calls = Cell::new(0);
        let result = suspension.foreground_export_cancellation(|| {
            calls.set(calls.get() + 1);
            if calls.get() == 2 {
                suspension.stop_recording();
            }
            Ok(1)
        });
        assert!(matches!(result, Err(error) if error.contains("active, unlocked")));
    }

    #[test]
    fn recording_selection_rejects_same_tab_replacement_but_not_tab_movement() {
        use crate::{Lifecycle, MobileApp, RecordingStart};
        use std::{cell::Cell, rc::Rc};

        let directory = private_recording_dir();
        let lifecycle = Rc::new(Cell::new(Lifecycle {
            active: true,
            ..Default::default()
        }));
        let mut app = MobileApp::new(lifecycle);
        app.open_local_tab();
        let id = app.active_tab.unwrap();
        let original = Arc::new(FakeSession::default());
        *app.sink.live.connection.borrow_mut() = Some(connection(original));
        let (_, selected) = app.recording_terminals(&[id]).unwrap();

        app.park_active();
        assert!(app.validate_recording_selection(&selected).is_ok());
        app.activate_tab(id);
        assert!(app.validate_recording_selection(&selected).is_ok());
        *app.sink.live.connection.borrow_mut() = Some(connection(Arc::new(FakeSession::default())));

        let error = app
            .start_terminal_recording_from(
                RecordingStart::Directory(directory.path()),
                &selected,
                &egui::Context::default(),
            )
            .unwrap_err();
        assert!(error.contains("changed"));
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
    }

    #[test]
    fn recording_refuses_to_checkpoint_a_scrolled_mobile_viewport() {
        use crate::{Lifecycle, MobileApp};
        use std::{cell::Cell, rc::Rc};

        let directory = private_recording_dir();
        let lifecycle = Rc::new(Cell::new(Lifecycle {
            active: true,
            ..Default::default()
        }));
        let mut app = MobileApp::new(lifecycle);
        app.open_local_tab();
        let id = app.active_tab.unwrap();
        *app.sink.live.connection.borrow_mut() = Some(connection(Arc::new(FakeSession::default())));
        for _ in 0..32 {
            app.terminal.ingest(b"old safe output\r\n");
        }
        app.terminal.ingest(b"hidden live secret");
        app.view.reveal_document_row(&app.terminal, 0);
        let error = app
            .start_terminal_recording(directory.path(), &[id], &egui::Context::default())
            .unwrap_err();
        assert!(error.contains("live screen"));
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
    }

    #[test]
    fn recording_start_retains_completion_after_a_privacy_generation_change() {
        use festerm_recording::{Completion, FinishReason};
        use std::{cell::Cell, sync::mpsc};

        let directory = private_recording_dir();
        let terminal = terminal();
        let suspension = crate::SuspensionHandle::default();
        let calls = Cell::new(0);
        let (wake, ready) = mpsc::channel();
        let result = suspension.start_recording(
            directory.path(),
            &[&terminal],
            || {
                calls.set(calls.get() + 1);
                Ok(if calls.get() < 3 { 1 } else { 2 })
            },
            move || {
                let _ = wake.send(());
            },
        );
        assert!(result.is_err());
        assert!(!suspension
            .current_recording()
            .unwrap()
            .unwrap()
            .is_recording()
            .unwrap());
        let completion = loop {
            ready.recv_timeout(Duration::from_secs(10)).unwrap();
            if let Some(completion) = suspension.poll_recording_completion().unwrap() {
                break completion;
            }
        };
        assert!(matches!(
            completion,
            Completion::Complete {
                reason: FinishReason::Lifecycle,
                ..
            }
        ));
        assert!(suspension.current_recording().unwrap().is_none());
    }

    #[test]
    fn recording_start_stops_capture_if_the_privacy_check_unwinds() {
        use festerm_recording::{Completion, FinishReason};
        use std::{cell::Cell, panic::AssertUnwindSafe, sync::mpsc};

        let directory = private_recording_dir();
        let terminal = terminal();
        let suspension = crate::SuspensionHandle::default();
        let calls = Cell::new(0);
        let (wake, ready) = mpsc::channel();
        let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
            let _ = suspension.start_recording(
                directory.path(),
                &[&terminal],
                || {
                    calls.set(calls.get() + 1);
                    assert!(calls.get() < 3, "simulated privacy check panic");
                    Ok(1)
                },
                move || {
                    let _ = wake.send(());
                },
            );
        }));
        assert!(result.is_err());
        assert!(!suspension
            .current_recording()
            .unwrap()
            .unwrap()
            .is_recording()
            .unwrap());
        let completion = loop {
            ready.recv_timeout(Duration::from_secs(10)).unwrap();
            if let Some(completion) = suspension.poll_recording_completion().unwrap() {
                break completion;
            }
        };
        assert!(matches!(
            completion,
            Completion::Complete {
                reason: FinishReason::Lifecycle,
                ..
            }
        ));
    }

    #[test]
    fn mobile_recording_binds_selected_active_and_parked_sessions() {
        use crate::{app_lock::LockHost, Lifecycle, MobileApp};
        use festerm_ios_lock::{
            AppLock, Availability, BiometryKind, FakeLocker, LifecycleEvent, ManualClock,
        };
        use festerm_recording::{ArtifactEvent, ArtifactReader, Completion, FinishReason};
        use std::{cell::Cell, fs::File, rc::Rc, thread};

        let directory = private_recording_dir();
        let lifecycle = Rc::new(Cell::new(Lifecycle {
            active: true,
            ..Default::default()
        }));
        let mut app = MobileApp::new(lifecycle);
        app.open_local_tab();
        let parked_id = app.active_tab.unwrap();
        let parked_session = Arc::new(FakeSession::default());
        *app.sink.live.connection.borrow_mut() = Some(connection(parked_session.clone()));
        app.park_active();
        let active_id = app.push_tab("Active".to_owned(), None);
        app.active_tab = Some(active_id);
        app.terminal = terminal();
        let active_session = Arc::new(FakeSession::default());
        *app.sink.live.connection.borrow_mut() = Some(connection(active_session.clone()));

        let lock = AppLock::new_with_content_lock_callback(
            None,
            FakeLocker::new(Availability::Available(BiometryKind::FaceId)),
            Arc::new(ManualClock::new()),
            || {},
            app.sink.live.content_lock_callback(),
        );
        let host = LockHost::new(lock.clone(), None);
        assert!(host.recording_privacy_generation().is_err());
        app.lock = Some(host);
        let context = egui::Context::default();
        assert!(app
            .start_terminal_recording(directory.path(), &[parked_id], &context)
            .unwrap_err()
            .contains("native privacy monitor"));
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
        app.lock
            .as_mut()
            .unwrap()
            .enable_recording_bridge_for_test();
        assert!(app
            .start_terminal_recording(directory.path(), &[active_id, active_id], &context)
            .is_err());
        app.start_terminal_recording(directory.path(), &[parked_id], &context)
            .unwrap();
        for _ in 0..32 {
            app.terminal.ingest(b"older output\r\n");
        }
        app.terminal.ingest(b"hidden live output");
        app.view.reveal_document_row(&app.terminal, 0);
        assert!(app
            .include_terminal_in_recording(active_id)
            .unwrap_err()
            .contains("live screen"));
        app.view.reveal_document_row(&app.terminal, usize::MAX);
        app.include_terminal_in_recording(active_id).unwrap();
        assert!(app.include_terminal_in_recording(active_id).is_err());

        for (session, output) in [
            (&parked_session, b"parked before lock".as_slice()),
            (&active_session, b"active before lock".as_slice()),
        ] {
            session
                .events
                .lock()
                .unwrap()
                .push_back(SessionEvent::Output(output.to_vec()));
        }
        {
            let mut background = app.sink.live.background.borrow_mut();
            let (_, live) = background
                .iter_mut()
                .find(|(id, _)| *id == parked_id)
                .unwrap();
            let tab = app.tabs.iter_mut().find(|tab| tab.id == parked_id).unwrap();
            live.poll(&mut tab.parked.as_mut().unwrap().0, &context);
        }
        app.sink
            .live
            .connection
            .borrow_mut()
            .as_mut()
            .unwrap()
            .poll(&mut app.terminal, &context);
        lock.handle_lifecycle(LifecycleEvent::DeviceLocked);
        active_session
            .events
            .lock()
            .unwrap()
            .push_back(SessionEvent::Output(b"after lock".to_vec()));
        app.sink
            .live
            .connection
            .borrow_mut()
            .as_mut()
            .unwrap()
            .poll(&mut app.terminal, &context);

        let deadline = Instant::now() + Duration::from_secs(10);
        let completion = loop {
            if let Some(completion) = app.poll_terminal_recording().unwrap() {
                break completion;
            }
            assert!(Instant::now() < deadline, "recording did not finish");
            thread::sleep(Duration::from_millis(5));
        };
        let Completion::Complete { path, reason, .. } = completion else {
            panic!("expected a completed lifecycle recording");
        };
        assert_eq!(reason, FinishReason::Lifecycle);
        let mut reader = ArtifactReader::new(File::open(path).unwrap()).unwrap();
        let mut output = Vec::new();
        while let Some(event) = reader.next_event().unwrap() {
            if let ArtifactEvent::Output { stream, bytes, .. } = event {
                output.push((stream.index(), bytes));
            }
        }
        assert_eq!(
            output,
            vec![
                (1, b"parked before lock".to_vec()),
                (2, b"active before lock".to_vec())
            ]
        );
    }

    #[test]
    fn recording_captures_active_and_parked_output_then_stops_on_native_suspension() {
        use crate::{Lifecycle, NativeLifecycle, SuspensionHandle};
        use festerm_recording::{ArtifactEvent, ArtifactReader, Completion, FinishReason};
        use std::{cell::Cell, fs, rc::Rc, sync::mpsc};

        let directory = private_recording_dir();
        let mut active_terminal = terminal();
        let mut parked_terminal = terminal();
        let (wake, ready) = mpsc::channel();
        let (recording, mut streams) = festerm_recording::SharedRecording::start(
            directory.path(),
            &[&active_terminal, &parked_terminal],
            move || {
                let _ = wake.send(());
            },
        )
        .unwrap();
        let active_session = Arc::new(FakeSession::default());
        let parked_session = Arc::new(FakeSession::default());
        active_session
            .events
            .lock()
            .unwrap()
            .push_back(SessionEvent::Output(b"active output".to_vec()));
        parked_session
            .events
            .lock()
            .unwrap()
            .push_back(SessionEvent::Output(b"parked output".to_vec()));
        let mut active = connection(active_session.clone());
        active.recording_stream = Some(streams.remove(0));
        let mut parked = connection(parked_session);
        parked.recording_stream = Some(streams.remove(0));
        active.send(b"private local input");
        let context = egui::Context::default();
        assert!(active.poll(&mut active_terminal, &context));
        assert!(parked.poll(&mut parked_terminal, &context));
        assert_eq!(
            active_session.input.lock().unwrap().concat(),
            b"private local input"
        );

        let suspension = SuspensionHandle::default();
        suspension.install_test_recording(recording.clone());
        *suspension.connection.borrow_mut() = Some(active);
        suspension.background.borrow_mut().push((2, parked));
        let lifecycle = Rc::new(Cell::new(Lifecycle {
            active: true,
            ..Default::default()
        }));
        let mut native = NativeLifecycle::new(lifecycle, suspension);
        assert!(native.suspended(false));
        assert!(!recording.is_recording().unwrap());
        ready.recv_timeout(Duration::from_secs(10)).unwrap();
        let Completion::Complete { path, reason, .. } =
            recording.poll_completion().unwrap().unwrap()
        else {
            panic!("expected a completed lifecycle recording");
        };
        assert_eq!(reason, FinishReason::Lifecycle);
        let bytes = fs::read(path).unwrap();
        assert!(!bytes
            .windows(b"private local input".len())
            .any(|window| window == b"private local input"));
        let mut reader = ArtifactReader::new(std::io::Cursor::new(bytes)).unwrap();
        let mut outputs = Vec::new();
        while let Some(event) = reader.next_event().unwrap() {
            if let ArtifactEvent::Output { stream, bytes, .. } = event {
                outputs.push((stream.index(), bytes));
            }
        }
        assert_eq!(
            outputs,
            vec![
                (1, b"active output".to_vec()),
                (2, b"parked output".to_vec())
            ]
        );
    }

    #[test]
    fn recovery_palette_mismatch_stops_recording_before_later_output() {
        use festerm_recording::{ArtifactEvent, ArtifactReader, Completion, FinishReason};
        use std::sync::mpsc;

        let directory = private_recording_dir();
        let mut terminal = terminal();
        terminal.ingest(b"\x1b[8msecret");
        let (wake, ready) = mpsc::channel();
        let (recording, mut streams) =
            festerm_recording::SharedRecording::start(directory.path(), &[&terminal], move || {
                let _ = wake.send(());
            })
            .unwrap();
        let foreign = festerm_core::ColorScheme::new(
            festerm_core::ColorScheme::DEFAULT_FOREGROUND,
            festerm_core::Rgb::new(0, 0, 0),
            festerm_core::ColorScheme::DEFAULT_FOREGROUND,
            festerm_core::ColorScheme::DEFAULT_ANSI,
        );
        let session = Arc::new(FakeSession::default());
        session
            .events
            .lock()
            .unwrap()
            .push_back(SessionEvent::Output(b"AFTER-PALETTE-LOSS".to_vec()));
        let mut live = connection(session);
        live.recording_stream = Some(streams.remove(0));
        let bytes =
            serde_json::to_vec(&festerm_mobile_tunnels::RecoverySync::Colors(foreign)).unwrap();
        let command = festerm_mobile_tunnels::decode_recovery_sync(&bytes).unwrap();
        live.apply_tunnel_recovery(&mut terminal, command);
        live.poll(&mut terminal, &egui::Context::default());
        assert!(terminal.row_text(0).unwrap().contains("AFTER-PALETTE-LOSS"));
        assert!(!recording.is_recording().unwrap());
        assert!(recording
            .take_failure()
            .unwrap()
            .unwrap()
            .contains("colors do not match"));
        let completion = loop {
            ready.recv_timeout(Duration::from_secs(10)).unwrap();
            if let Some(completion) = recording.poll_completion().unwrap() {
                break completion;
            }
        };
        let Completion::Incomplete {
            path,
            reason: FinishReason::IntegrityFailure,
            ..
        } = completion
        else {
            panic!("a palette mismatch must leave an incomplete recording");
        };
        let mut reader = ArtifactReader::new(std::fs::File::open(path).unwrap()).unwrap();
        let Some(ArtifactEvent::Started { terminal, .. }) = reader.next_event().unwrap() else {
            panic!("expected the safe checkpoint");
        };
        assert!(!terminal.row_text(0).unwrap().contains("secret"));
        while let Some(event) = reader.next_event().unwrap() {
            assert!(!matches!(
                event,
                ArtifactEvent::Configured { .. } | ArtifactEvent::Output { .. }
            ));
        }
    }

    #[test]
    fn unsupported_recovery_stops_the_whole_recording_as_incomplete() {
        use festerm_recording::{Completion, FinishReason};
        use std::sync::mpsc;

        let directory = private_recording_dir();
        let mut terminal = terminal();
        let (wake, ready) = mpsc::channel();
        let (recording, mut streams) =
            festerm_recording::SharedRecording::start(directory.path(), &[&terminal], move || {
                let _ = wake.send(());
            })
            .unwrap();
        let session = Arc::new(FakeSession::default());
        session
            .events
            .lock()
            .unwrap()
            .push_back(SessionEvent::RecoverySync(vec![1]));
        let mut live = connection(session);
        live.recording_stream = Some(streams.remove(0));
        live.poll(&mut terminal, &egui::Context::default());
        assert!(!recording.is_recording().unwrap());
        assert!(recording
            .take_failure()
            .unwrap()
            .unwrap()
            .contains("recovery data was unsupported"));
        let completion = loop {
            ready.recv_timeout(Duration::from_secs(10)).unwrap();
            if let Some(completion) = recording.poll_completion().unwrap() {
                break completion;
            }
        };
        assert!(matches!(
            completion,
            Completion::Incomplete {
                reason: FinishReason::IntegrityFailure,
                ..
            }
        ));
    }

    #[test]
    fn failed_mobile_session_stops_all_recorded_streams_incomplete() {
        use festerm_recording::{ArtifactEvent, ArtifactReader, Completion, FinishReason};
        use festerm_session::{SessionError, SessionErrorKind};
        use std::{fs::File, sync::mpsc};

        let error = SessionError::new(SessionErrorKind::Output, "synthetic failure");
        for event in [
            Some(SessionEvent::Error(error.clone())),
            Some(SessionEvent::Lifecycle(SessionLifecycle::Failed(
                error.clone(),
            ))),
            Some(SessionEvent::Lifecycle(SessionLifecycle::Disconnected(
                error.clone(),
            ))),
            None,
        ] {
            let directory = private_recording_dir();
            let mut first_terminal = terminal();
            let mut second_terminal = terminal();
            let (wake, ready) = mpsc::channel();
            let (recording, mut streams) = festerm_recording::SharedRecording::start(
                directory.path(),
                &[&first_terminal, &second_terminal],
                move || {
                    let _ = wake.send(());
                },
            )
            .unwrap();
            let session = Arc::new(FakeSession {
                closed: event.is_none(),
                ..Default::default()
            });
            let mut events = session.events.lock().unwrap();
            events.push_back(SessionEvent::Output(b"before failure".to_vec()));
            if let Some(event) = event {
                events.push_back(event);
            }
            drop(events);
            let mut live = connection(session);
            live.recording_stream = Some(streams.remove(0));
            let other = streams.remove(0);
            live.poll(&mut first_terminal, &egui::Context::default());
            assert!(!recording.is_recording().unwrap());
            assert!(recording.take_failure().unwrap().is_some());
            other
                .ingest_output(&mut second_terminal, b"after failure")
                .unwrap();
            assert!(second_terminal
                .row_text(0)
                .unwrap()
                .contains("after failure"));
            let completion = loop {
                ready.recv_timeout(Duration::from_secs(10)).unwrap();
                if let Some(completion) = recording.poll_completion().unwrap() {
                    break completion;
                }
            };
            let Completion::Incomplete {
                path,
                reason: FinishReason::IntegrityFailure,
                ..
            } = completion
            else {
                panic!("a failed session must make the shared recording incomplete");
            };
            let mut reader = ArtifactReader::new(File::open(path).unwrap()).unwrap();
            let mut output = Vec::new();
            while let Some(event) = reader.next_event().unwrap() {
                if let ArtifactEvent::Output { bytes, .. } = event {
                    output.extend(bytes);
                }
            }
            assert_eq!(output, b"before failure");
        }
    }

    #[test]
    fn clean_mobile_session_exit_leaves_other_streams_recording() {
        use festerm_recording::{Completion, FinishReason};
        use festerm_session::SessionExit;
        use std::sync::mpsc;

        let directory = private_recording_dir();
        let mut first_terminal = terminal();
        let second_terminal = terminal();
        let (wake, ready) = mpsc::channel();
        let (recording, mut streams) = festerm_recording::SharedRecording::start(
            directory.path(),
            &[&first_terminal, &second_terminal],
            move || {
                let _ = wake.send(());
            },
        )
        .unwrap();
        let session = Arc::new(FakeSession::default());
        session
            .events
            .lock()
            .unwrap()
            .push_back(SessionEvent::Lifecycle(SessionLifecycle::Exited(
                SessionExit::with_exit_code(0),
            )));
        let mut live = connection(session);
        live.recording_stream = Some(streams.remove(0));
        live.poll(&mut first_terminal, &egui::Context::default());
        assert!(recording.is_recording().unwrap());
        recording.stop(FinishReason::User).unwrap();
        let completion = loop {
            ready.recv_timeout(Duration::from_secs(10)).unwrap();
            if let Some(completion) = recording.poll_completion().unwrap() {
                break completion;
            }
        };
        assert!(matches!(completion, Completion::Complete { .. }));
    }

    #[test]
    fn app_and_device_locks_stop_recording_without_a_ui_frame() {
        use crate::SuspensionHandle;
        use festerm_ios_lock::{
            AppLock, Availability, BiometryKind, FakeLocker, LifecycleEvent, LockSettings,
            ManualClock,
        };
        use festerm_recording::{ArtifactEvent, ArtifactReader, Completion, FinishReason};
        use std::{fs::File, sync::mpsc};

        for cut in ["app", "device", "background"] {
            let directory = private_recording_dir();
            let mut terminal = terminal();
            let (wake, ready) = mpsc::channel();
            let (recording, mut streams) = festerm_recording::SharedRecording::start(
                directory.path(),
                &[&terminal],
                move || {
                    let _ = wake.send(());
                },
            )
            .unwrap();
            let suspension = SuspensionHandle::default();
            suspension.install_test_recording(recording.clone());
            let locker = FakeLocker::new(Availability::Available(BiometryKind::FaceId));
            let lock = AppLock::new_with_content_lock_callback(
                None,
                locker,
                Arc::new(ManualClock::new()),
                || {},
                suspension.content_lock_callback(),
            );
            let generation = lock.recording_privacy_generation().unwrap();
            if cut == "app" {
                lock.apply_settings(LockSettings {
                    enabled: true,
                    ..LockSettings::default()
                })
                .unwrap();
                lock.lock_now();
            } else if cut == "device" {
                lock.handle_lifecycle(LifecycleEvent::DeviceLocked);
            } else {
                lock.handle_lifecycle(LifecycleEvent::DidEnterBackground);
            }
            assert_ne!(lock.recording_privacy_generation(), Some(generation));
            assert!(!recording.is_recording().unwrap());
            streams[0]
                .ingest_output(&mut terminal, b"after lock")
                .unwrap();
            drop(streams.remove(0));
            ready.recv_timeout(Duration::from_secs(10)).unwrap();
            let Completion::Complete { path, reason, .. } =
                recording.poll_completion().unwrap().unwrap()
            else {
                panic!("expected a completed lock recording");
            };
            assert_eq!(reason, FinishReason::Lifecycle);
            let mut reader = ArtifactReader::new(File::open(path).unwrap()).unwrap();
            while let Some(event) = reader.next_event().unwrap() {
                assert!(!matches!(event, ArtifactEvent::Output { .. }));
            }
        }
    }

    #[test]
    fn terminal_keyboards_send_encoded_keys_and_local_shortcuts_without_replay() {
        use crate::{terminal_keyboard::KeyboardKind, Lifecycle, MobileApp};
        use egui_kittest::{kittest::Queryable, Harness};
        use std::{cell::Cell, rc::Rc};

        for kind in [KeyboardKind::HoldSymbols, KeyboardKind::ShellRow] {
            let session = Arc::new(FakeSession::default());
            let mut app = MobileApp::new_terminal(Rc::new(Cell::new(Lifecycle {
                active: true,
                ..Default::default()
            })));
            *app.sink.live.connection.borrow_mut() = Some(connection(session.clone()));
            app.select_keyboard(kind);
            let mut harness = Harness::builder()
                .with_size(egui::vec2(390.0, 844.0))
                .build_ui_state(|ui, app: &mut MobileApp| app.show(ui), app);
            harness.run_steps(3);
            for label in ["1", "q", "Space", "Tab", "Return", "Backspace", "Left"] {
                harness.get_by_label(label).click();
                harness.run_steps(3);
            }
            assert_eq!(session.input.lock().unwrap().concat(), b"1q \t\r\x7f\x1b[D");
            session.input.lock().unwrap().clear();
            harness.get_by_label("Ctrl").click();
            harness.run_steps(3);
            harness.get_by_label("c").click();
            harness.run_steps(3);
            assert_eq!(session.input.lock().unwrap().concat(), b"\x03");
            assert!(!harness.state().control);
            session.input.lock().unwrap().clear();
            harness.state_mut().control = true;
            harness.state_mut().shift = true;
            harness.run_steps(3);
            harness.get_by_label("C").click();
            harness.run_steps(3);
            assert!(
                session.input.lock().unwrap().is_empty(),
                "copy is local, even without a selection"
            );
            assert!(!harness.state().control && !harness.state().shift);
            harness.state_mut().shift = true;
            harness.run_steps(3);
            harness.get_by_label("!").click();
            harness.run_steps(3);
            assert_eq!(session.input.lock().unwrap().concat(), b"!");
            session.input.lock().unwrap().clear();
            harness.get_by_label("#+=").click();
            harness.run_steps(3);
            harness.get_by_label("`").click();
            harness.run_steps(3);
            assert_eq!(session.input.lock().unwrap().concat(), b"`");
            session.input.lock().unwrap().clear();
            harness.get_by_label("Extended keys").click();
            harness.run_steps(3);
            harness.get_by_label("F-keys").click();
            harness.run_steps(3);
            harness.get_by_label("F1").click();
            harness.run_steps(3);
            assert_eq!(session.input.lock().unwrap().concat(), b"\x1bOP");
        }
    }

    #[test]
    fn mobile_disconnected_history_scrolls_and_copies_without_transport_input() {
        use crate::{Lifecycle, MobileApp};
        use std::{cell::Cell, rc::Rc};

        let session = Arc::new(FakeSession::default());
        let mut live = connection(session.clone());
        live.stop("Connection lost. Connect again for a fresh shell.");
        let mut app = MobileApp::new_terminal(Rc::new(Cell::new(Lifecycle {
            active: true,
            ..Default::default()
        })));
        app.control = true;
        app.alt = true;
        app.shift = true;
        app.terminal.ingest(b"\x1b[?1003h\x1b[?1006h\x1b[?1004h");
        *app.sink.live.connection.borrow_mut() = Some(live);
        let context = egui::Context::default();
        let render = |app: &mut MobileApp, events| {
            context.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(390.0, 844.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ui| app.show_with_occlusion(ui, 0.0),
            )
        };
        for _ in 0..4 {
            let output = render(&mut app, vec![]);
            assert!(output.platform_output.ime.is_none());
            output.drop_without_applying_deltas();
        }
        assert!(!app.control && !app.alt && !app.shift);
        assert!(app.toolbar_rect.is_none());
        assert!(app.gestures.helper().is_none());
        let grid = app.view.diagnostics().grid_rect.unwrap();
        render(
            &mut app,
            vec![
                egui::Event::WindowFocused(true),
                egui::Event::Text("must not run".into()),
                egui::Event::Paste("must not paste\n".into()),
                egui::Event::PointerMoved(grid.center()),
                egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Line,
                    delta: egui::vec2(0.0, 6.0),
                    phase: egui::TouchPhase::Move,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        )
        .drop_without_applying_deltas();
        assert!(app.view.history_offset_rows() > 0);
        let grid = app.view.diagnostics().grid_rect.unwrap();
        let dimensions = app.terminal.dimensions();
        let cell = egui::vec2(
            grid.width() / dimensions.columns() as f32,
            grid.height() / dimensions.rows() as f32,
        );
        let start = grid.min + egui::vec2(cell.x * 0.2, cell.y * 0.5);
        let end = start + egui::vec2(cell.x * 2.0, 0.0);
        for (point, pressed) in [(start, true), (end, false)] {
            render(
                &mut app,
                vec![
                    egui::Event::PointerMoved(point),
                    egui::Event::PointerButton {
                        pos: point,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            )
            .drop_without_applying_deltas();
        }
        let selected = app
            .view
            .selected_text(&app.terminal)
            .expect("local selection");
        assert!(!selected.is_empty());
        let output = render(&mut app, vec![egui::Event::Copy]);
        assert!(output.platform_output.commands.iter().any(|command| {
            matches!(command, egui::OutputCommand::CopyText(text) if text == &selected)
        }));
        assert!(output.platform_output.ime.is_none());
        output.drop_without_applying_deltas();
        assert!(session.input.lock().unwrap().is_empty());
        assert_eq!(app.sink.diagnostics.byte_count, 0);
        assert_eq!(*session.shutdowns.lock().unwrap(), 1);
        assert_eq!(
            app.terminal.modes().mouse_tracking(),
            festerm_core::MouseTrackingMode::AnyMotion
        );

        app.editor.open_new();
        for _ in 0..4 {
            render(&mut app, vec![]).drop_without_applying_deltas();
        }
        let offset = app.view.history_offset_rows();
        render(
            &mut app,
            vec![
                egui::Event::PointerMoved(grid.center()),
                egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Line,
                    delta: egui::vec2(0.0, 6.0),
                    phase: egui::TouchPhase::Move,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        )
        .drop_without_applying_deltas();
        assert_eq!(app.view.history_offset_rows(), offset);
        assert_eq!(app.sink.diagnostics.byte_count, 0);
    }

    fn plain_spec() -> crate::profiles::SessionSpec {
        crate::profiles::SessionSpec {
            profile: None,
            name: "fixture".to_owned(),
            ssh: festerm_mobile_profiles::SshProfile {
                host: "example.test".to_owned(),
                port: 22,
                username: "fixture-user".to_owned(),
                auth: festerm_mobile_profiles::SshAuth::Ask,
                remote_session: festerm_mobile_profiles::RemoteSession::Plain,
                recovery: Default::default(),
            },
        }
    }

    #[test]
    fn mobile_reconnect_banner_fits_and_failed_reconnect_keeps_history_without_replay() {
        use crate::{Lifecycle, MobileApp};
        use egui_kittest::{kittest::Queryable, Harness};
        use std::{cell::Cell, rc::Rc};

        for size in [
            egui::vec2(834.0, 1194.0),
            egui::vec2(844.0, 390.0),
            egui::vec2(390.0, 260.0),
        ] {
            let mut app = MobileApp::new_terminal(Rc::new(Cell::new(Lifecycle {
                active: true,
                ..Default::default()
            })));
            app.fixture_ssh_tab(plain_spec());
            *app.sink.live.connection.borrow_mut() = Some(disconnected_fixture());
            let mut harness = Harness::builder()
                .with_size(size)
                .build_ui_state(|ui, app: &mut MobileApp| app.show(ui), app);
            harness.run();
            let button = harness.get_by_label("Reconnect · new shell").rect();
            assert!(
                egui::Rect::from_min_size(egui::Pos2::ZERO, size).contains_rect(button),
                "recovery action outside {size:?}: {button:?}"
            );
        }

        let mut app = MobileApp::new_terminal(Rc::new(Cell::new(Lifecycle {
            active: true,
            ..Default::default()
        })));
        app.fixture_ssh_tab(plain_spec());
        app.terminal.ingest(b"retained-evidence");
        *app.sink.live.connection.borrow_mut() = Some(disconnected_fixture());
        let mut harness = Harness::builder()
            .with_size(egui::vec2(390.0, 844.0))
            .build_ui_state(|ui, app: &mut MobileApp| app.show(ui), app);
        harness.run();
        // No profile store: the reconnect is refused before touching history.
        harness.get_by_label("Reconnect · new shell").click();
        harness.run();
        let state = harness.state();
        let text = (0..state.terminal.dimensions().rows())
            .filter_map(|row| state.terminal.row_text(row))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("retained-evidence") && !text.contains("new shell"));
        assert!(state.notice.is_some());
        assert_eq!(state.sink.diagnostics.byte_count, 0);
        assert!(state
            .sink
            .live
            .connection
            .borrow()
            .as_ref()
            .unwrap()
            .can_reset());
    }

    #[test]
    fn mobile_tabs_switch_without_reconnect_keep_background_output_and_close() {
        use crate::{Lifecycle, MobileApp};
        use std::{cell::Cell, rc::Rc};

        let mut app = MobileApp::new_terminal(Rc::new(Cell::new(Lifecycle {
            active: true,
            ..Default::default()
        })));
        let demo = app.active_tab.expect("demo tab");
        let session = Arc::new(FakeSession::default());
        app.fixture_ssh_tab(plain_spec());
        let remote = app.active_tab.unwrap();
        *app.sink.live.connection.borrow_mut() = Some(connection(session.clone()));
        app.terminal.ingest(b"remote-prompt$ ");

        app.activate_tab(demo);
        assert_eq!(app.active_tab, Some(demo));
        assert!(app.sink.live.connection.borrow().is_none());
        assert_eq!(app.sink.live.background.borrow().len(), 1);
        assert_eq!(
            *session.shutdowns.lock().unwrap(),
            0,
            "switching must not disconnect"
        );

        let unread = |app: &MobileApp| {
            app.tab_surfaces()
                .iter()
                .find(|surface| surface.id == remote)
                .is_some_and(|surface| surface.unread)
        };
        app.poll_background(&egui::Context::default());
        assert!(!unread(&app), "a quiet background tab has no unread marker");

        // Output for the parked tab keeps flowing into its own terminal.
        session
            .events
            .lock()
            .unwrap()
            .push_back(SessionEvent::Output(b"while-away".to_vec()));
        app.poll_background(&egui::Context::default());
        assert!(!app
            .terminal
            .row_text(0)
            .unwrap_or_default()
            .contains("while-away"));
        assert!(unread(&app), "background output marks the tab's chip");

        app.activate_tab(remote);
        assert!(!unread(&app), "showing the tab clears its marker");
        let rows = (0..app.terminal.dimensions().rows())
            .filter_map(|row| app.terminal.row_text(row))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rows.contains("remote-prompt$") && rows.contains("while-away"));
        assert!(app.sink.live.connection.borrow().is_some());
        assert_eq!(app.tab_surfaces().len(), 2);

        app.close_tab(remote);
        assert!(app.sink.live.connection.borrow().is_none());
        assert_eq!(app.tab_surfaces().len(), 1);
        assert_eq!(*session.shutdowns.lock().unwrap(), 1);
    }

    #[test]
    fn mobile_live_ssh_pump_uses_core_replies_and_bounded_event_work() {
        let session = Arc::new(FakeSession::default());
        session
            .events
            .lock()
            .unwrap()
            .push_back(SessionEvent::Output(b"hello\x1b[6n".to_vec()));
        let mut live = connection(session.clone());
        live.poll(&mut terminal(), &egui::Context::default());
        assert_eq!(*session.input.lock().unwrap(), vec![b"\x1b[1;6R".to_vec()]);
        for _ in 0..EVENT_BUDGET + 1 {
            session
                .events
                .lock()
                .unwrap()
                .push_back(SessionEvent::Lifecycle(SessionLifecycle::Running));
        }
        live.poll(&mut terminal(), &egui::Context::default());
        assert_eq!(session.events.lock().unwrap().len(), 1);
    }

    #[test]
    fn mobile_live_ssh_hidden_workspaces_keep_pumping_output_and_replies() {
        use crate::{workspaces::Workspace, Lifecycle, MobileApp};
        use std::{cell::Cell, rc::Rc};

        for workspace in [Workspace::Files, Workspace::Markdown] {
            let session = Arc::new(FakeSession::default());
            session
                .events
                .lock()
                .unwrap()
                .push_back(SessionEvent::Output(b"hidden\x1b[6n".to_vec()));
            for _ in 0..EVENT_BUDGET {
                session
                    .events
                    .lock()
                    .unwrap()
                    .push_back(SessionEvent::Lifecycle(SessionLifecycle::Running));
            }
            let mut app = MobileApp::new_terminal(Rc::new(Cell::new(Lifecycle {
                active: true,
                ..Default::default()
            })));
            app.terminal = terminal();
            *app.sink.live.connection.borrow_mut() = Some(connection(session.clone()));
            app.switch_workspace(workspace);
            let context = egui::Context::default();
            context
                .run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(390.0, 844.0),
                        )),
                        ..Default::default()
                    },
                    |ui| app.show(ui),
                )
                .drop_without_applying_deltas();
            assert_eq!(app.workspace, workspace);
            assert_eq!(*session.input.lock().unwrap(), vec![b"\x1b[1;7R".to_vec()]);
            assert_eq!(session.events.lock().unwrap().len(), 1);
            context
                .run_ui(egui::RawInput::default(), |ui| app.show(ui))
                .drop_without_applying_deltas();
            assert!(session.events.lock().unwrap().is_empty());
        }
    }

    #[test]
    fn mobile_live_ssh_native_suspension_retires_without_a_ui_frame() {
        use crate::{Lifecycle, MobileApp, SuspensionHandle};
        use std::{cell::Cell, rc::Rc};

        let suspension = SuspensionHandle::default();
        let app = MobileApp::new_terminal(Rc::new(Cell::new(Lifecycle::default())))
            .with_suspension(suspension.clone());
        let session = Arc::new(FakeSession::default());
        for shutdowns in 1..=2 {
            *app.sink.live.connection.borrow_mut() = None;
            let mut live = connection(session.clone());
            live.password.0.push_str("synthetic-secret");
            live.host_prompt = Some(HostKeyPrompt::new("example.test", 22, "SHA256:fixture"));
            live.resize(Dimensions::new(90, 30).unwrap());
            *app.sink.live.connection.borrow_mut() = Some(live);

            suspension.suspend();
            suspension.suspend();

            let connection = app.sink.live.connection.borrow();
            let live = connection.as_ref().unwrap();
            assert!(live.session.is_none());
            assert!(!live.input_enabled());
            assert!(live.password.0.is_empty());
            assert!(live.host_prompt.is_none());
            assert!(live.password_prompt.is_none());
            assert!(live.pending_resize.is_none());
            assert!(live.status.starts_with("Suspended"));
            assert_eq!(*session.shutdowns.lock().unwrap(), shutdowns);
        }
        // Parked tabs keep a worker too; suspension must retire those as well.
        let parked = Arc::new(FakeSession::default());
        suspension
            .background
            .borrow_mut()
            .push((9, connection(parked.clone())));
        suspension.suspend();
        assert_eq!(*parked.shutdowns.lock().unwrap(), 1);
    }

    #[test]
    fn mobile_live_ssh_dictation_permission_alert_keeps_plain_session_until_background() {
        use crate::{Lifecycle, MobileApp, NativeLifecycle, SuspensionHandle};
        use std::{cell::Cell, rc::Rc};

        let lifecycle = Rc::new(Cell::new(Lifecycle {
            active: true,
            ..Default::default()
        }));
        let suspension = SuspensionHandle::default();
        let app = MobileApp::new_terminal(lifecycle.clone()).with_suspension(suspension.clone());
        let mut native = NativeLifecycle::new(lifecycle.clone(), suspension);
        let session = Arc::new(FakeSession::default());
        *app.sink.live.connection.borrow_mut() = Some(connection(session.clone()));

        // Speech/Microphone alert: resign-active only, then active again.
        assert!(!native.suspended(true));
        assert!(!native.resumed());
        assert!(lifecycle.get().active);
        assert_eq!(lifecycle.get().suspensions, 0);
        assert!(app.terminal_input_ready(lifecycle.get()));
        assert_eq!(*session.shutdowns.lock().unwrap(), 0);

        // A genuine background while the alert is up still retires the shell.
        assert!(!native.suspended(true));
        assert!(native.entered_background());
        assert!(!lifecycle.get().active);
        assert_eq!(*session.shutdowns.lock().unwrap(), 1);
        assert!(app
            .sink
            .live
            .connection
            .borrow()
            .as_ref()
            .unwrap()
            .status
            .starts_with("Suspended"));
    }

    #[test]
    fn app_lock_dialog_preserves_active_and_parked_ssh_but_real_background_still_stops() {
        use crate::{Lifecycle, MobileApp, NativeLifecycle, SuspensionHandle};
        use festerm_ios_lock::{
            AppLock, AuthFailure, Availability, BiometryKind, FakeLocker, LifecycleEvent,
            LockSettings, ManualClock,
        };
        use std::{cell::Cell, rc::Rc};

        let lifecycle = Rc::new(Cell::new(Lifecycle {
            active: true,
            ..Default::default()
        }));
        let suspension = SuspensionHandle::default();
        let app = MobileApp::new_terminal(lifecycle.clone()).with_suspension(suspension.clone());
        let locker = FakeLocker::new(Availability::Available(BiometryKind::FaceId));
        locker.set_deferred(true);
        let lock = AppLock::new(None, locker.clone(), Arc::new(ManualClock::new()), || {});
        lock.apply_settings(LockSettings {
            enabled: true,
            ..Default::default()
        })
        .unwrap();
        lock.lock_now();
        let mut native =
            NativeLifecycle::new(lifecycle.clone(), suspension.clone()).with_app_lock(lock.clone());
        let active = Arc::new(FakeSession::default());
        let parked = Arc::new(FakeSession::default());
        let active_session: Arc<dyn Session> = active.clone();
        let parked_session: Arc<dyn Session> = parked.clone();
        *app.sink.live.connection.borrow_mut() = Some(connection(active.clone()));
        suspension
            .background
            .borrow_mut()
            .push((9, connection(parked.clone())));
        for passcode in [false, true, false] {
            if passcode {
                lock.begin_passcode_unlock();
            } else {
                lock.retry_unlock();
            }
            lock.handle_lifecycle(LifecycleEvent::WillResignActive);
            // Exercise the completion-before-winit-suspension ordering too.
            locker.complete_pending(Err(AuthFailure::Cancelled));
            assert!(!native.suspended(false));
            assert!(!native.resumed());
            lock.handle_lifecycle(LifecycleEvent::DidBecomeActive);
            assert_eq!(lifecycle.get().suspensions, 0);
            assert_eq!(*active.shutdowns.lock().unwrap(), 0);
            assert_eq!(*parked.shutdowns.lock().unwrap(), 0);
            assert!(Arc::ptr_eq(
                app.sink
                    .live
                    .connection
                    .borrow()
                    .as_ref()
                    .unwrap()
                    .session
                    .as_ref()
                    .unwrap(),
                &active_session
            ));
            assert!(Arc::ptr_eq(
                suspension.background.borrow()[0]
                    .1
                    .session
                    .as_ref()
                    .unwrap(),
                &parked_session
            ));
        }

        lock.retry_unlock();
        lock.handle_lifecycle(LifecycleEvent::WillResignActive);
        assert!(!native.suspended(false));
        lock.handle_lifecycle(LifecycleEvent::DidEnterBackground);
        assert!(native.entered_background());
        assert_eq!(*active.shutdowns.lock().unwrap(), 1);
        assert_eq!(*parked.shutdowns.lock().unwrap(), 1);
        assert!(!lifecycle.get().active);
    }

    #[test]
    fn dictation_refuses_a_replacement_transport_in_the_same_tab() {
        let lifecycle = std::rc::Rc::new(std::cell::Cell::new(crate::Lifecycle {
            active: true,
            ..Default::default()
        }));
        let mut app = crate::MobileApp::new_terminal(lifecycle);
        let first = Arc::new(FakeSession::default());
        *app.sink.live.connection.borrow_mut() = Some(connection(first.clone()));
        app.begin_dictation(&egui::Context::default());
        app.dictation.as_ref().unwrap().with_text(|draft| {
            draft.phase = festerm_ios_window::DictationPhase::Review;
            draft.text = "must not replay".to_owned();
        });
        let original_tab = app.active_tab;
        let replacement = Arc::new(FakeSession::default());
        *app.sink.live.connection.borrow_mut() = Some(connection(replacement.clone()));
        assert_eq!(app.active_tab, original_tab);
        assert!(!app.insert_dictation());
        assert!(!app.dictation_open());
        assert!(first.input.lock().unwrap().is_empty());
        assert!(replacement.input.lock().unwrap().is_empty());
    }

    #[test]
    fn recorded_mobile_session_input_refusal_stops_the_shared_recording() {
        use festerm_recording::{Completion, FinishReason, SharedRecording};
        use std::sync::mpsc;

        let directory = private_recording_dir();
        let first = terminal();
        let second = terminal();
        let (wake, ready) = mpsc::channel();
        let (recording, mut streams) =
            SharedRecording::start(directory.path(), &[&first, &second], move || {
                let _ = wake.send(());
            })
            .unwrap();
        let session = Arc::new(FakeSession {
            rejected: true,
            ..Default::default()
        });
        let mut live = connection(session);
        live.recording_stream = Some(streams.remove(0));
        live.send(b"command");
        assert!(live.session.is_none());
        assert!(!recording.is_recording().unwrap());
        assert!(recording.take_failure().unwrap().is_some());
        let completion = loop {
            ready.recv_timeout(Duration::from_secs(10)).unwrap();
            if let Some(completion) = recording.poll_completion().unwrap() {
                break completion;
            }
        };
        assert!(matches!(
            completion,
            Completion::Incomplete {
                reason: FinishReason::IntegrityFailure,
                ..
            }
        ));
    }

    #[test]
    fn mobile_live_ssh_input_refusal_retires_session_without_replay() {
        let session = Arc::new(FakeSession {
            rejected: true,
            ..Default::default()
        });
        let mut live = connection(session.clone());
        live.send(b"command");
        assert!(live.session.is_none());
        assert!(!live.input_enabled());
        assert!(live.status.starts_with("Input rejected"));
        live.send(b"continuation\n");
        assert!(session.input.lock().unwrap().is_empty());
        assert_eq!(*session.shutdowns.lock().unwrap(), 1);
    }

    #[test]
    fn mobile_live_ssh_stop_clears_prompts_password_resize_and_stale_output() {
        let session = Arc::new(FakeSession::default());
        let mut live = connection(session.clone());
        live.password.0.push_str("synthetic-secret");
        live.host_prompt = Some(HostKeyPrompt::new("example.test", 22, "SHA256:fixture"));
        live.resize(Dimensions::new(90, 30).unwrap());
        live.send(b"blocked by trust prompt");
        assert!(session.input.lock().unwrap().is_empty());
        live.stop("Suspended");
        assert!(live.password.0.is_empty());
        assert!(live.host_prompt.is_none());
        assert!(live.pending_resize.is_none());
        session
            .events
            .lock()
            .unwrap()
            .push_back(SessionEvent::Output(b"stale\x1b[6n".to_vec()));
        live.poll(&mut terminal(), &egui::Context::default());
        assert_eq!(session.events.lock().unwrap().len(), 1);
        assert!(session.input.lock().unwrap().is_empty());
    }

    #[test]
    fn mobile_live_ssh_resize_coalesces_and_rejects_unrepresentable_grids() {
        let session = Arc::new(FakeSession::default());
        let mut live = connection(session.clone());
        live.resize(Dimensions::new(90, 30).unwrap());
        live.resize(Dimensions::new(100, 40).unwrap());
        live.pending_resize.as_mut().unwrap().1 = Instant::now() - Duration::from_millis(200);
        live.poll(&mut terminal(), &egui::Context::default());
        assert_eq!(
            *session.resizes.lock().unwrap(),
            vec![TerminalSize::new(100, 40).unwrap()]
        );
        assert!(session_size(Dimensions::new(65536, 1).unwrap()).is_err());
    }

    #[test]
    fn mobile_live_ssh_password_buffer_bounds_unicode_edits_and_take() {
        use egui::text::CharIndex;
        use egui::TextBuffer;
        let mut password = PasswordBuffer::default();
        assert_eq!(password.insert_text("a中é", CharIndex(0)), 3);
        password.delete_char_range(CharIndex(1)..CharIndex(2));
        assert_eq!(password.as_str(), "aé");
        assert_eq!(password.insert_text("x", CharIndex(1)), 1);
        assert_eq!(password.as_str(), "axé");
        let value = Zeroizing::new(password.take());
        assert_eq!(value.as_str(), "axé");
        assert!(password.as_str().is_empty());
        assert_eq!(password.insert_text(&"x".repeat(5000), CharIndex(0)), 4096);
        password.clear();
        assert!(password.as_str().is_empty());
    }
}
