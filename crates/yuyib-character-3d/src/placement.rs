//! High-level playermodel root placement for animated characters.
//!
//! Games usually need feet on the ground, a horizontal facing direction and a
//! uniform scale — not a hand-written column-major matrix in every example.

use std::{error::Error, fmt};

use yuyib_physics::{Vec2, Vec3};

use crate::CharacterController3d;

/// Validated feet / facing / scale used to build a model-to-world root matrix.
///
/// Convention: the source model's local **+Z** axis faces `facing_xz` on the
/// horizontal plane (`facing_xz.x` → world X, `facing_xz.y` → world Z). Local
/// **+Y** stays world up. Translation is the model root at the character feet.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CharacterModelPlacement3d {
    feet: Vec3,
    facing_xz: Vec2,
    uniform_scale: f32,
}

impl CharacterModelPlacement3d {
    /// Creates a placement from explicit feet, horizontal facing and scale.
    ///
    /// `facing_xz` is normalized when longer than zero. A near-zero facing
    /// vector is rejected so the model cannot collapse to a zero basis.
    ///
    /// # Errors
    ///
    /// Returns [`CharacterModelPlacementError3d`] for non-finite inputs, a
    /// non-positive scale, or a zero/non-finite facing direction.
    pub fn new(
        feet: Vec3,
        facing_xz: Vec2,
        uniform_scale: f32,
    ) -> Result<Self, CharacterModelPlacementError3d> {
        validate_vec3(feet, "feet")?;
        let facing_xz = normalize_facing_xz(facing_xz)?;
        validate_positive_scale(uniform_scale)?;
        Ok(Self {
            feet,
            facing_xz,
            uniform_scale,
        })
    }

    /// Places the model at the controller feet using the capsule centre and radius.
    ///
    /// # Errors
    ///
    /// Same failures as [`Self::new`].
    pub fn from_controller(
        controller: CharacterController3d,
        facing_xz: Vec2,
        uniform_scale: f32,
    ) -> Result<Self, CharacterModelPlacementError3d> {
        Self::new(controller.feet_position(), facing_xz, uniform_scale)
    }

    /// World-space feet position (model root translation).
    #[must_use]
    pub const fn feet(self) -> Vec3 {
        self.feet
    }

    /// Unit horizontal facing (`x` → world X, `y` → world Z).
    #[must_use]
    pub const fn facing_xz(self) -> Vec2 {
        self.facing_xz
    }

    /// Uniform model scale applied on all axes.
    #[must_use]
    pub const fn uniform_scale(self) -> f32 {
        self.uniform_scale
    }

    /// Replaces feet while keeping facing and scale.
    ///
    /// # Errors
    ///
    /// Rejects non-finite feet coordinates.
    pub fn with_feet(mut self, feet: Vec3) -> Result<Self, CharacterModelPlacementError3d> {
        validate_vec3(feet, "feet")?;
        self.feet = feet;
        Ok(self)
    }

    /// Replaces horizontal facing while keeping feet and scale.
    ///
    /// # Errors
    ///
    /// Rejects a zero or non-finite facing vector.
    pub fn with_facing_xz(
        mut self,
        facing_xz: Vec2,
    ) -> Result<Self, CharacterModelPlacementError3d> {
        self.facing_xz = normalize_facing_xz(facing_xz)?;
        Ok(self)
    }

    /// Replaces uniform scale while keeping feet and facing.
    ///
    /// # Errors
    ///
    /// Rejects a non-positive or non-finite scale.
    pub fn with_uniform_scale(
        mut self,
        uniform_scale: f32,
    ) -> Result<Self, CharacterModelPlacementError3d> {
        validate_positive_scale(uniform_scale)?;
        self.uniform_scale = uniform_scale;
        Ok(self)
    }

    /// Yaw around world +Y that maps model +Z onto [`Self::facing_xz`].
    #[must_use]
    pub fn yaw_radians(self) -> f32 {
        self.facing_xz.x.atan2(self.facing_xz.y)
    }

    /// Column-major model-to-world matrix for skeletal / static draws.
    #[must_use]
    pub fn model_to_world(self) -> [f32; 16] {
        let scale = self.uniform_scale;
        let facing_x = self.facing_xz.x;
        let facing_z = self.facing_xz.y;
        [
            facing_z * scale,
            0.0,
            -facing_x * scale,
            0.0,
            0.0,
            scale,
            0.0,
            0.0,
            facing_x * scale,
            0.0,
            facing_z * scale,
            0.0,
            self.feet.x,
            self.feet.y,
            self.feet.z,
            1.0,
        ]
    }
}

impl CharacterController3d {
    /// World-space feet point under the capsule centre.
    #[must_use]
    pub fn feet_position(self) -> Vec3 {
        let centre = self.position();
        Vec3::new(centre.x, centre.y - self.config().radius, centre.z)
    }

