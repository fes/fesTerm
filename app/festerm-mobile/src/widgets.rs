//! Reusable mobile widgets painted from the design tokens.
//!
//! Every tappable control uses `Sense::CLICK` (non-focusable) so taps never
//! steal focus from the terminal and restart the native keyboard.

use super::theme::{color, radius, space, type_scale};
use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, Id, Margin, Rect, Response, Sense, Stroke,
    StrokeKind, Ui, WidgetInfo, WidgetType,
};
use festerm_ui_egui::icon::{self, Icon};

pub fn labelled(response: Response, label: &str, enabled: bool) -> Response {
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, enabled, label));
    response
}

/// Paints single-line text clipped with an ellipsis to `max_width`.
pub fn paint_fit(
    painter: &egui::Painter,
    pos: egui::Pos2,
    anchor: Align2,
    text: &str,
    font: FontId,
    text_color: Color32,
    max_width: f32,
) -> Rect {
    let mut job = egui::text::LayoutJob::simple_singleline(text.to_owned(), font, text_color);
    job.wrap = egui::text::TextWrapping {
        max_width: max_width.max(1.0),
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    let galley = painter.layout_job(job);
    let rect = anchor.anchor_size(pos, galley.size());
    painter.galley(rect.min, galley, text_color);
    rect
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ButtonKind {
    Primary,
    Secondary,
    Destructive,
}

pub fn button_sized(
    ui: &mut Ui,
    kind: ButtonKind,
    label: &str,
    enabled: bool,
    size: egui::Vec2,
) -> Response {
    let sense = if enabled {
        Sense::CLICK
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(size, sense);
    let pressed = response.is_pointer_button_down_on();
    let (fill, text, stroke) = match kind {
        ButtonKind::Primary => (
            if pressed {
                color::PRIMARY_PRESSED
            } else {
                color::PRIMARY
            },
            color::ON_PRIMARY,
            Stroke::NONE,
        ),
        ButtonKind::Secondary => (
            if pressed {
                color::SURFACE
            } else {
                color::SURFACE_RAISED
            },
            color::TEXT,
            Stroke::new(1.0, color::BORDER),
        ),
        ButtonKind::Destructive => (
            color::WARNING_FILL,
            color::WARNING,
            Stroke::new(1.0, Color32::from_rgb(120, 92, 62)),
        ),
    };
    let (fill, text) = if enabled {
        (fill, text)
    } else {
        (color::SURFACE, color::TEXT_FAINT)
    };
    let painter = ui.painter();
    let corner = CornerRadius::same(radius::BUTTON);
    painter.rect_filled(rect, corner, fill);
    if stroke != Stroke::NONE {
        painter.rect_stroke(rect, corner, stroke, StrokeKind::Inside);
    }
    paint_fit(
        painter,
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        type_scale::font(type_scale::BODY),
        text,
        rect.width() - 16.0,
    );
    labelled(response, label, enabled)
}

pub fn button(ui: &mut Ui, kind: ButtonKind, label: &str, enabled: bool) -> Response {
    let size = egui::vec2(ui.available_width(), space::BUTTON_HEIGHT);
    button_sized(ui, kind, label, enabled, size)
}

/// Rounded surface for grouped rows; rows draw their own dividers.
pub fn card<R>(ui: &mut Ui, add_contents: impl FnOnce(&mut Ui) -> R) -> R {
    egui::Frame::new()
        .fill(color::SURFACE)
        .stroke(Stroke::new(1.0, color::DIVIDER))
        .corner_radius(CornerRadius::same(radius::CARD))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.set_width(ui.available_width());
            add_contents(ui)
        })
        .inner
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Dot {
    Connected,
    Idle,
    Warning,
}

impl Dot {
    pub fn color(self) -> Color32 {
        match self {
            Self::Connected => color::SUCCESS,
            Self::Idle => color::TEXT_FAINT,
            Self::Warning => color::WARNING,
        }
    }
}

pub fn status_dot(painter: &egui::Painter, center: egui::Pos2, dot: Dot) {
    painter.circle_filled(center, 3.5, dot.color());
}

pub fn icon_tile(painter: &egui::Painter, rect: Rect, glyph: Icon, tint: Color32) {
    painter.rect_filled(rect, CornerRadius::same(10), color::ICON_TILE);
    painter.rect_stroke(
        rect,
        CornerRadius::same(10),
        Stroke::new(1.0, color::DIVIDER),
        StrokeKind::Inside,
    );
    icon::paint(painter, glyph, rect.shrink(rect.width() * 0.22), tint);
}

pub fn small_pill(painter: &egui::Painter, left_center: egui::Pos2, text: &str) -> f32 {
    let font = type_scale::font(type_scale::MICRO);
    let galley = painter.layout_no_wrap(text.to_owned(), font, color::TEXT_MUTED);
    let rect = Rect::from_min_size(
        egui::pos2(left_center.x, left_center.y - 9.0),
        egui::vec2(galley.size().x + 12.0, 18.0),
    );
    painter.rect_filled(rect, CornerRadius::same(5), color::SURFACE_RAISED);
    painter.galley(
        egui::pos2(rect.left() + 6.0, rect.center().y - galley.size().y / 2.0),
        galley,
        color::TEXT_MUTED,
    );
    rect.width()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Trailing<'a> {
    None,
    Chevron,
    Check,
    Text(&'a str),
    Action(&'a str),
}

pub struct RowSpec<'a> {
    pub icon: Option<Icon>,
    pub title: &'a str,
    pub badge: Option<&'a str>,
    pub subtitle: Option<&'a str>,
    pub status: Option<(Dot, &'a str)>,
    pub trailing: Trailing<'a>,
    pub selected: bool,
    pub divider: bool,
}

impl<'a> RowSpec<'a> {
    pub fn new(title: &'a str) -> Self {
        Self {
            icon: None,
            title,
            badge: None,
            subtitle: None,
            status: None,
            trailing: Trailing::None,
            selected: false,
            divider: false,
        }
    }
}

/// Icon tile, two or three text lines and a trailing affordance.
pub fn list_row(ui: &mut Ui, spec: &RowSpec<'_>) -> Response {
    let lines = 1 + usize::from(spec.subtitle.is_some()) + usize::from(spec.status.is_some());
    let height = (22.0 + lines as f32 * 17.0).max(space::TOUCH + 12.0);
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), height), Sense::CLICK);
    let painter = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
    if spec.selected {
        painter.rect_filled(rect, 0.0, color::SELECTED);
    } else if response.is_pointer_button_down_on() {
        painter.rect_filled(rect, 0.0, color::SURFACE_RAISED);
    }
    if spec.divider {
        painter.hline(rect.x_range(), rect.top(), Stroke::new(1.0, color::DIVIDER));
    }
    let mut left = rect.left() + space::MD;
    if let Some(glyph) = spec.icon {
        let tile = Rect::from_center_size(
            egui::pos2(left + 20.0, rect.center().y),
            egui::vec2(40.0, 40.0),
        );
        icon_tile(&painter, tile, glyph, color::ACCENT_TEXT);
        left = tile.right() + space::MD;
    }
    let mut right = rect.right() - space::MD;
    match spec.trailing {
        Trailing::None => {}
        Trailing::Chevron => {
            paint_chevron(
                &painter,
                egui::pos2(right - 6.0, rect.center().y),
                5.0,
                color::TEXT_MUTED,
            );
            right -= 24.0;
        }
        Trailing::Check => {
            check_mark(
                &painter,
                egui::pos2(right - 8.0, rect.center().y),
                color::ACCENT_TEXT,
            );
            right -= 28.0;
        }
        Trailing::Text(text) => {
            let used = paint_fit(
                &painter,
                egui::pos2(right, rect.center().y),
                Align2::RIGHT_CENTER,
                text,
                type_scale::font(type_scale::CAPTION),
                color::TEXT_MUTED,
                120.0,
            );
            right = used.left() - space::SM;
        }
        Trailing::Action(text) => {
            let galley = painter.layout_no_wrap(
                text.to_owned(),
                type_scale::font(type_scale::CAPTION),
                color::TEXT,
            );
            let pill = Rect::from_center_size(
                egui::pos2(right - (galley.size().x + 24.0) / 2.0, rect.center().y),
                egui::vec2(galley.size().x + 24.0, 34.0),
            );
            painter.rect_filled(pill, CornerRadius::same(9), color::SURFACE_RAISED);
            painter.rect_stroke(
                pill,
                CornerRadius::same(9),
                Stroke::new(1.0, color::BORDER),
                StrokeKind::Inside,
            );
            painter.galley(pill.center() - galley.size() / 2.0, galley, color::TEXT);
            right = pill.left() - space::SM;
        }
    }
    let text_width = (right - left).max(20.0);
    let block = lines as f32 * 17.0;
    let mut y = rect.center().y - block / 2.0 + 8.5;
    let title = paint_fit(
        &painter,
        egui::pos2(left, y),
        Align2::LEFT_CENTER,
        spec.title,
        type_scale::font(type_scale::BODY),
        color::TEXT,
        text_width - spec.badge.map_or(0.0, |_| 64.0),
    );
    if let Some(badge) = spec.badge {
        small_pill(&painter, egui::pos2(title.right() + 8.0, y), badge);
    }
    y += 17.0;
    if let Some(subtitle) = spec.subtitle {
        paint_fit(
            &painter,
            egui::pos2(left, y),
            Align2::LEFT_CENTER,
            subtitle,
            type_scale::font(type_scale::CAPTION),
            color::TEXT_MUTED,
            text_width,
        );
        y += 17.0;
    }
    if let Some((dot, text)) = spec.status {
        status_dot(&painter, egui::pos2(left + 3.5, y), dot);
        paint_fit(
            &painter,
            egui::pos2(left + 12.0, y),
            Align2::LEFT_CENTER,
            text,
            type_scale::font(type_scale::CAPTION),
            color::TEXT_MUTED,
            text_width - 12.0,
        );
    }
    labelled(response, spec.title, true)
}

pub fn check_mark(painter: &egui::Painter, center: egui::Pos2, tint: Color32) {
    painter.line(
        vec![
            center + egui::vec2(-6.0, 0.0),
            center + egui::vec2(-2.0, 4.5),
            center + egui::vec2(6.0, -5.0),
        ],
        Stroke::new(2.0, tint),
    );
}

/// "SECTION TITLE ........ trailing" caption. Returns the trailing response.
pub fn section_header(
    ui: &mut Ui,
    title: &str,
    trailing: Option<&str>,
    action: bool,
) -> Option<Response> {
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), 30.0),
        if action { Sense::CLICK } else { Sense::hover() },
    );
    let painter = ui.painter();
    paint_fit(
        painter,
        egui::pos2(rect.left(), rect.center().y + 4.0),
        Align2::LEFT_CENTER,
        &title.to_uppercase(),
        type_scale::font(type_scale::MICRO),
        color::TEXT_FAINT,
        rect.width() * 0.6,
    );
    trailing.map(|text| {
        paint_fit(
            painter,
            egui::pos2(rect.right(), rect.center().y + 4.0),
            Align2::RIGHT_CENTER,
            text,
            type_scale::font(type_scale::CAPTION),
            if action {
                color::ACCENT_TEXT
            } else {
                color::TEXT_FAINT
            },
            rect.width() * 0.5,
        );
        labelled(response, text, action)
    })
}

