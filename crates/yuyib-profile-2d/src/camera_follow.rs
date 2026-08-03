//! High-level 2D camera follow policy.
//!
//! Keeps [`Camera2d`] locked to an actor centre (plus optional offset / pan),
//! with optional zoom (via base PPU × zoom), trauma shake, and
//! [`WorldBounds2d`] clamp so the visible viewport stays inside a map AABB
//! when the host supplies surface size (via
//! [`CameraFollow2d::apply_with_surface`]).

use yuyib_render_2d::Camera2d;

/// Inclusive world-space AABB used to clamp the camera viewport.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WorldBounds2d {
    min: [f32; 2],
    max: [f32; 2],
}

impl WorldBounds2d {
    /// Creates bounds from min/max corners (both finite, `max >= min` per axis).
    ///
    /// # Errors
    ///
    /// Returns [`WorldBoundsError2d`] when any channel is non-finite or inverted.
    pub fn new(min: [f32; 2], max: [f32; 2]) -> Result<Self, WorldBoundsError2d> {
        if !min.iter().chain(max.iter()).all(|value| value.is_finite()) {
            return Err(WorldBoundsError2d::NonFinite);
        }
        if max[0] < min[0] || max[1] < min[1] {
            return Err(WorldBoundsError2d::Inverted);
        }
        Ok(Self { min, max })
    }

    /// Bounds from top-left origin + size (both finite, size ≥ 0).
    ///
    /// # Errors
    ///
    /// Forwards [`Self::new`] validation.
    pub fn from_origin_size(
        origin: [f32; 2],
        size: [f32; 2],
    ) -> Result<Self, WorldBoundsError2d> {
        if !size.iter().all(|value| value.is_finite() && *value >= 0.0) {
            return Err(WorldBoundsError2d::NonFinite);
        }
        Self::new(origin, [origin[0] + size[0], origin[1] + size[1]])
    }

    /// Minimum corner.
    #[must_use]
    pub const fn min(self) -> [f32; 2] {
        self.min
    }

    /// Maximum corner.
    #[must_use]
    pub const fn max(self) -> [f32; 2] {
        self.max
    }

    /// Clamps a camera centre so the axis-aligned viewport stays inside bounds.
    ///
    /// `half_extents` is half the visible world size (from surface / PPU). When
    /// the viewport is larger than the bounds on an axis, the camera is centred
    /// on that axis of the bounds.
    #[must_use]
    pub fn clamp_camera_center(self, center: [f32; 2], half_extents: [f32; 2]) -> [f32; 2] {
        let half = [
            half_extents[0].abs().max(0.0),
            half_extents[1].abs().max(0.0),
        ];
        [
            clamp_axis(center[0], self.min[0], self.max[0], half[0]),
            clamp_axis(center[1], self.min[1], self.max[1], half[1]),
        ]
    }
}

fn clamp_axis(center: f32, min: f32, max: f32, half: f32) -> f32 {
    let span = max - min;
    if span <= half * 2.0 {
        return min + span * 0.5;
    }
    center.clamp(min + half, max - half)
}

/// Invalid [`WorldBounds2d`] construction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorldBoundsError2d {
    /// A coordinate was non-finite (or size was negative).
    NonFinite,
    /// `max` was less than `min` on an axis.
    Inverted,
}

impl std::fmt::Display for WorldBoundsError2d {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NonFinite => formatter.write_str("world bounds require finite coordinates"),
            Self::Inverted => formatter.write_str("world bounds max must be >= min"),
        }
    }
}

impl std::error::Error for WorldBoundsError2d {}

/// Trauma-based screen shake (squared falloff, deterministic sin/cos).
///
/// Hosts call [`Self::add_trauma`] on hits/explosions and [`Self::tick`] each
/// frame; the resulting offset is applied by [`CameraFollow2d`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CameraShake2d {
    trauma: f32,
    max_offset: f32,
    frequency: f32,
    decay_per_second: f32,
    time: f32,
}

impl Default for CameraShake2d {
    fn default() -> Self {
        Self::new()
    }
}

