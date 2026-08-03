//! High-level 2D camera follow policy.
//!
//! Keeps [`Camera2d`] locked to an actor centre (plus optional offset). Bounds,
//! dead-zones, zoom, and shake stay out of this first slice so hosts can grow
//! the policy without rewriting playable glue.

use yuyib_render_2d::Camera2d;

/// Follows a world-space target (typically a sprite centre).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CameraFollow2d {
    /// Added to the target before writing the camera position.
    offset: [f32; 2],
    /// When false, [`Self::apply`] leaves the camera untouched (fixed shot).
    enabled: bool,
}

impl Default for CameraFollow2d {
    fn default() -> Self {
        Self::new()
    }
}

impl CameraFollow2d {
    /// Builds a follow policy with zero offset.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            offset: [0.0, 0.0],
            enabled: true,
        }
    }

    /// Fixed camera: playable loops will not overwrite [`Camera2d::position`].
    #[must_use]
    pub const fn disabled() -> Self {
        Self {
            offset: [0.0, 0.0],
            enabled: false,
        }
    }

    /// Sets a constant world-space offset from the follow target.
    #[must_use]
    pub const fn with_offset(mut self, offset: [f32; 2]) -> Self {
        self.offset = offset;
        self
    }

    /// Returns the authored offset.
    #[must_use]
    pub const fn offset(self) -> [f32; 2] {
        self.offset
    }

    /// Returns whether follow is active.
    #[must_use]
    pub const fn is_enabled(self) -> bool {
        self.enabled
    }

    /// Computes the camera position for `target`.
    ///
    /// Non-finite inputs fall back to the raw `target` (or `[0, 0]` if that is
    /// also non-finite) so a bad frame cannot poison the camera forever.
    #[must_use]
    pub fn camera_position(self, target: [f32; 2]) -> [f32; 2] {
        let position = [target[0] + self.offset[0], target[1] + self.offset[1]];
        if position.iter().all(|channel| channel.is_finite()) {
            position
        } else if target.iter().all(|channel| channel.is_finite()) {
            target
        } else {
            [0.0, 0.0]
        }
    }

    /// Writes [`Self::camera_position`] into `camera` when follow is enabled.
    pub fn apply(self, camera: &mut Camera2d, target: [f32; 2]) {
        if !self.enabled {
            return;
        }
        camera.position = self.camera_position(target);
    }
}

#[cfg(test)]
mod tests {
    use super::CameraFollow2d;
    use yuyib_render_2d::Camera2d;

    #[test]
    fn follow_applies_offset() {
        let mut camera = Camera2d::default();
        CameraFollow2d::new()
            .with_offset([10.0, -5.0])
            .apply(&mut camera, [100.0, 200.0]);
        assert_eq!(camera.position, [110.0, 195.0]);
    }

    #[test]
    fn disabled_follow_leaves_camera() {
        let mut camera = Camera2d::new([1.0, 2.0], 1.0);
        CameraFollow2d::disabled().apply(&mut camera, [99.0, 99.0]);
        assert_eq!(camera.position, [1.0, 2.0]);
    }
}
