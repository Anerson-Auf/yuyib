//! M5 high-level 3D composition profiles.
//!
//! [`Game3dProfile`] owns the repeated playable wiring: shared [`TaskPool`],
//! [`Game3dScene`], and one [`GltfSceneLoad`] / [`LoadedGltfScene`] lifecycle.
//! [`CharacterGame3d`] is an opt-in adapter for mesh character spawn/step.
//! [`AnimatedCharacter3d`] is an opt-in skeletal presenter (load + animate +
//! GPU). [`DynamicsOverlay3d`] is an opt-in Rapier props overlay (feature
//! `rapier`) that stays side-by-side with mesh character collision. Genre
//! policy stays explicit — none of these are folded into one god type.
//!
//! Escape hatches remain: [`Game3dProfile::scene_mut`], [`Game3dProfile::loaded_mut`],
//! [`CharacterGame3d::controller_mut`], and [`AnimatedCharacter3d::gpu_mut`].

#![forbid(unsafe_code)]

mod animated_character;
mod dynamics_overlay;
mod environment;
mod playable_load;
mod playable_loop;

pub use animated_character::{
    AnimatedCharacter3d, AnimatedCharacterError, AnimatedCharacterLoad3d, AnimatedCharacterStatus,
};
pub use dynamics_overlay::{DynamicsOverlay3d, DynamicsOverlayError3d};
pub use environment::{
    EnvironmentPreset, EnvironmentPresetError, EnvironmentProbeSource, OUTDOOR_PROBE_HDR,
};
pub use playable_load::{Game3dPlayableLoad, Game3dPlayableLoadError, Game3dPlayableLoadStatus};
pub use playable_loop::{
    PlayableDrawStatus, PlayableLoop3d, PlayableLoopDesc3d, PlayableLoopError3d,
};

