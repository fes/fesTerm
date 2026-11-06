//! App-lock UI (mock-ups 14 and 15). Policy, persistence and the native
//! privacy shield live in `festerm-ios-lock`; this module only draws them.

use super::theme::{color, radius, space, type_scale};
use super::widgets::{
    button, card, footnote, list_row, section_header, switch_row, ButtonKind, RowSpec, Trailing,
};
use eframe::egui::{self, Align2, CornerRadius, Sense, Ui};
use festerm_ios_lock::{
    ui_model::{
        failure_message, UnlockScreen, BACKGROUND_GRACE_LABEL, GRACE_CHOICES, HIDE_PREVIEW_LABEL,
        LOCK_NOW_LABEL, NOT_SESSION_RECOVERY_NOTE, PROTECT_LABEL,
        REQUIRE_UNLOCK_AFTER_DEVICE_LOCK_LABEL, REQUIRE_UNLOCK_ON_LAUNCH_LABEL,
    },
    AppLock, AuthFailure, Availability, BiometryKind, LoadStatus, LockSettings, LockState,
    SettingsError,
};
use festerm_ui_egui::icon::Icon;

/// What the settings screen needs, free of the live handle so the gallery can
/// build it directly.
#[derive(Clone, Debug)]
pub struct LockView {
    pub settings: LockSettings,
    pub availability: Availability,
    pub untrusted_settings: bool,
    pub error: Option<String>,
}

