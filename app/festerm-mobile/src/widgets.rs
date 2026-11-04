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
    Danger,
}

impl Dot {
    pub fn color(self) -> Color32 {
        match self {
            Self::Connected => color::SUCCESS,
            Self::Idle => color::TEXT_FAINT,
            Self::Warning => color::WARNING,
            Self::Danger => color::DANGER,
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
    /// Chevron plus an `Edit` zone; see [`edit_zone_clicked`].
    Edit,
}

const EDIT_ZONE: f32 = 72.0;

/// Whether a click on an `Trailing::Edit` row landed on its Edit zone.
pub fn edit_zone_clicked(response: &Response) -> bool {
    response.clicked()
        && response
            .interact_pointer_pos()
            .is_some_and(|pos| pos.x > response.rect.right() - EDIT_ZONE)
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
    let height = list_row_height(spec);
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
        Trailing::Edit => {
            paint_chevron(
                &painter,
                egui::pos2(right - 6.0, rect.center().y),
                5.0,
                color::TEXT_MUTED,
            );
            let used = paint_fit(
                &painter,
                egui::pos2(right - 22.0, rect.center().y),
                Align2::RIGHT_CENTER,
                "Edit",
                type_scale::font(type_scale::CAPTION),
                color::ACCENT_TEXT,
                48.0,
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

/// How long a still press must last to count as a long press.
pub const LONG_PRESS_SECS: f64 = 0.5;

/// Classifies a press on `response` as a tap or a long press. A long press
/// fires once while the finger is still down (so the reaction is immediate),
/// and the release that ends it is not reported as a tap. Movement beyond a
/// click's slop (e.g. scrolling the chip strip) cancels both.
pub fn long_press_or_click(ui: &Ui, response: &Response) -> (bool, bool) {
    let fired_id = response.id.with("long-press");
    let fired = ui
        .data(|data| data.get_temp::<bool>(fired_id))
        .unwrap_or(false);
    if !response.is_pointer_button_down_on() {
        if fired {
            ui.data_mut(|data| data.remove::<bool>(fired_id));
            return (false, false);
        }
        return (false, response.clicked());
    }
    if fired {
        return (false, false);
    }
    let (elapsed, moved) = ui.input(|input| {
        let pointer = &input.pointer;
        let moved = match (pointer.press_origin(), pointer.latest_pos()) {
            (Some(origin), Some(latest)) => origin.distance(latest),
            _ => 0.0,
        };
        (
            pointer.press_start_time().map(|start| input.time - start),
            moved,
        )
    });
    if moved > 8.0 {
        return (false, false);
    }
    match elapsed {
        Some(elapsed) if elapsed >= LONG_PRESS_SECS => {
            ui.data_mut(|data| data.insert_temp(fired_id, true));
            (true, false)
        }
        Some(elapsed) => {
            ui.ctx()
                .request_repaint_after_secs((LONG_PRESS_SECS - elapsed) as f32);
            (false, false)
        }
        None => (false, false),
    }
}

/// Width of the action revealed behind a swiped row.
pub const SWIPE_ACTION_WIDTH: f32 = 88.0;

#[derive(Clone, Copy, Debug, Default)]
struct SwipeState {
    open: bool,
    /// Offset when the current press began; `None` while not pressed.
    base: Option<f32>,
    horizontal: bool,
    live: f32,
}

pub struct SwipeRow {
    pub row: Response,
    /// The row was tapped while its action was hidden.
    pub opened: bool,
    /// The revealed action button was tapped.
    pub action: bool,
}

/// Mail-style swipe: dragging a row left reveals a destructive `action`
/// button at its trailing edge; tapping the button confirms it, while tapping
/// the row or swiping back hides it. Vertical drags still scroll the list.
pub fn swipe_row(
    ui: &mut Ui,
    id: Id,
    action: &str,
    height: f32,
    add_row: impl FnOnce(&mut Ui) -> Response,
) -> SwipeRow {
    let state_id = id.with("swipe");
    let mut state: SwipeState = ui.data(|data| data.get_temp(state_id)).unwrap_or_default();
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), height), Sense::hover());
    let target = if state.base.is_some() && state.horizontal {
        state.live
    } else if state.open {
        SWIPE_ACTION_WIDTH
    } else {
        0.0
    };
    let instant = state.base.is_some() && state.horizontal;
    let offset = ui.ctx().animate_value_with_time(
        state_id.with("offset"),
        target,
        if instant { 0.0 } else { 0.15 },
    );
    let clip = rect.intersect(ui.clip_rect());
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .id_salt(id)
            .max_rect(rect.translate(egui::vec2(-offset, 0.0))),
    );
    child.set_clip_rect(clip);
    let row = add_row(&mut child);
    let mut action_clicked = false;
    if offset > 0.5 {
        let revealed =
            Rect::from_x_y_ranges((rect.right() - offset)..=rect.right(), rect.y_range());
        let painter = ui.painter().with_clip_rect(clip);
        painter.rect_filled(revealed, 0.0, color::DANGER);
        paint_fit(
            &painter,
            revealed.center(),
            Align2::CENTER_CENTER,
            action,
            type_scale::font(type_scale::BODY),
            color::ON_PRIMARY,
            revealed.width() - 8.0,
        );
        if offset >= SWIPE_ACTION_WIDTH - 1.0 {
            let response = ui.interact(revealed, id.with("swipe-action"), Sense::CLICK);
            action_clicked = labelled(response, action, true).clicked();
        }
    }
    let mut opened = false;
    // Once the finger moves, egui hands the drag to the enclosing ScrollArea
    // and the row stops reporting the press, so follow the pointer until the
    // press that started on the row ends.
    let pressing = state.base.is_some() && ui.input(|input| input.pointer.primary_down());
    if row.is_pointer_button_down_on() || pressing {
        let base = *state.base.get_or_insert(target);
        let delta =
            ui.input(
                |input| match (input.pointer.press_origin(), input.pointer.latest_pos()) {
                    (Some(origin), Some(latest)) => latest - origin,
                    _ => egui::Vec2::ZERO,
                },
            );
        if !state.horizontal && delta.x.abs() > 8.0 && delta.x.abs() > delta.y.abs() {
            state.horizontal = true;
        }
        if state.horizontal {
            state.live = (base - delta.x).clamp(0.0, SWIPE_ACTION_WIDTH * 1.25);
        }
    } else if state.base.take().is_some() {
        if state.horizontal {
            state.open = state.live > SWIPE_ACTION_WIDTH / 2.0;
        } else if row.clicked() {
            opened = !state.open;
            state.open = false;
        }
        state.horizontal = false;
    } else if row.clicked() {
        opened = !state.open;
        state.open = false;
    }
    if action_clicked {
        state = SwipeState::default();
    }
    ui.data_mut(|data| data.insert_temp(state_id, state));
    SwipeRow {
        row,
        opened,
        action: action_clicked,
    }
}

/// Height `list_row` allocates for `spec`.
pub fn list_row_height(spec: &RowSpec<'_>) -> f32 {
    let lines = 1 + usize::from(spec.subtitle.is_some()) + usize::from(spec.status.is_some());
    (22.0 + lines as f32 * 17.0).max(space::TOUCH + 12.0)
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
    text_field_with_limit(ui, caption, text, hint, width, 255)
}

pub fn text_field_with_limit(
    ui: &mut Ui,
    caption: &str,
    text: &mut dyn egui::TextBuffer,
    hint: &str,
    width: f32,
    char_limit: usize,
) -> Response {
    let layout = egui::Layout::top_down(egui::Align::Min);
    ui.allocate_ui_with_layout(egui::vec2(width, 0.0), layout, |ui| {
        ui.spacing_mut().item_spacing.y = 6.0;
        ui.set_width(width);
        caption_label(ui, caption);
        let margin = Margin::symmetric(14, 14);
        // A stable id keeps focus (and the keyboard) when content above the
        // field appears or disappears between frames.
        let response = form_field(
            ui,
            [width, space::FIELD_HEIGHT],
            ("text_field", caption),
            egui::TextEdit::singleline(text)
                .hint_text(hint)
                .margin(margin)
                .char_limit(char_limit),
            false,
        );
        if response.has_focus() && !ui.clip_rect().contains_rect(response.rect) {
            response.scroll_to_me(Some(egui::Align::Center));
        }
        response
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

/// Header session chip: connection dot (a ring around it while a background
/// tab has unseen output, as on desktop) and the tab's name.
pub fn session_chip(ui: &mut Ui, label: &str, dot: Dot, unread: bool, active: bool) -> Response {
    let font = type_scale::font(type_scale::CAPTION + 0.5);
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font.clone(), color::TEXT);
    let width = (galley.size().x + 40.0).clamp(56.0, 150.0);
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(width, space::CHIP_HEIGHT), Sense::CLICK);
    let painter = ui.painter();
    let corner = CornerRadius::same(radius::CHIP);
    painter.rect_filled(
        rect,
        corner,
        if active {
            color::TERMINAL_KEY
        } else {
            color::TERMINAL_KEY_MUTED
        },
    );
    painter.rect_stroke(
        rect,
        corner,
        if active {
            Stroke::new(1.5, color::ACCENT_TEXT)
        } else {
            Stroke::new(1.0, color::TERMINAL_BORDER)
        },
        StrokeKind::Inside,
    );
    let center = egui::pos2(rect.left() + 16.0, rect.center().y);
    if unread {
        painter.circle_stroke(center, 4.5, Stroke::new(1.0, dot.color()));
        painter.circle_filled(center, 2.5, dot.color());
    } else {
        painter.circle_filled(center, 4.0, dot.color());
    }
    paint_fit(
        painter,
        egui::pos2(rect.left() + 28.0, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        font,
        if active {
            color::TEXT
        } else {
            color::TEXT_MUTED
        },
        rect.right() - rect.left() - 38.0,
    );
    let name = if unread {
        format!("{label}, new output")
    } else {
        label.to_owned()
    };
    labelled(response, &name, true)
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
    painter.rect_filled(
        rect,
        CornerRadius::same(radius::CHIP),
        color::TERMINAL_KEY_MUTED,
    );
    painter.rect_stroke(
        rect,
        CornerRadius::same(radius::CHIP),
        Stroke::new(1.0, color::TERMINAL_BORDER),
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
    // A child Ui does not advance the row layout, which would pull the next row up.
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(switch));
    toggle_switch(&mut child, on, label);
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
    // The area may extend below the bounds so a growing sheet is not squeezed by
    // the room left under its previous top edge.
    let max_height = bounds.height() * 0.88;
    let measured = ctx
        .data_mut(|data| data.get_temp::<f32>(height_id))
        .unwrap_or(0.0)
        .min(max_height);
    let sheet = egui::Area::new(Id::new(("sheet", id)))
        .order(egui::Order::Foreground)
        .constrain_to(Rect::from_min_max(
            bounds.left_top(),
            egui::pos2(bounds.right(), bounds.bottom() + max_height),
        ))
        .fixed_pos(egui::pos2(bounds.left(), bounds.bottom() - measured))
        .show(ctx, |ui| {
            let frame = egui::Frame::new()
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
                    // A ScrollArea sizes itself from last frame's content, so the
                    // content height is tracked here to follow in-place changes.
                    let cap = max_height - 40.0;
                    ui.set_max_height(cap);
                    let content_id = Id::new(("sheet-content", id));
                    let known = ctx.data_mut(|data| data.get_temp::<f32>(content_id));
                    let scroll = egui::ScrollArea::vertical()
                        .max_height(known.map_or(cap, |known| known.min(cap)))
                        .auto_shrink([false, known.is_none()])
                        .show(ui, |ui| {
                            ui.spacing_mut().item_spacing.y = space::SM;
                            content(ui);
                            ui.min_rect().height()
                        });
                    let content_height = scroll.inner;
                    if known.is_none_or(|known| (known - content_height).abs() > 0.5) {
                        ctx.data_mut(|data| data.insert_temp(content_id, content_height));
                        ctx.request_repaint();
                    }
                });
            frame.response.rect.height()
        });
    // The frame height is only known after layout; settle it next frame.
    let height = sheet.inner;
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

/// Hold time before a form field offers its edit menu (UIKit uses ~0.5 s).
pub const FIELD_LONG_PRESS_SECS: f64 = 0.5;
/// Finger travel that turns a hold into a scroll or selection drag instead.
const FIELD_LONG_PRESS_SLOP: f32 = 10.0;

/// An edit the field menu asked for, applied to the focused field next pass.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FieldEdit {
    Paste,
    SelectAll,
    Copy,
    Cut,
}

#[derive(Clone, Copy, Debug)]
struct FieldMenu {
    field: Id,
    opened_at: f64,
}

#[derive(Clone, Copy, Debug)]
struct PendingFieldEdit {
    field: Id,
    edit: FieldEdit,
}

fn field_menu_id() -> Id {
    Id::new("festerm-field-edit-menu")
}

fn pending_field_edit_id() -> Id {
    Id::new("festerm-field-pending-edit")
}

/// The field whose edit menu is showing, if any.
pub fn field_menu_owner(ctx: &egui::Context) -> Option<Id> {
    ctx.data(|data| data.get_temp::<FieldMenu>(field_menu_id()))
        .map(|menu| menu.field)
}

/// Removes the edit queued by the field menu if its field still has focus.
pub fn take_field_edit(ctx: &egui::Context) -> Option<FieldEdit> {
    let pending = ctx.data_mut(|data| {
        let pending = data.get_temp::<PendingFieldEdit>(pending_field_edit_id());
        data.remove::<PendingFieldEdit>(pending_field_edit_id());
        pending
    })?;
    (ctx.memory(|memory| memory.focused()) == Some(pending.field)).then_some(pending.edit)
}

/// egui draws form fields itself, so UIKit has no text view to attach its
/// Paste menu to. A press-and-hold on a field opens an equivalent menu; its
/// edits are replayed as ordinary egui clipboard events into that field.
pub fn field_edit_menu(ui: &mut Ui, response: &Response, has_text: bool, secret: bool) {
    let ctx = ui.ctx().clone();
    let field = response.id;
    let now = ui.input(|input| input.time);
    let drag = caret_drag(&ctx).filter(|drag| drag.field == field);
    if let Some(mut drag) = drag {
        if ui.input(|input| input.pointer.any_down()) {
            let latest = ui.input(|input| input.pointer.latest_pos());
            if !drag.moved
                && latest.is_some_and(|at| at.distance(drag.origin) > FIELD_LONG_PRESS_SLOP)
            {
                drag.moved = true;
                ctx.data_mut(|data| data.insert_temp(caret_drag_id(), drag));
            }
        } else {
            ctx.data_mut(|data| data.remove::<CaretDrag>(caret_drag_id()));
            // Like UIKit: a hold that never moved offers the edit menu.
            if !drag.moved {
                ctx.data_mut(|data| {
                    data.insert_temp(
                        field_menu_id(),
                        FieldMenu {
                            field,
                            opened_at: now,
                        },
                    );
                });
                ctx.memory_mut(|memory| memory.request_focus(field));
            }
        }
    } else if response.is_pointer_button_down_on() && ui.input(|input| input.any_touches()) {
        let (start, origin, latest) = ui.input(|input| {
            (
                input.pointer.press_start_time(),
                input.pointer.press_origin(),
                input.pointer.latest_pos(),
            )
        });
        let still = matches!((origin, latest), (Some(a), Some(b)) if a.distance(b) <= FIELD_LONG_PRESS_SLOP);
        if let (Some(start), true, Some(latest)) = (start, still, latest) {
            let held = now - start;
            if held >= FIELD_LONG_PRESS_SECS {
                // The held finger now carries the caret until it lifts.
                close_field_menu(&ctx);
                ctx.data_mut(|data| {
                    data.insert_temp(
                        caret_drag_id(),
                        CaretDrag {
                            field,
                            origin: latest,
                            moved: false,
                        },
                    );
                });
                ctx.memory_mut(|memory| memory.request_focus(field));
                ctx.request_repaint();
            } else {
                // A stationary finger delivers no events; wake to notice the hold.
                ctx.request_repaint_after(std::time::Duration::from_secs_f64(
                    FIELD_LONG_PRESS_SECS - held + 0.01,
                ));
            }
        }
    }
    let Some(menu) = ctx
        .data(|data| data.get_temp::<FieldMenu>(field_menu_id()))
        .filter(|menu| menu.field == field)
    else {
        return;
    };
    let area_id = field.with("edit-menu");
    let pressing_menu = ui.input(|input| input.pointer.any_down() || input.pointer.any_released())
        && ui
            .input(|input| input.pointer.interact_pos())
            .zip(ctx.memory(|memory| memory.area_rect(area_id)))
            .is_some_and(|(origin, rect)| rect.contains(origin));
    if pressing_menu {
        // The field surrenders focus to a press outside it; hold on to it so
        // UIKit's keyboard stays up while the finger rests on the menu.
        ctx.memory_mut(|memory| memory.request_focus(field));
    }
    if ctx.memory(|memory| memory.focused()) != Some(field) && now > menu.opened_at {
        close_field_menu(&ctx);
        return;
    }
    let selected = egui::TextEdit::load_state(&ctx, field)
        .and_then(|state| state.cursor.char_range())
        .is_some_and(|range| !range.is_empty());
    let mut items = Vec::with_capacity(4);
    if selected && !secret {
        items.push(("Cut", FieldEdit::Cut));
        items.push(("Copy", FieldEdit::Copy));
    }
    items.push(("Paste", FieldEdit::Paste));
    if has_text {
        items.push(("Select All", FieldEdit::SelectAll));
    }
    let bounds = ctx.content_rect();
    let item_width = 92.0;
    let size = egui::vec2(item_width * items.len() as f32, space::TOUCH);
    let mut pos = egui::pos2(
        response.rect.left() + space::SM,
        response.rect.top() - size.y - space::SM,
    );
    if pos.y < bounds.top() {
        pos.y = response.rect.bottom() + space::SM;
    }
    pos.x = pos
        .x
        .min(bounds.right() - size.x - space::SM)
        .max(bounds.left() + space::SM);
    let mut chosen = None;
    let area = egui::Area::new(area_id)
        .order(egui::Order::Foreground)
        .fixed_pos(pos)
        .show(&ctx, |ui| {
            egui::Frame::new()
                .fill(color::SURFACE_RAISED)
                .stroke(Stroke::new(1.0, color::BORDER))
                .corner_radius(CornerRadius::same(radius::CONTROL))
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    ui.horizontal(|ui| {
                        for (index, (label, edit)) in items.iter().enumerate() {
                            if index > 0 {
                                let x = ui.cursor().left();
                                ui.painter().vline(
                                    x,
                                    ui.max_rect().y_range().shrink(space::SM),
                                    Stroke::new(1.0, color::DIVIDER),
                                );
                            }
                            let (rect, item) = ui.allocate_exact_size(
                                egui::vec2(item_width, space::TOUCH),
                                Sense::CLICK,
                            );
                            if item.is_pointer_button_down_on() {
                                ui.painter().rect_filled(
                                    rect,
                                    CornerRadius::same(radius::CONTROL),
                                    color::SELECTED,
                                );
                            }
                            ui.painter().text(
                                rect.center(),
                                Align2::CENTER_CENTER,
                                *label,
                                FontId::proportional(type_scale::BODY),
                                color::TEXT,
                            );
                            let item = labelled(item, label, true);
                            if item.clicked() {
                                chosen = Some(*edit);
                            }
                        }
                    });
                });
        });
    if let Some(edit) = chosen {
        if edit == FieldEdit::SelectAll {
            // Like UIKit, keep the menu up so the selection can be copied.
            ctx.data_mut(|data| {
                data.insert_temp(
                    field_menu_id(),
                    FieldMenu {
                        field,
                        opened_at: now,
                    },
                );
            });
        } else {
            close_field_menu(&ctx);
        }
        ctx.data_mut(|data| {
            data.insert_temp(pending_field_edit_id(), PendingFieldEdit { field, edit });
        });
        // Tapping the menu is a click outside the field; keep its keyboard.
        ctx.memory_mut(|memory| memory.request_focus(field));
        ctx.request_repaint();
    } else if now > menu.opened_at
        && ui.input(|input| input.pointer.any_pressed())
        && !ui
            .input(|input| input.pointer.interact_pos())
            .is_some_and(|pos| area.response.rect.contains(pos))
    {
        close_field_menu(&ctx);
    }
}

