//! Composed playable loop: controls + follow camera + mesh character + avatar.
//!
//! Owns input/camera/step/draw for the animated character. Map
//! [`super::Game3dProfile`] remains a separate owner so hosts can still attach
//! optional dynamics overlays between fixed steps.

use std::{error::Error, fmt};

use yuyib_character_3d::{
    CharacterController3d, CharacterControllerConfig3d, CharacterControllerError3d,
    CharacterInput3d, CharacterModelPlacement3d, CharacterModelPlacementError3d,
    CharacterSpawnOptions3d, CharacterSpawnReport3d, LocomotionClipSet8, LocomotionController8,
    LocomotionFacingError, LocomotionFacingSmoother, LocomotionState8,
};
use yuyib_game_3d::SceneBoundsResult3d;
use yuyib_gltf::AnimationClipIndex;
use yuyib_input::{
    CharacterCameraMode3d, CharacterFollowCamera3d, CharacterFollowCameraError3d,
    FreeCameraEvent3d, PlayerCharacterControlConfig3d, PlayerCharacterControlError3d,
    PlayerCharacterControls3d,
};
use yuyib_physics::{TriangleMesh3d, Vec2};
use yuyib_platform::{
    CursorControl,
    winit::event::{DeviceEvent, WindowEvent},
};
use yuyib_render::RenderFrame;
use yuyib_render_3d::{
    Camera3d, DepthLoad, GltfSceneColliderLayerId3d, GltfSceneGpuProgress, LambertLighting3d,
    ModelUploadBudget3d,
};

use super::{
    AnimatedCharacter3d, AnimatedCharacterError, Game3dProfile, Game3dProfileError,
};

const MAX_FIXED_STEPS_PER_FRAME: usize = 8;

/// Construction knobs for [`PlayableLoop3d`].
#[derive(Clone, Debug)]
pub struct PlayableLoopDesc3d {
    /// Remappable WASD / jump / view + chase look numbers.
    pub controls: PlayerCharacterControlConfig3d,
    /// Uniform playermodel scale.
    pub model_scale: f32,
    /// Facing turn rate (radians / second).
    pub turn_speed_radians_per_second: f32,
    /// Left eye / camera socket bone name.
    pub left_eye_bone: String,
    /// Right eye / camera socket bone name.
    pub right_eye_bone: String,
    /// Optional `(min, max)` height above feet for chase focus.
    pub camera_height_clamp: Option<(f32, f32)>,
    /// Extra chase eye height on first spawn.
    pub camera_initial_height: f32,
    /// Solid collision layer for motor steps + chase collision.
    pub solid_layer: GltfSceneColliderLayerId3d,
    /// Walkable surface layer for spawn.
    pub street_layer: GltfSceneColliderLayerId3d,
    /// Spawn selection policy.
    pub spawn: CharacterSpawnOptions3d,
    /// Optional flat key light for the skeletal presenter.
    pub character_lighting: Option<LambertLighting3d>,
    /// Texture upload budget while the avatar is residency-streaming.
    pub upload_budget: ModelUploadBudget3d,
}

impl PlayableLoopDesc3d {
    /// Builds a desc with required layer/spawn fields and default control numbers.
    #[must_use]
    pub fn new(
        solid_layer: GltfSceneColliderLayerId3d,
        street_layer: GltfSceneColliderLayerId3d,
        spawn: CharacterSpawnOptions3d,
    ) -> Self {
        Self {
            controls: PlayerCharacterControlConfig3d::default(),
            model_scale: 1.0,
            turn_speed_radians_per_second: std::f32::consts::TAU,
            left_eye_bone: String::new(),
            right_eye_bone: String::new(),
            camera_height_clamp: None,
            camera_initial_height: 0.0,
            solid_layer,
            street_layer,
            spawn,
            character_lighting: None,
            upload_budget: ModelUploadBudget3d::default(),
        }
    }

    /// Sets control / chase camera numbers.
    #[must_use]
    pub const fn with_controls(mut self, controls: PlayerCharacterControlConfig3d) -> Self {
        self.controls = controls;
        self
    }

    /// Sets uniform model scale.
    #[must_use]
    pub const fn with_model_scale(mut self, model_scale: f32) -> Self {
        self.model_scale = model_scale;
        self
    }

    /// Sets facing turn speed.
    #[must_use]
    pub const fn with_turn_speed(mut self, turn_speed_radians_per_second: f32) -> Self {
        self.turn_speed_radians_per_second = turn_speed_radians_per_second;
        self
    }

    /// Sets eye-socket bone names used for chase / FPS focus.
    #[must_use]
    pub fn with_eye_bones(mut self, left: impl Into<String>, right: impl Into<String>) -> Self {
        self.left_eye_bone = left.into();
        self.right_eye_bone = right.into();
        self
    }

