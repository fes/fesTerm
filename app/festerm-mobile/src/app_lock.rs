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

/// Owns the lock handle and the one-shot automatic unlock prompt.
pub struct LockHost {
    lock: AppLock,
    #[allow(dead_code)] // keeps the native shield and observers alive on iOS
    bridge: Option<festerm_ios_lock::LockBridge>,
    auto_prompted: bool,
    pub error: Option<String>,
}

impl LockHost {
    pub fn new(lock: AppLock, bridge: Option<festerm_ios_lock::LockBridge>) -> Self {
        Self {
            lock,
            bridge,
            auto_prompted: false,
            error: None,
        }
    }

    pub fn view(&self) -> LockView {
        LockView {
            error: self.error.clone(),
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
                        "Set a device passcode in iOS Settings before turning app lock on."
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
    pub fn frame_presented(&self) {
        self.lock.note_frame_presented();
        if let Some(bridge) = &self.bridge {
            bridge.sync();
        }
    }

    /// Draws the unlock screen when content must stay hidden. Returns whether
    /// it drew (and so content must not).
    pub fn gate(&mut self, ui: &mut Ui, active: bool) -> bool {
        if self.lock.should_draw_content() {
            self.auto_prompted = false;
            return false;
        }
        if active
            && !self.auto_prompted
            && self.lock.state() == LockState::Locked
            && self.lock.last_failure().is_none()
        {
            self.auto_prompted = true;
            self.lock.begin_unlock();
        }
        let screen = UnlockScreen::new(self.lock.biometry(), self.lock.last_failure());
        let unlocking = self.lock.state() == LockState::Unlocking;
        match unlock_screen(ui, &screen, unlocking, self.lock.last_failure()) {
            UnlockAction::None => {}
            UnlockAction::Unlock => self.lock.begin_unlock(),
        }
        true
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnlockAction {
    None,
    Unlock,
}

/// Mock-up 14. Nothing sensitive is drawn here.
pub fn unlock_screen(
    ui: &mut Ui,
    screen: &UnlockScreen,
    unlocking: bool,
    failure: Option<AuthFailure>,
) -> UnlockAction {
    let mut action = UnlockAction::None;
    let rect = ui.max_rect();
    ui.painter().rect_filled(rect, 0.0, color::BG);
    let center_x = rect.center().x;
    let tile = egui::Rect::from_center_size(
        egui::pos2(center_x, rect.top() + rect.height() * 0.30),
        egui::vec2(88.0, 88.0),
    );
    ui.painter()
        .rect_filled(tile, CornerRadius::same(radius::CARD + 8), color::ICON_TILE);
    festerm_ui_egui::icon::paint(
        ui.painter(),
        Icon::AppMark,
        tile.shrink(22.0),
        color::ACCENT_TEXT,
    );
    super::widgets::paint_fit(
        ui.painter(),
        egui::pos2(center_x, tile.bottom() + 36.0),
        Align2::CENTER_CENTER,
        screen.title,
        type_scale::font(type_scale::LARGE_TITLE),
        color::TEXT,
        rect.width() - 48.0,
    );
    super::widgets::paint_fit(
        ui.painter(),
        egui::pos2(center_x, tile.bottom() + 66.0),
        Align2::CENTER_CENTER,
        "Sessions keep running while fesTerm is locked.",
        type_scale::font(type_scale::CAPTION),
        color::TEXT_MUTED,
        rect.width() - 48.0,
    );
    let error = screen.error.or_else(|| failure.and_then(failure_message));
    if let Some(error) = error {
        super::widgets::paint_fit(
            ui.painter(),
            egui::pos2(center_x, tile.bottom() + 100.0),
            Align2::CENTER_CENTER,
            error,
            type_scale::font(type_scale::CAPTION),
            color::WARNING,
            rect.width() - 48.0,
        );
    }
    let width = (rect.width() - 48.0).min(420.0);
    let buttons = egui::Rect::from_min_size(
        egui::pos2(center_x - width / 2.0, rect.bottom() - 200.0),
        egui::vec2(width, 2.0 * space::BUTTON_HEIGHT + space::MD),
    );
    ui.scope_builder(egui::UiBuilder::new().max_rect(buttons), |ui| {
        ui.spacing_mut().item_spacing.y = space::MD;
        if button(ui, ButtonKind::Primary, screen.primary_button, !unlocking).clicked() {
            action = UnlockAction::Unlock;
        }
        if button(
            ui,
            ButtonKind::Secondary,
            screen.passcode_button,
            !unlocking,
        )
        .clicked()
        {
            action = UnlockAction::Unlock;
        }
    });
    // Pointer events must not fall through to hidden content.
    ui.allocate_rect(rect, Sense::click());
    action
}

fn availability_note(availability: Availability) -> Option<&'static str> {
    match availability {
        Availability::Available(_) => None,
        Availability::PasscodeNotSet => {
            Some("Set a device passcode in iOS Settings before turning app lock on.")
        }
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