fn close_field_menu(ctx: &egui::Context) {
    ctx.data_mut(|data| data.remove::<FieldMenu>(field_menu_id()));
}

/// A long-press that turned the finger into the field's caret.
#[derive(Clone, Copy, Debug)]
struct CaretDrag {
    field: Id,
    origin: egui::Pos2,
    moved: bool,
}

fn caret_drag_id() -> Id {
    Id::new("festerm-field-caret-drag")
}

fn caret_drag(ctx: &egui::Context) -> Option<CaretDrag> {
    ctx.data(|data| data.get_temp::<CaretDrag>(caret_drag_id()))
}

/// Where a field's text was drawn last frame.
#[derive(Clone)]
struct FieldLayout {
    galley: std::sync::Arc<egui::Galley>,
    galley_pos: egui::Pos2,
    multiline: bool,
    geometry: Option<std::sync::Arc<FieldGeometry>>,
}

#[derive(Clone)]
struct FieldGeometry {
    carets: Vec<f32>,
    rows: Vec<(f32, f32)>,
}

fn field_geometry(galley: &egui::Galley, pos: egui::Pos2, multiline: bool) -> FieldGeometry {
    use egui::epaint::text::cursor::LayoutCursor;
    let chars = galley.text().chars().count();
    let mut geometry = FieldGeometry {
        carets: Vec::with_capacity(chars + 1),
        rows: if multiline {
            Vec::with_capacity(chars + 1)
        } else {
            Vec::new()
        },
    };
    let mut base = 0;
    for (row_index, row) in galley.rows.iter().enumerate() {
        for column in 0..=row.char_count_excluding_newline().0 {
            // A soft-wrap offset belongs to the preceding row's endpoint.
            if base + column < geometry.carets.len() {
                continue;
            }
            let rect = galley
                .pos_from_layout_cursor(&LayoutCursor {
                    row: row_index,
                    column: egui::text::CharIndex(column),
                })
                .translate(pos.to_vec2());
            geometry.carets.push(rect.left());
            if multiline {
                geometry
                    .rows
                    .push((rect.top(), rect.bottom().max(rect.top() + 1.0)));
            }
        }
        base += row.char_count_including_newline().0;
    }
    let end = galley
        .pos_from_layout_cursor(&LayoutCursor {
            row: galley.rows.len(),
            column: egui::text::CharIndex(0),
        })
        .translate(pos.to_vec2());
    geometry.carets.resize(chars + 1, end.left());
    if multiline {
        geometry
            .rows
            .resize(chars + 1, (end.top(), end.bottom().max(end.top() + 1.0)));
    }
    geometry
}

