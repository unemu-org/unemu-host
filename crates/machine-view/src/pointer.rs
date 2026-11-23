// SPDX-License-Identifier: AGPL-3.0-or-later
//! Host pointer capture for a machine's relative pointing device (backlog/docs/architecture.md, Observation and presentation),
//! and the uncaptured host pointer for an absolute one ([`Aiming`]).
//!
//! A machine with a relative pointer captures on a client-area click, and
//! that click is swallowed. Command+G on macOS (Ctrl+Alt+G elsewhere), focus
//! loss and suspension release capture and the guest's buttons. Raw host
//! motion uses one host unit per device count, positive Y up, with carried
//! fractions; the machine owns coalescing, back-pressure and overflow. See
//! `backlog/docs/architecture.md` § Core boundary and host timing for capture, release and
//! boundary ordering.

use std::collections::BTreeSet;
use unemu_core_api::{InputEvent, PortId};
use winit::{
    event::{ElementState, MouseButton},
    keyboard::KeyCode,
};

/// Pure capture policy; event handlers never borrow the machine. Guest
/// events wait in [`PointerCapture::take_events`] until the session hands
/// them to the machine. A newly captured pointer must also be grabbed by the
/// host window. If that fails, call [`Self::release`] before accepting
/// further events.
pub struct PointerCapture {
    port: Option<PortId>,
    captured: bool,
    focused: bool,
    inside: bool,
    keys: BTreeSet<KeyCode>,
    suppress_g: bool,
    capture_button: Option<MouseButton>,
    fraction: (f64, f64),
    /// The guest's held buttons as packet bits: left 1, right 2, middle 4.
    buttons: u8,
    events: Vec<InputEvent>,
}

impl PointerCapture {
    /// Capture only for a machine with a relative pointer on `port`.
    pub fn new(port: Option<PortId>) -> Self {
        Self {
            port,
            captured: false,
            focused: true,
            inside: false,
            keys: BTreeSet::new(),
            suppress_g: false,
            capture_button: None,
            fraction: (0.0, 0.0),
            buttons: 0,
            events: Vec::new(),
        }
    }

    /// Whether raw movement and subsequent clicks belong to the guest.
    pub fn captured(&self) -> bool {
        self.captured
    }

    /// Client-area presence gates the initial capture click only.
    pub fn pointer_inside(&mut self, inside: bool) {
        self.inside = inside;
    }

    /// The guest events queued since the last call, in order.
    pub fn take_events(&mut self) -> Vec<InputEvent> {
        std::mem::take(&mut self.events)
    }

    /// Capture on a client-area press, swallowing both edges of that click.
    /// Returns true only when a host grab needs to be attempted.
    pub fn button_event(&mut self, button: MouseButton, state: ElementState) -> bool {
        if self.capture_button == Some(button) {
            if state == ElementState::Released {
                self.capture_button = None;
            }
            return false;
        }
        let Some(port) = self.port else {
            return false;
        };
        if !self.captured {
            if self.focused && self.inside && state == ElementState::Pressed {
                self.captured = true;
                self.capture_button = Some(button);
                return true;
            }
            return false;
        }
        let (number, bit) = match button {
            MouseButton::Left => (0, 1),
            MouseButton::Right => (1, 2),
            MouseButton::Middle => (2, 4),
            _ => return false,
        };
        let pressed = state == ElementState::Pressed;
        let buttons = if pressed {
            self.buttons | bit
        } else {
            self.buttons & !bit
        };
        if buttons != self.buttons {
            self.buttons = buttons;
            self.events.push(InputEvent::PointerButton {
                port,
                button: number,
                pressed,
            });
        }
        false
    }

    /// Consume the G pair of Ctrl+Alt+G while captured. Modifier keys continue
    /// through the ordinary keyboard path, including their eventual releases.
    /// A G already held by the guest is never stolen by a later modifier press.
    pub fn key_event(&mut self, key: KeyCode, state: ElementState) -> bool {
        use KeyCode::*;
        let fresh = if state == ElementState::Pressed {
            self.keys.insert(key)
        } else {
            self.keys.remove(&key);
            false
        };
        if key == KeyG && self.suppress_g {
            if state == ElementState::Released {
                self.suppress_g = false;
            }
            return true;
        }
        if self.captured
            && key == KeyG
            && state == ElementState::Pressed
            && fresh
            && [ControlLeft, ControlRight]
                .iter()
                .any(|key| self.keys.contains(key))
            && [AltLeft, AltRight]
                .iter()
                .any(|key| self.keys.contains(key))
        {
            self.suppress_g = true;
            self.release();
            return true;
        }
        false
    }

    /// Resume a capture that a host dialog interrupted, without a capture
    /// click. Returns true only when a host grab needs to be attempted.
    pub fn recapture(&mut self) -> bool {
        if self.captured || self.port.is_none() || !self.focused {
            return false;
        }
        self.captured = true;
        true
    }

    /// Focus loss releases guest buttons and discards sub-count capture
    /// residue. With `released_by_machine` the session has already told the
    /// machine to release everything ([`InputEvent::ReleaseAll`]), so the
    /// buttons go up there in one step and no event is queued here.
    pub fn focus_changed(&mut self, focused: bool, released_by_machine: bool) {
        self.focused = focused;
        if !focused {
            if released_by_machine {
                self.buttons = 0;
            }
            self.release();
            self.inside = false;
            self.keys.clear();
            self.suppress_g = false;
            self.capture_button = None;
        }
    }

