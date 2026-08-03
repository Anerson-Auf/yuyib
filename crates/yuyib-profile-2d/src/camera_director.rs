//! Multi-camera cuts / blends on top of [`CameraFollow2d`].
//!
//! [`CameraDirector2d`] owns one follow policy + cinematic runtime, and can
//! run a timed linear blend between two camera centres (and optional zoom)
//! for hard cuts / soft transitions without a second render camera.

use yuyib_render_2d::Camera2d;

use super::camera_follow::{CameraFollow2d, CameraFollowRuntime2d};

/// One timed blend between two camera poses.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CameraCut2d {
    from_position: [f32; 2],
    to_position: [f32; 2],
    from_zoom: f32,
    to_zoom: f32,
    duration: f32,
    elapsed: f32,
}

impl CameraCut2d {
    /// Starts a blend lasting `duration_seconds` (> 0).
    ///
    /// Non-finite inputs or non-positive duration yield a finished cut that
    /// snaps to `to_*` immediately.
    #[must_use]
    pub fn new(
        from_position: [f32; 2],
        to_position: [f32; 2],
        from_zoom: f32,
        to_zoom: f32,
        duration_seconds: f32,
    ) -> Self {
        let valid = from_position.iter().chain(to_position.iter()).all(|v| v.is_finite())
            && from_zoom.is_finite()
            && to_zoom.is_finite()
            && from_zoom > 0.0
            && to_zoom > 0.0
            && duration_seconds.is_finite()
            && duration_seconds > 0.0;
        if valid {
            Self {
                from_position,
                to_position,
                from_zoom,
                to_zoom,
                duration: duration_seconds,
                elapsed: 0.0,
            }
        } else {
            Self {
                from_position: to_position,
                to_position,
                from_zoom: to_zoom.max(1.0),
                to_zoom: to_zoom.max(1.0),
                duration: 0.0,
                elapsed: 0.0,
            }
        }
    }

    /// Instant hard cut (duration 0).
    #[must_use]
    pub fn hard(to_position: [f32; 2], to_zoom: f32) -> Self {
        Self::new(to_position, to_position, to_zoom, to_zoom, 0.0)
    }

    /// Progress in `0..=1`.
    #[must_use]
    pub fn progress(self) -> f32 {
        if self.duration <= 0.0 {
            return 1.0;
        }
        (self.elapsed / self.duration).clamp(0.0, 1.0)
    }

    /// Whether the blend has reached the end pose.
    #[must_use]
    pub fn is_finished(self) -> bool {
        self.progress() >= 1.0
    }

    /// Advances the cut; returns the blended pose.
    pub fn tick(&mut self, dt_seconds: f32) -> ([f32; 2], f32) {
        if self.duration <= 0.0 {
            return (self.to_position, self.to_zoom);
        }
        let dt = if dt_seconds.is_finite() && dt_seconds > 0.0 {
            dt_seconds
        } else {
            0.0
        };
        self.elapsed = (self.elapsed + dt).min(self.duration);
        let t = self.progress();
        // Smoothstep ease-in-out for cinematic feel without a curve table.
        let eased = t * t * (3.0 - 2.0 * t);
        let position = [
            self.from_position[0] + (self.to_position[0] - self.from_position[0]) * eased,
            self.from_position[1] + (self.to_position[1] - self.from_position[1]) * eased,
        ];
        let zoom = self.from_zoom + (self.to_zoom - self.from_zoom) * eased;
        (position, zoom)
    }
}

/// Follow policy + optional active cut / blend.
#[derive(Clone, Debug, PartialEq)]
pub struct CameraDirector2d {
    follow: CameraFollow2d,
    runtime: CameraFollowRuntime2d,
    cut: Option<CameraCut2d>,
}

impl CameraDirector2d {
    /// Creates a director with the given follow policy.
    #[must_use]
    pub const fn new(follow: CameraFollow2d) -> Self {
        Self {
            follow,
            runtime: CameraFollowRuntime2d::new(),
            cut: None,
        }
    }

    /// Returns the follow policy.
    #[must_use]
    pub const fn follow(&self) -> CameraFollow2d {
        self.follow
    }