fn published_field_id() -> Id {
    Id::new("festerm-field-document")
}

/// The focused form field this frame, for the native keyboard to edit.
pub fn take_field_document(ctx: &egui::Context) -> Option<festerm_ios_window::FieldDocument> {
    ctx.data_mut(|data| {
        let document = data.get_temp::<festerm_ios_window::FieldDocument>(published_field_id());
        data.remove::<festerm_ios_window::FieldDocument>(published_field_id());
        let owner = published_field_id().with("owner");
        let current = data.get_temp::<Id>(published_field_id().with("current"));
        data.remove::<Id>(published_field_id().with("current"));
        if let Some(previous) = data.get_temp::<Id>(owner) {
            if current != Some(previous) {
                data.remove::<FieldLayout>(previous.with("layout"));
            }
        }
        match current {
            Some(current) => {
                data.insert_temp(owner, current);
            }
            None => data.remove::<Id>(owner),
        }
        document
    })
}

pub fn forget_local_text_edit(ctx: &egui::Context, field: Id) {
    let focused = ctx.memory(|memory| memory.has_focus(field));
    ctx.memory_mut(|memory| memory.surrender_focus(field));
    if focused {
        ctx.output_mut(|output| output.ime = None);
    }
    ctx.data_mut(|data| {
        data.remove::<egui::text_edit::TextEditState>(field);
        data.remove::<FieldLayout>(field.with("layout"));
    });
    if field_menu_owner(ctx) == Some(field) {
        close_field_menu(ctx);
    }
}