/// Rounded search field with a leading icon. Returns the text-edit response.
pub fn search_field(ui: &mut Ui, text: &mut String, hint: &str) -> Response {
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), space::FIELD_HEIGHT),
        Sense::hover(),
    );
    ui.painter().rect_filled(
        rect,
        CornerRadius::same(radius::FIELD),
        color::SURFACE_RAISED,
    );
    ui.painter().rect_stroke(
        rect,
        CornerRadius::same(radius::FIELD),
        Stroke::new(1.0, color::BORDER),
        StrokeKind::Inside,
    );
    icon::paint(
        ui.painter(),
        Icon::Search,
        Rect::from_center_size(
            egui::pos2(rect.left() + 24.0, rect.center().y),
            egui::vec2(18.0, 18.0),
        ),
        color::TEXT_MUTED,
    );
    let inner = Rect::from_min_max(
        egui::pos2(rect.left() + 42.0, rect.top()),
        egui::pos2(rect.right() - 12.0, rect.bottom()),
    );
    ui.put(
        inner,
        egui::TextEdit::singleline(text)
            .hint_text(hint)
            .frame(egui::Frame::NONE)
            .vertical_align(egui::Align::Center)
            .desired_width(inner.width())
            .char_limit(120),
    )
}

/// Labelled single-line form field with the mock-up's caption above it.
pub fn text_field(
    ui: &mut Ui,
    caption: &str,
    text: &mut dyn egui::TextBuffer,
    hint: &str,
    width: f32,
) -> Response {
    let layout = egui::Layout::top_down(egui::Align::Min);
    ui.allocate_ui_with_layout(egui::vec2(width, 0.0), layout, |ui| {
        ui.spacing_mut().item_spacing.y = 6.0;
        ui.set_width(width);
        caption_label(ui, caption);
        let margin = Margin::symmetric(14, 14);
        ui.add_sized(
            [width, space::FIELD_HEIGHT],
            egui::TextEdit::singleline(text)
                .hint_text(hint)
                .margin(margin)
                .char_limit(255),
        )
    })
    .inner
}

