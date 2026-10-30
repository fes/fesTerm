//! "Browse my tunnels": signs in with the GitHub device flow, lists the
//! account's Dev Tunnels that host a fesTerm bridge, and creates an unsigned
//! Dev Tunnel profile from the one the user picks. Nothing from the sign-in is
//! retained; the profile still signs in on its own the first time it connects.

use std::sync::mpsc;
use std::time::Duration;

use eframe::egui;
use festerm_config::Profile;
use festerm_tunnel_client::{
    identity::{self, DevicePrompt},
    service::{self, TunnelSummary},
    Error, Https,
};
use festerm_ui_egui::controls::{action_button_monospace, ActionButtonRole};
use festerm_ui_egui::theme;
use tokio_util::sync::CancellationToken;

use super::FesTermApp;

enum Event {
    Device(DevicePrompt),
    Listed(Result<(String, Vec<TunnelSummary>), Error>),
}

enum Stage {
    Working,
    Device(DevicePrompt),
    Pick {
        login: String,
        tunnels: Vec<TunnelSummary>,
    },
    Error(Error),
}

pub(super) struct Browser {
    cancel: CancellationToken,
    receiver: mpsc::Receiver<Event>,
    stage: Stage,
    focus_cancel: bool,
}

impl Drop for Browser {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

impl Browser {
    fn start(context: &egui::Context) -> Self {
        let cancel = CancellationToken::new();
        let (sender, receiver) = mpsc::sync_channel(2);
        let mut browser = Self {
            cancel: cancel.clone(),
            receiver,
            stage: Stage::Working,
            focus_cancel: true,
        };
        let Some(client_id) = identity::registered_client_id() else {
            browser.stage = Stage::Error(Error::Registration);
            return browser;
        };
        let context = context.clone();
        let spawned = std::thread::Builder::new()
            .name("native-tunnel-browse".into())
            .spawn(move || {
                let notify = |event| {
                    let sent = sender.try_send(event).is_ok();
                    context.request_repaint();
                    sent
                };
                let result = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|_| Error::Network)
                    .and_then(|runtime| {
                        runtime.block_on(async {
                            tokio::select! {
                                _ = cancel.cancelled() => Err(Error::Cancelled),
                                _ = tokio::time::sleep(Duration::from_secs(1800)) => Err(Error::Expired),
                                value = list(client_id, &cancel, &notify) => value,
                            }
                        })
                    });
                notify(Event::Listed(result));
            });
        if spawned.is_err() {
            browser.stage = Stage::Error(Error::Network);
        }
        browser
    }
}

async fn list(
    client_id: &str,
    cancel: &CancellationToken,
    notify: &impl Fn(Event) -> bool,
) -> Result<(String, Vec<TunnelSummary>), Error> {
    let http = Https::new()?;
    let identity = identity::sign_in(&http, client_id, None, cancel, |prompt| {
        if !notify(Event::Device(prompt)) {
            cancel.cancel();
        }
    })
    .await?;
    let tunnels = service::list(&identity, cancel).await?;
    Ok((identity.account.login.clone(), tunnels))
}

/// A profile identifier that does not collide with an existing profile.
pub(super) fn unique_profile_id(summary: &TunnelSummary, exists: impl Fn(&str) -> bool) -> String {
    let base = summary
        .name
        .as_deref()
        .filter(|name| !name.trim().is_empty())
        .or(summary
            .description
            .as_deref()
            .filter(|text| !text.trim().is_empty()))
        .map(|text| text.chars().filter(|c| !c.is_control()).collect::<String>())
        .map(|text| text.trim().chars().take(60).collect::<String>())
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| summary.locator.clone());
    if !exists(&base) {
        return base;
    }
    (2..)
        .map(|n| format!("{base} ({n})"))
        .find(|candidate| !exists(candidate))
        .expect("unbounded range")
}

impl FesTermApp {
    pub(super) fn begin_tunnel_browser(&mut self, context: &egui::Context) {
        if self.tunnel_browser.is_some() || self.native_tunnel.is_some() {
            return;
        }
        self.tunnel_browser = Some(Browser::start(context));
        self.overlays.native_tunnel = true;
        context.request_repaint();
    }

    pub(super) fn poll_tunnel_browser(&mut self) {
        let Some(browser) = self.tunnel_browser.as_mut() else {
            return;
        };
        while let Ok(event) = browser.receiver.try_recv() {
            browser.stage = match event {
                Event::Device(prompt) => Stage::Device(prompt),
                Event::Listed(Ok((login, tunnels))) => Stage::Pick { login, tunnels },
                Event::Listed(Err(error)) => Stage::Error(error),
            };
            browser.focus_cancel = true;
        }
    }