    /// Sets optional focus height clamp above feet.
    #[must_use]
    pub const fn with_camera_height_clamp(mut self, clamp: Option<(f32, f32)>) -> Self {
        self.camera_height_clamp = clamp;
        self
    }

    /// Sets initial chase eye height bias.
    #[must_use]
    pub const fn with_camera_initial_height(mut self, height: f32) -> Self {
        self.camera_initial_height = height;
        self
    }

    /// Sets optional skeletal key light.
    #[must_use]
    pub const fn with_character_lighting(mut self, lighting: Option<LambertLighting3d>) -> Self {
        self.character_lighting = lighting;
        self
    }

    /// Sets avatar GPU upload budget.
    #[must_use]
    pub const fn with_upload_budget(mut self, budget: ModelUploadBudget3d) -> Self {
        self.upload_budget = budget;
        self
    }
}

/// Result of one prepare/draw call.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlayableDrawStatus {
    /// Map and/or character GPU residency is still streaming.
    Loading,
    /// Frame was recorded.
    Drawn,
}

/// Engine-owned playable character loop (input → motor → avatar → camera).
pub struct PlayableLoop3d {
    controls: PlayerCharacterControls3d,
    follow_camera: CharacterFollowCamera3d,
    controller: CharacterController3d,
    character: AnimatedCharacter3d,
    locomotion: LocomotionController8,
    locomotion_clips: LocomotionClipSet8<AnimationClipIndex>,
    facing: LocomotionFacingSmoother,
    fixed_accumulator_seconds: f32,
    solid_layer: GltfSceneColliderLayerId3d,
    model_scale: f32,
    left_eye_bone: String,
    right_eye_bone: String,
    camera_height_clamp: Option<(f32, f32)>,
    upload_budget: ModelUploadBudget3d,
    cursor_activated: bool,
    reported_draw_stats: bool,
}

impl PlayableLoop3d {
    /// Spawns the mesh character and wires controls + chase camera + avatar.
    ///
    /// # Errors
    ///
    /// Returns spawn, control, camera, or placement failures.
    pub fn new(
        profile: &Game3dProfile,
        character: AnimatedCharacter3d,
        desc: PlayableLoopDesc3d,
    ) -> Result<(Self, CharacterSpawnReport3d), PlayableLoopError3d> {
        let street = profile
            .collider_layer(&desc.street_layer)
            .ok_or(PlayableLoopError3d::MissingColliderLayer)?;
        let solid = profile
            .collider_layer(&desc.solid_layer)
            .ok_or(PlayableLoopError3d::MissingColliderLayer)?;
        let mut control_config = desc.controls;
        let (controller, report) = CharacterController3d::spawn_on_surface_mesh_with_report(
            CharacterControllerConfig3d {
                radius: control_config.radius,
                move_speed: control_config.move_speed,
                jump_speed: control_config.jump_speed,
                gravity_y: control_config.gravity_y,
                ..CharacterControllerConfig3d::default()
            },
            street.mesh(),
            solid.mesh(),
            desc.spawn,
        )
        .map_err(PlayableLoopError3d::Controller)?;

        let far = match profile.loaded().and_then(|map| match map.bounds() {
            SceneBoundsResult3d::Bounds(bounds) => Some(bounds.radius().max(20.0) * 8.0),
            SceneBoundsResult3d::Empty => None,
        }) {
            Some(far) => far,
            None => control_config.far,
        };
        control_config.far = far;
        control_config.near = control_config.near.min(far * 0.5);

        let controls =
            PlayerCharacterControls3d::new(control_config).map_err(PlayableLoopError3d::Controls)?;
        let mut character = character;
        if let Some(lighting) = desc.character_lighting {
            character = character.with_lighting(lighting);
        }
        let facing = LocomotionFacingSmoother::new(
            Vec2::new(0.0, -1.0),
            desc.turn_speed_radians_per_second,
        )
        .map_err(PlayableLoopError3d::Facing)?;
        let initial_root = model_root(controller, facing.direction(), desc.model_scale)?;
        let focus = character.camera_focus_from_bones(
            &desc.left_eye_bone,
            &desc.right_eye_bone,
            initial_root,
            desc.camera_height_clamp,
        )?;
        let chase_eye = [
            focus[0],
            focus[1] + desc.camera_initial_height,
            focus[2] + control_config.chase_distance,
        ];
        let follow_camera = CharacterFollowCamera3d::looking_at(
            control_config.look_config(),
            control_config.chase_config(),
            chase_eye,
            focus,
        )
        .map_err(PlayableLoopError3d::Camera)?;

        Ok((
            Self {
                controls,
                follow_camera,
                controller,
                character,
                locomotion: LocomotionController8::default(),
                locomotion_clips: LocomotionClipSet8::new(AnimationClipIndex::new(0)),
                facing,
                fixed_accumulator_seconds: 0.0,
                solid_layer: desc.solid_layer,
                model_scale: desc.model_scale,
                left_eye_bone: desc.left_eye_bone,
                right_eye_bone: desc.right_eye_bone,
                camera_height_clamp: desc.camera_height_clamp,
                upload_budget: desc.upload_budget,
                cursor_activated: false,
                reported_draw_stats: false,
            },
            report,
        ))
    }