pub fn caption_label(ui: &mut Ui, text: &str) {
    ui.label(
        egui::RichText::new(text.to_uppercase())
            .size(type_scale::MICRO)
            .color(color::TEXT_FAINT),
    );
}

pub fn footnote(ui: &mut Ui, text: &str) {
    ui.add(
        egui::Label::new(
            egui::RichText::new(text)
                .size(type_scale::CAPTION)
                .color(color::TEXT_FAINT),
        )
        .wrap(),
    );
}

/// Equal-width segmented control with optional icons stacked over labels.
pub fn segmented(
    ui: &mut Ui,
    items: &[(Option<Icon>, &str)],
    selected: &mut usize,
    stacked: bool,
) -> bool {
    let height = if stacked { 56.0 } else { 40.0 };
    let gap = if stacked { space::SM } else { 0.0 };
    let width = ui.available_width();
    let count = items.len().max(1) as f32;
    let item_width = (width - gap * (count - 1.0)) / count;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, height), Sense::hover());
    if !stacked {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(radius::CONTROL), color::FIELD);
        ui.painter().rect_stroke(
            rect,
            CornerRadius::same(radius::CONTROL),
            Stroke::new(1.0, color::BORDER),
            StrokeKind::Inside,
        );
    }
    let mut changed = false;
    for (index, (glyph, label)) in items.iter().enumerate() {
        let item = Rect::from_min_size(
            egui::pos2(rect.left() + index as f32 * (item_width + gap), rect.top()),
            egui::vec2(item_width, height),
        );
        let response = ui.interact(item, ui.id().with(("segment", index)), Sense::CLICK);
        let active = *selected == index;
        let painter = ui.painter();
        let (fill, stroke) = match (stacked, active) {
            (true, true) => (color::KEY_ACTIVE, Stroke::new(1.5, color::ACCENT_TEXT)),
            (true, false) => (color::SURFACE, Stroke::new(1.0, color::BORDER)),
            (false, true) => (color::KEY_ACTIVE, Stroke::NONE),
            (false, false) => (Color32::TRANSPARENT, Stroke::NONE),
        };
        let corner = CornerRadius::same(if stacked { 12 } else { 8 });
        let body = if stacked { item } else { item.shrink(3.0) };
        painter.rect_filled(body, corner, fill);
        if stroke != Stroke::NONE {
            painter.rect_stroke(body, corner, stroke, StrokeKind::Inside);
        }
        let text_color = if active {
            color::TEXT
        } else {
            color::TEXT_MUTED
        };
        if let (true, Some(glyph)) = (stacked, glyph) {
            icon::paint(
                painter,
                *glyph,
                Rect::from_center_size(
                    egui::pos2(item.center().x, item.top() + 18.0),
                    egui::vec2(20.0, 20.0),
                ),
                if active {
                    color::ACCENT_TEXT
                } else {
                    color::TEXT_MUTED
                },
            );
            paint_fit(
                painter,
                egui::pos2(item.center().x, item.bottom() - 12.0),
                Align2::CENTER_CENTER,
                label,
                type_scale::font(type_scale::CAPTION),
                text_color,
                item_width - 8.0,
            );
        } else {
            paint_fit(
                painter,
                item.center(),
                Align2::CENTER_CENTER,
                label,
                type_scale::font(type_scale::CAPTION + 0.5),
                text_color,
                item_width - 8.0,
            );
        }
        labelled(response.clone(), label, true);
        if response.clicked() && !active {
            *selected = index;
            changed = true;
        }
    }
    changed
}

