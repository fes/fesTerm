//! Touch navigation arbitration. Owns no terminal bytes or platform APIs.
use eframe::egui::{self, Event, Key as EguiKey, Pos2, Rect, TouchDeviceId, TouchId, TouchPhase};
use festerm_core::Key;

const HOLD_SECONDS: f64 = 0.45;
const DRAG_SLOP: f32 = 10.0;
const DEAD_ZONE: f32 = 12.0;
const MIN_PINCH_SPAN: f32 = 20.0;
const PINCH_SLOP: f32 = 2.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Left,
    Down,
    Up,
    Right,
}

impl Direction {
    pub fn key(self) -> Key {
        match self {
            Self::Left => Key::ArrowLeft,
            Self::Down => Key::ArrowDown,
            Self::Up => Key::ArrowUp,
            Self::Right => Key::ArrowRight,
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Pending,
    Arrows,
    Pinch,
    Passthrough,
    Cancelled,
}

struct Contact {
    owner: (TouchDeviceId, TouchId),
    origin: Pos2,
    position: Pos2,
    started: f64,
    mode: Mode,
    direction: Option<Direction>,
    next_repeat: f64,
    bounds: Rect,
    pinch: Option<Pinch>,
}

struct Pinch {
    owner: (TouchDeviceId, TouchId),
    position: Pos2,
    previous_span: f32,
}

#[derive(Default)]
pub struct TerminalGesture {
    contact: Option<Contact>,
}

#[derive(Default)]
pub struct GestureFrame {
    pub block_pointer: bool,
    pub key: Option<Key>,
    pub repaint_after: Option<f64>,
    pub zoom_factor: Option<f32>,
}

fn pointer_event(pos: Pos2, pressed: bool) -> Event {
    Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    }
}

impl TerminalGesture {
    pub fn cancel(&mut self) {
        if let Some(contact) = &mut self.contact {
            contact.mode = Mode::Cancelled;
        }
    }

    /// Returns Some(None) for an active helper in its neutral dead zone.
    pub fn helper(&self) -> Option<Option<Direction>> {
        self.contact
            .as_ref()
            .filter(|c| c.mode == Mode::Arrows)
            .map(|c| c.direction)
    }