/// A single-line form field: a sized `TextEdit` with the long-press caret
/// drag, the edit menu, and the native keyboard's view of its text.
pub fn form_field(
    ui: &mut Ui,
    size: impl Into<egui::Vec2>,
    id_salt: impl egui::AsIdSalt,
    edit: egui::TextEdit<'_>,
    secret: bool,
) -> Response {
    let layout = egui::Layout::centered_and_justified(ui.layout().main_dir());
    ui.allocate_ui_with_layout(size.into(), layout, |ui| {
        let id = ui.make_persistent_id(id_salt);
        local_text_edit(ui, id, edit, secret, false)
            .response
            .response
    })
    .inner
}

/// Shared local field presentation for forms, source editors and drafts.
pub fn local_text_edit(
    ui: &mut Ui,
    id: Id,
    edit: egui::TextEdit<'_>,
    secret: bool,
    multiline: bool,
) -> egui::text_edit::TextEditOutput {
    follow_caret_drag(ui, id);
    let output = edit.id(id).show(ui);
    let response = output.response.response.clone();
    let ctx = ui.ctx().clone();
    let geometry = response.has_focus().then(|| {
        ctx.data(|data| data.get_temp::<FieldLayout>(id.with("layout")))
            .filter(|previous| {
                std::sync::Arc::ptr_eq(&previous.galley, &output.galley)
                    && previous.galley_pos == output.galley_pos
                    && previous.multiline == multiline
            })
            .and_then(|previous| previous.geometry)
            .unwrap_or_else(|| {
                std::sync::Arc::new(field_geometry(&output.galley, output.galley_pos, multiline))
            })
    });
    let layout = FieldLayout {
        galley: output.galley.clone(),
        galley_pos: output.galley_pos,
        multiline,
        geometry,
    };
    if response.has_focus() {
        let galley = &layout.galley;
        let chars = galley.text().chars().count();
        let geometry = layout.geometry.as_ref().expect("focused field geometry");
        let selection = output
            .cursor_range
            .map(|range| range.as_sorted_char_range())
            .map_or(chars..chars, |range| range.start.0..range.end.0);
        let row = galley.rect.translate(layout.galley_pos.to_vec2());
        ctx.data_mut(|data| {
            data.insert_temp(published_field_id().with("current"), response.id);
            data.insert_temp(
                published_field_id(),
                festerm_ios_window::FieldDocument {
                    field: response.id.value(),
                    text: galley.text().to_owned(),
                    selection,
                    carets: geometry.carets.clone(),
                    top: row.top(),
                    bottom: row.bottom().max(row.top() + 1.0),
                    rows: geometry.rows.clone(),
                    multiline,
                    secure: secret,
                },
            );
        });
    }
    ctx.data_mut(|data| data.insert_temp(response.id.with("layout"), layout));
    field_edit_menu(ui, &response, !output.galley.text().is_empty(), secret);
    output
}