impl CameraShake2d {
    /// Default shake: 8 wu peak, ~18 Hz, trauma decays at 1.5 / s.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            trauma: 0.0,
            max_offset: 8.0,
            frequency: 18.0,
            decay_per_second: 1.5,
            time: 0.0,
        }
    }

    /// Sets peak world-unit offset at trauma = 1.
    #[must_use]
    pub const fn with_max_offset(mut self, max_offset: f32) -> Self {
        self.max_offset = max_offset;
        self
    }

    /// Sets oscillation frequency in Hz.
    #[must_use]
    pub const fn with_frequency(mut self, frequency: f32) -> Self {
        self.frequency = frequency;
        self
    }

    /// Sets how fast trauma decays per second.
    #[must_use]
    pub const fn with_decay_per_second(mut self, decay_per_second: f32) -> Self {
        self.decay_per_second = decay_per_second;
        self
    }

    /// Current trauma in `0..=1`.
    #[must_use]
    pub const fn trauma(self) -> f32 {
        self.trauma
    }

    /// Adds trauma (clamped to `0..=1`). Non-finite / negative amounts are ignored.
    pub fn add_trauma(&mut self, amount: f32) {
        if !amount.is_finite() || amount <= 0.0 {
            return;
        }
        self.trauma = (self.trauma + amount).clamp(0.0, 1.0);
    }

    /// Clears trauma and phase.
    pub fn clear(&mut self) {
        self.trauma = 0.0;
        self.time = 0.0;
    }

    /// Advances phase, decays trauma, returns the current world-space offset.
    #[must_use]
    pub fn tick(&mut self, dt_seconds: f32) -> [f32; 2] {
        let dt = if dt_seconds.is_finite() && dt_seconds > 0.0 {
            dt_seconds
        } else {
            0.0
        };
        if dt > 0.0 {
            self.time = self.time.wrapping_add_finite(dt);
            let decay = if self.decay_per_second.is_finite() && self.decay_per_second > 0.0 {
                self.decay_per_second
            } else {
                0.0
            };
            self.trauma = (self.trauma - decay * dt).clamp(0.0, 1.0);
        }
        self.offset()
    }

    /// Offset for the current trauma / phase without advancing time.
    #[must_use]
    pub fn offset(self) -> [f32; 2] {
        if self.trauma <= 0.0
            || !self.trauma.is_finite()
            || !self.max_offset.is_finite()
            || !self.frequency.is_finite()
        {
            return [0.0, 0.0];
        }
        let magnitude = self.trauma * self.trauma * self.max_offset.abs();
        let angle = self.time * self.frequency * std::f32::consts::TAU;
        // Cheap deterministic “noise”: two phase-shifted sinusoids.
        [
            angle.sin() * magnitude,
            (angle * 1.273_239_5 + 1.0).cos() * magnitude,
        ]
    }
}

trait WrappingAddFinite {
    fn wrapping_add_finite(self, dt: f32) -> f32;
}

impl WrappingAddFinite for f32 {
    fn wrapping_add_finite(self, dt: f32) -> f32 {
        let next = self + dt;
        if next.is_finite() {
            next
        } else {
            0.0
        }
    }
}

/// Follows a world-space target (typically a sprite centre).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CameraFollow2d {
    /// Added to the target before writing the camera position.
    offset: [f32; 2],
    /// Extra look / drag offset applied after follow, before shake.
    pan: [f32; 2],
    /// When false, apply helpers leave the camera untouched (fixed shot).
    enabled: bool,
    /// Optional map AABB used when applying with a known surface size.
    bounds: Option<WorldBounds2d>,
    /// Base PPU rewritten every apply as `base * zoom`. `None` = leave camera PPU.
    base_pixels_per_unit: Option<f32>,
    /// Zoom multiplier (`1.0` = base). Must stay finite and `> 0` when applied.
    zoom: f32,
    /// Active trauma shake.
    shake: CameraShake2d,
    /// Exponential smooth-time in seconds (`0` = snap to ideal each frame).
    smoothing: f32,
    /// Multiplies target velocity for look-ahead (`0` = none).
    look_ahead_scale: f32,
}

