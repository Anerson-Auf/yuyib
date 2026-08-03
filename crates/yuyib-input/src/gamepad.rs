//! Optional gilrs HID adapter for 2D virtual sticks.
//!
//! Enable with crate feature `gamepad`. Host pumps events each frame, then
//! injects [`Self::left_stick_axis`] into playable loops via
//! `set_external_move_axis`. Y is flipped to match Y-down sprite space.

use std::{error::Error, fmt};

use gilrs::{Axis, Button, EventType, GamepadId, Gilrs};

use crate::VirtualStick2d;

/// Polls gilrs and exposes a filtered left-stick axis for 2D movement.
pub struct GilrsGamepadAdapter2d {
    gilrs: Gilrs,
    stick: VirtualStick2d,
    active: Option<GamepadId>,
    raw: [f32; 2],
    jump_pressed: bool,
}

/// Failure constructing the gilrs adapter.
#[derive(Debug)]
pub enum GilrsGamepadError2d {
    /// gilrs backend failed to start.
    Init(String),
}

impl fmt::Display for GilrsGamepadError2d {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Init(message) => write!(formatter, "gilrs gamepad: {message}"),
        }
    }
}

impl Error for GilrsGamepadError2d {}

impl GilrsGamepadAdapter2d {
    /// Opens gilrs with the given stick filter.
    ///
    /// # Errors
    ///
    /// Returns gilrs init failures.
    pub fn new(stick: VirtualStick2d) -> Result<Self, GilrsGamepadError2d> {
        let gilrs = Gilrs::new().map_err(|error| GilrsGamepadError2d::Init(error.to_string()))?;
        Ok(Self {
            gilrs,
            stick,
            active: None,
            raw: [0.0, 0.0],
            jump_pressed: false,
        })
    }

    /// Opens gilrs with the default ~15% deadzone.
    ///
    /// # Errors
    ///
    /// Returns gilrs init failures.
    pub fn with_default_stick() -> Result<Self, GilrsGamepadError2d> {
        Self::new(VirtualStick2d::default())
    }

    /// Drains gilrs events and updates stick / jump edge state.
    pub fn pump(&mut self) {
        while let Some(event) = self.gilrs.next_event() {
            if self.active.is_none() {
                self.active = Some(event.id);
            }
            if self.active != Some(event.id) {
                continue;
            }
            match event.event {
                EventType::AxisChanged(Axis::LeftStickX, value, _) => {
                    self.raw[0] = value;
                }
                EventType::AxisChanged(Axis::LeftStickY, value, _) => {
                    // gilrs Y-up → sprite Y-down
                    self.raw[1] = -value;
                }
                EventType::ButtonPressed(Button::South, _) => {
                    self.jump_pressed = true;
                }
                EventType::Disconnected => {
                    if self.active == Some(event.id) {
                        self.active = None;
                        self.raw = [0.0, 0.0];
                    }
                }
                _ => {}
            }
        }
    }

    /// Filtered left-stick axis in sprite right/down space, typically `[-1, 1]`.
    #[must_use]
    pub fn left_stick_axis(&self) -> [f32; 2] {
        self.stick.filter(self.raw)
    }

    /// Consumes a South-button press edge (A / Cross) for jump.
    pub fn take_jump_pressed(&mut self) -> bool {
        let pressed = self.jump_pressed;
        self.jump_pressed = false;
        pressed
    }

    /// Returns whether any gamepad is currently tracked.
    #[must_use]
    pub const fn has_gamepad(&self) -> bool {
        self.active.is_some()
    }
}