/// Session chip: optional icon, label and an optional trailing count badge.
pub fn chip(ui: &mut Ui, glyph: Option<Icon>, label: &str, active: bool) -> Response {
    let font = type_scale::font(type_scale::CAPTION + 0.5);
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font, color::TEXT);
    let icon_width = if glyph.is_some() { 26.0 } else { 0.0 };
    let width = (galley.size().x + icon_width + 28.0).clamp(56.0, 120.0);
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(width, space::CHIP_HEIGHT), Sense::CLICK);
    let painter = ui.painter();
    let corner = CornerRadius::same(radius::CHIP);
    painter.rect_filled(
        rect,
        corner,
        if active {
            color::SURFACE_RAISED
        } else {
            color::SURFACE
        },
    );
    painter.rect_stroke(
        rect,
        corner,
        if active {
            Stroke::new(1.5, color::ACCENT_TEXT)
        } else {
            Stroke::new(1.0, color::BORDER)
        },
        StrokeKind::Inside,
    );
    let mut x = rect.left() + 14.0;
    if let Some(glyph) = glyph {
        icon::paint(
            painter,
            glyph,
            Rect::from_center_size(egui::pos2(x + 8.0, rect.center().y), egui::vec2(18.0, 18.0)),
            if active {
                color::ACCENT_TEXT
            } else {
                color::TEXT_MUTED
            },
        );
        x += 26.0;
    }
    paint_fit(
        painter,
        egui::pos2(x, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        type_scale::font(type_scale::CAPTION + 0.5),
        color::TEXT,
        rect.right() - x - 10.0,
    );
    labelled(response, label, true)
}

