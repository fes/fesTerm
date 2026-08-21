//! Viewport-level connection overlay.
//!
//! Implements `docs/gui-design.md` "Reconnect presentation": a restrained,
//! restorable overlay for non-nominal connection states that "does not
//! destroy prior terminal content unnecessarily." This is painted as a
//! floating [`egui::Area`] above the terminal viewport rather than replacing
//! it, so the underlying grid remains intact and visible beneath the
//! overlay.
//!
//! This module is pure presentation: it owns no session or tab state and
//! performs no protocol or backend work. Callers translate the returned
//! [`OverlayAction`] into application commands per
//! `docs/application-command-model.md`.

use egui::{Align2, Area, Context, Frame, Order, RichText};

use crate::chrome::ChipStatus;

/// Actions a user can take from the connection overlay. There is no
/// close-tab action here: the established pattern for dismissing a tab is
/// the chip's own close ("×") button, not a redundant control duplicated
/// into every overlay.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OverlayAction {
    /// Opens (or focuses) the session inspector for lifecycle/error detail.
    OpenDiagnostics,
}

/// Returns `true` if `status` warrants a viewport overlay. Nominal states
/// (connected, starting, and non-session surfaces) never show one.
const fn overlay_status(status: ChipStatus) -> bool {
    matches!(
        status,
        ChipStatus::Reconnecting
            | ChipStatus::AuthRequired
            | ChipStatus::Failed
            | ChipStatus::Disconnected
            | ChipStatus::Exited
    )
}

/// Shows a restrained, centered overlay describing `status` if it is
/// non-nominal, returning the user's chosen action (if any) for this frame.
/// Draws nothing and returns `None` for connected/starting/neutral states.
pub fn show(ctx: &Context, status: ChipStatus) -> Option<OverlayAction> {
    if !overlay_status(status) {
        return None;
    }

    let mut action = None;
    Area::new("festerm_connection_overlay".into())
        .order(Order::Foreground)
        .anchor(Align2::CENTER_BOTTOM, egui::vec2(0.0, -24.0))
        .interactable(true)
        .show(ctx, |ui| {
            Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_max_width(220.0);
                ui.vertical_centered(|ui| {
                    ui.label(RichText::new(status.accessible_label()).strong().small());
                    ui.add_space(4.0);
                    if ui.button("Open Diagnostics").clicked() {
                        action = Some(OverlayAction::OpenDiagnostics);
                    }
                });
            });
        });
    action
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::{kittest::Queryable, Harness};

    fn harness(status: ChipStatus) -> Harness<'static, Option<OverlayAction>> {
        Harness::builder()
            .with_size(egui::vec2(400.0, 300.0))
            .build_ui_state(
                move |ui, action: &mut Option<OverlayAction>| {
                    if let Some(clicked) = show(ui.ctx(), status) {
                        *action = Some(clicked);
                    }
                },
                None,
            )
    }

    #[test]
    fn no_overlay_is_drawn_for_nominal_states() {
        for status in [
            ChipStatus::Connected,
            ChipStatus::Starting,
            ChipStatus::Neutral,
        ] {
            let mut harness = harness(status);
            harness.run();
            assert!(
                harness.query_by_label("Open Diagnostics").is_none(),
                "unexpected overlay for {status:?}"
            );
        }
    }

    #[test]
    fn failed_state_shows_an_overlay_with_the_diagnostics_action() {
        let mut harness = harness(ChipStatus::Failed);
        harness.run();

        assert!(harness.get_by_label_contains("Failed").rect().width() > 0.0);
        harness.get_by_label("Open Diagnostics").click();
        harness.run();
        assert_eq!(*harness.state(), Some(OverlayAction::OpenDiagnostics));
    }

    #[test]
    fn reconnecting_and_auth_required_each_show_an_overlay() {
        for status in [
            ChipStatus::Reconnecting,
            ChipStatus::AuthRequired,
            ChipStatus::Exited,
        ] {
            let mut harness = harness(status);
            harness.run();
            assert!(
                harness.query_by_label("Open Diagnostics").is_some(),
                "expected overlay for {status:?}"
            );
        }
    }
}