impl Default for CameraFollow2d {
    fn default() -> Self {
        Self::new()
    }
}

impl CameraFollow2d {
    /// Builds a follow policy with zero offset/pan, zoom `1.0`, and no bounds.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            offset: [0.0, 0.0],
            pan: [0.0, 0.0],
            enabled: true,
            bounds: None,
            base_pixels_per_unit: None,
            zoom: 1.0,
            shake: CameraShake2d::new(),
            smoothing: 0.0,
            look_ahead_scale: 0.0,
        }
    }

    /// Fixed camera: playable loops will not overwrite [`Camera2d::position`].
    #[must_use]
    pub const fn disabled() -> Self {
        Self {
            offset: [0.0, 0.0],
            pan: [0.0, 0.0],
            enabled: false,
            bounds: None,
            base_pixels_per_unit: None,
            zoom: 1.0,
            shake: CameraShake2d::new(),
            smoothing: 0.0,
            look_ahead_scale: 0.0,
        }
    }

    /// Sets a constant world-space offset from the follow target.
    #[must_use]
    pub const fn with_offset(mut self, offset: [f32; 2]) -> Self {
        self.offset = offset;
        self
    }

    /// Sets a manual pan / look offset (added after follow offset).
    #[must_use]
    pub const fn with_pan(mut self, pan: [f32; 2]) -> Self {
        self.pan = pan;
        self
    }

    /// Sets zoom multiplier (`1.0` = no change vs base PPU).
    #[must_use]
    pub const fn with_zoom(mut self, zoom: f32) -> Self {
        self.zoom = zoom;
        self
    }

    /// Sets the base pixels-per-unit rewritten on every apply (`base * zoom`).
    #[must_use]
    pub const fn with_base_pixels_per_unit(mut self, pixels_per_unit: f32) -> Self {
        self.base_pixels_per_unit = Some(pixels_per_unit);
        self
    }

    /// Clears an authored base PPU (apply leaves [`Camera2d::pixels_per_unit`]).
    #[must_use]
    pub const fn without_base_pixels_per_unit(mut self) -> Self {
        self.base_pixels_per_unit = None;
        self
    }

    /// Replaces the shake policy (trauma starts at zero unless copied in).
    #[must_use]
    pub const fn with_shake(mut self, shake: CameraShake2d) -> Self {
        self.shake = shake;
        self
    }

    /// Sets exponential smooth-time in seconds (`0` = hard lock / snap).
    #[must_use]
    pub const fn with_smoothing(mut self, smooth_time_seconds: f32) -> Self {
        self.smoothing = smooth_time_seconds;
        self
    }

    /// Sets look-ahead as a scale on target velocity (world units per wu/s).
    #[must_use]
    pub const fn with_look_ahead_scale(mut self, scale: f32) -> Self {
        self.look_ahead_scale = scale;
        self
    }

    /// Clamps the visible viewport inside `bounds` when surface size is known.
    #[must_use]
    pub const fn with_bounds(mut self, bounds: WorldBounds2d) -> Self {
        self.bounds = Some(bounds);
        self
    }

    /// Clears any authored world bounds.
    #[must_use]
    pub const fn without_bounds(mut self) -> Self {
        self.bounds = None;
        self
    }

    /// Returns the authored offset.
    #[must_use]
    pub const fn offset(self) -> [f32; 2] {
        self.offset
    }

    /// Returns the manual pan offset.
    #[must_use]
    pub const fn pan(self) -> [f32; 2] {
        self.pan
    }

    /// Sets pan in place (for frame-to-frame look / drag).
    pub fn set_pan(&mut self, pan: [f32; 2]) {
        self.pan = pan;
    }

    /// Returns the zoom multiplier.
    #[must_use]
    pub const fn zoom(self) -> f32 {
        self.zoom
    }

    /// Sets zoom in place.
    pub fn set_zoom(&mut self, zoom: f32) {
        self.zoom = zoom;
    }

    /// Returns the optional base PPU.
    #[must_use]
    pub const fn base_pixels_per_unit(self) -> Option<f32> {
        self.base_pixels_per_unit
    }

    /// Returns whether follow is active.
    #[must_use]
    pub const fn is_enabled(self) -> bool {
        self.enabled
    }

    /// Returns the optional world bounds.
    #[must_use]
    pub const fn bounds(self) -> Option<WorldBounds2d> {
        self.bounds
    }

    /// Mutable access to shake state (trauma impulses).
    pub fn shake_mut(&mut self) -> &mut CameraShake2d {
        &mut self.shake
    }

    /// Returns a copy of the shake state.
    #[must_use]
    pub const fn shake(self) -> CameraShake2d {
        self.shake
    }

    /// Returns the smooth-time in seconds (`0` = snap).
    #[must_use]
    pub const fn smoothing(self) -> f32 {
        self.smoothing
    }

    /// Returns the look-ahead velocity scale.
    #[must_use]
    pub const fn look_ahead_scale(self) -> f32 {
        self.look_ahead_scale
    }

    /// Advances shake and returns the current shake offset.
    pub fn tick(&mut self, dt_seconds: f32) -> [f32; 2] {
        self.shake.tick(dt_seconds)
    }

    /// Effective PPU written to the camera, if a base was authored.
    #[must_use]
    pub fn effective_pixels_per_unit(self) -> Option<f32> {
        let base = self.base_pixels_per_unit?;
        if !base.is_finite() || base <= 0.0 || !self.zoom.is_finite() || self.zoom <= 0.0 {
            return None;
        }
        let value = base * self.zoom;
        value.is_finite().then_some(value).filter(|ppu| *ppu > 0.0)
    }

    /// Computes the unclamped camera position for `target` (offset + pan + shake).
    ///
    /// Non-finite inputs fall back to the raw `target` (or `[0, 0]` if that is
    /// also non-finite) so a bad frame cannot poison the camera forever.
    #[must_use]
    pub fn camera_position(self, target: [f32; 2]) -> [f32; 2] {
        self.ideal_position(target, [0.0, 0.0])
    }

    /// Ideal follow point before smoothing: target + offset + pan + look-ahead + shake.
    #[must_use]
    pub fn ideal_position(self, target: [f32; 2], velocity: [f32; 2]) -> [f32; 2] {
        let shake = self.shake.offset();
        let look = if self.look_ahead_scale.is_finite() {
            [
                velocity[0] * self.look_ahead_scale,
                velocity[1] * self.look_ahead_scale,
            ]
        } else {
            [0.0, 0.0]
        };
        let position = [
            target[0] + self.offset[0] + self.pan[0] + look[0] + shake[0],
            target[1] + self.offset[1] + self.pan[1] + look[1] + shake[1],
        ];
        if position.iter().all(|channel| channel.is_finite()) {
            position
        } else if target.iter().all(|channel| channel.is_finite()) {
            target
        } else {
            [0.0, 0.0]
        }
    }

    /// Writes follow position (and optional zoom) without viewport clamping.
    pub fn apply(self, camera: &mut Camera2d, target: [f32; 2]) {
        if !self.enabled {
            return;
        }
        self.write_zoom(camera);
        camera.position = self.camera_position(target);
    }

    /// Follows `target` and clamps so the camera viewport stays in bounds.
    ///
    /// When `bounds` is unset, or `surface_size` / PPU cannot form a viewport,
    /// this falls back to [`Self::apply`]. Shake is applied before clamp so the
    /// viewport cannot be shaken outside world bounds.
    #[allow(clippy::cast_precision_loss)] // Surface pixel counts stay well inside f32.
    pub fn apply_with_surface(
        self,
        camera: &mut Camera2d,
        target: [f32; 2],
        surface_size: [u32; 2],
    ) {
        self.apply_cinematic(
            &mut CameraFollowRuntime2d::new(),
            camera,
            target,
            [0.0, 0.0],
            0.0,
            Some(surface_size),
        );
    }

    /// Cinematic apply: look-ahead + exponential smoothing + optional bounds.
    ///
    /// `runtime` stores the smoothed centre across frames. Pass `dt_seconds = 0`
    /// (or `smoothing = 0`) to snap. Shake is part of the ideal point and is
    /// re-clamped so trauma cannot push the viewport outside world bounds.
    #[allow(clippy::cast_precision_loss)]
    pub fn apply_cinematic(
        self,
        runtime: &mut CameraFollowRuntime2d,
        camera: &mut Camera2d,
        target: [f32; 2],
        velocity: [f32; 2],
        dt_seconds: f32,
        surface_size: Option<[u32; 2]>,
    ) {
        if !self.enabled {
            return;
        }
        self.write_zoom(camera);
        let mut ideal = self.ideal_position(target, velocity);
        if let (Some(bounds), Some(surface)) = (self.bounds, surface_size) {
            if surface[0] > 0
                && surface[1] > 0
                && camera.pixels_per_unit.is_finite()
                && camera.pixels_per_unit > 0.0
            {
                let half = [
                    surface[0] as f32 / camera.pixels_per_unit * 0.5,
                    surface[1] as f32 / camera.pixels_per_unit * 0.5,
                ];
                ideal = bounds.clamp_camera_center(ideal, half);
            }
        }
        let position = runtime.smooth_toward(ideal, self.smoothing, dt_seconds);
        camera.position = position;
    }

    fn write_zoom(self, camera: &mut Camera2d) {
        if let Some(ppu) = self.effective_pixels_per_unit() {
            camera.pixels_per_unit = ppu;
        }
    }
}

