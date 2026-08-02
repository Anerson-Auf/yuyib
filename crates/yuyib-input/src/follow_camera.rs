//! Composed chase / first-person follow camera for playable characters.
//!
//! [`CharacterFollowCamera3d`] owns mouse-look ([`FreeCameraController3d`]) and a
//! collision-aware boom ([`CollisionAwareThirdPersonCamera3d`]), then exposes one
//! [`Camera3d`] for either third-person or eye-socket first-person rendering.

use winit::event::{DeviceEvent, WindowEvent};
use yuyib_physics::TriangleMesh3d;
use yuyib_platform::CursorControl;
use yuyib_render_3d::Camera3d;

use crate::{
    CollisionAwareThirdPersonCamera3d, FreeCameraConfig3d, FreeCameraController3d,
    FreeCameraError3d, FreeCameraEvent3d, ThirdPersonCameraConfig3d, ThirdPersonCameraError3d,
    ThirdPersonCameraUpdate3d, ThirdPersonOrbit3d,
};

use std::{error::Error, fmt};

/// Playable follow-camera presentation mode.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CharacterCameraMode3d {
    /// Collision-aware chase boom behind the character.
    #[default]
    ThirdPerson,
    /// Eye-socket camera; the host normally skips drawing the playermodel.
    FirstPerson,
}

/// Mouse-look + chase boom with a first/third-person toggle.
#[derive(Clone, Debug, PartialEq)]
pub struct CharacterFollowCamera3d {
    look: FreeCameraController3d,
    chase: CollisionAwareThirdPersonCamera3d,
    mode: CharacterCameraMode3d,
}

impl CharacterFollowCamera3d {
    /// Creates a third-person follow rig looking from `chase_eye` at `eye_focus`.
    ///
    /// # Errors
    ///
    /// Forwards free-look or chase-camera construction failures, or rejects a
    /// degenerate initial look direction.
    pub fn looking_at(
        look_config: FreeCameraConfig3d,
        chase_config: ThirdPersonCameraConfig3d,
        chase_eye: [f32; 3],
        eye_focus: [f32; 3],
    ) -> Result<Self, CharacterFollowCameraError3d> {
        let look = FreeCameraController3d::looking_at(look_config, chase_eye, eye_focus)
            .map_err(CharacterFollowCameraError3d::FreeLook)?;
        let orbit = orbit_from_camera(look.camera())
            .ok_or(CharacterFollowCameraError3d::DegenerateLookDirection)?;
        let chase = CollisionAwareThirdPersonCamera3d::new(chase_config, eye_focus, orbit)
            .map_err(CharacterFollowCameraError3d::Chase)?;
        Ok(Self {
            look,
            chase,
            mode: CharacterCameraMode3d::ThirdPerson,
        })
    }

    /// Current presentation mode.
    #[must_use]
    pub const fn mode(&self) -> CharacterCameraMode3d {
        self.mode
    }

    /// Replaces the presentation mode.
    pub fn set_mode(&mut self, mode: CharacterCameraMode3d) {
        self.mode = mode;
    }

    /// Toggles third-person ↔ first-person and returns the new mode.
    pub fn toggle_mode(&mut self) -> CharacterCameraMode3d {
        self.mode = match self.mode {
            CharacterCameraMode3d::ThirdPerson => CharacterCameraMode3d::FirstPerson,
            CharacterCameraMode3d::FirstPerson => CharacterCameraMode3d::ThirdPerson,
        };
        self.mode
    }

    /// Whether the host should draw the playermodel for the active mode.
    #[must_use]
    pub const fn draws_playermodel(&self) -> bool {
        matches!(self.mode, CharacterCameraMode3d::ThirdPerson)
    }

    /// Shared mouse-look controller (also drives camera-relative movement).
    #[must_use]
    pub const fn look(&self) -> &FreeCameraController3d {
        &self.look
    }

    /// Mutable access for Winit event routing.
    pub fn look_mut(&mut self) -> &mut FreeCameraController3d {
        &mut self.look
    }

    /// Cursor policy from the free-look config.
    #[must_use]
    pub const fn initial_cursor_control(&self) -> CursorControl {
        self.look.initial_cursor_control()
    }

    /// Forwards a window event to the free-look controller.
    #[must_use]
    pub fn handle_window_event(&mut self, event: &WindowEvent) -> FreeCameraEvent3d {
        self.look.handle_window_event(event)
    }

    /// Forwards a device event to the free-look controller.
    #[must_use]
    pub fn handle_device_event(&mut self, event: &DeviceEvent) -> FreeCameraEvent3d {
        self.look.handle_device_event(event)
    }

    /// Consumes queued mouse deltas without translating the free-look eye.
    ///
    /// Playable hosts keep character locomotion on a separate controller, so
    /// look motion uses `delta_seconds = 0.0`.
    ///
    /// # Errors
    ///
    /// Forwards [`FreeCameraController3d::step`] validation failures.
    pub fn apply_look_input(&mut self) -> Result<(), FreeCameraError3d> {
        self.look.step(0.0)
    }

