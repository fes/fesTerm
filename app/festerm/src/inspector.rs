//! Session Inspector overlay presentation.
//!
//! The inspector is deliberately an [`egui::Area`], not a side panel: opening
//! it must not change the terminal viewport or generate a PTY resize.

use eframe::egui::{
    self, vec2, Area, Color32, Frame, Label, Margin, Order, Rect, RichText, ScrollArea, Sense,
    Stroke,
};
use festerm_ui_egui::theme;

const DESKTOP_WIDTH: f32 = 320.0;
const DESKTOP_INSET: f32 = 8.0;
const NARROW_THRESHOLD: f32 = 480.0;
const NARROW_HORIZONTAL_MARGIN: f32 = 16.0;

#[derive(Clone)]
pub enum TransportFacts<'a> {
    Local,
    Ssh {
        username: &'a str,
        host: &'a str,
        port: u16,
    },
    Sftp {
        username: &'a str,
        host: &'a str,
        port: u16,
    },
    Serial {
        device: &'a str,
        baud_rate: u32,
        data_bits: &'a str,
        parity: &'a str,
        stop_bits: &'a str,
        flow_control: &'a str,
    },
}

#[derive(Clone)]
pub struct InspectorContent<'a> {
    pub subject_id: u64,
    pub identity: &'a str,
    pub type_label: &'a str,
    pub state: &'a str,
    pub state_message: Option<&'a str>,
    pub state_color: Color32,
    pub grid: Option<&'a str>,
    pub terminal_title: Option<&'a str>,
    pub profile: Option<&'a str>,
    pub transport: TransportFacts<'a>,
    pub trust_fingerprint: Option<&'a str>,
    pub diagnostics: &'a str,
    pub reconnect_available: bool,
    pub open_sftp_available: bool,
    /// The durable remote-session provider and name this SSH connection
    /// attaches to or creates, if any (ADR 0018). `None` for local sessions
    /// and for ordinary manual-recovery plain SSH shells; drives the
    /// Reconnect-vs-Resume language distinction below.
    pub persistent_session: Option<PersistentSessionFacts<'a>>,
}

/// Non-secret durable-session facts surfaced in the Session Inspector.
#[derive(Clone, Copy)]
pub struct PersistentSessionFacts<'a> {
    pub provider_label: &'a str,
    pub session_name: &'a str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InspectorAction {
    Close,
    Reconnect,
    OpenSftp,
}

/// Computes the overlay bounds within the content viewport. Kept pure so
/// responsive geometry can be checked without a renderer.
pub fn overlay_rect(content: Rect) -> Rect {
    let (left, right) = if content.width() <= NARROW_THRESHOLD {
        (
            content.left() + NARROW_HORIZONTAL_MARGIN,
            content.right() - NARROW_HORIZONTAL_MARGIN,
        )
    } else {
        (
            content.right() - DESKTOP_INSET - DESKTOP_WIDTH,
            content.right() - DESKTOP_INSET,
        )
    };
    Rect::from_min_max(
        egui::pos2(left, content.top() + DESKTOP_INSET),
        egui::pos2(right, content.bottom() - DESKTOP_INSET),
    )
}

