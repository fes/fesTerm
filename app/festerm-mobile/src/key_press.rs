//! Shared press feedback for every app-owned key: an enlarged preview while a
//! key is down and real-keyboard repeat (one key, a pause, then a steady rate).

use super::theme::{color, type_scale};
use eframe::egui::{self, Align2, Color32, CornerRadius, Id, Rect, Response, Stroke, Ui};
use festerm_core::Key;
use festerm_ui_egui::icon::{self, Icon};

pub(super) const HOLD_DELAY: f64 = 0.4;
pub(super) const REPEAT_DELAY: f64 = 0.5;
pub(super) const REPEAT_INTERVAL: f64 = 0.08;
const PREVIEW_FILL: Color32 = color::TERMINAL_KEY;

#[derive(Clone, Debug, PartialEq)]
pub enum Glyph {
    Text(String),
    Arrow(Key),
    Backspace,
    Shift,
    Tab { reverse: bool },
    Icon(Icon),
    Grid,
}

impl Glyph {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text(text.into())
    }
}

/// How a held key behaves.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Repeat {
    /// Fires once on release, never repeats (Insert, a mode toggle).
    Release,
    /// Fires on release, or its alternate once after [`HOLD_DELAY`].
    Hold(char),
    /// Fires on press, then repeats after [`REPEAT_DELAY`] every [`REPEAT_INTERVAL`].
    Auto,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Fired {
    Base,
    Alternate(char),
}

pub(super) struct Press {
    pub(super) id: Id,
    origin: egui::Pos2,
    pub(super) next: f64,
    pub(super) emitted: bool,
    /// Started with a sticky modifier: that latch is one-shot, so the key is too.
    once: bool,
}

#[derive(Default)]
pub struct KeyPress {
    pub(super) press: Option<Press>,
    pub(super) blocked: bool,
    /// The glyph previewed this pass; painted immediately, kept for tests.
    preview: Option<Glyph>,
    pass: Option<u64>,
    latched: bool,
}

impl KeyPress {
    pub fn cancel(&mut self) {
        self.press = None;
        self.preview = None;
        self.blocked = true;
    }

    /// Whether a one-shot Ctrl/Alt/Shift is latched for presses that start now.
    pub fn latch(&mut self, latched: bool) {
        self.latched = latched;
    }

    /// Call before drawing each key surface; `cancel` adds a caller-specific
    /// reason such as a resized keyboard.
    pub fn begin(&mut self, ui: &Ui, cancel: bool) {
        let pass = ui.ctx().cumulative_pass_nr();
        if self.pass != Some(pass) {
            self.pass = Some(pass);
            self.preview = None;
        }
        let cancelled = cancel
            || !ui.is_enabled()
            || ui.input(|input| {
                !input.focused
                    || input
                        .multi_touch()
                        .is_some_and(|touch| touch.num_touches > 1)
                    || input.events.iter().any(|event| {
                        matches!(
                            event,
                            egui::Event::Touch {
                                phase: egui::TouchPhase::Cancel,
                                ..
                            }
                        )
                    })
            });
        if cancelled {
            self.cancel();
        }
        if ui.input(|input| !input.pointer.any_down() && !input.pointer.any_released()) {
            self.press = None;
            self.blocked = false;
        }
    }

    /// A key that sends input. Returns what to send this pass, if anything.
    pub fn key(
        &mut self,
        ui: &Ui,
        response: &Response,
        glyph: Glyph,
        repeat: Repeat,
    ) -> Option<Fired> {
        let (now, position) = ui.input(|input| (input.time, input.pointer.interact_pos()));
        if self.press.as_ref().is_some_and(|press| {
            press.id == response.id
                && position.is_none_or(|pos| {
                    !response.rect.contains(pos) || pos.distance(press.origin) > 10.0
                })
        }) {
            self.cancel();
        }
        if self.blocked {
            return None;
        }
        let owns = self
            .press
            .as_ref()
            .is_some_and(|press| press.id == response.id);
        let mut fired = None;
        if response.is_pointer_button_down_on() || (owns && ui.input(|i| i.pointer.primary_down()))
        {
            if self.press.is_none() {
                let auto = repeat == Repeat::Auto;
                self.press = Some(Press {
                    id: response.id,
                    origin: position.unwrap_or(response.rect.center()),
                    next: now + if auto { REPEAT_DELAY } else { HOLD_DELAY },
                    emitted: auto,
                    once: self.latched,
                });
                if auto {
                    fired = Some(Fired::Base);
                }
            }
            if let Some(press) = self.press.as_mut().filter(|press| press.id == response.id) {
                let mut shown = glyph;
                match repeat {
                    Repeat::Auto if fired.is_none() && !press.once && now >= press.next => {
                        press.next = now + REPEAT_INTERVAL;
                        fired = Some(Fired::Base);
                    }
                    Repeat::Hold(alternate) if !press.emitted && now >= press.next => {
                        press.emitted = true;
                        fired = Some(Fired::Alternate(alternate));
                    }
                    _ => {}
                }
                if let (Repeat::Hold(alternate), true) = (repeat, press.emitted) {
                    shown = Glyph::Text(alternate.to_string());
                }
                let waiting = match repeat {
                    Repeat::Release => false,
                    Repeat::Hold(_) => !press.emitted,
                    Repeat::Auto => !press.once,
                };
                if waiting {
                    ui.ctx()
                        .request_repaint_after(std::time::Duration::from_secs_f64(
                            (press.next - now).max(0.0),
                        ));
                }
                self.show_preview(ui.ctx(), shown, response.rect);
            }
            return fired;
        }
        let released = owns && ui.input(|input| input.pointer.primary_released());
        if response.clicked() || released {
            let emitted = self
                .press
                .as_ref()
                .is_some_and(|press| press.id == response.id && press.emitted);
            self.press = None;
            return (!emitted).then_some(Fired::Base);
        }
        None
    }