    /// Returns whether map+character GPU residency is complete (host supplies map flag).
    #[must_use]
    pub fn character_gpu_ready(&self) -> bool {
        self.character.is_ready()
    }

    /// Approximate character GPU progress.
    #[must_use]
    pub fn character_gpu_progress(&self) -> f32 {
        self.character.gpu_progress_fraction()
    }

    /// First-frame cursor lock request (once).
    pub fn take_initial_cursor_control(&mut self) -> Option<CursorControl> {
        if self.cursor_activated {
            return None;
        }
        self.cursor_activated = true;
        Some(self.follow_camera.initial_cursor_control())
    }

    /// Forwards window input to controls + follow camera.
    pub fn handle_window_event(&mut self, event: &WindowEvent) -> FreeCameraEvent3d {
        self.controls.handle_window_event(event);
        self.follow_camera.handle_window_event(event)
    }

    /// Forwards device (mouse) events to the follow camera.
    pub fn handle_device_event(&mut self, event: &DeviceEvent) -> FreeCameraEvent3d {
        self.follow_camera.handle_device_event(event)
    }

    /// Returns the mesh controller.
    #[must_use]
    pub const fn controller(&self) -> &CharacterController3d {
        &self.controller
    }

    /// Mutable controller escape hatch (dynamics reaction, etc.).
    pub const fn controller_mut(&mut self) -> &mut CharacterController3d {
        &mut self.controller
    }

    /// Current camera mode.
    #[must_use]
    pub const fn camera_mode(&self) -> CharacterCameraMode3d {
        self.follow_camera.mode()
    }

    /// Steps look, fixed motor, animation, and chase camera.
    ///
    /// `on_fixed_step` runs after each successful motor step (Rapier overlay hook).
    pub fn step(
        &mut self,
        profile: &Game3dProfile,
        frame_delta_seconds: f32,
        mut on_fixed_step: impl FnMut(&mut CharacterController3d, f32, &TriangleMesh3d),
    ) {
        if self.follow_camera.apply_look_input().is_err()
            || !frame_delta_seconds.is_finite()
            || frame_delta_seconds < 0.0
        {
            return;
        }
        if self.controls.take_view_toggle() {
            let _ = self.follow_camera.toggle_mode();
        }
        let speed = self.controls.effective_move_speed();
        let mut config = self.controller.config();
        if (config.move_speed - speed).abs() > f32::EPSILON {
            config.move_speed = speed;
            let _ = self.controller.set_config(config);
        }

        let fixed_delta = self.controller.config().fixed_delta_seconds;
        self.fixed_accumulator_seconds = (self.fixed_accumulator_seconds
            + frame_delta_seconds.min(0.125))
        .min(fixed_delta * 8.0);
        let camera_relative = self.controls.movement_axes();
        let movement = self
            .controls
            .movement_in_camera_space(self.follow_camera.look());
        let Ok(locomotion) = self
            .locomotion
            .update(camera_relative, &self.locomotion_clips)
        else {
            return;
        };
        if locomotion.state() == LocomotionState8::Moving
            && let Some(facing) = normalized_vec2(movement)
        {
            let _ = self
                .facing
                .update(facing, frame_delta_seconds.min(0.125));
        }
        let mut first_step = true;
        for _ in 0..MAX_FIXED_STEPS_PER_FRAME {
            if self.fixed_accumulator_seconds < fixed_delta {
                break;
            }
            let Ok(input) =
                CharacterInput3d::new(movement, first_step && self.controls.take_jump())
            else {
                break;
            };
            first_step = false;
            let Some(collider) = profile.collider_layer(&self.solid_layer) else {
                break;
            };
            if self
                .controller
                .step_on_triangle_mesh(input, collider.mesh())
                .is_err()
            {
                break;
            }
            on_fixed_step(&mut self.controller, fixed_delta, collider.mesh());
            self.fixed_accumulator_seconds -= fixed_delta;
        }

        self.character.advance(
            frame_delta_seconds,
            locomotion.state() == LocomotionState8::Moving,
        );
        if let Ok(root) = model_root(self.controller, self.facing.direction(), self.model_scale)
            && let Ok(focus) = self.character.camera_focus_from_bones(
                &self.left_eye_bone,
                &self.right_eye_bone,
                root,
                self.camera_height_clamp,
            )
            && let Some(collider) = profile.collider_layer(&self.solid_layer)
        {
            let _ = self
                .follow_camera
                .update_chase(focus, frame_delta_seconds, collider.mesh());
        }
    }