    /// Mutable follow (shake trauma, pan, zoom knobs).
    pub fn follow_mut(&mut self) -> &mut CameraFollow2d {
        &mut self.follow
    }

    /// Replaces follow and clears cinematic runtime + cut.
    pub fn set_follow(&mut self, follow: CameraFollow2d) {
        self.follow = follow;
        self.runtime.reset();
        self.cut = None;
    }

    /// Returns whether a cut is in progress.
    #[must_use]
    pub const fn is_cutting(&self) -> bool {
        self.cut.is_some()
    }

    /// Starts a blend; replaces any active cut.
    pub fn begin_cut(&mut self, cut: CameraCut2d) {
        self.cut = Some(cut);
    }

    /// Soft blend from the current camera pose toward `to_*`.
    pub fn cut_to(
        &mut self,
        camera: &Camera2d,
        to_position: [f32; 2],
        to_zoom: f32,
        duration_seconds: f32,
    ) {
        let from_zoom = if camera.pixels_per_unit.is_finite() && camera.pixels_per_unit > 0.0 {
            // Prefer follow base×zoom when authored; else current PPU as "zoom 1".
            match self.follow.base_pixels_per_unit() {
                Some(base) if base > 0.0 => camera.pixels_per_unit / base,
                _ => 1.0,
            }
        } else {
            1.0
        };
        self.begin_cut(CameraCut2d::new(
            camera.position,
            to_position,
            from_zoom,
            to_zoom,
            duration_seconds,
        ));
    }

    /// Cancels an active cut (next apply resumes follow).
    pub fn cancel_cut(&mut self) {
        self.cut = None;
    }

    /// Advances shake / cut and writes the camera.
    ///
    /// While a cut is active, follow is suspended and the blended pose is
    /// written directly. When the cut finishes, follow resumes from a hard
    /// snap at the cut end (runtime reset).
    pub fn apply(
        &mut self,
        camera: &mut Camera2d,
        target: [f32; 2],
        velocity: [f32; 2],
        dt_seconds: f32,
        surface_size: Option<[u32; 2]>,
    ) {
        let _ = self.follow.tick(dt_seconds);
        if let Some(cut) = self.cut.as_mut() {
            let (position, zoom) = cut.tick(dt_seconds);
            if let Some(base) = self.follow.base_pixels_per_unit() {
                if base.is_finite() && base > 0.0 && zoom.is_finite() && zoom > 0.0 {
                    camera.pixels_per_unit = base * zoom;
                }
            }
            camera.position = position;
            if cut.is_finished() {
                self.cut = None;
                self.runtime.reset();
                // Snap follow state to the landed pose so smoothing does not yank back.
                let _ = self.runtime.smooth_toward(position, 0.0, 0.0);
            }
            return;
        }
        self.follow.apply_cinematic(
            &mut self.runtime,
            camera,
            target,
            velocity,
            dt_seconds,
            surface_size,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{CameraCut2d, CameraDirector2d};
    use crate::camera_follow::CameraFollow2d;
    use yuyib_render_2d::Camera2d;

    #[test]
    fn cut_blends_toward_target() {
        let mut cut = CameraCut2d::new([0.0, 0.0], [100.0, 0.0], 1.0, 1.0, 1.0);
        let (mid, _) = cut.tick(0.5);
        assert!(mid[0] > 40.0 && mid[0] < 60.0);
        let (end, _) = cut.tick(1.0);
        assert!((end[0] - 100.0).abs() < 1e-3);
        assert!(cut.is_finished());
    }

    #[test]
    fn director_cut_overrides_follow() {
        let mut director = CameraDirector2d::new(CameraFollow2d::new());
        let mut camera = Camera2d::new([0.0, 0.0], 1.0);
        director.cut_to(&camera, [50.0, 0.0], 1.0, 0.2);
        director.apply(&mut camera, [999.0, 999.0], [0.0, 0.0], 0.1, None);
        assert!(camera.position[0] > 0.0 && camera.position[0] < 50.0);
        assert!(director.is_cutting());
        director.apply(&mut camera, [999.0, 999.0], [0.0, 0.0], 0.2, None);
        assert!(!director.is_cutting());
        assert!((camera.position[0] - 50.0).abs() < 1e-3);
    }
}