/// "All 4" counter chip and the "+" affordance share this compact shape.
pub fn count_chip(ui: &mut Ui, label: &str, count: usize) -> Response {
    let galley = ui.painter().layout_no_wrap(
        label.to_owned(),
        type_scale::font(type_scale::CAPTION),
        color::TEXT_MUTED,
    );
    let width = galley.size().x + 44.0;
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(width, space::CHIP_HEIGHT), Sense::CLICK);
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(radius::CHIP), color::SURFACE);
    painter.rect_stroke(
        rect,
        CornerRadius::same(radius::CHIP),
        Stroke::new(1.0, color::BORDER),
        StrokeKind::Inside,
    );
    painter.galley(
        egui::pos2(rect.left() + 11.0, rect.center().y - galley.size().y / 2.0),
        galley,
        color::TEXT_MUTED,
    );
    let badge = Rect::from_center_size(
        egui::pos2(rect.right() - 17.0, rect.center().y),
        egui::vec2(20.0, 20.0),
    );
    painter.rect_filled(badge, CornerRadius::same(6), color::KEY_ACTIVE);
    painter.text(
        badge.center(),
        Align2::CENTER_CENTER,
        count.to_string(),
        type_scale::font(type_scale::CAPTION),
        color::TEXT,
    );
    labelled(response, &format!("{label} {count}"), true)
}

pub fn plus_chip(ui: &mut Ui, label: &str) -> Response {
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(36.0, space::CHIP_HEIGHT), Sense::CLICK);
    paint_plus(ui.painter(), rect.center(), 9.0, color::TEXT_MUTED);
    labelled(response, label, true)
}

pub fn toggle_switch(ui: &mut Ui, on: &mut bool, label: &str) -> Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(48.0, 28.0), Sense::CLICK);
    if response.clicked() {
        *on = !*on;
    }
    let painter = ui.painter();
    painter.rect_filled(
        rect,
        CornerRadius::same(14),
        if *on {
            color::PRIMARY
        } else {
            color::SURFACE_RAISED
        },
    );
    let knob_x = if *on {
        rect.right() - 14.0
    } else {
        rect.left() + 14.0
    };
    painter.circle_filled(egui::pos2(knob_x, rect.center().y), 11.0, Color32::WHITE);
    response.widget_info(|| WidgetInfo::selected(WidgetType::Checkbox, true, *on, label));
    response
}

/// Card row with a label (and optional caption) and a trailing switch.
pub fn switch_row(ui: &mut Ui, label: &str, caption: Option<&str>, on: &mut bool, divider: bool) {
    let height = if caption.is_some() { 64.0 } else { 52.0 };
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), height), Sense::hover());
    if divider {
        ui.painter()
            .hline(rect.x_range(), rect.top(), Stroke::new(1.0, color::DIVIDER));
    }
    let painter = ui.painter();
    let y = if caption.is_some() {
        rect.center().y - 9.0
    } else {
        rect.center().y
    };
    paint_fit(
        painter,
        egui::pos2(rect.left() + space::MD, y),
        Align2::LEFT_CENTER,
        label,
        type_scale::font(type_scale::BODY),
        color::TEXT,
        rect.width() - 90.0,
    );
    if let Some(caption) = caption {
        paint_fit(
            painter,
            egui::pos2(rect.left() + space::MD, y + 20.0),
            Align2::LEFT_CENTER,
            caption,
            type_scale::font(type_scale::CAPTION),
            color::TEXT_MUTED,
            rect.width() - 90.0,
        );
    }
    let switch = Rect::from_center_size(
        egui::pos2(rect.right() - space::MD - 24.0, rect.center().y),
        egui::vec2(48.0, 28.0),
    );
    ui.scope_builder(egui::UiBuilder::new().max_rect(switch), |ui| {
        toggle_switch(ui, on, label)
    });
}