    /// Prepares map + character GPU and draws both (character only in third person).
    ///
    /// # Errors
    ///
    /// Forwards prepare/draw failures.
    pub fn prepare_and_draw(
        &mut self,
        profile: &mut Game3dProfile,
        frame: &mut RenderFrame<'_>,
        map_gpu: &mut GltfSceneGpuProgress,
        after_map: impl FnOnce(&mut RenderFrame<'_>, Camera3d) -> Result<(), PlayableLoopError3d>,
    ) -> Result<PlayableDrawStatus, PlayableLoopError3d> {
        let root = model_root(self.controller, self.facing.direction(), self.model_scale)?;
        let eye_focus = self.character.camera_focus_from_bones(
            &self.left_eye_bone,
            &self.right_eye_bone,
            root,
            self.camera_height_clamp,
        )?;
        let camera = self.follow_camera.camera(eye_focus);
        *profile.scene_mut().camera_mut() = camera;
        *map_gpu = profile
            .prepare_for_frame(frame, ModelUploadBudget3d::default())
            .map_err(PlayableLoopError3d::Profile)?;
        let _ = self
            .character
            .prepare_for_frame(frame, self.upload_budget)?;
        if !(map_gpu.ready && self.character.is_ready()) {
            return Ok(PlayableDrawStatus::Loading);
        }
        let map_stats = profile
            .render_map(frame)
            .map_err(PlayableLoopError3d::Profile)?;
        if !self.reported_draw_stats {
            self.reported_draw_stats = true;
            eprintln!("playable draw: {}", map_stats.summary_line());
        }
        if self.follow_camera.draws_playermodel() {
            self.character
                .draw(frame, camera, root, DepthLoad::Load)
                .map_err(PlayableLoopError3d::Character)?;
        }
        after_map(frame, camera)?;
        Ok(PlayableDrawStatus::Drawn)
    }
}

fn model_root(
    controller: CharacterController3d,
    facing: Vec2,
    scale: f32,
) -> Result<[f32; 16], PlayableLoopError3d> {
    Ok(CharacterModelPlacement3d::from_controller(controller, facing, scale)
        .map_err(PlayableLoopError3d::Placement)?
        .model_to_world())
}

fn normalized_vec2(value: Vec2) -> Option<Vec2> {
    let length = value.length_squared().sqrt();
    (length.is_finite() && length > f32::EPSILON).then_some(value * length.recip())
}

/// Failure while constructing or driving [`PlayableLoop3d`].
#[derive(Debug)]
pub enum PlayableLoopError3d {
    /// Profile collider layer missing.
    MissingColliderLayer,
    /// Character controller failure.
    Controller(CharacterControllerError3d),
    /// Player control construction failed.
    Controls(PlayerCharacterControlError3d),
    /// Follow camera construction failed.
    Camera(CharacterFollowCameraError3d),
    /// Facing smoother validation failed.
    Facing(LocomotionFacingError),
    /// Model placement failed.
    Placement(CharacterModelPlacementError3d),
    /// Animated character failure.
    Character(AnimatedCharacterError),
    /// Profile prepare/draw failure.
    Profile(Game3dProfileError),
    /// Host overlay / callback failure (dynamics draw, etc.).
    Host(String),
}

impl fmt::Display for PlayableLoopError3d {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingColliderLayer => {
                formatter.write_str("playable loop collider layer missing")
            }
            Self::Controller(error) => write!(formatter, "playable loop controller: {error}"),
            Self::Controls(error) => write!(formatter, "playable loop controls: {error}"),
            Self::Camera(error) => write!(formatter, "playable loop camera: {error}"),
            Self::Facing(error) => write!(formatter, "playable loop facing: {error}"),
            Self::Placement(error) => write!(formatter, "playable loop placement: {error}"),
            Self::Character(error) => write!(formatter, "playable loop character: {error}"),
            Self::Profile(error) => write!(formatter, "playable loop profile: {error}"),
            Self::Host(message) => write!(formatter, "playable loop host: {message}"),
        }
    }
}

impl Error for PlayableLoopError3d {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Controller(error) => Some(error),
            Self::Controls(error) => Some(error),
            Self::Camera(error) => Some(error),
            Self::Facing(error) => Some(error),
            Self::Placement(error) => Some(error),
            Self::Character(error) => Some(error),
            Self::Profile(error) => Some(error),
            Self::MissingColliderLayer | Self::Host(_) => None,
        }
    }
}

impl From<AnimatedCharacterError> for PlayableLoopError3d {
    fn from(value: AnimatedCharacterError) -> Self {
        Self::Character(value)
    }
}
