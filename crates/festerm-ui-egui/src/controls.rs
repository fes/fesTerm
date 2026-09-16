use egui::{Color32, Response, RichText, Sense, Stroke, TextEdit, Ui, UiBuilder, WidgetInfo};

use crate::{
    icon::{self, Icon},
    theme,
};

pub const ACTION_BUTTON_CORNER_RADIUS: f32 = 6.0;

#[derive(Clone, Copy)]
pub enum ActionButtonRole {
    Accent,
    Secondary,
    DangerOutline,
}

#[derive(Clone, Copy)]
pub struct ActionButtonVisuals {
    pub fill: Color32,
    pub stroke: Stroke,
    pub foreground: Color32,
}

pub fn action_button_visuals(role: ActionButtonRole, hovered: bool) -> ActionButtonVisuals {
    match role {
        ActionButtonRole::Accent => ActionButtonVisuals {
            fill: theme::ACCENT_ACTION,
            stroke: Stroke::NONE,
            foreground: theme::TEXT_ON_ACCENT,
        },
        ActionButtonRole::Secondary => ActionButtonVisuals {
            fill: if hovered {
                theme::SURFACE_OVERLAY
            } else {
                theme::SURFACE_TAB_ACTIVE
            },
            stroke: Stroke::new(1.0, theme::BORDER_SUBTLE),
            foreground: theme::TEXT_PRIMARY,
        },
        ActionButtonRole::DangerOutline => ActionButtonVisuals {
            fill: Color32::TRANSPARENT,
            stroke: Stroke::new(1.0, theme::STATUS_ERROR.gamma_multiply(0.75)),
            foreground: theme::STATUS_ERROR,
        },
    }
}

pub fn action_button(ui: &mut Ui, role: ActionButtonRole, label: &str) -> Response {
    action_button_enabled(ui, true, role, label)
}

pub fn action_button_enabled(
    ui: &mut Ui,
    enabled: bool,
    role: ActionButtonRole,
    label: &str,
) -> Response {
    scoped_action_button(ui, role, label, |ui, button| {
        ui.add_enabled(enabled, button)
    })
}

pub fn action_button_sized(
    ui: &mut Ui,
    size: impl Into<egui::Vec2>,
    role: ActionButtonRole,
    label: &str,
) -> Response {
    let size = size.into();
    scoped_action_button(ui, role, label, |ui, button| ui.add_sized(size, button))
}

fn scoped_action_button(
    ui: &mut Ui,
    role: ActionButtonRole,
    label: &str,
    add: impl FnOnce(&mut Ui, egui::Button<'_>) -> Response,
) -> Response {
    let rest = action_button_visuals(role, false);
    let hover = action_button_visuals(role, true);
    ui.scope(|ui| {
        let visuals = &mut ui.style_mut().visuals.widgets;
        for widget in [
            &mut visuals.inactive,
            &mut visuals.active,
            &mut visuals.open,
        ] {
            widget.weak_bg_fill = rest.fill;
            widget.bg_fill = rest.fill;
            widget.bg_stroke = rest.stroke;
            widget.fg_stroke = Stroke::new(1.0, rest.foreground);
        }
        visuals.hovered.weak_bg_fill = hover.fill;
        visuals.hovered.bg_fill = hover.fill;
        visuals.hovered.bg_stroke = hover.stroke;
        visuals.hovered.fg_stroke = Stroke::new(1.0, hover.foreground);
        // Disabled buttons are faded by `Ui::disable`'s opacity multiplier, so the
        // label colour is the same in both states.
        add(
            ui,
            egui::Button::new(RichText::new(label).color(rest.foreground))
                .corner_radius(ACTION_BUTTON_CORNER_RADIUS),
        )
    })
    .inner
}

#[derive(Clone, Copy)]
pub struct SearchField<'a> {
    pub width: f32,
    pub height: f32,
    pub icon_inset: f32,
    pub icon_size: f32,
    pub text_size: f32,
    pub hint: &'a str,
}

impl SearchField<'_> {
    pub fn show(self, ui: &mut Ui, value: &mut String) -> Response {
        search_field(ui, self, value)
    }
}

pub fn search_field(ui: &mut Ui, field_config: SearchField<'_>, value: &mut String) -> Response {
    let (field, _) = ui.allocate_exact_size(
        egui::vec2(field_config.width, field_config.height),
        Sense::hover(),
    );
    ui.painter().rect(
        field,
        8.0,
        theme::SURFACE_FIELD,
        Stroke::new(1.0, theme::BORDER_SUBTLE),
        egui::StrokeKind::Inside,
    );
    let glass = egui::Rect::from_center_size(
        egui::pos2(field.left() + field_config.icon_inset, field.center().y),
        egui::Vec2::splat(field_config.icon_size),
    );
    icon::paint(ui.painter(), Icon::Search, glass, theme::TEXT_MUTED);
    let line = ui
        .painter()
        .layout_no_wrap(
            "Ag".to_owned(),
            egui::FontId::proportional(field_config.text_size),
            theme::TEXT_PRIMARY,
        )
        .size()
        .y;
    let entry = egui::Rect::from_min_max(
        egui::pos2(glass.right() + 8.0, field.center().y - line / 2.0),
        egui::pos2(field.right() - 10.0, field.center().y + line / 2.0),
    );
    let response = ui
        .scope_builder(UiBuilder::new().max_rect(entry), |ui| {
            ui.add_sized(
                entry.size(),
                TextEdit::singleline(value)
                    .frame(egui::Frame::NONE)
                    .background_color(Color32::TRANSPARENT)
                    .font(egui::FontId::proportional(field_config.text_size))
                    .hint_text(field_config.hint)
                    .margin(egui::Margin::ZERO),
            )
        })
        .inner;
    let value = value.clone();
    response.widget_info(|| {
        let mut info = WidgetInfo::text_edit(ui.is_enabled(), &value, &value, field_config.hint);
        info.label = Some(field_config.hint.to_owned());
        info
    });
    response
}