#[derive(Clone, Copy)]
pub struct TabItem<'a> {
    pub glyph: Icon,
    pub label: &'a str,
    pub badge: usize,
}

/// Bottom tab bar. Returns the tapped tab index.
pub fn tab_bar(ui: &mut Ui, tabs: &[TabItem<'_>], selected: usize) -> Option<usize> {
    let mut tapped = None;
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), space::TAB_BAR_HEIGHT),
        Sense::hover(),
    );
    ui.painter().rect_filled(rect, 0.0, color::BAR);
    ui.painter()
        .hline(rect.x_range(), rect.top(), Stroke::new(1.0, color::DIVIDER));
    let width = rect.width() / tabs.len().max(1) as f32;
    for (index, tab) in tabs.iter().enumerate() {
        let item = Rect::from_min_size(
            egui::pos2(rect.left() + index as f32 * width, rect.top()),
            egui::vec2(width, rect.height()),
        );
        let response = ui.interact(item, ui.id().with(("tab", index)), Sense::CLICK);
        let tint = if index == selected {
            color::ACCENT_TEXT
        } else {
            color::TEXT_MUTED
        };
        let painter = ui.painter();
        icon::paint(
            painter,
            tab.glyph,
            Rect::from_center_size(
                egui::pos2(item.center().x, item.top() + 20.0),
                egui::vec2(22.0, 22.0),
            ),
            tint,
        );
        painter.text(
            egui::pos2(item.center().x, item.bottom() - 12.0),
            Align2::CENTER_CENTER,
            tab.label,
            type_scale::font(type_scale::MICRO),
            tint,
        );
        if tab.badge > 0 {
            let badge = Rect::from_center_size(
                egui::pos2(item.center().x + 16.0, item.top() + 11.0),
                egui::vec2(18.0, 15.0),
            );
            painter.rect_filled(badge, CornerRadius::same(5), color::KEY_ACTIVE);
            painter.text(
                badge.center(),
                Align2::CENTER_CENTER,
                tab.badge.to_string(),
                type_scale::font(10.0),
                color::TEXT,
            );
        }
        labelled(response.clone(), tab.label, true);
        if response.clicked() {
            tapped = Some(index);
        }
    }
    tapped
}

#[derive(Default)]
pub struct SheetOutcome {
    pub dismissed: bool,
}

/// Bottom sheet over `bounds` with scrim, grabber and bounded height.
/// `content` receives the padded sheet `Ui`; tapping the scrim dismisses.
pub fn bottom_sheet(
    ctx: &egui::Context,
    id: &str,
    bounds: Rect,
    content: impl FnOnce(&mut Ui),
) -> SheetOutcome {
    let mut outcome = SheetOutcome::default();
    let scrim_id = Id::new(("sheet-scrim", id));
    egui::Area::new(scrim_id)
        .order(egui::Order::Foreground)
        .fixed_pos(bounds.min)
        .show(ctx, |ui| {
            let (rect, response) = ui.allocate_exact_size(bounds.size(), Sense::CLICK);
            ui.painter().rect_filled(rect, 0.0, color::SCRIM);
            outcome.dismissed |= response.clicked();
        });
    let corner = CornerRadius {
        nw: radius::SHEET,
        ne: radius::SHEET,
        sw: 0,
        se: 0,
    };
    let height_id = Id::new(("sheet-height", id));
    let measured = ctx
        .data_mut(|data| data.get_temp::<f32>(height_id))
        .unwrap_or(0.0)
        .min(bounds.height());
    let sheet = egui::Area::new(Id::new(("sheet", id)))
        .order(egui::Order::Foreground)
        .pivot(if measured > 0.0 {
            Align2::LEFT_TOP
        } else {
            Align2::LEFT_BOTTOM
        })
        .fixed_pos(if measured > 0.0 {
            egui::pos2(bounds.left(), bounds.bottom() - measured)
        } else {
            bounds.left_bottom()
        })
        .show(ctx, |ui| {
            egui::Frame::new()
                .fill(color::SHEET)
                .stroke(Stroke::new(1.0, color::BORDER))
                .corner_radius(corner)
                .shadow(super::theme::sheet_shadow())
                .inner_margin(Margin {
                    left: 16,
                    right: 16,
                    top: 8,
                    bottom: 16,
                })
                .show(ui, |ui| {
                    ui.set_width(bounds.width() - 32.0);
                    let (grabber, _) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width(), 10.0),
                        Sense::hover(),
                    );
                    ui.painter().rect_filled(
                        Rect::from_center_size(
                            egui::pos2(grabber.center().x, grabber.top() + 3.0),
                            egui::vec2(40.0, 5.0),
                        ),
                        CornerRadius::same(3),
                        color::TEXT_FAINT,
                    );
                    egui::ScrollArea::vertical()
                        .max_height(bounds.height() * 0.88 - 40.0)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            ui.spacing_mut().item_spacing.y = space::SM;
                            content(ui)
                        });
                });
        });
    // A bottom-pivoted area learns its height one frame late; settle it.
    let height = sheet.response.rect.height();
    let previous = ctx.data_mut(|data| data.get_temp::<f32>(height_id));
    if previous.is_none_or(|previous| (previous - height).abs() > 0.5) {
        ctx.data_mut(|data| data.insert_temp(height_id, height));
        ctx.request_repaint();
    }
    outcome
}