/// Frame-to-frame state for cinematic / smoothed follow.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CameraFollowRuntime2d {
    position: Option<[f32; 2]>,
}

impl CameraFollowRuntime2d {
    /// Empty runtime (first apply snaps to the ideal point).
    #[must_use]
    pub const fn new() -> Self {
        Self { position: None }
    }

    /// Clears smoothed state (hard cut on the next apply).
    pub fn reset(&mut self) {
        self.position = None;
    }

    /// Last smoothed camera centre, if any.
    #[must_use]
    pub const fn position(self) -> Option<[f32; 2]> {
        self.position
    }

    /// Moves toward `ideal` with exponential smoothing.
    ///
    /// `smooth_time_seconds ≤ 0` or non-finite/`dt ≤ 0` snaps immediately.
    pub fn smooth_toward(
        &mut self,
        ideal: [f32; 2],
        smooth_time_seconds: f32,
        dt_seconds: f32,
    ) -> [f32; 2] {
        if !ideal.iter().all(|value| value.is_finite()) {
            return self.position.unwrap_or([0.0, 0.0]);
        }
        let Some(current) = self.position else {
            self.position = Some(ideal);
            return ideal;
        };
        if !smooth_time_seconds.is_finite()
            || smooth_time_seconds <= 0.0
            || !dt_seconds.is_finite()
            || dt_seconds <= 0.0
        {
            self.position = Some(ideal);
            return ideal;
        }
        // 1 - e^(-dt / τ): reaches ~63% of the remaining gap in one smooth-time.
        let alpha = 1.0 - (-dt_seconds / smooth_time_seconds).exp();
        let next = [
            current[0] + (ideal[0] - current[0]) * alpha,
            current[1] + (ideal[1] - current[1]) * alpha,
        ];
        let next = if next.iter().all(|value| value.is_finite()) {
            next
        } else {
            ideal
        };
        self.position = Some(next);
        next
    }
}