pub fn show(
    ctx: &egui::Context,
    content_rect: Rect,
    content: InspectorContent<'_>,
    close_requested: bool,
) -> Option<InspectorAction> {
    let mut action = close_requested.then_some(InspectorAction::Close);

    // This transparent foreground surface ensures the first click on the
    // uncovered terminal dismisses the inspector without reaching terminal
    // mouse handling. A subsequent click occurs after the surface is gone.
    Area::new("session_inspector_click_catcher".into())
        .order(Order::Foreground)
        .fixed_pos(content_rect.min)
        .show(ctx, |ui| {
            let (_, response) = ui.allocate_exact_size(content_rect.size(), Sense::click());
            if response.clicked() {
                action = Some(InspectorAction::Close);
            }
        });

    let panel_rect = overlay_rect(content_rect);
    Area::new("session_inspector".into())
        .order(Order::Foreground)
        .fixed_pos(panel_rect.min)
        .show(ctx, |ui| {
            ui.set_min_size(panel_rect.size());
            ui.set_max_size(panel_rect.size());
            Frame::new()
                .fill(theme::SURFACE_OVERLAY)
                .stroke(Stroke::new(1.0, theme::BORDER_SUBTLE))
                .inner_margin(Margin::same(20))
                .show(ui, |ui| {
                    ui.set_min_size(panel_rect.size() - vec2(40.0, 40.0));
                    ui.set_max_size(panel_rect.size() - vec2(40.0, 40.0));
                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.label(
                                RichText::new("Session Inspector")
                                    .size(20.0)
                                    .color(theme::TEXT_PRIMARY),
                            );
                            ui.label(
                                RichText::new(format!(
                                    "{} · {}",
                                    content.type_label, content.state
                                ))
                                .size(13.0)
                                .color(theme::TEXT_SECONDARY),
                            );
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::TOP), |ui| {
                            let close = ui.button("Close").on_hover_text("Close Session Inspector");
                            close.widget_info(|| {
                                egui::WidgetInfo::labeled(
                                    egui::WidgetType::Button,
                                    true,
                                    "Close Session Inspector",
                                )
                            });
                            let focus_id = egui::Id::new("session_inspector_focus_subject");
                            let frame = ui.ctx().cumulative_frame_nr();
                            let prior = ui.data(|data| data.get_temp::<(u64, u64)>(focus_id));
                            let stayed_open = prior.is_some_and(|(subject, prior_frame)| {
                                subject == content.subject_id && prior_frame + 1 == frame
                            });
                            if !stayed_open {
                                close.request_focus();
                            }
                            ui.data_mut(|data| {
                                data.insert_temp(focus_id, (content.subject_id, frame))
                            });
                            if close.clicked() {
                                action = Some(InspectorAction::Close);
                            }
                        });
                    });
                    ui.add_space(12.0);
                    ui.separator();
                    ui.add_space(8.0);

                    let body_height = ui.available_height();
                    ScrollArea::vertical()
                        .id_salt(("session_inspector_body", content.subject_id))
                        .max_height(body_height)
                        .show(ui, |ui| {
                            if let Some(message) = content.state_message {
                                ui.colored_label(content.state_color, message);
                                ui.add_space(12.0);
                            }

                            section_heading(ui, "Session");
                            fact(ui, "Name", content.identity, true);
                            fact(ui, "State", content.state, false);
                            if let Some(profile) = content.profile {
                                fact(ui, "Profile", profile, true);
                            }
                            if let Some(grid) = content.grid {
                                fact(ui, "Grid", grid, false);
                            }
                            if let Some(title) = content.terminal_title {
                                fact(ui, "Terminal title", title, true);
                            }

                            match content.transport {
                                TransportFacts::Local => {
                                    section_heading(ui, "Process");
                                    fact(ui, "Type", "Local shell", false);
                                }
                                TransportFacts::Ssh {
                                    username,
                                    host,
                                    port,
                                } => {
                                    section_heading(ui, "Connection");
                                    fact(ui, "Destination", &format!("{host}:{port}"), true);
                                    fact(ui, "Username", username, true);
                                    fact(ui, "Type", "SSH", false);
                                }
                                TransportFacts::Sftp {
                                    username,
                                    host,
                                    port,
                                } => {
                                    section_heading(ui, "Connection");
                                    fact(ui, "Destination", &format!("{host}:{port}"), true);
                                    fact(ui, "Username", username, true);
                                    fact(ui, "Type", "SFTP", false);
                                }
                                TransportFacts::Serial {
                                    device,
                                    baud_rate,
                                    data_bits,
                                    parity,
                                    stop_bits,
                                    flow_control,
                                } => {
                                    section_heading(ui, "Serial Port");
                                    fact(ui, "Device", device, true);
                                    fact(ui, "Baud rate", &baud_rate.to_string(), false);
                                    fact(ui, "Data bits", data_bits, false);
                                    fact(ui, "Parity", parity, false);
                                    fact(ui, "Stop bits", stop_bits, false);
                                    fact(ui, "Flow control", flow_control, false);
                                }
                            }

                            if let Some(fingerprint) = content.trust_fingerprint {
                                section_heading(ui, "Trust");
                                fact(ui, "Scope", "Verification pending", false);
                                fact(ui, "SHA-256 fingerprint", fingerprint, true);
                            }

                            if let Some(persistence) = content.persistent_session {
                                section_heading(ui, "Durable session");
                                fact(ui, "Provider", persistence.provider_label, false);
                                fact(ui, "Session name", persistence.session_name, true);
                            }

                            if content.reconnect_available || content.open_sftp_available {
                                section_heading(ui, "Actions");
                                // ADR 0018: a plain shell only gets a fresh
                                // transport ("Reconnect"); a durable-session
                                // profile reattaches to remote state that
                                // outlives the transport ("Resume").
                                let label = if content.persistent_session.is_some() {
                                    "Resume"
                                } else {
                                    "Reconnect"
                                };
                                if ui.button(label).clicked() {
                                    action = Some(InspectorAction::Reconnect);
                                }
                                if content.open_sftp_available && ui.button("Open SFTP").clicked() {
                                    action = Some(InspectorAction::OpenSftp);
                                }
                            }

                            ui.add_space(14.0);
                            egui::CollapsingHeader::new("Diagnostics")
                                .id_salt(("session_inspector_diagnostics", content.subject_id))
                                .default_open(false)
                                .show(ui, |ui| {
                                    ui.add(
                                        Label::new(
                                            RichText::new(content.diagnostics)
                                                .monospace()
                                                .size(10.0)
                                                .color(theme::TEXT_SECONDARY),
                                        )
                                        .selectable(true)
                                        .wrap(),
                                    );
                                });
                        });
                });
        });
    action
}