    /// The machine released every button by itself ([`InputEvent::ReleaseAll`]
    /// without a focus loss): none is held any more and capture stays.
    pub fn buttons_released(&mut self) {
        self.buttons = 0;
    }

    /// Keep queued input, then release any guest buttons at the next boundary.
    pub fn release(&mut self) {
        self.captured = false;
        self.fraction = (0.0, 0.0);
        let Some(port) = self.port else {
            return;
        };
        for (number, bit) in [(0, 1), (1, 2), (2, 4)] {
            if self.buttons & bit != 0 {
                self.events.push(InputEvent::PointerButton {
                    port,
                    button: number,
                    pressed: false,
                });
            }
        }
        self.buttons = 0;
    }

    /// Queue raw relative host movement (Y down), with a fixed 1:1 scale.
    /// Finite values truncate toward zero with carried fractions. Conversion
    /// saturates at i32 limits, leaving packet limits to the target device.
    /// Non-finite values are ignored per axis so one bad event cannot poison it.
    pub fn motion(&mut self, dx: f64, dy: f64) {
        let Some(port) = self.port.filter(|_| self.captured) else {
            return;
        };
        fn counts(delta: f64, fraction: &mut f64) -> i32 {
            if !delta.is_finite() {
                return 0;
            }
            let total = delta + *fraction;
            let whole = total.trunc();
            *fraction = total - whole;
            whole as i32
        }
        let dx = counts(dx, &mut self.fraction.0);
        let dy = counts(-dy, &mut self.fraction.1);
        if dx == 0 && dy == 0 {
            return;
        }
        self.events.push(InputEvent::PointerMotion { port, dx, dy });
    }
}

/// The host pointer over the screen for a machine's absolute pointing device,
/// a light gun, a light pen or a touch screen (core-contract.md §13.3, "The
/// absolute pointer"). Nothing is captured: the view says where the pointer
/// is on the picture ([`crate::screen::ScreenLayout::pointer_position`]) and
/// which clicks are meant for the machine.
///
/// A place is sent only when it changes, and `None` when the pointer leaves
/// the screen or the window loses focus. The buttons are the host's left,
/// right and middle as 0, 1 and 2, as many as the port has; button 0 is the
/// primary one. A press counts wherever the pointer is, since aiming off the
/// picture and pulling the trigger is a thing a light gun does.
pub struct Aiming {
    port: Option<PortId>,
    buttons: u8,
    /// The place the machine was last told, or `None` when it is unknown:
    /// before the first move, and after the machine's state was replaced.
    sent: Option<Option<(u16, u16)>>,
    /// The machine's held buttons as bits, button 0 in bit 0.
    held: u8,
    events: Vec<InputEvent>,
}

impl Aiming {
    /// Aim only for a machine with an absolute pointer on `port`, with
    /// `buttons` buttons.
    pub fn new(port: Option<(PortId, u8)>) -> Self {
        Self {
            port: port.map(|(port, _)| port),
            buttons: port.map_or(0, |(_, buttons)| buttons),
            sent: None,
            held: 0,
            events: Vec::new(),
        }
    }

    /// Whether the machine has a device to aim.
    pub fn aims(&self) -> bool {
        self.port.is_some()
    }

    /// The guest events queued since the last call, in order.
    pub fn take_events(&mut self) -> Vec<InputEvent> {
        std::mem::take(&mut self.events)
    }

    /// The host pointer is now at `position` on the whole picture, or off
    /// it.
    pub fn moved(&mut self, position: Option<(u16, u16)>) {
        let Some(port) = self.port else {
            return;
        };
        if self.sent == Some(position) {
            return;
        }
        self.sent = Some(position);
        self.events
            .push(InputEvent::PointerPosition { port, position });
    }

    /// A host button the view meant for the machine.
    pub fn button_event(&mut self, button: MouseButton, state: ElementState) {
        let Some(port) = self.port else {
            return;
        };
        let number = match button {
            MouseButton::Left => 0,
            MouseButton::Right => 1,
            MouseButton::Middle => 2,
            _ => return,
        };
        if number >= self.buttons {
            return;
        }
        let bit = 1 << number;
        let pressed = state == ElementState::Pressed;
        if pressed == (self.held & bit != 0) {
            return;
        }
        self.held ^= bit;
        self.events.push(InputEvent::PointerButton {
            port,
            button: number,
            pressed,
        });
    }

    /// The window lost focus and the machine was told to release
    /// everything ([`InputEvent::ReleaseAll`]), which leaves an absolute
    /// pointer's place: the device now points nowhere.
    pub fn focus_lost(&mut self) {
        self.held = 0;
        self.moved(None);
    }

    /// The machine's device starts again from what it was told before: its
    /// state was loaded or it was switched off and on. Nothing is held, and
    /// the host pointer's last place, if one was sent, is sent again.
    pub fn restate(&mut self) {
        self.held = 0;
        if let (Some(port), Some(position)) = (self.port, self.sent) {
            self.events
                .push(InputEvent::PointerPosition { port, position });
        }
    }

    /// Input stopped reaching the machine while the host pointer moved on.
    /// Nothing is held and the next place is sent whatever it is.
    pub fn forget(&mut self) {
        self.held = 0;
        self.sent = None;
    }
}