#[cfg(test)]
mod tests {
    use super::{CameraFollow2d, CameraFollowRuntime2d, CameraShake2d, WorldBounds2d};
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
    fn pan_adds_after_offset() {
        let mut camera = Camera2d::default();
        CameraFollow2d::new()
            .with_offset([1.0, 2.0])
            .with_pan([3.0, 4.0])
            .apply(&mut camera, [10.0, 20.0]);
        assert_eq!(camera.position, [14.0, 26.0]);
    }

    #[test]
    fn zoom_writes_base_times_zoom() {
        let mut camera = Camera2d::new([0.0, 0.0], 1.0);
        CameraFollow2d::new()
            .with_base_pixels_per_unit(32.0)
            .with_zoom(2.0)
            .apply(&mut camera, [0.0, 0.0]);
        assert!((camera.pixels_per_unit - 64.0).abs() < f32::EPSILON);
    }

    #[test]
    fn disabled_follow_leaves_camera() {
        let mut camera = Camera2d::new([1.0, 2.0], 1.0);
        CameraFollow2d::disabled().apply(&mut camera, [99.0, 99.0]);
        assert_eq!(camera.position, [1.0, 2.0]);
    }

    #[test]
    fn bounds_clamp_keeps_viewport_inside_map() {
        let bounds = WorldBounds2d::from_origin_size([0.0, 0.0], [100.0, 80.0]).expect("bounds");
        // Viewport 40×40 world → half 20. Near top-left target should clamp to 20,20.
        let clamped = bounds.clamp_camera_center([0.0, 0.0], [20.0, 20.0]);
        assert_eq!(clamped, [20.0, 20.0]);
        let far = bounds.clamp_camera_center([200.0, 200.0], [20.0, 20.0]);
        assert_eq!(far, [80.0, 60.0]);
    }