use std::{
    error::Error,
    fmt,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use yuyib_character_3d::{
    CharacterController3d, CharacterControllerConfig3d, CharacterControllerError3d,
    CharacterControllerStep3d, CharacterInput3d, CharacterSpawnOptions3d, CharacterSpawnReport3d,
};
use yuyib_game_3d::StaticSceneCollider3d;
use yuyib_model_assets::ModelTextureLoaderInitError;
use yuyib_physics::TriangleMesh3d;
use yuyib_render::RenderFrame;
use yuyib_render_3d::{
    Game3dScene, Game3dSceneConfig, Game3dSceneError, Game3dSceneStats, GltfSceneColliderLayerId3d,
    GltfSceneGpuProgress, GltfSceneLoad, GltfSceneLoadConfig, GltfSceneLoadProgress,
    GltfSceneLoadStage, GltfSceneLoadStartError, LoadedGltfScene, LoadedGltfSceneRenderError,
    ModelUploadBudget3d,
};
use yuyib_tasks::{TaskPool, TaskPoolConfig, TaskPoolCreateError};

/// Configuration for [`Game3dProfile`].
#[derive(Clone, Debug)]
pub struct Game3dProfileConfig {
    /// Root confining external model textures for [`Game3dScene`].
    pub asset_root: PathBuf,
    /// High-level scene extraction/render policy.
    ///
    /// Ignored when [`Self::environment`] is set — the preset owns shading/lighting.
    pub scene: Game3dSceneConfig,
    /// Default glTF load policy used by [`Game3dProfile::start_gltf`].
    pub load: GltfSceneLoadConfig,
    /// Shared CPU worker pool for scene jobs (used by [`Game3dProfile::new`]).
    pub task_pool: TaskPoolConfig,
    /// Optional IBL / shadow / SSAO composition recipe.
    pub environment: Option<EnvironmentPreset>,
}

impl Game3dProfileConfig {
    /// Builds a profile config with default scene/load policies under `asset_root`.
    #[must_use]
    pub fn new(asset_root: impl Into<PathBuf>) -> Self {
        Self {
            asset_root: asset_root.into(),
            scene: Game3dSceneConfig::default(),
            load: GltfSceneLoadConfig::default(),
            task_pool: TaskPoolConfig::default(),
            environment: None,
        }
    }

    /// Replaces the scene policy.
    #[must_use]
    pub fn with_scene(mut self, scene: Game3dSceneConfig) -> Self {
        self.scene = scene;
        self
    }

    /// Replaces the default glTF load policy.
    #[must_use]
    pub fn with_load(mut self, load: GltfSceneLoadConfig) -> Self {
        self.load = load;
        self
    }

    /// Replaces the shared task-pool configuration.
    #[must_use]
    pub fn with_task_pool(mut self, task_pool: TaskPoolConfig) -> Self {
        self.task_pool = task_pool;
        self
    }

    /// Attaches an outdoor/IBL composition recipe (street-city look, etc.).
    #[must_use]
    pub fn with_environment(mut self, preset: EnvironmentPreset) -> Self {
        self.environment = Some(preset);
        self
    }
}

/// Status of the profile's single scene load slot.
#[derive(Clone, Debug)]
pub enum Game3dProfileStatus {
    /// No load has been started (or the previous result was taken).
    Idle,
    /// Background import is in progress.
    Loading {
        /// Latest progress snapshot from [`GltfSceneLoad::update`].
        progress: GltfSceneLoadProgress,
    },
    /// A loaded scene is available via [`Game3dProfile::loaded`].
    Ready,
    /// The active load failed.
    Failed {
        /// Human-readable failure message.
        message: String,
    },
}

/// Owns shared task pool + scene renderer + one glTF load lifecycle.
pub struct Game3dProfile {
    task_pool: Arc<TaskPool>,
    scene: Game3dScene,
    load_config: GltfSceneLoadConfig,
    loading: Option<GltfSceneLoad>,
    loaded: Option<LoadedGltfScene>,
    last_failure: Option<String>,
}

impl Game3dProfile {
    /// Creates a profile with a shared worker pool and empty load slot.
    ///
    /// When [`Game3dProfileConfig::environment`] is set, the preset builds the
    /// [`Game3dScene`] (shading/lighting/IBL/shadows/SSAO). Otherwise
    /// [`Game3dProfileConfig::scene`] is used as today.
    ///
    /// # Errors
    ///
    /// Returns [`Game3dProfileError`] when the task pool, environment, or scene
    /// cannot be created.
    pub fn new(config: Game3dProfileConfig) -> Result<Self, Game3dProfileError> {
        let task_pool = Arc::new(
            TaskPool::new(config.task_pool).map_err(Game3dProfileError::TaskPool)?,
        );
        // `task_pool` is Copy; config is still fully usable below.
        Self::with_shared_pool(task_pool, config)
    }

    /// Creates a profile on an existing shared [`TaskPool`] (co-load with character).
    ///
    /// [`Game3dProfileConfig::task_pool`] is ignored; pass the same `Arc` to
    /// [`AnimatedCharacterLoad3d::start_on`].
    ///
    /// # Errors
    ///
    /// Returns [`Game3dProfileError`] when the environment or scene cannot be created.
    pub fn with_shared_pool(
        task_pool: Arc<TaskPool>,
        config: Game3dProfileConfig,
    ) -> Result<Self, Game3dProfileError> {
        let scene = match config.environment {
            Some(preset) => preset
                .build_scene(&config.asset_root)
                .map_err(Game3dProfileError::Environment)?,
            None => Game3dScene::new(&config.asset_root, config.scene)
                .map_err(Game3dProfileError::Scene)?,
        };
        Self::from_scene(task_pool, config.load, scene)
    }

    /// Creates a profile around an already-built [`Game3dScene`].
    ///
    /// Prefer [`Self::new`] / [`Self::with_shared_pool`] with
    /// [`EnvironmentPreset`] for outdoor IBL. Keep this escape hatch for
    /// editor/custom scenes.
    ///
    /// # Errors
    ///
    /// Returns [`Game3dProfileError`] only if future validation is added; today this
    /// always succeeds when `task_pool` is valid.
    pub fn from_scene(
        task_pool: Arc<TaskPool>,
        load_config: GltfSceneLoadConfig,
        scene: Game3dScene,
    ) -> Result<Self, Game3dProfileError> {
        Ok(Self {
            task_pool,
            scene,
            load_config,
            loading: None,
            loaded: None,
            last_failure: None,
        })
    }

    /// Returns CPU load progress for the active map request (UI / diagnostics).
    #[must_use]
    pub fn load_progress(&self) -> GltfSceneLoadProgress {
        if let Some(loading) = self.loading.as_ref() {
            return loading.progress();
        }
        if self.loaded.is_some() {
            return GltfSceneLoadProgress {
                stage: GltfSceneLoadStage::Ready,
                completed_work: 1,
                total_work: 1,
            };
        }
        GltfSceneLoadProgress {
            stage: GltfSceneLoadStage::Queued,
            completed_work: 0,
            total_work: 0,
        }
    }

    /// Starts (or restarts) a glTF scene load on the shared pool.
    ///
    /// Clears any previously loaded scene. Poll with [`Self::poll`] / [`Self::wait_ready`].
    ///
    /// # Errors
    ///
    /// Returns [`Game3dProfileError`] when the load cannot be started.
    pub fn start_gltf(&mut self, path: impl AsRef<Path>) -> Result<(), Game3dProfileError> {
        self.loaded = None;
        self.last_failure = None;
        let loading = GltfSceneLoad::start_on(path, self.load_config.clone(), Arc::clone(&self.task_pool))
            .map_err(Game3dProfileError::LoadStart)?;
        self.loading = Some(loading);
        Ok(())
    }

    /// Polls the active load and returns the current status.
    pub fn poll(&mut self) -> Game3dProfileStatus {
        if self.loaded.is_some() {
            return Game3dProfileStatus::Ready;
        }
        if let Some(message) = self.last_failure.as_ref() {
            return Game3dProfileStatus::Failed {
                message: message.clone(),
            };
        }
        let Some(loading) = self.loading.as_mut() else {
            return Game3dProfileStatus::Idle;
        };
        let progress = loading.update();
        match progress.stage {
            GltfSceneLoadStage::Ready => match loading.take_ready() {
                Ok(scene) => {
                    self.loaded = Some(scene);
                    self.loading = None;
                    Game3dProfileStatus::Ready
                }
                Err(error) => {
                    let message = error.to_string();
                    self.last_failure = Some(message.clone());
                    self.loading = None;
                    Game3dProfileStatus::Failed { message }
                }
            },
            GltfSceneLoadStage::Failed => {
                let message = loading.failure().map_or_else(
                    || "glTF scene load failed".to_owned(),
                    ToString::to_string,
                );
                self.last_failure = Some(message.clone());
                self.loading = None;
                Game3dProfileStatus::Failed { message }
            }
            GltfSceneLoadStage::Queued | GltfSceneLoadStage::Reading | GltfSceneLoadStage::Processing => {
                Game3dProfileStatus::Loading { progress }
            }
            GltfSceneLoadStage::Taken => Game3dProfileStatus::Idle,
        }
    }

    /// Blocks until the active load is ready or `timeout` elapses.
    ///
    /// # Errors
    ///
    /// Returns [`Game3dProfileError`] on failure, idle state, or timeout.
    pub fn wait_ready(
        &mut self,
        timeout: Duration,
    ) -> Result<&LoadedGltfScene, Game3dProfileError> {
        let started = Instant::now();
        loop {
            match self.poll() {
                Game3dProfileStatus::Ready => {
                    return self
                        .loaded
                        .as_ref()
                        .ok_or(Game3dProfileError::NotReady);
                }
                Game3dProfileStatus::Failed { message } => {
                    return Err(Game3dProfileError::LoadFailed { message });
                }
                Game3dProfileStatus::Idle => return Err(Game3dProfileError::Idle),
                Game3dProfileStatus::Loading { .. } => {
                    if started.elapsed() >= timeout {
                        return Err(Game3dProfileError::Timeout { timeout });
                    }
                    std::thread::sleep(Duration::from_millis(2));
                }
            }
        }
    }

    /// Returns the shared task pool.
    #[must_use]
    pub fn task_pool(&self) -> &Arc<TaskPool> {
        &self.task_pool
    }

    /// Returns the scene renderer.
    #[must_use]
    pub const fn scene(&self) -> &Game3dScene {
        &self.scene
    }

    /// Returns a mutable scene renderer escape hatch.
    pub const fn scene_mut(&mut self) -> &mut Game3dScene {
        &mut self.scene
    }

    /// Returns the loaded scene when ready.
    #[must_use]
    pub const fn loaded(&self) -> Option<&LoadedGltfScene> {
        self.loaded.as_ref()
    }

    /// Returns a mutable loaded scene escape hatch.
    pub const fn loaded_mut(&mut self) -> Option<&mut LoadedGltfScene> {
        self.loaded.as_mut()
    }

    /// Takes ownership of the loaded scene, leaving the profile Idle.
    pub fn take_loaded(&mut self) -> Option<LoadedGltfScene> {
        self.loaded.take()
    }

    /// Looks up a semantic collider layer on the loaded scene.
    #[must_use]
    pub fn collider_layer(
        &self,
        id: &GltfSceneColliderLayerId3d,
    ) -> Option<&StaticSceneCollider3d> {
        self.loaded.as_ref()?.collider_layer(id)
    }

    /// Publishes map GPU residency for one frame under `budget`.
    ///
    /// # Errors
    ///
    /// Returns [`Game3dProfileError`] when no scene is ready or prepare fails.
    pub fn prepare_for_frame(
        &mut self,
        frame: &RenderFrame<'_>,
        budget: ModelUploadBudget3d,
    ) -> Result<GltfSceneGpuProgress, Game3dProfileError> {
        let loaded = self.loaded.as_mut().ok_or(Game3dProfileError::NotReady)?;
        loaded
            .prepare_for_frame_with_budget(frame, &mut self.scene, budget)
            .map_err(Game3dProfileError::SceneRender)
    }

    /// Renders the loaded map through the owned [`Game3dScene`].
    ///
    /// # Errors
    ///
    /// Returns [`Game3dProfileError`] when no scene is ready or draw fails.
    pub fn render_map(
        &mut self,
        frame: &mut RenderFrame<'_>,
    ) -> Result<Game3dSceneStats, Game3dProfileError> {
        let loaded = self.loaded.as_mut().ok_or(Game3dProfileError::NotReady)?;
        loaded
            .render(frame, &mut self.scene)
            .map_err(Game3dProfileError::MapRender)
    }
}

/// Opt-in mesh character adapter for use with [`Game3dProfile`] layers.
pub struct CharacterGame3d {
    controller: CharacterController3d,
}

impl CharacterGame3d {
    /// Spawns a character on explicit surface/collision meshes.
    ///
    /// # Errors
    ///
    /// Forwards [`CharacterController3d`] spawn failures.
    pub fn spawn(
        surface_mesh: &TriangleMesh3d,
        collision_mesh: &TriangleMesh3d,
        config: CharacterControllerConfig3d,
        options: CharacterSpawnOptions3d,
    ) -> Result<(Self, CharacterSpawnReport3d), Game3dProfileError> {
        let (controller, report) = CharacterController3d::spawn_on_surface_mesh_with_report(
            config,
            surface_mesh,
            collision_mesh,
            options,
        )
        .map_err(Game3dProfileError::Character)?;
        Ok((Self { controller }, report))
    }

    /// Spawns using semantic collider layers from a ready [`Game3dProfile`].
    ///
    /// # Errors
    ///
    /// Returns [`Game3dProfileError`] when layers are missing or spawn fails.
    pub fn spawn_on_profile_layers(
        profile: &Game3dProfile,
        surface_layer: &GltfSceneColliderLayerId3d,
        collision_layer: &GltfSceneColliderLayerId3d,
        config: CharacterControllerConfig3d,
        options: CharacterSpawnOptions3d,
    ) -> Result<(Self, CharacterSpawnReport3d), Game3dProfileError> {
        let surface = profile
            .collider_layer(surface_layer)
            .ok_or(Game3dProfileError::MissingColliderLayer)?;
        let collision = profile
            .collider_layer(collision_layer)
            .ok_or(Game3dProfileError::MissingColliderLayer)?;
        Self::spawn(surface.mesh(), collision.mesh(), config, options)
    }

    /// Returns the underlying controller.
    #[must_use]
    pub const fn controller(&self) -> &CharacterController3d {
        &self.controller
    }

    /// Returns a mutable controller escape hatch.
    pub const fn controller_mut(&mut self) -> &mut CharacterController3d {
        &mut self.controller
    }

    /// Steps the character against a triangle mesh.
    ///
    /// # Errors
    ///
    /// Forwards controller step failures.
    pub fn step_on_mesh(
        &mut self,
        input: CharacterInput3d,
        mesh: &TriangleMesh3d,
    ) -> Result<CharacterControllerStep3d, Game3dProfileError> {
        self.controller
            .step_on_triangle_mesh(input, mesh)
            .map_err(Game3dProfileError::Character)
    }
}

/// Failure while configuring or driving a 3D profile.
#[derive(Debug)]
pub enum Game3dProfileError {
    /// Task pool creation failed.
    TaskPool(TaskPoolCreateError),
    /// Scene renderer construction failed.
    Scene(ModelTextureLoaderInitError),
    /// glTF load could not be started.
    LoadStart(GltfSceneLoadStartError),
    /// Background load failed.
    LoadFailed {
        /// Failure message.
        message: String,
    },
    /// No load is active.
    Idle,
    /// Loaded scene is not available yet.
    NotReady,
    /// Load did not finish before the timeout.
    Timeout {
        /// Caller-supplied timeout.
        timeout: Duration,
    },
    /// Semantic collider layer was not present on the loaded scene.
    MissingColliderLayer,
    /// Map prepare/render through [`Game3dScene`] failed.
    SceneRender(Game3dSceneError),
    /// Map draw failed.
    MapRender(LoadedGltfSceneRenderError),
    /// Character controller failure.
    Character(CharacterControllerError3d),
    /// Environment preset (IBL/shadows) failed.
    Environment(EnvironmentPresetError),
}

impl fmt::Display for Game3dProfileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TaskPool(error) => write!(f, "3d profile task pool: {error}"),
            Self::Scene(error) => write!(f, "3d profile scene: {error}"),
            Self::LoadStart(error) => write!(f, "3d profile load start: {error}"),
            Self::LoadFailed { message } => write!(f, "3d profile load failed: {message}"),
            Self::Idle => f.write_str("3d profile has no active glTF load"),
            Self::NotReady => f.write_str("3d profile scene is not ready"),
            Self::Timeout { timeout } => {
                write!(f, "3d profile load timed out after {timeout:?}")
            }
            Self::MissingColliderLayer => f.write_str("3d profile collider layer missing"),
            Self::SceneRender(error) => write!(f, "3d profile scene render: {error}"),
            Self::MapRender(error) => write!(f, "3d profile map render: {error}"),
            Self::Character(error) => write!(f, "3d profile character: {error}"),
            Self::Environment(error) => write!(f, "3d profile environment: {error}"),
        }
    }
}