    /// Updates the collision-aware chase boom from the current mouse-look orbit.
    ///
    /// # Errors
    ///
    /// Returns a typed chase or degenerate-look failure.
    pub fn update_chase(
        &mut self,
        eye_focus: [f32; 3],
        delta_seconds: f32,
        collision_mesh: &TriangleMesh3d,
    ) -> Result<ThirdPersonCameraUpdate3d, CharacterFollowCameraError3d> {
        let orbit = orbit_from_camera(self.look.camera())
            .ok_or(CharacterFollowCameraError3d::DegenerateLookDirection)?;
        self.chase
            .update(eye_focus, orbit, delta_seconds, collision_mesh)
            .map_err(CharacterFollowCameraError3d::Chase)
    }

    /// Active render camera for the current mode.
    ///
    /// First-person places the eye at `eye_focus` and reuses free-look yaw/pitch.
    #[must_use]
    pub fn camera(&self, eye_focus: [f32; 3]) -> Camera3d {
        match self.mode {
            CharacterCameraMode3d::ThirdPerson => self.chase.camera(),
            CharacterCameraMode3d::FirstPerson => {
                let look = self.look.camera();
                let forward = [
                    look.target[0] - look.position[0],
                    look.target[1] - look.position[1],
                    look.target[2] - look.position[2],
                ];
                Camera3d::new(
                    eye_focus,
                    [
                        eye_focus[0] + forward[0],
                        eye_focus[1] + forward[1],
                        eye_focus[2] + forward[2],
                    ],
                    [0.0, 1.0, 0.0],
                    look.vertical_fov_radians,
                    look.near,
                    look.far,
                )
            }
        }
    }
}

fn orbit_from_camera(camera: Camera3d) -> Option<ThirdPersonOrbit3d> {
    let forward = [
        camera.target[0] - camera.position[0],
        camera.target[1] - camera.position[1],
        camera.target[2] - camera.position[2],
    ];
    let length = forward[0]
        .mul_add(
            forward[0],
            forward[1].mul_add(forward[1], forward[2] * forward[2]),
        )
        .sqrt();
    if !length.is_finite() || length <= f32::EPSILON {
        return None;
    }
    let reciprocal = length.recip();
    let forward = [
        forward[0] * reciprocal,
        forward[1] * reciprocal,
        forward[2] * reciprocal,
    ];
    Some(ThirdPersonOrbit3d::new(
        forward[0].atan2(-forward[2]),
        (-forward[1]).asin(),
    ))
}

/// Failure while constructing or updating [`CharacterFollowCamera3d`].
#[derive(Clone, Debug, PartialEq)]
pub enum CharacterFollowCameraError3d {
    /// Free-look controller failed.
    FreeLook(FreeCameraError3d),
    /// Chase boom failed.
    Chase(ThirdPersonCameraError3d),
    /// Mouse-look direction was zero or non-finite.
    DegenerateLookDirection,
}

impl fmt::Display for CharacterFollowCameraError3d {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FreeLook(error) => write!(formatter, "character follow free-look: {error}"),
            Self::Chase(error) => write!(formatter, "character follow chase camera: {error}"),
            Self::DegenerateLookDirection => {
                formatter.write_str("character follow look direction is degenerate")
            }
        }
    }
}

impl Error for CharacterFollowCameraError3d {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::FreeLook(error) => Some(error),
            Self::Chase(error) => Some(error),
            Self::DegenerateLookDirection => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{CharacterCameraMode3d, CharacterFollowCamera3d};
    use crate::{FreeCameraConfig3d, ThirdPersonCameraConfig3d};
    use yuyib_physics::TriangleMesh3d;

    fn unit_floor() -> TriangleMesh3d {
        use yuyib_physics::Vec3;
        TriangleMesh3d::from_indexed(
            &[
                Vec3::new(-10.0, 0.0, -10.0),
                Vec3::new(10.0, 0.0, -10.0),
                Vec3::new(10.0, 0.0, 10.0),
                Vec3::new(-10.0, 0.0, 10.0),
            ],
            &[0, 2, 1, 0, 3, 2],
        )
        .expect("floor")
    }

    #[test]
    fn toggle_hides_playermodel_in_first_person() {
        let mut camera = CharacterFollowCamera3d::looking_at(
            FreeCameraConfig3d::default(),
            ThirdPersonCameraConfig3d::default(),
            [0.0, 2.0, 4.0],
            [0.0, 1.5, 0.0],
        )
        .expect("follow camera");
        assert!(camera.draws_playermodel());
        assert_eq!(camera.toggle_mode(), CharacterCameraMode3d::FirstPerson);
        assert!(!camera.draws_playermodel());
        let eye = [0.0, 1.5, 0.0];
        let first = camera.camera(eye);
        assert_eq!(first.position, eye);
    }

    #[test]
    fn chase_update_keeps_third_person_behind_focus() {
        let mesh = unit_floor();
        let mut camera = CharacterFollowCamera3d::looking_at(
            FreeCameraConfig3d {
                near: 0.08,
                far: 200.0,
                ..FreeCameraConfig3d::default()
            },
            ThirdPersonCameraConfig3d {
                distance: 3.0,
                near: 0.08,
                far: 200.0,
                ..ThirdPersonCameraConfig3d::default()
            },
            [0.0, 2.0, 4.0],
            [0.0, 1.5, 0.0],
        )
        .expect("follow camera");
        camera
            .update_chase([0.0, 1.5, 0.0], 1.0 / 60.0, &mesh)
            .expect("chase update");
        let view = camera.camera([0.0, 1.5, 0.0]);
        assert!(view.position[2] > 0.5);
    }
}