    #[test]
    fn apply_with_surface_uses_bounds() {
        // Map larger than the viewport so edges clamp instead of centering.
        let bounds =
            WorldBounds2d::from_origin_size([0.0, 0.0], [2_000.0, 2_000.0]).expect("bounds");
        let mut camera = Camera2d::new([0.0, 0.0], 2.0); // 960×540 → half 240×135
        CameraFollow2d::new()
            .with_bounds(bounds)
            .apply_with_surface(&mut camera, [0.0, 0.0], [960, 540]);
        assert_eq!(camera.position, [240.0, 135.0]);
    }

    #[test]
    fn shake_trauma_decays_and_offsets() {
        let mut shake = CameraShake2d::new()
            .with_max_offset(10.0)
            .with_frequency(1.0)
            .with_decay_per_second(1.0);
        shake.add_trauma(1.0);
        let first = shake.tick(0.0);
        assert!(first[0].abs() > 0.0 || first[1].abs() > 0.0);
        for _ in 0..5 {
            let _ = shake.tick(0.5);
        }
        assert!(shake.trauma() < 0.1);
    }

    #[test]
    fn follow_tick_feeds_shake_into_position() {
        let mut follow = CameraFollow2d::new().with_shake(
            CameraShake2d::new()
                .with_max_offset(4.0)
                .with_frequency(2.0)
                .with_decay_per_second(0.0),
        );
        follow.shake_mut().add_trauma(1.0);
        let _ = follow.tick(0.1);
        let mut camera = Camera2d::default();
        follow.apply(&mut camera, [0.0, 0.0]);
        assert!(camera.position != [0.0, 0.0]);
    }

    #[test]
    fn cinematic_smoothing_approaches_target() {
        let follow = CameraFollow2d::new().with_smoothing(0.2);
        let mut runtime = CameraFollowRuntime2d::new();
        let mut camera = Camera2d::default();
        follow.apply_cinematic(
            &mut runtime,
            &mut camera,
            [100.0, 0.0],
            [0.0, 0.0],
            0.0,
            None,
        );
        assert_eq!(camera.position, [100.0, 0.0]);
        follow.apply_cinematic(
            &mut runtime,
            &mut camera,
            [0.0, 0.0],
            [0.0, 0.0],
            0.05,
            None,
        );
        assert!(camera.position[0] > 0.0 && camera.position[0] < 100.0);
    }

    #[test]
    fn look_ahead_offsets_ideal() {
        let follow = CameraFollow2d::new().with_look_ahead_scale(0.5);
        assert_eq!(
            follow.ideal_position([10.0, 20.0], [4.0, -2.0]),
            [12.0, 19.0]
        );
    }
}