    pub fn update(
        &mut self,
        events: &mut Vec<Event>,
        now: f64,
        bounds: Rect,
        any_touches: bool,
        enabled: bool,
    ) -> GestureFrame {
        let mut frame = GestureFrame::default();
        let enabled = enabled
            && !events.iter().any(|event| {
                matches!(
                    event,
                    Event::WindowFocused(false)
                        | Event::Key {
                            key: EguiKey::Escape,
                            pressed: true,
                            ..
                        }
                )
            });
        if !enabled {
            self.cancel();
        }
        if self.contact.as_ref().is_some_and(|c| c.bounds != bounds) {
            self.cancel();
        }
        let mut suppress = self
            .contact
            .as_ref()
            .is_some_and(|c| c.mode != Mode::Passthrough);
        let mut replay = Vec::new();
        for event in events.iter() {
            let Event::Touch {
                device_id,
                id,
                phase,
                pos,
                ..
            } = event
            else {
                continue;
            };
            let owner = (*device_id, *id);
            if *phase == TouchPhase::Start {
                if let Some(contact) = &mut self.contact {
                    if contact.owner != owner && contact.mode != Mode::Passthrough {
                        let span = contact.position.distance(*pos);
                        if matches!(contact.mode, Mode::Pending | Mode::Arrows)
                            && device_id == &contact.owner.0
                            && bounds.contains(*pos)
                            && span.is_finite()
                            && span >= MIN_PINCH_SPAN
                        {
                            contact.mode = Mode::Pinch;
                            contact.pinch = Some(Pinch {
                                owner,
                                position: *pos,
                                previous_span: span,
                            });
                        } else {
                            contact.mode = Mode::Cancelled;
                        }
                        suppress = true;
                    }
                } else if enabled {
                    // Remember an outside start without capturing it. A second
                    // finger in the terminal must not steal a toolbar gesture.
                    let inside = bounds.contains(*pos);
                    self.contact = Some(Contact {
                        owner,
                        origin: *pos,
                        position: *pos,
                        started: now,
                        mode: if inside {
                            Mode::Pending
                        } else {
                            Mode::Passthrough
                        },
                        direction: None,
                        next_repeat: now,
                        bounds,
                        pinch: None,
                    });
                    suppress |= inside;
                }
            }
            let Some(contact) = &mut self.contact else {
                continue;
            };
            if contact.owner != owner {
                if contact.mode == Mode::Pinch {
                    if let Some(pinch) = &mut contact.pinch {
                        if pinch.owner == owner {
                            match phase {
                                TouchPhase::Move => pinch.position = *pos,
                                TouchPhase::End | TouchPhase::Cancel => {
                                    contact.mode = Mode::Cancelled
                                }
                                TouchPhase::Start => {}
                            }
                        }
                    }
                }
                continue;
            }
            match phase {
                TouchPhase::Move => {
                    contact.position = *pos;
                    if contact.mode == Mode::Pending
                        && now - contact.started < HOLD_SECONDS
                        && contact.origin.distance(*pos) > DRAG_SLOP
                    {
                        contact.mode = Mode::Passthrough;
                        // The shared renderer did not receive the held-down
                        // event. Replay it once before the ordinary drag.
                        replay.push(pointer_event(contact.origin, true));
                        suppress = false;
                    }
                }
                TouchPhase::End => {
                    if contact.mode == Mode::Pinch {
                        // Keep both contacts captured until all fingers lift;
                        // the remaining finger must not become a fresh arrow/click.
                        contact.mode = Mode::Cancelled;
                    }
                    if contact.mode == Mode::Pending && now - contact.started < HOLD_SECONDS {
                        replay.push(pointer_event(contact.origin, true));
                        replay.push(pointer_event(*pos, false));
                    }
                    frame.block_pointer |= matches!(contact.mode, Mode::Arrows | Mode::Cancelled);
                    if contact.mode != Mode::Cancelled {
                        self.contact = None;
                    }
                }
                TouchPhase::Cancel => {
                    frame.block_pointer |= contact.mode != Mode::Passthrough;
                    if contact.mode != Mode::Passthrough {
                        contact.mode = Mode::Cancelled;
                    } else {
                        self.contact = None;
                    }
                }
                TouchPhase::Start => {}
            }
        }
        // Missing/cleared touch state is cancellation, never one final repeat.
        if !any_touches {
            self.cancel();
        }
        if let Some(contact) = &mut self.contact {
            if contact.mode == Mode::Pending && now - contact.started >= HOLD_SECONDS {
                contact.mode = Mode::Arrows;
            }
            frame.block_pointer |= contact.mode != Mode::Passthrough;
            if contact.mode == Mode::Pending {
                frame.repaint_after = Some((contact.started + HOLD_SECONDS - now).max(0.0));
            }
            if contact.mode == Mode::Arrows {
                let delta = contact.position - contact.origin;
                let distance = delta.x.abs().max(delta.y.abs());
                let direction = (distance >= DEAD_ZONE).then(|| {
                    if delta.x.abs() > delta.y.abs() {
                        if delta.x < 0.0 {
                            Direction::Left
                        } else {
                            Direction::Right
                        }
                    } else if delta.y < 0.0 {
                        Direction::Up
                    } else {
                        Direction::Down
                    }
                });
                if direction != contact.direction {
                    contact.next_repeat = now;
                }
                contact.direction = direction;
                if let Some(direction) = direction {
                    let interval = if distance >= 80.0 {
                        0.055
                    } else if distance >= 40.0 {
                        0.10
                    } else {
                        0.18
                    };
                    if now >= contact.next_repeat {
                        frame.key = Some(direction.key());
                        // At most one key per frame, never catch up after a stall.
                        contact.next_repeat = now + interval;
                    }
                    frame.repaint_after = Some((contact.next_repeat - now).max(0.0));
                }
            }
            if contact.mode == Mode::Pinch {
                if let Some(pinch) = &mut contact.pinch {
                    let span = contact.position.distance(pinch.position);
                    if !span.is_finite() || span < MIN_PINCH_SPAN {
                        contact.mode = Mode::Cancelled;
                    } else if (span - pinch.previous_span).abs() >= PINCH_SLOP {
                        // Coalesce all moves in this frame. Updating the span
                        // even at the font limit makes reversing immediately responsive.
                        frame.zoom_factor = Some(span / pinch.previous_span);
                        pinch.previous_span = span;
                    }
                }
            }
        }
        if suppress {
            // winit emits both Touch and mouse-emulation events for one finger.
            // Never deliver a trailing click/mouse-report after a held gesture.
            events.retain(|e| {
                !matches!(
                    e,
                    Event::Touch { .. }
                        | Event::PointerMoved(_)
                        | Event::PointerGone
                        | Event::PointerButton { .. }
                )
            });
        } else if !replay.is_empty() {
            // Avoid duplicate down if Start and Move arrived in one frame.
            events.retain(|e| !matches!(e, Event::PointerButton { pressed: true, .. }));
        }
        replay.append(events);
        *events = replay;
        if !any_touches {
            self.contact = None;
        }
        frame
    }