/// Sheet heading with a trailing text action ("Done") or close glyph.
pub fn sheet_title(ui: &mut Ui, title: &str, action: Option<&str>) -> bool {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 40.0), Sense::hover());
    paint_fit(
        ui.painter(),
        egui::pos2(rect.left(), rect.center().y),
        Align2::LEFT_CENTER,
        title,
        type_scale::font(type_scale::TITLE),
        color::TEXT,
        rect.width() - 80.0,
    );
    let tap = Rect::from_center_size(
        egui::pos2(rect.right() - 22.0, rect.center().y),
        egui::vec2(60.0, 40.0),
    );
    let response = ui.interact(tap, ui.id().with(("sheet-action", title)), Sense::CLICK);
    match action {
        Some(text) => {
            paint_fit(
                ui.painter(),
                egui::pos2(rect.right(), rect.center().y),
                Align2::RIGHT_CENTER,
                text,
                type_scale::font(type_scale::BODY),
                color::ACCENT_TEXT,
                60.0,
            );
            labelled(response, text, true).clicked()
        }
        None => {
            icon::paint(
                ui.painter(),
                Icon::Close,
                Rect::from_center_size(
                    egui::pos2(rect.right() - 12.0, rect.center().y),
                    egui::vec2(18.0, 18.0),
                ),
                color::ACCENT_TEXT,
            );
            labelled(response, "Close", true).clicked()
        }
    }
}

/// Large screen title with a muted subtitle, used by tab screens.
pub fn screen_title(ui: &mut Ui, title: &str, subtitle: &str) {
    ui.add_space(space::SM);
    ui.label(
        egui::RichText::new(title)
            .size(type_scale::LARGE_TITLE)
            .strong()
            .color(color::TEXT),
    );
    ui.label(
        egui::RichText::new(subtitle)
            .size(type_scale::CAPTION)
            .color(color::TEXT_MUTED),
    );
    ui.add_space(space::SM);
}

/// Right-pointing chevron; the shipped icon set only has an arrow.
pub fn paint_chevron(painter: &egui::Painter, center: egui::Pos2, half: f32, tint: Color32) {
    let stroke = Stroke::new(1.8, tint);
    let tip = center + egui::vec2(half * 0.5, 0.0);
    painter.line_segment([tip + egui::vec2(-half, -half), tip], stroke);
    painter.line_segment([tip, tip + egui::vec2(-half, half)], stroke);
}

pub fn paint_plus(painter: &egui::Painter, center: egui::Pos2, half: f32, tint: Color32) {
    let stroke = Stroke::new(1.8, tint);
    painter.line_segment(
        [
            center - egui::vec2(half, 0.0),
            center + egui::vec2(half, 0.0),
        ],
        stroke,
    );
    painter.line_segment(
        [
            center - egui::vec2(0.0, half),
            center + egui::vec2(0.0, half),
        ],
        stroke,
    );
}