fn section_heading(ui: &mut egui::Ui, heading: &str) {
    ui.add_space(14.0);
    ui.label(
        RichText::new(heading.to_uppercase())
            .size(10.0)
            .color(theme::TEXT_MUTED),
    );
    ui.add_space(4.0);
}

fn fact(ui: &mut egui::Ui, label: &str, value: &str, selectable: bool) {
    ui.label(RichText::new(label).size(10.0).color(theme::TEXT_MUTED));
    let value = Label::new(RichText::new(value).size(13.0).color(theme::TEXT_PRIMARY));
    ui.add(if selectable {
        value.selectable(true)
    } else {
        value
    });
    ui.add_space(6.0);
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::{kittest::Queryable, Harness};

    #[test]
    fn desktop_overlay_is_exactly_320_points_with_eight_point_insets() {
        let content = Rect::from_min_size(egui::pos2(0.0, 42.0), vec2(752.0, 450.0));
        let overlay = overlay_rect(content);
        assert_eq!(overlay.width(), 320.0);
        assert_eq!(content.right() - overlay.right(), 8.0);
        assert_eq!(overlay.top() - content.top(), 8.0);
        assert_eq!(content.bottom() - overlay.bottom(), 8.0);
    }

    #[test]
    fn narrow_overlay_keeps_sixteen_point_horizontal_margins() {
        let content = Rect::from_min_size(egui::pos2(4.0, 42.0), vec2(480.0, 300.0));
        let overlay = overlay_rect(content);
        assert_eq!(overlay.left() - content.left(), 16.0);
        assert_eq!(content.right() - overlay.right(), 16.0);
    }

    fn base_content(subject_id: u64) -> InspectorContent<'static> {
        InspectorContent {
            subject_id,
            identity: "test-user@example.invalid",
            type_label: "SSH",
            state: "Connected",
            state_message: None,
            state_color: Color32::WHITE,
            grid: None,
            terminal_title: None,
            profile: None,
            transport: TransportFacts::Ssh {
                username: "test-user",
                host: "example.invalid",
                port: 22,
            },
            trust_fingerprint: None,
            diagnostics: "",
            reconnect_available: true,
            open_sftp_available: true,
            persistent_session: None,
        }
    }

    fn harness_for(content: InspectorContent<'static>) -> Harness<'static, ()> {
        Harness::builder()
            .with_size(egui::vec2(760.0, 480.0))
            .build_ui(move |ui| {
                let content_rect = ui.max_rect();
                show(ui.ctx(), content_rect, content.clone(), false);
            })
    }

    #[test]
    fn a_plain_shell_offers_reconnect_and_no_durable_session_facts() {
        let mut harness = harness_for(base_content(1));
        harness.run();

        harness.get_by_label("Reconnect");
        assert!(harness.query_by_label("Resume").is_none());
        assert!(harness.query_by_label("DURABLE SESSION").is_none());
    }

    #[test]
    fn a_persistent_strategy_offers_resume_and_shows_durable_session_facts() {
        let mut content = base_content(2);
        content.persistent_session = Some(PersistentSessionFacts {
            provider_label: "tmux",
            session_name: "build",
        });
        let mut harness = harness_for(content);
        harness.run();

        harness.get_by_label("Resume");
        assert!(harness.query_by_label("Reconnect").is_none());
        harness.get_by_label("DURABLE SESSION");
        harness.get_by_label("build");
    }
}