impl LockView {
    pub fn of(lock: &AppLock) -> Self {
        Self {
            settings: lock.settings(),
            availability: lock.availability(),
            untrusted_settings: lock.load_status() == LoadStatus::Untrusted,
            error: None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LockAction {
    Apply(LockSettings),
    LockNow,
}

/// Owns lock presentation; authentication lifetime belongs to the lock handle.
pub struct LockHost {
    lock: AppLock,
    #[allow(dead_code)] // keeps the native shield and observers alive on iOS
    bridge: Option<festerm_ios_lock::LockBridge>,
    last_frame: Option<u64>,
    pub error: Option<String>,
}

impl LockHost {
    pub fn new(lock: AppLock, bridge: Option<festerm_ios_lock::LockBridge>) -> Self {
        Self {
            lock,
            bridge,
            last_frame: None,
            error: None,
        }
    }

    /// Privacy mode (App Lock with a hidden preview) keeps terminal text out
    /// of the accessibility tree (ADR 0075).
    pub fn hides_terminal_text(&self) -> bool {
        let settings = self.lock.settings();
        settings.enabled && settings.hide_app_switcher_preview
    }

    pub fn view(&self) -> LockView {
        LockView {
            error: self
                .error
                .clone()
                .or_else(|| self.lock.settings_error().map(str::to_owned)),
            ..LockView::of(&self.lock)
        }
    }

    pub fn apply(&mut self, action: LockAction) {
        self.error = None;
        match action {
            LockAction::Apply(settings) => {
                self.error = match self.lock.apply_settings(settings) {
                    Ok(()) | Err(SettingsError::AuthenticationStarted) => None,
                    Err(SettingsError::CannotAuthenticate(_)) => Some(
                        super::platform_text(
                            "Set a device passcode in iOS Settings before turning app lock on.",
                            "Set a screen lock in Android Settings before turning app lock on.",
                        )
                        .to_owned(),
                    ),
                    Err(SettingsError::Persist(_)) => {
                        Some("Could not save the lock settings.".to_owned())
                    }
                };
            }
            LockAction::LockNow => self.lock.lock_now(),
        }
    }

    /// Per-frame bookkeeping after the frame's content decision.
    pub fn frame_presented(&mut self, ctx: &egui::Context) {
        let frame = ctx.cumulative_frame_nr();
        if self.last_frame != Some(frame) {
            self.last_frame = Some(frame);
            self.lock.note_frame_presented();
        }
        if let Some(bridge) = &self.bridge {
            bridge.sync();
        }
    }

    /// Draws the unlock screen when content must stay hidden. Returns whether
    /// it drew (and so content must not).
    pub fn gate(&mut self, ui: &mut Ui, active: bool) -> bool {
        if self.lock.should_draw_content() {
            return false;
        }
        if active {
            self.lock.begin_automatic_unlock();
        }
        let biometry = self.lock.biometry();
        let mut screen = UnlockScreen::new(biometry, self.lock.last_failure());
        let unlocking = self.lock.state() == LockState::Unlocking;
        if unlocking {
            screen.primary_button = biometry.retry_button_label();
        }
        let action = ui
            .add_enabled_ui(active, |ui| {
                unlock_screen(ui, &screen, unlocking, self.lock.last_failure())
            })
            .inner;
        match action {
            UnlockAction::None => {}
            UnlockAction::Unlock => self.lock.retry_unlock(),
            UnlockAction::Passcode => self.lock.begin_passcode_unlock(),
        }
        true
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnlockAction {
    None,
    Unlock,
    Passcode,
}

/// Mock-up 14. Nothing sensitive is drawn here.
pub fn unlock_screen(
    ui: &mut Ui,
    screen: &UnlockScreen,
    unlocking: bool,
    failure: Option<AuthFailure>,
) -> UnlockAction {
    let rect = ui.max_rect();
    ui.painter().rect_filled(rect, 0.0, color::BG);
    // Register the backdrop before controls so it cannot cover their hit targets.
    ui.interact(rect, ui.id().with("lock-backdrop"), Sense::click());
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let size = egui::vec2(ui.available_width(), rect.height().max(330.0));
            let (content, _) = ui.allocate_exact_size(size, Sense::hover());
            unlock_contents(ui, content, screen, unlocking, failure)
        })
        .inner
}

fn unlock_contents(
    ui: &mut Ui,
    rect: egui::Rect,
    screen: &UnlockScreen,
    unlocking: bool,
    failure: Option<AuthFailure>,
) -> UnlockAction {
    let mut action = UnlockAction::None;
    let compact = rect.height() < 540.0;
    let center_x = rect.center().x;
    let tile = egui::Rect::from_center_size(
        egui::pos2(
            center_x,
            rect.top() + if compact { 64.0 } else { rect.height() * 0.30 },
        ),
        egui::Vec2::splat(if compact { 64.0 } else { 88.0 }),
    );
    ui.painter()
        .rect_filled(tile, CornerRadius::same(radius::CARD + 8), color::ICON_TILE);
    festerm_ui_egui::icon::paint(
        ui.painter(),
        Icon::AppMark,
        tile.shrink(if compact { 16.0 } else { 22.0 }),
        color::ACCENT_TEXT,
    );
    super::widgets::paint_fit(
        ui.painter(),
        egui::pos2(center_x, tile.bottom() + if compact { 24.0 } else { 36.0 }),
        Align2::CENTER_CENTER,
        screen.title,
        type_scale::font(type_scale::LARGE_TITLE),
        color::TEXT,
        rect.width() - 48.0,
    );
    super::widgets::paint_fit(
        ui.painter(),
        egui::pos2(center_x, tile.bottom() + if compact { 52.0 } else { 66.0 }),
        Align2::CENTER_CENTER,
        if unlocking {
            "Waiting for iOS authentication"
        } else {
            "Sessions keep running while fesTerm is locked."
        },
        type_scale::font(type_scale::CAPTION),
        color::TEXT_MUTED,
        rect.width() - 48.0,
    );
    let error = screen.error.or_else(|| failure.and_then(failure_message));
    if let Some(error) = error {
        ui.put(
            egui::Rect::from_center_size(
                egui::pos2(center_x, tile.bottom() + if compact { 76.0 } else { 100.0 }),
                egui::vec2(rect.width() - 48.0, 40.0),
            ),
            egui::Label::new(
                egui::RichText::new(error)
                    .font(type_scale::font(type_scale::CAPTION))
                    .color(color::WARNING),
            )
            .wrap()
            .halign(egui::Align::Center),
        );
    }
    let width = (rect.width() - 48.0).min(420.0);
    let buttons = egui::Rect::from_min_size(
        egui::pos2(
            center_x - width / 2.0,
            rect.bottom() - if compact { 132.0 } else { 200.0 },
        ),
        egui::vec2(width, 2.0 * space::BUTTON_HEIGHT + space::MD),
    );
    ui.scope_builder(egui::UiBuilder::new().max_rect(buttons), |ui| {
        ui.spacing_mut().item_spacing.y = space::MD;
        if button(ui, ButtonKind::Primary, screen.primary_button, true).clicked() {
            action = UnlockAction::Unlock;
        }
        if button(ui, ButtonKind::Secondary, screen.passcode_button, true).clicked() {
            action = UnlockAction::Passcode;
        }
    });
    action
}

fn availability_note(availability: Availability) -> Option<&'static str> {
    match availability {
        Availability::Available(_) => None,
        Availability::PasscodeNotSet => Some(super::platform_text(
            "Set a device passcode in iOS Settings before turning app lock on.",
            "Set a screen lock in Android Settings before turning app lock on.",
        )),
        Availability::Unavailable => Some("This device cannot authenticate the owner."),
    }
}

fn biometry_name(kind: BiometryKind) -> &'static str {
    match kind {
        BiometryKind::FaceId => "Face ID",
        BiometryKind::TouchId => "Touch ID",
        BiometryKind::OpticId => "Optic ID",
        BiometryKind::None => "Device passcode",
    }
}

/// Mock-up 15, drawn inside the Settings tab.
pub fn settings_section(ui: &mut Ui, view: &LockView) -> Vec<LockAction> {
    let mut actions = Vec::new();
    let mut next = view.settings;
    section_header(ui, "App lock", None, false);
    let usable = matches!(view.availability, Availability::Available(_));
    card(ui, |ui| {
        ui.add_enabled_ui(usable || view.settings.enabled, |ui| {
            switch_row(
                ui,
                PROTECT_LABEL,
                Some(match view.availability {
                    Availability::Available(kind) => biometry_name(kind),
                    _ => "Needs a device passcode",
                }),
                &mut next.enabled,
                false,
            );
        });
        if view.settings.enabled {
            switch_row(
                ui,
                REQUIRE_UNLOCK_ON_LAUNCH_LABEL,
                None,
                &mut next.on_launch,
                true,
            );
            switch_row(
                ui,
                REQUIRE_UNLOCK_AFTER_DEVICE_LOCK_LABEL,
                None,
                &mut next.after_device_lock,
                true,
            );
            switch_row(
                ui,
                HIDE_PREVIEW_LABEL,
                Some("Cover fesTerm in the app switcher"),
                &mut next.hide_app_switcher_preview,
                true,
            );
        }
    });
    if view.settings.enabled {
        section_header(ui, BACKGROUND_GRACE_LABEL, None, false);
        card(ui, |ui| {
            for (index, grace) in GRACE_CHOICES.iter().enumerate() {
                let mut row = RowSpec::new(grace.label());
                row.trailing = if *grace == view.settings.background_grace {
                    Trailing::Check
                } else {
                    Trailing::None
                };
                row.divider = index > 0;
                if list_row(ui, &row).clicked() {
                    next.background_grace = *grace;
                }
            }
        });
        ui.add_space(space::SM);
        if button(ui, ButtonKind::Secondary, LOCK_NOW_LABEL, true).clicked() {
            actions.push(LockAction::LockNow);
        }
    }
    if view.untrusted_settings {
        footnote(
            ui,
            "The saved lock settings could not be read, so fesTerm stayed locked. Review them here.",
        );
    }
    if let Some(note) = availability_note(view.availability) {
        footnote(ui, note);
    }
    if let Some(error) = &view.error {
        footnote(ui, error);
    }
    footnote(ui, NOT_SESSION_RECOVERY_NOTE);
    if next != view.settings {
        actions.push(LockAction::Apply(next));
    }
    actions
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::{kittest::Queryable, Harness};
    use festerm_ios_lock::{Availability, BiometryKind, FakeLocker, ManualClock};
    use std::sync::Arc;

    fn gate_hides(host: &mut LockHost) -> bool {
        let ctx = egui::Context::default();
        let mut hidden = false;
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            hidden = host.gate(ui, true);
        });
        output.textures_delta.clear();
        hidden
    }

    #[test]
    fn content_stays_hidden_until_authentication_succeeds_and_lock_now_hides_it_again() {
        let locker = FakeLocker::new(Availability::Available(BiometryKind::FaceId));
        let lock = AppLock::new(None, locker.clone(), Arc::new(ManualClock::new()), || {});
        let mut host = LockHost::new(lock, None);
        host.apply(LockAction::Apply(LockSettings {
            enabled: true,
            ..LockSettings::default()
        }));
        assert!(host.error.is_none());
        host.apply(LockAction::LockNow);
        locker.set_deferred(true);
        assert!(gate_hides(&mut host), "locked content is never drawn");
        assert_eq!(locker.pending_count(), 1, "the unlock prompt starts once");
        assert!(gate_hides(&mut host));
        assert_eq!(locker.pending_count(), 1);
        locker.complete_pending(Ok(()));
        assert!(!gate_hides(&mut host));
        host.apply(LockAction::LockNow);
        assert!(gate_hides(&mut host));
    }

    #[test]
    fn enabling_without_a_device_passcode_is_reported_and_not_enabled() {
        let locker = FakeLocker::new(Availability::Unavailable);
        let lock = AppLock::new(None, locker, Arc::new(ManualClock::new()), || {});
        let mut host = LockHost::new(lock, None);
        host.apply(LockAction::Apply(LockSettings {
            enabled: true,
            ..LockSettings::default()
        }));
        assert!(host.error.is_some());
        assert!(!host.view().settings.enabled);
    }

    #[test]
    fn unlock_buttons_receive_pointer_input_after_cancelled_automatic_authentication() {
        for size in [
            egui::vec2(390.0, 844.0),
            egui::vec2(320.0, 568.0),
            egui::vec2(844.0, 390.0),
            egui::vec2(390.0, 240.0),
        ] {
            let locker = FakeLocker::new(Availability::Available(BiometryKind::FaceId));
            locker.set_deferred(true);
            let lock = AppLock::new(None, locker.clone(), Arc::new(ManualClock::new()), || {});
            lock.apply_settings(LockSettings {
                enabled: true,
                ..LockSettings::default()
            })
            .unwrap();
            lock.lock_now();
            let mut harness = Harness::builder().with_size(size).build_ui_state(
                |ui, host: &mut LockHost| {
                    assert!(host.gate(ui, true));
                },
                LockHost::new(lock, None),
            );
            harness.run();
            assert_eq!(locker.pending_count(), 1);
            locker.complete_pending(Err(AuthFailure::Cancelled));
            harness.run();
            if size.y < 330.0 {
                harness.event(egui::Event::PointerMoved(egui::pos2(
                    size.x / 2.0,
                    size.y / 2.0,
                )));
                harness.event(egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: egui::vec2(0.0, -180.0),
                    phase: egui::TouchPhase::Move,
                    modifiers: egui::Modifiers::NONE,
                });
                harness.run();
            }
            for label in [
                "Try Face ID Again",
                festerm_ios_lock::ui_model::USE_PASSCODE_LABEL,
            ] {
                let rect = harness.get_by_label(label).rect();
                assert!(rect.width() >= 44.0 && rect.height() >= 44.0);
                assert!(egui::Rect::from_min_size(egui::Pos2::ZERO, size).contains_rect(rect));
            }
            harness.get_by_label("Try Face ID Again").click();
            harness.run();
            assert_eq!(
                locker.pending_count(),
                1,
                "retry button must not be covered by the lock-screen input shield"
            );
            harness
                .get_by_label(festerm_ios_lock::ui_model::USE_PASSCODE_LABEL)
                .click();
            harness.run();
            assert_eq!(locker.cancellation_count(), 1);
            assert_eq!(
                locker.prompt_methods().last(),
                Some(&festerm_ios_lock::AuthMethod::DevicePasscode)
            );
        }
    }

    #[test]
    fn unlock_errors_wrap_without_covering_retry_controls() {
        for size in [egui::vec2(320.0, 568.0), egui::vec2(320.0, 330.0)] {
            for failure in [
                AuthFailure::NotRecognized,
                AuthFailure::BiometryLockedOut,
                AuthFailure::PasscodeNotSet,
                AuthFailure::Unavailable,
            ] {
                let mut harness = Harness::builder().with_size(size).build_ui(move |ui| {
                    unlock_screen(
                        ui,
                        &UnlockScreen::new(BiometryKind::FaceId, Some(failure)),
                        false,
                        Some(failure),
                    );
                });
                harness.run();
                let error = harness
                    .get_by_label(failure_message(failure).unwrap())
                    .rect();
                let button = harness.get_by_label("Try Face ID Again").rect();
                assert!(error.bottom() < button.top(), "{failure:?} at {size:?}");
                assert!(error.width() <= size.x - 48.0);
                assert!(error.height() <= 40.0);
            }
        }
    }

    #[test]
    fn shield_warmup_counts_frames_not_repeated_egui_passes() {
        let locker = FakeLocker::new(Availability::Available(BiometryKind::FaceId));
        let lock = AppLock::new(None, locker, Arc::new(ManualClock::new()), || {});
        lock.apply_settings(LockSettings {
            enabled: true,
            ..LockSettings::default()
        })
        .unwrap();
        lock.handle_lifecycle(festerm_ios_lock::LifecycleEvent::DidBecomeActive);
        let mut host = LockHost::new(lock.clone(), None);
        let ctx = egui::Context::default();
        let passes = ctx.cumulative_pass_nr();
        ctx.run_ui(egui::RawInput::default(), |ui| {
            host.frame_presented(ui.ctx());
            host.frame_presented(ui.ctx());
            if ui.ctx().current_pass_index() == 0 {
                ui.ctx()
                    .request_discard("exercise actual shield layout retry");
            }
        })
        .drop_without_applying_deltas();
        assert_eq!(ctx.cumulative_pass_nr() - passes, 2);
        assert!(lock.shield_should_cover());
        ctx.run_ui(egui::RawInput::default(), |ui| {
            host.frame_presented(ui.ctx())
        })
        .drop_without_applying_deltas();
        assert!(!lock.shield_should_cover());
    }
}
