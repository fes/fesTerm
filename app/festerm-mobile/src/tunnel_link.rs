//! Dev Tunnel flows (ADR 0061): blocking `festerm-mobile-tunnels` calls run on
//! short-lived worker threads and report back through channels, so the UI
//! thread never waits on GitHub or the tunnel service.

use std::sync::{mpsc, Arc};

use eframe::egui::{self, Rect};
use festerm_mobile_tunnels::{
    Attachment, CancelToken, SignIn, SignInPrompt, TunnelClient, TunnelDescription, TunnelError,
    TunnelInfo,
};

use super::profiles::TunnelSpec;
use super::theme::{color, space, type_scale};
use super::widgets::{
    bottom_sheet, button, caption_label, card, footnote, sheet_title, ButtonKind,
};

pub(super) enum Poll<T> {
    Pending,
    Done(T),
    /// The worker vanished without a result; treated as a failed attempt.
    Lost,
}

/// One blocking call on its own thread. Dropping it cancels the call.
pub(super) struct Job<T> {
    rx: mpsc::Receiver<T>,
    cancel: CancelToken,
}

impl<T: Send + 'static> Job<T> {
    pub(super) fn spawn(
        ctx: &egui::Context,
        work: impl FnOnce(&CancelToken) -> T + Send + 'static,
    ) -> Self {
        let (tx, rx) = mpsc::channel();
        let cancel = CancelToken::new();
        let token = cancel.clone();
        let ctx = ctx.clone();
        let _ = std::thread::Builder::new()
            .name("festerm-tunnel-job".to_owned())
            .spawn(move || {
                let _ = tx.send(work(&token));
                ctx.request_repaint();
            });
        Self { rx, cancel }
    }

    pub(super) fn poll(&self) -> Poll<T> {
        match self.rx.try_recv() {
            Ok(value) => Poll::Done(value),
            Err(mpsc::TryRecvError::Empty) => Poll::Pending,
            Err(mpsc::TryRecvError::Disconnected) => Poll::Lost,
        }
    }
}

impl<T> Drop for Job<T> {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

/// What to do once a sign-in finishes.
pub(super) enum Resume {
    Nothing,
    /// Retry connecting this profile (in this tab when it is a re-attach).
    Connect {
        profile: String,
        tab: Option<u64>,
    },
}

pub(super) struct SignInFlow {
    pub(super) start: Option<Job<Result<SignIn, TunnelError>>>,
    /// The finished start job; its token governs the whole sign-in, so it
    /// must outlive the call that returned the code.
    pub(super) started: Option<Job<Result<SignIn, TunnelError>>>,
    pub(super) active: Option<SignIn>,
    pub(super) error: Option<String>,
    pub(super) resume: Resume,
}

pub(super) enum ConnectStage {
    Describing(Job<Result<TunnelDescription, TunnelError>>),
    /// The host is new or changed and waits for the person's decision.
    Verify(TunnelDescription),
    Attaching(Job<Result<Attachment, TunnelError>>),
}

pub(super) struct ConnectFlow {
    pub(super) spec: TunnelSpec,
    /// Set for an explicit re-attach of an existing tab.
    pub(super) tab: Option<u64>,
    pub(super) stage: ConnectStage,
}

pub(super) struct TunnelList {
    pub(super) account: String,
    pub(super) job: Option<Job<Result<Vec<TunnelInfo>, TunnelError>>>,
    pub(super) items: Vec<TunnelInfo>,
    pub(super) error: Option<String>,
}

#[derive(Default)]
pub(super) struct TunnelUi {
    pub(super) client: Option<Arc<TunnelClient>>,
    pub(super) sign_in: Option<SignInFlow>,
    pub(super) connect: Option<ConnectFlow>,
    pub(super) list: Option<TunnelList>,
    /// Why the last re-attach of a tab failed, shown on its banner.
    pub(super) attach_error: Option<String>,
    /// A fixed prompt for deterministic previews.
    #[cfg(test)]
    pub(super) preview_prompt: Option<SignInPrompt>,
    /// Keeps a fixture's editor choices instead of the live backend's.
    #[cfg(test)]
    pub(super) preview_editor: bool,
}

/// Who held a session, in words for a notice.
pub(super) fn holder_label(by: Option<festerm_mobile_tunnels::Displacer>) -> &'static str {
    use festerm_mobile_tunnels::Displacer;
    match by {
        Some(Displacer::Local) => "the host computer",
        Some(Displacer::Remote) => "another device",
        _ => "another client",
    }
}