impl Error for Game3dProfileError {}

#[cfg(test)]
mod tests {
    use super::{CharacterGame3d, Game3dProfile, Game3dProfileConfig};
    use std::time::{SystemTime, UNIX_EPOCH};
    use yuyib_character_3d::{CharacterControllerConfig3d, CharacterSpawnOptions3d};
    use yuyib_physics::{TriangleMesh3d, Vec2, Vec3};

    fn tiny_triangle_gltf() -> Vec<u8> {
        br#"{"asset":{"version":"2.0"},"scene":0,"scenes":[{"nodes":[0]}],"nodes":[{"mesh":0}],"buffers":[{"uri":"data:application/octet-stream;base64,AAABAAIAAAAAAAAAAAAAAAAAAAAAAIA/AAAAAAAAAAAAAAAAAACAPwAAAAA=","byteLength":44}],"bufferViews":[{"buffer":0,"byteOffset":0,"byteLength":6},{"buffer":0,"byteOffset":8,"byteLength":36}],"accessors":[{"bufferView":0,"componentType":5123,"count":3,"type":"SCALAR"},{"bufferView":1,"componentType":5126,"count":3,"type":"VEC3","min":[0,0,0],"max":[1,1,0]}],"meshes":[{"primitives":[{"attributes":{"POSITION":1},"indices":0}]}]}"#.to_vec()
    }