    /// Builds a playermodel placement at the controller feet.
    ///
    /// # Errors
    ///
    /// Forwards [`CharacterModelPlacement3d::from_controller`] validation.
    pub fn model_placement(
        self,
        facing_xz: Vec2,
        uniform_scale: f32,
    ) -> Result<CharacterModelPlacement3d, CharacterModelPlacementError3d> {
        CharacterModelPlacement3d::from_controller(self, facing_xz, uniform_scale)
    }
}

/// Failure while constructing a [`CharacterModelPlacement3d`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CharacterModelPlacementError3d {
    /// A named field was NaN or infinite.
    NonFinite {
        /// Invalid field name.
        field: &'static str,
    },
    /// Uniform scale was zero or negative.
    NonPositiveScale,
    /// Horizontal facing had zero length or was non-finite.
    InvalidFacing,
}

impl fmt::Display for CharacterModelPlacementError3d {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NonFinite { field } => {
                write!(
                    formatter,
                    "character model placement `{field}` must be finite"
                )
            }
            Self::NonPositiveScale => {
                formatter.write_str("character model uniform scale must be finite and positive")
            }
            Self::InvalidFacing => formatter.write_str(
                "character model facing_xz must be a finite non-zero horizontal direction",
            ),
        }
    }
}

impl Error for CharacterModelPlacementError3d {}

fn validate_vec3(value: Vec3, field: &'static str) -> Result<(), CharacterModelPlacementError3d> {
    if !value.x.is_finite() || !value.y.is_finite() || !value.z.is_finite() {
        return Err(CharacterModelPlacementError3d::NonFinite { field });
    }
    Ok(())
}

fn validate_positive_scale(scale: f32) -> Result<(), CharacterModelPlacementError3d> {
    if !scale.is_finite() || scale <= 0.0 {
        return Err(CharacterModelPlacementError3d::NonPositiveScale);
    }
    Ok(())
}

fn normalize_facing_xz(facing_xz: Vec2) -> Result<Vec2, CharacterModelPlacementError3d> {
    if !facing_xz.x.is_finite() || !facing_xz.y.is_finite() {
        return Err(CharacterModelPlacementError3d::InvalidFacing);
    }
    let length = facing_xz.length_squared().sqrt();
    if !length.is_finite() || length <= f32::EPSILON {
        return Err(CharacterModelPlacementError3d::InvalidFacing);
    }
    Ok(facing_xz * length.recip())
}

#[cfg(test)]
mod tests {
    use super::{CharacterModelPlacement3d, CharacterModelPlacementError3d};
    use crate::{CharacterController3d, CharacterControllerConfig3d};
    use yuyib_physics::{Vec2, Vec3};

    #[test]
    fn placement_builds_scaled_facing_matrix_at_feet() {
        let placement =
            CharacterModelPlacement3d::new(Vec3::new(1.0, 2.0, 3.0), Vec2::new(0.0, -1.0), 0.5)
                .expect("valid placement");
        let matrix = placement.model_to_world();
        assert_eq!(&matrix[12..15], &[1.0, 2.0, 3.0]);
        assert!((matrix[0] - -0.5).abs() < 1e-5);
        assert!((matrix[5] - 0.5).abs() < 1e-5);
        assert!((matrix[10] - -0.5).abs() < 1e-5);
        assert!((matrix[8]).abs() < 1e-5);
        assert!((matrix[2]).abs() < 1e-5);
    }

    #[test]
    fn controller_feet_and_placement_match_capsule_radius() {
        let controller = CharacterController3d::new(
            CharacterControllerConfig3d {
                radius: 0.28,
                ..CharacterControllerConfig3d::default()
            },
            Vec3::new(4.0, 1.28, -2.0),
        )
        .expect("valid controller");
        let feet = controller.feet_position();
        assert!((feet.y - 1.0).abs() < 1e-5);
        let matrix = controller
            .model_placement(Vec2::new(1.0, 0.0), 0.3)
            .expect("valid placement")
            .model_to_world();
        assert_eq!(&matrix[12..15], &[feet.x, feet.y, feet.z]);
        assert!((matrix[8] - 0.3).abs() < 1e-5);
        assert!((matrix[10]).abs() < 1e-5);
    }

    #[test]
    fn rejects_zero_scale_and_zero_facing() {
        assert_eq!(
            CharacterModelPlacement3d::new(Vec3::ZERO, Vec2::new(0.0, 1.0), 0.0),
            Err(CharacterModelPlacementError3d::NonPositiveScale)
        );
        assert_eq!(
            CharacterModelPlacement3d::new(Vec3::ZERO, Vec2::ZERO, 1.0),
            Err(CharacterModelPlacementError3d::InvalidFacing)
        );
    }

    #[test]
    fn with_uniform_scale_is_the_knob_games_want() {
        let placement = CharacterModelPlacement3d::new(Vec3::ZERO, Vec2::new(0.0, 1.0), 1.0)
            .expect("valid")
            .with_uniform_scale(0.3)
            .expect("scale");
        assert!((placement.uniform_scale() - 0.3).abs() < 1e-6);
        assert!((placement.model_to_world()[5] - 0.3).abs() < 1e-6);
    }
}