    /// A latch or command key: previews while down and acts on a normal click.
    pub fn toggle(&mut self, ui: &Ui, response: &Response, glyph: Glyph) -> bool {
        if response.is_pointer_button_down_on() && !self.blocked {
            self.show_preview(ui.ctx(), glyph, response.rect);
        }
        response.clicked()
    }

    fn show_preview(&mut self, ctx: &egui::Context, glyph: Glyph, rect: Rect) {
        paint_preview(ctx, &glyph, rect);
        self.preview = Some(glyph);
    }

    #[cfg(test)]
    pub(super) fn preview_glyph(&self) -> Option<&Glyph> {
        self.preview.as_ref()
    }
}

fn glyph_font(text: &str) -> egui::FontId {
    let mut characters = text.chars();
    match (characters.next(), characters.next()) {
        (Some(character), None) if character.is_ascii_alphabetic() => type_scale::font(32.0),
        (Some(_), None) => type_scale::mono(30.0),
        _ => type_scale::font(20.0),
    }
}

fn paint_preview(ctx: &egui::Context, glyph: &Glyph, key: Rect) {
    let viewport = ctx.content_rect().shrink(4.0);
    let content_width = match glyph {
        Glyph::Text(text) => ctx.fonts_mut(|fonts| {
            fonts
                .layout_no_wrap(text.clone(), glyph_font(text), color::TEXT)
                .size()
                .x
        }),
        _ => 30.0,
    };
    let width = (content_width + 28.0)
        .max(52.0)
        .max((key.width() + 22.0).min(76.0))
        .min(viewport.width());
    let center_x = key.center().x.clamp(
        viewport.left() + width / 2.0,
        viewport.right() - width / 2.0,
    );
    let bubble = Rect::from_min_size(
        egui::pos2(
            center_x - width / 2.0,
            (key.top() - 60.0).max(viewport.top()),
        ),
        egui::vec2(width, 54.0),
    );
    let painter = ctx
        .layer_painter(egui::LayerId::new(
            egui::Order::Tooltip,
            Id::new("key-preview"),
        ))
        .with_clip_rect(viewport);
    let neck = (width - 16.0).min(key.width() - 6.0).max(8.0) / 2.0;
    let bottom = key.top() + 7.0;
    painter.add(egui::Shape::convex_polygon(
        vec![
            bubble.left_bottom() + egui::vec2(8.0, -8.0),
            bubble.right_bottom() + egui::vec2(-8.0, -8.0),
            egui::pos2((key.center().x + neck).min(key.right() - 3.0), bottom),
            egui::pos2((key.center().x - neck).max(key.left() + 3.0), bottom),
        ],
        PREVIEW_FILL,
        Stroke::NONE,
    ));
    painter.rect_filled(bubble, CornerRadius::same(9), PREVIEW_FILL);
    paint_glyph(&painter, glyph, bubble.center(), 2.0, color::TEXT);
}

/// Draws a key face glyph; `scale` 1.0 is the key itself, 2.0 the preview.
pub(super) fn paint_glyph(
    painter: &egui::Painter,
    glyph: &Glyph,
    center: egui::Pos2,
    scale: f32,
    tint: Color32,
) {
    match glyph {
        Glyph::Text(text) => {
            painter.text(center, Align2::CENTER_CENTER, text, glyph_font(text), tint);
        }
        Glyph::Arrow(key) => paint_arrow(painter, center, *key, tint, scale),
        Glyph::Backspace => paint_backspace(painter, center, tint, scale),
        Glyph::Shift => paint_shift(painter, center, tint, scale),
        Glyph::Tab { reverse } => {
            let direction = if *reverse { -1.0 } else { 1.0 };
            paint_arrow(
                painter,
                center - egui::vec2(direction * scale, 0.0),
                if *reverse {
                    Key::ArrowLeft
                } else {
                    Key::ArrowRight
                },
                tint,
                scale * 0.72,
            );
            let edge = center.x + direction * 7.0 * scale;
            painter.line_segment(
                [
                    egui::pos2(edge, center.y - 5.0 * scale),
                    egui::pos2(edge, center.y + 5.0 * scale),
                ],
                Stroke::new(1.6 * scale.sqrt(), tint),
            );
        }
        Glyph::Icon(icon) => icon::paint(painter, *icon, icon_rect(*icon, center, scale), tint),
        Glyph::Grid => paint_grid(
            painter,
            Rect::from_center_size(center, egui::Vec2::splat(18.0 * scale)),
            tint,
        ),
    }
}