    pub(super) fn show_tunnel_browser(&mut self, context: &egui::Context, escape: bool) {
        let Some(browser) = self.tunnel_browser.as_mut() else {
            return;
        };
        let mut dismiss = escape;
        let mut picked = None;
        egui::Modal::new(egui::Id::new("browse_dev_tunnels")).show(context, |ui| {
            super::confirmations::prepare_confirmation(ui, context, 520.0);
            ui.heading("Browse my Dev Tunnels");
            super::confirmations::confirmation_body(ui, |ui| match &browser.stage {
                Stage::Working => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Working…");
                    });
                }
                Stage::Device(prompt) => {
                    ui.label("Sign in to Microsoft Dev Tunnels");
                    ui.label("Enter this code on GitHub:");
                    ui.add_space(6.0);
                    if action_button_monospace(ui, ActionButtonRole::Secondary, &prompt.user_code)
                        .on_hover_text("Copy code and open GitHub")
                        .clicked()
                    {
                        ui.ctx().copy_text(prompt.user_code.clone());
                        ui.ctx()
                            .open_url(egui::OpenUrl::new_tab(identity::VERIFICATION_URI));
                    }
                    ui.add_space(6.0);
                    ui.label("Waiting for sign-in…");
                }
                Stage::Pick { login, tunnels } => {
                    ui.label(format!("GitHub · {login}"));
                    ui.add_space(6.0);
                    if tunnels.is_empty() {
                        ui.label("No fesTerm bridge tunnels found for this account.");
                    }
                    for tunnel in tunnels {
                        ui.horizontal_wrapped(|ui| {
                            let title = tunnel
                                .name
                                .as_deref()
                                .or(tunnel.description.as_deref())
                                .unwrap_or(&tunnel.locator);
                            let label = format!(
                                "{title} · {}{}",
                                tunnel.locator,
                                if tunnel.active { " · active" } else { "" }
                            );
                            if super::confirmations::dialog_action(
                                ui,
                                if tunnel.active {
                                    ActionButtonRole::Accent
                                } else {
                                    ActionButtonRole::Secondary
                                },
                                &label,
                            )
                            .clicked()
                            {
                                picked = Some(tunnel.clone());
                            }
                        });
                    }
                }
                Stage::Error(error) => {
                    ui.colored_label(theme::STATUS_ERROR, error.to_string());
                }
            });
            ui.add_space(10.0);
            let cancel = super::confirmations::dialog_action(
                ui,
                ActionButtonRole::Secondary,
                if matches!(browser.stage, Stage::Pick { .. } | Stage::Error(_)) {
                    "Close"
                } else {
                    "Cancel"
                },
            );
            if browser.focus_cancel {
                cancel.request_focus();
                browser.focus_cancel = false;
            }
            dismiss |= cancel.clicked();
        });
        if let Some(tunnel) = picked {
            self.create_profile_from_tunnel(&tunnel);
            dismiss = true;
        }
        if dismiss {
            self.tunnel_browser = None;
            self.overlays.native_tunnel = false;
        }
    }

    fn create_profile_from_tunnel(&mut self, tunnel: &TunnelSummary) {
        let configuration = self.state.configuration();
        let id = unique_profile_id(tunnel, |id| configuration.profile(id).is_some());
        let saved = Profile::named_dev_tunnel(id, tunnel.locator.clone())
            .is_ok_and(|profile| self.save_profile(profile));
        self.secure_storage_feedback = Some(if saved {
            "Dev Tunnel profile created. Open it and choose Sign in with GitHub."
        } else {
            "The Dev Tunnel profile could not be created."
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(name: Option<&str>, description: Option<&str>) -> TunnelSummary {
        TunnelSummary {
            locator: "abc123.use".into(),
            name: name.map(Into::into),
            description: description.map(Into::into),
            active: true,
        }
    }

    #[test]
    fn profile_names_prefer_tunnel_name_then_description_then_locator_and_never_collide() {
        let none = |_: &str| false;
        assert_eq!(
            unique_profile_id(&summary(Some("dev-box"), Some("x")), none),
            "dev-box"
        );
        assert_eq!(
            unique_profile_id(&summary(None, Some("laptop")), none),
            "laptop"
        );
        assert_eq!(unique_profile_id(&summary(None, None), none), "abc123.use");
        let taken = |id: &str| id == "dev-box" || id == "dev-box (2)";
        assert_eq!(
            unique_profile_id(&summary(Some("dev-box"), None), taken),
            "dev-box (3)"
        );
    }
}