/// Before the field runs, pins its caret under a long-pressed finger. egui's
/// own drag would select from the press point instead, so it is released;
/// egui hands out a new drag only on the next press.
fn follow_caret_drag(ui: &Ui, field: Id) {
    let ctx = ui.ctx();
    if caret_drag(ctx).is_none_or(|drag| drag.field != field)
        || !ui.input(|input| input.pointer.any_down())
    {
        return;
    }
    let (Some(pointer), Some(layout)) = (
        ui.input(|input| input.pointer.latest_pos()),
        ctx.data(|data| data.get_temp::<FieldLayout>(field.with("layout"))),
    ) else {
        return;
    };
    let at = egui::vec2(
        pointer.x - layout.galley_pos.x,
        if layout.multiline {
            pointer.y - layout.galley_pos.y
        } else {
            layout.galley.rect.center().y
        },
    );
    let caret = layout.galley.cursor_from_pos(at);
    ctx.stop_dragging();
    let mut state = egui::TextEdit::load_state(ctx, field).unwrap_or_default();
    state
        .cursor
        .set_char_range(Some(egui::text::CCursorRange::one(caret)));
    state.store(ctx, field);
}

#[cfg(test)]
mod field_geometry_tests {
    use super::*;
    use egui_kittest::Harness;

    #[test]
    fn linear_field_geometry_preserves_wrapped_unicode_and_newline_carets() {
        let mut harness = Harness::builder().build_ui_state(
            |ui, galleys: &mut Vec<std::sync::Arc<egui::Galley>>| {
                if galleys.is_empty() {
                    for text in [
                        "",
                        "a\n",
                        "a\n\nb",
                        "one two three four five",
                        "é👩\u{200d}💻\n最後\n",
                    ] {
                        galleys.push(ui.fonts_mut(|fonts| {
                            fonts.layout(
                                text.to_owned(),
                                egui::FontId::monospace(14.0),
                                Color32::WHITE,
                                35.0,
                            )
                        }));
                    }
                }
            },
            Vec::new(),
        );
        harness.run();
        let pos = egui::pos2(17.0, -23.0);
        for galley in harness.state() {
            for multiline in [false, true] {
                let geometry = field_geometry(galley, pos, multiline);
                assert_eq!(geometry.carets.len(), galley.text().chars().count() + 1);
                for (index, x) in geometry.carets.iter().enumerate() {
                    let rect = galley
                        .pos_from_cursor(egui::text::CCursor::new(index))
                        .translate(pos.to_vec2());
                    assert_eq!(*x, rect.left());
                    if multiline {
                        assert_eq!(
                            geometry.rows[index],
                            (rect.top(), rect.bottom().max(rect.top() + 1.0))
                        );
                    }
                }
                assert_eq!(geometry.rows.is_empty(), !multiline);
            }
        }
    }

    #[test]
    fn maximum_document_field_geometry_is_complete_and_cached_between_frames() {
        let text = format!("{}\n", "x".repeat(63)).repeat(16_000);
        let chars = text.chars().count();
        let id = Id::new("maximum-document-field");
        let mut harness = Harness::builder().build_ui_state(
            move |ui, text: &mut String| {
                local_text_edit(ui, id, egui::TextEdit::multiline(text), false, true);
            },
            text,
        );
        harness.ctx.memory_mut(|memory| memory.request_focus(id));
        harness.run();
        let first = harness
            .ctx
            .data(|data| data.get_temp::<FieldLayout>(id.with("layout")))
            .unwrap()
            .geometry
            .unwrap();
        assert_eq!(first.carets.len(), chars + 1);
        assert_eq!(first.rows.len(), chars + 1);
        harness.run();
        let second = harness
            .ctx
            .data(|data| data.get_temp::<FieldLayout>(id.with("layout")))
            .unwrap()
            .geometry
            .unwrap();
        assert!(std::sync::Arc::ptr_eq(&first, &second));
    }
}
