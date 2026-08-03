//! Host-side analog stick / touch pad filtering for semantic axes.

use std::{error::Error, fmt};

/// Radial deadzone filter for a unit-circle stick (gamepad or virtual).
///
/// The host samples raw hardware axes, runs [`Self::filter`], then injects the
/// result into playable loops (`set_external_move_axis`). This crate does not
/// open gilrs / HID — that stays an application adapter.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VirtualStick2d {
    deadzone: f32,
}

/// Invalid [`VirtualStick2d`] construction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VirtualStickError2d {
    /// Deadzone is non-finite, negative, or ≥ 1.
    InvalidDeadzone,
}

impl fmt::Display for VirtualStickError2d {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidDeadzone => {
                formatter.write_str("virtual stick 2d: deadzone must be in [0, 1)")
            }
        }
    }
}

impl Error for VirtualStickError2d {}

impl VirtualStick2d {
    /// Creates a radial deadzone filter.
    ///
    /// # Errors
    ///
    /// Returns [`VirtualStickError2d::InvalidDeadzone`] when `deadzone` is not
    /// in `[0, 1)`.
    pub fn new(deadzone: f32) -> Result<Self, VirtualStickError2d> {
        if !deadzone.is_finite() || !(0.0..1.0).contains(&deadzone) {
            return Err(VirtualStickError2d::InvalidDeadzone);
        }
        Ok(Self { deadzone })
    }

    /// Default ~15% radial deadzone (common gamepad feel).
    #[must_use]
    pub fn default_deadzone() -> Self {
        Self::new(0.15).expect("0.15 is a valid deadzone")
    }

    /// Returns the configured deadzone radius.
    #[must_use]
    pub const fn deadzone(self) -> f32 {
        self.deadzone
    }

    /// Applies radial deadzone and rescales remaining range to the unit circle.
    ///
    /// Non-finite inputs become `[0, 0]`. Magnitude above 1 is clamped.
    #[must_use]
    pub fn filter(self, raw: [f32; 2]) -> [f32; 2] {
        if !raw[0].is_finite() || !raw[1].is_finite() {
            return [0.0, 0.0];
        }
        let mag = (raw[0] * raw[0] + raw[1] * raw[1]).sqrt();
        if mag <= self.deadzone || mag == 0.0 {
            return [0.0, 0.0];
        }
        let clamped = mag.min(1.0);
        let scaled = (clamped - self.deadzone) / (1.0 - self.deadzone);
        let inv = scaled / mag;
        [raw[0] * inv, raw[1] * inv]
    }
}

impl Default for VirtualStick2d {
    fn default() -> Self {
        Self::default_deadzone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deadzone_zeros_small_deflection() {
        let stick = VirtualStick2d::new(0.2).expect("dz");
        assert_eq!(stick.filter([0.1, 0.0]), [0.0, 0.0]);
    }

    #[test]
    fn rescales_after_deadzone() {
        let stick = VirtualStick2d::new(0.2).expect("dz");
        let out = stick.filter([1.0, 0.0]);
        assert!((out[0] - 1.0).abs() < 1e-5);
        assert!(out[1].abs() < 1e-5);
    }

    #[test]
    fn rejects_invalid_deadzone() {
        assert_eq!(
            VirtualStick2d::new(1.0),
            Err(VirtualStickError2d::InvalidDeadzone)
        );
    }
}