    #[test]
    fn profile_loads_tiny_gltf_to_ready() {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("yuyib_profile3d_{stamp}"));
        std::fs::create_dir_all(&root).expect("mkdir");
        let path = root.join("triangle.gltf");
        std::fs::write(&path, tiny_triangle_gltf()).expect("write");

        let mut profile = Game3dProfile::new(Game3dProfileConfig::new(&root)).expect("profile");
        profile.start_gltf(&path).expect("start");
        let _loaded = profile
            .wait_ready(std::time::Duration::from_secs(30))
            .expect("ready");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn character_spawns_on_flat_mesh() {
        let floor = TriangleMesh3d::from_indexed(
            &[
                Vec3::new(-2.0, 0.0, -2.0),
                Vec3::new(2.0, 0.0, -2.0),
                Vec3::new(2.0, 0.0, 2.0),
                Vec3::new(-2.0, 0.0, 2.0),
            ],
            &[0, 2, 1, 0, 3, 2],
        )
        .expect("floor");
        let (character, _report) = CharacterGame3d::spawn(
            &floor,
            &floor,
            CharacterControllerConfig3d::default(),
            CharacterSpawnOptions3d::outdoor_lowest(Vec2::ZERO),
        )
        .expect("spawn");
        assert!(character.controller().is_grounded());
    }
}