/// `RemoteGlobe` is badge art: a small globe in the lower-right of its
/// 24-unit box, centered at (18.3, 15.4). Enlarge the box and shift it so the
/// globe itself is the size of other key icons and sits on `center`.
const GLOBE_CENTER: egui::Vec2 = egui::vec2(18.3, 15.4);
const GLOBE_BOX: f32 = 44.0;

fn icon_rect(icon: Icon, center: egui::Pos2, scale: f32) -> Rect {
    if icon == Icon::RemoteGlobe {
        let side = GLOBE_BOX * scale;
        let offset = (GLOBE_CENTER - egui::Vec2::splat(12.0)) / 24.0 * side;
        Rect::from_center_size(center - offset, egui::Vec2::splat(side))
    } else {
        Rect::from_center_size(center, egui::Vec2::splat(20.0 * scale))
    }
}

pub(super) fn paint_arrow(
    painter: &egui::Painter,
    center: egui::Pos2,
    key: Key,
    tint: Color32,
    scale: f32,
) {
    let direction = match key {
        Key::ArrowLeft => egui::vec2(-1.0, 0.0),
        Key::ArrowRight => egui::vec2(1.0, 0.0),
        Key::ArrowUp => egui::vec2(0.0, -1.0),
        _ => egui::vec2(0.0, 1.0),
    };
    let side = egui::vec2(-direction.y, direction.x);
    let stroke = Stroke::new(1.6 * scale.sqrt(), tint);
    let tail = center - direction * 7.0 * scale;
    let head = center + direction * 7.0 * scale;
    painter.line_segment([tail, head], stroke);
    painter.line_segment(
        [head, head - (direction * 5.0 - side * 4.5) * scale],
        stroke,
    );
    painter.line_segment(
        [head, head - (direction * 5.0 + side * 4.5) * scale],
        stroke,
    );
}

fn paint_backspace(painter: &egui::Painter, center: egui::Pos2, ink: Color32, scale: f32) {
    let points = [
        (-10.0, 0.0),
        (-4.0, -7.0),
        (10.0, -7.0),
        (10.0, 7.0),
        (-4.0, 7.0),
    ]
    .map(|(x, y)| center + egui::vec2(x, y) * scale);
    let stroke = Stroke::new(1.3 * scale.sqrt(), ink);
    for index in 0..points.len() {
        painter.line_segment([points[index], points[(index + 1) % points.len()]], stroke);
    }
    for direction in [-1.0, 1.0] {
        painter.line_segment(
            [
                center + egui::vec2(-2.0, -3.0 * direction) * scale,
                center + egui::vec2(5.0, 3.0 * direction) * scale,
            ],
            stroke,
        );
    }
}

fn paint_shift(painter: &egui::Painter, center: egui::Pos2, ink: Color32, scale: f32) {
    paint_arrow(
        painter,
        center - egui::vec2(0.0, 2.0) * scale,
        Key::ArrowUp,
        ink,
        scale,
    );
    painter.hline(
        (center.x - 6.0 * scale)..=(center.x + 6.0 * scale),
        center.y + 8.0 * scale,
        Stroke::new(1.5 * scale.sqrt(), ink),
    );
}

fn paint_grid(painter: &egui::Painter, rect: Rect, tint: Color32) {
    let cell = rect.width() * 0.42;
    for (dx, dy) in [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)] {
        let min = rect.min + egui::vec2(dx * (rect.width() - cell), dy * (rect.height() - cell));
        painter.rect_stroke(
            Rect::from_min_size(min, egui::vec2(cell, cell)),
            CornerRadius::same(2),
            Stroke::new(1.6, tint),
            egui::StrokeKind::Inside,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globe_glyph_is_centered_on_its_key() {
        let ctx = egui::Context::default();
        let center = egui::pos2(100.0, 80.0);
        let mut output = ctx.run_ui(Default::default(), |ui| {
            paint_glyph(
                ui.painter(),
                &Glyph::Icon(Icon::RemoteGlobe),
                center,
                1.0,
                Color32::WHITE,
            );
        });
        let bounds = output.shapes.iter().fold(Rect::NOTHING, |bounds, shape| {
            bounds.union(shape.shape.visual_bounding_rect())
        });
        assert!((bounds.center() - center).length() < 1.0, "{bounds:?}");
        assert!(bounds.width() > 14.0, "{bounds:?}");
        output.textures_delta.clear();
    }
}