    pub fn paint_helper(&self, ui: &egui::Ui, bounds: Rect) {
        let Some(direction) = self.helper() else {
            return;
        };
        let size = 78.0_f32.min(bounds.width()).min(bounds.height());
        if size < 32.0 {
            return;
        }
        let rect = Rect::from_min_size(
            egui::pos2(
                (bounds.right() - size - 12.0).max(bounds.left()),
                (bounds.top() + 12.0).min(bounds.bottom() - size),
            ),
            egui::vec2(size, size),
        );
        let painter = ui
            .ctx()
            .layer_painter(egui::LayerId::new(
                egui::Order::Foreground,
                egui::Id::new("arrow-gesture-helper"),
            ))
            .with_clip_rect(bounds);
        painter.rect_filled(
            rect,
            12.0,
            egui::Color32::from_rgba_unmultiplied(205, 210, 220, 230),
        );
        for (value, label, offset) in [
            (Direction::Left, "←", egui::vec2(-0.29, 0.0)),
            (Direction::Down, "↓", egui::vec2(0.0, 0.29)),
            (Direction::Up, "↑", egui::vec2(0.0, -0.29)),
            (Direction::Right, "→", egui::vec2(0.29, 0.0)),
        ] {
            let active = direction == Some(value);
            painter.text(
                rect.center() + offset * size,
                egui::Align2::CENTER_CENTER,
                label,
                egui::FontId::proportional(size * 0.31),
                if active {
                    egui::Color32::from_rgb(30, 110, 235)
                } else {
                    egui::Color32::from_rgb(80, 85, 95)
                },
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn bounds() -> Rect {
        Rect::from_min_size(Pos2::ZERO, egui::vec2(300.0, 400.0))
    }
    fn touch(id: u64, phase: TouchPhase, x: f32, y: f32) -> Event {
        Event::Touch {
            device_id: TouchDeviceId(1),
            id: TouchId(id),
            phase,
            pos: egui::pos2(x, y),
            force: None,
        }
    }

    #[test]
    fn mobile_pinch_takes_over_arrows_and_quarantines_remaining_finger() {
        let mut gesture = TerminalGesture::default();
        gesture.update(
            &mut vec![touch(1, TouchPhase::Start, 100.0, 100.0)],
            0.0,
            bounds(),
            true,
            true,
        );
        gesture.update(&mut vec![], 0.5, bounds(), true, true);
        assert!(gesture
            .update(
                &mut vec![touch(1, TouchPhase::Move, 130.0, 100.0)],
                0.6,
                bounds(),
                true,
                true
            )
            .key
            .is_some());
        let start = gesture.update(
            &mut vec![touch(2, TouchPhase::Start, 230.0, 100.0)],
            0.7,
            bounds(),
            true,
            true,
        );
        assert!(start.block_pointer && start.key.is_none() && start.zoom_factor.is_none());
        assert!(gesture.helper().is_none());
        let mut moves = vec![
            touch(1, TouchPhase::Move, 105.0, 100.0),
            touch(2, TouchPhase::Move, 255.0, 100.0),
            Event::PointerMoved(egui::pos2(105.0, 100.0)),
        ];
        let zoom = gesture.update(&mut moves, 0.8, bounds(), true, true);
        assert_eq!(zoom.zoom_factor, Some(1.5));
        assert!(moves.is_empty() && zoom.key.is_none() && zoom.repaint_after.is_none());
        assert!(gesture
            .update(&mut vec![], 0.9, bounds(), true, true)
            .zoom_factor
            .is_none());
        let mut lift = vec![
            touch(2, TouchPhase::End, 255.0, 100.0),
            pointer_event(egui::pos2(255.0, 100.0), false),
        ];
        assert!(
            gesture
                .update(&mut lift, 1.0, bounds(), true, true)
                .block_pointer
        );
        assert!(lift.is_empty());
        let remaining = gesture.update(
            &mut vec![touch(1, TouchPhase::Move, 180.0, 100.0)],
            2.0,
            bounds(),
            true,
            true,
        );
        assert!(
            remaining.block_pointer && remaining.key.is_none() && remaining.zoom_factor.is_none()
        );
        gesture.update(
            &mut vec![touch(1, TouchPhase::End, 180.0, 100.0)],
            2.1,
            bounds(),
            false,
            true,
        );
        gesture.update(
            &mut vec![touch(3, TouchPhase::Start, 100.0, 100.0)],
            3.0,
            bounds(),
            true,
            true,
        );
        gesture.update(&mut vec![], 3.5, bounds(), true, true);
        assert_eq!(gesture.helper(), Some(None));
    }

    #[test]
    fn mobile_pinch_cancellation_and_existing_pointer_ownership_are_respected() {
        for cause in 0..7 {
            let mut gesture = TerminalGesture::default();
            gesture.update(
                &mut vec![
                    touch(1, TouchPhase::Start, 100.0, 100.0),
                    touch(2, TouchPhase::Start, 200.0, 100.0),
                ],
                0.0,
                bounds(),
                true,
                true,
            );
            let mut events = match cause {
                0 => vec![touch(3, TouchPhase::Start, 250.0, 100.0)],
                1 => vec![touch(1, TouchPhase::Cancel, 100.0, 100.0)],
                2 => vec![touch(2, TouchPhase::Cancel, 200.0, 100.0)],
                3 => vec![Event::WindowFocused(false)],
                _ => vec![],
            };
            let rect = if cause == 4 {
                bounds().shrink(1.0)
            } else {
                bounds()
            };
            let result = gesture.update(&mut events, 0.1, rect, cause != 6, cause != 5);
            assert!(result.key.is_none() && result.zoom_factor.is_none());
            let result = gesture.update(
                &mut vec![touch(2, TouchPhase::Move, 290.0, 100.0)],
                0.2,
                rect,
                true,
                true,
            );
            assert!(result.key.is_none() && result.zoom_factor.is_none());
        }
        for outside in [true, false] {
            let mut gesture = TerminalGesture::default();
            gesture.update(
                &mut vec![touch(
                    1,
                    TouchPhase::Start,
                    100.0,
                    if outside { 450.0 } else { 100.0 },
                )],
                0.0,
                bounds(),
                true,
                true,
            );
            if !outside {
                gesture.update(
                    &mut vec![touch(1, TouchPhase::Move, 130.0, 100.0)],
                    0.1,
                    bounds(),
                    true,
                    true,
                );
            }
            let mut events = vec![
                touch(2, TouchPhase::Start, 230.0, 100.0),
                touch(2, TouchPhase::Move, 280.0, 100.0),
            ];
            let result = gesture.update(&mut events, 0.2, bounds(), true, true);
            assert!(!result.block_pointer && result.zoom_factor.is_none() && result.key.is_none());
            assert_eq!(events.len(), 2);
        }
    }
    #[test]
    fn mobile_arrow_hold_drag_repeats_with_dead_zone_and_stops_on_release() {
        let mut gesture = TerminalGesture::default();
        let start = gesture.update(
            &mut vec![touch(1, TouchPhase::Start, 100.0, 100.0)],
            0.0,
            bounds(),
            true,
            true,
        );
        assert!(start.block_pointer && start.key.is_none());
        assert!(gesture.helper().is_none());
        gesture.update(&mut vec![], 0.46, bounds(), true, true);
        assert_eq!(gesture.helper(), Some(None));
        let moved = gesture.update(
            &mut vec![touch(1, TouchPhase::Move, 135.0, 101.0)],
            0.5,
            bounds(),
            true,
            true,
        );
        assert_eq!(moved.key, Some(Key::ArrowRight));
        assert!((moved.repaint_after.unwrap() - 0.18).abs() < 0.0001);
        assert_eq!(gesture.helper(), Some(Some(Direction::Right)));
        assert!(gesture
            .update(&mut vec![], 0.51, bounds(), true, true)
            .key
            .is_none());
        assert_eq!(
            gesture.update(&mut vec![], 5.0, bounds(), true, true).key,
            Some(Key::ArrowRight)
        );
        let neutral = gesture.update(
            &mut vec![touch(1, TouchPhase::Move, 102.0, 100.0)],
            5.1,
            bounds(),
            true,
            true,
        );
        assert!(neutral.key.is_none() && neutral.repaint_after.is_none());
        for (now, x, y, key, interval) in [
            (5.11, 100.0, 50.0, Key::ArrowUp, 0.10),
            (5.12, 10.0, 100.0, Key::ArrowLeft, 0.055),
            (5.13, 100.0, 125.0, Key::ArrowDown, 0.18),
        ] {
            let frame = gesture.update(
                &mut vec![touch(1, TouchPhase::Move, x, y)],
                now,
                bounds(),
                true,
                true,
            );
            assert_eq!(frame.key, Some(key));
            assert!((frame.repaint_after.unwrap() - interval).abs() < 0.0001);
        }
        let mut release = vec![
            touch(1, TouchPhase::End, 150.0, 100.0),
            pointer_event(egui::pos2(150.0, 100.0), false),
        ];
        assert!(gesture
            .update(&mut release, 5.2, bounds(), false, true)
            .key
            .is_none());
        assert!(release.is_empty() && gesture.helper().is_none());
        assert!(gesture
            .update(&mut vec![], 10.0, bounds(), false, true)
            .key
            .is_none());
    }
    #[test]
    fn mobile_arrow_tap_and_early_drag_reach_existing_pointer_routing() {
        let mut gesture = TerminalGesture::default();
        gesture.update(
            &mut vec![touch(1, TouchPhase::Start, 100.0, 100.0)],
            0.0,
            bounds(),
            true,
            true,
        );
        let mut drag = vec![
            touch(1, TouchPhase::Move, 100.0, 130.0),
            Event::PointerMoved(egui::pos2(100.0, 130.0)),
        ];
        assert!(
            !gesture
                .update(&mut drag, 0.1, bounds(), true, true)
                .block_pointer
        );
        assert!(matches!(
            drag[0],
            Event::PointerButton { pressed: true, .. }
        ));
        assert!(gesture.helper().is_none());
        assert!(gesture
            .update(&mut vec![], 1.0, bounds(), true, true)
            .key
            .is_none());
        gesture.update(
            &mut vec![touch(1, TouchPhase::End, 100.0, 130.0)],
            1.1,
            bounds(),
            false,
            true,
        );
        gesture.update(
            &mut vec![touch(2, TouchPhase::Start, 100.0, 100.0)],
            2.0,
            bounds(),
            true,
            true,
        );
        let mut tap = vec![touch(2, TouchPhase::End, 100.0, 100.0)];
        gesture.update(&mut tap, 2.1, bounds(), false, true);
        assert!(matches!(
            tap.as_slice(),
            [
                Event::PointerButton { pressed: true, .. },
                Event::PointerButton { pressed: false, .. }
            ]
        ));
    }
    #[test]
    fn mobile_arrow_multitouch_resize_and_background_cancel_without_keys() {
        for cause in 0..7 {
            let mut gesture = TerminalGesture::default();
            gesture.update(
                &mut vec![touch(1, TouchPhase::Start, 100.0, 100.0)],
                0.0,
                bounds(),
                true,
                true,
            );
            gesture.update(&mut vec![], 0.5, bounds(), true, true);
            assert!(gesture
                .update(
                    &mut vec![touch(1, TouchPhase::Move, 140.0, 100.0)],
                    0.55,
                    bounds(),
                    true,
                    true
                )
                .key
                .is_some());
            let mut events = match cause {
                0 => vec![touch(2, TouchPhase::Start, 140.0, 100.0)],
                3 => vec![touch(1, TouchPhase::Cancel, 140.0, 100.0)],
                4 => vec![Event::WindowFocused(false)],
                5 => vec![Event::Key {
                    key: EguiKey::Escape,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }],
                _ => vec![],
            };
            let rect = if cause == 1 {
                bounds().shrink(1.0)
            } else {
                bounds()
            };
            let frame = gesture.update(&mut events, 0.8, rect, cause != 6, cause != 2);
            assert!(frame.key.is_none() && gesture.helper().is_none());
            assert!(gesture
                .update(
                    &mut vec![touch(1, TouchPhase::Move, 180.0, 100.0)],
                    1.0,
                    rect,
                    true,
                    true
                )
                .key
                .is_none());
        }
    }
}