/// A message that is safe to show and tells the person what to do next.
pub(super) fn describe_error(error: &TunnelError) -> String {
    match error {
        TunnelError::SessionInUse { by } => format!(
            "The session is in use on {}. Turn on \u{201c}Take over if attached\u{201d} in this \
             profile to attach anyway.",
            holder_label(*by)
        ),
        other => other.to_string(),
    }
}

pub(super) struct SignInView<'a> {
    pub(super) prompt: Option<&'a SignInPrompt>,
    pub(super) error: Option<&'a str>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SignInAction {
    None,
    CopyCode,
    OpenBrowser,
    Cancel,
}

/// GitHub device-code sign-in (mock-up 13).
pub(super) fn sign_in_sheet(
    ctx: &egui::Context,
    bounds: Rect,
    view: &SignInView<'_>,
) -> SignInAction {
    let mut action = SignInAction::None;
    let outcome = bottom_sheet(ctx, "tunnel-sign-in", bounds, |ui| {
        if sheet_title(ui, "Sign in with GitHub", None) {
            action = SignInAction::Cancel;
        }
        match (view.prompt, view.error) {
            (_, Some(error)) => {
                ui.label(egui::RichText::new(error).color(color::DANGER));
            }
            (Some(prompt), None) => {
                ui.label(
                    egui::RichText::new("Open GitHub, enter this code, then return to fesTerm.")
                        .size(type_scale::CAPTION + 0.5)
                        .color(color::TEXT_MUTED),
                );
                card(ui, |ui| {
                    ui.add_space(space::LG);
                    ui.vertical_centered(|ui| {
                        ui.label(
                            egui::RichText::new(&prompt.user_code)
                                .monospace()
                                .size(32.0)
                                .strong()
                                .color(color::TEXT),
                        );
                        ui.label(
                            egui::RichText::new(&prompt.verification_url)
                                .size(type_scale::CAPTION)
                                .color(color::ACCENT_TEXT),
                        );
                    });
                    ui.add_space(space::LG);
                });
                if button(ui, ButtonKind::Primary, "Open GitHub", true).clicked() {
                    action = SignInAction::OpenBrowser;
                }
                if button(ui, ButtonKind::Secondary, "Copy code", true).clicked() {
                    action = SignInAction::CopyCode;
                }
                footnote(
                    ui,
                    "Waiting for you to approve in the browser. fesTerm only receives access to \
                     your Dev Tunnels.",
                );
            }
            (None, None) => {
                ui.label(
                    egui::RichText::new("Contacting GitHub\u{2026}")
                        .size(type_scale::BODY)
                        .color(color::TEXT_MUTED),
                );
            }
        }
        if button(ui, ButtonKind::Secondary, "Cancel", true).clicked() {
            action = SignInAction::Cancel;
        }
    });
    if outcome.dismissed {
        action = SignInAction::Cancel;
    }
    action
}

/// A non-dismissable status sheet with a Cancel button; returns whether it
/// was chosen.
pub(super) fn progress_sheet(ctx: &egui::Context, bounds: Rect, title: &str, detail: &str) -> bool {
    let mut cancel = false;
    bottom_sheet(ctx, "tunnel-progress", bounds, |ui| {
        if sheet_title(ui, title, None) {
            cancel = true;
        }
        caption_label(ui, detail);
        if button(ui, ButtonKind::Secondary, "Cancel", true).clicked() {
            cancel = true;
        }
    });
    cancel
}
