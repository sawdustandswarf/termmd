//! `wl_pointer`: position, buttons and scrolling (accumulated per `frame`),
//! and the cursor image via `cursor-shape-v1`.

use super::protocols::{wl_pointer, wp_cursor_shape_device_v1};
use super::wire::Outgoing;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Left,
    Right,
    Middle,
    Back,
    Forward,
    /// Another evdev button code.
    Other(u32),
}

impl Button {
    fn from_evdev(code: u32) -> Button {
        match code {
            0x110 => Button::Left,
            0x111 => Button::Right,
            0x112 => Button::Middle,
            0x113 => Button::Back,
            0x114 => Button::Forward,
            c => Button::Other(c),
        }
    }
}

/// Pointer input; positions are in buffer pixels.
#[derive(Debug, Clone, PartialEq)]
pub enum PointerEvent {
    Enter {
        x: f64,
        y: f64,
    },
    Leave,
    Motion {
        x: f64,
        y: f64,
    },
    Button {
        button: Button,
        pressed: bool,
        serial: u32,
    },
    /// Positive is down/right. `steps` are wheel notches (fractional for
    /// high-resolution wheels); `pixels` is continuous (touchpad) scrolling,
    /// in buffer pixels. A frame carries one or the other.
    Scroll {
        steps: (f64, f64),
        pixels: (f64, f64),
    },
}

/// The pointer image over our surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorShape {
    Text,
    Default,
    /// Over a link.
    Pointer,
}

#[derive(Debug, Default)]
struct ScrollFrame {
    value120: (i32, i32),
    value: (f64, f64),
}

#[derive(Debug)]
pub(super) struct Pointer {
    pub handle: wl_pointer::WlPointer,
    shape_device: Option<wp_cursor_shape_device_v1::WpCursorShapeDeviceV1>,
    enter_serial: Option<u32>,
    shape: CursorShape,
    scroll: ScrollFrame,
}

impl Pointer {
    pub fn new(
        handle: wl_pointer::WlPointer,
        shape_device: Option<wp_cursor_shape_device_v1::WpCursorShapeDeviceV1>,
    ) -> Pointer {
        Pointer { handle, shape_device, enter_serial: None, shape: CursorShape::Text, scroll: ScrollFrame::default() }
    }

    fn apply_shape(&self, out: &mut Outgoing) {
        if let (Some(dev), Some(serial)) = (self.shape_device, self.enter_serial) {
            let shape = match self.shape {
                CursorShape::Text => wp_cursor_shape_device_v1::Shape::TEXT,
                CursorShape::Default => wp_cursor_shape_device_v1::Shape::DEFAULT,
                CursorShape::Pointer => wp_cursor_shape_device_v1::Shape::POINTER,
            };
            dev.set_shape(out, serial, shape);
        }
    }

    pub fn set_shape(&mut self, shape: CursorShape, out: &mut Outgoing) {
        if shape != self.shape {
            self.shape = shape;
            self.apply_shape(out);
        }
    }

    pub fn handle(&mut self, ev: wl_pointer::Event, scale120: u32, out: &mut Outgoing, events: &mut Vec<PointerEvent>) {
        let px = |v: super::wire::Fixed| v.to_f64() * scale120 as f64 / 120.0;
        match ev {
            wl_pointer::Event::Enter { serial, surface_x, surface_y, .. } => {
                self.enter_serial = Some(serial);
                self.apply_shape(out);
                events.push(PointerEvent::Enter { x: px(surface_x), y: px(surface_y) });
            }
            wl_pointer::Event::Leave { .. } => {
                self.enter_serial = None;
                events.push(PointerEvent::Leave);
            }
            wl_pointer::Event::Motion { surface_x, surface_y, .. } => {
                events.push(PointerEvent::Motion { x: px(surface_x), y: px(surface_y) });
            }
            wl_pointer::Event::Button { serial, button, state, .. } => events.push(PointerEvent::Button {
                button: Button::from_evdev(button),
                pressed: state == wl_pointer::ButtonState::PRESSED,
                serial,
            }),
            wl_pointer::Event::Axis { axis, value, .. } => {
                let v = px(value);
                if axis == wl_pointer::Axis::HORIZONTAL_SCROLL {
                    self.scroll.value.0 += v;
                } else {
                    self.scroll.value.1 += v;
                }
            }
            wl_pointer::Event::AxisValue120 { axis, value120 } => {
                if axis == wl_pointer::Axis::HORIZONTAL_SCROLL {
                    self.scroll.value120.0 += value120;
                } else {
                    self.scroll.value120.1 += value120;
                }
            }
            wl_pointer::Event::Frame => {
                let s = std::mem::take(&mut self.scroll);
                if s.value120 != (0, 0) {
                    let steps = (s.value120.0 as f64 / 120.0, s.value120.1 as f64 / 120.0);
                    events.push(PointerEvent::Scroll { steps, pixels: (0.0, 0.0) });
                } else if s.value != (0.0, 0.0) {
                    events.push(PointerEvent::Scroll { steps: (0.0, 0.0), pixels: s.value });
                }
            }
            _ => {}
        }
    }
}
