//! Opt-in animated playermodel presenter for M5.2 composition.
//!
//! [`AnimatedCharacterLoad3d`] imports a skeletal glTF on a shared [`TaskPool`]
//! and prepares CPU textures. [`AnimatedCharacter3d`] owns animation playback and
//! [`GltfAnimationPreviewGpu`] residency. Physics remains [`super::CharacterGame3d`].

use std::{
    error::Error,
    fmt,
    path::{Path, PathBuf},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use yuyib_assets::{
    AssetLoadId, AssetLoadQueue, AssetLoadState, AssetLoadSubmitError, AssetLoadTakeError,
};
use yuyib_gltf::{
    AnimationClipIndex, AnimationPlayer, AnimationSnapshot, ImportOptions, ImportedAsset,
    NodeIndex, import_scene_path_with_options, sample_bind_pose,
};
use yuyib_model_assets::{ModelTextureLoader, PreparedModelTextures};
use yuyib_render::RenderFrame;
use yuyib_render_3d::{
    Camera3d, DepthLoad, GltfAnimationPreviewGpu, GltfAnimationPreviewGpuError, LambertLighting3d,
    ModelUploadBudget3d,
};
use yuyib_tasks::TaskPool;

/// Background payload before GPU residency begins.
struct PreparedAnimatedCharacter {
    asset: ImportedAsset,
    textures: PreparedModelTextures,
}

/// Status of one animated-character load slot.
#[derive(Clone, Debug)]
pub enum AnimatedCharacterStatus {
    /// No load has been started (or the previous result was taken).
    Idle,
    /// Background import / texture prepare is in progress.
    Loading {
        /// Approximate completed / total worker units.
        completed_work: u64,
        /// Approximate total worker units (may be zero early on).
        total_work: u64,
    },
    /// CPU-ready character is available via [`AnimatedCharacterLoad3d::take_ready`].
    Ready,
    /// The active load failed.
    Failed {
        /// Human-readable failure message.
        message: String,
    },
}

/// Owns one shared-pool skeletal import + texture prepare request.
pub struct AnimatedCharacterLoad3d {
    queue: AssetLoadQueue<PreparedAnimatedCharacter, String>,
    request: Option<AssetLoadId>,
    ready: Option<PreparedAnimatedCharacter>,
    last_failure: Option<String>,
    clip_index: usize,
}

impl AnimatedCharacterLoad3d {
    /// Starts importing `path` on `pool`, resolving textures under `asset_root`.
    ///
    /// # Errors
    ///
    /// Returns when the task pool rejects the queue submission.
    pub fn start_on(
        pool: &Arc<TaskPool>,
        path: impl Into<PathBuf>,
        asset_root: impl Into<PathBuf>,
        clip_index: usize,
    ) -> Result<Self, AnimatedCharacterError> {
        let path = path.into();
        let asset_root = asset_root.into();
        let mut queue = AssetLoadQueue::new();
        let label = format!("animated character {}", path.display());
        let request = queue
            .try_queue(pool.as_ref(), label, move |progress| {
                progress.set_total_work(2);
                progress.decoding();
                let asset = import_animated_character(&path)?;
                progress.advance(1);
                let loader = ModelTextureLoader::new(&asset_root).map_err(|error| error.to_string())?;
                let textures = loader
                    .prepare(&asset.model)
                    .map_err(|error| error.to_string())?;
                progress.advance(1);
                Ok(PreparedAnimatedCharacter { asset, textures })
            })
            .map_err(AnimatedCharacterError::Submit)?;
        Ok(Self {
            queue,
            request: Some(request),
            ready: None,
            last_failure: None,
            clip_index,
        })
    }

    /// Polls the background load and returns the current status.
    pub fn poll(&mut self) -> AnimatedCharacterStatus {
        if self.ready.is_some() {
            return AnimatedCharacterStatus::Ready;
        }
        let Some(request) = self.request else {
            return self
                .last_failure
                .clone()
                .map_or(AnimatedCharacterStatus::Idle, |message| {
                    AnimatedCharacterStatus::Failed { message }
                });
        };
        self.queue.poll();
        let Some(info) = self.queue.info(request) else {
            self.last_failure = Some("animated character request disappeared".to_owned());
            self.request = None;
            return AnimatedCharacterStatus::Failed {
                message: self
                    .last_failure
                    .clone()
                    .expect("failure just recorded"),
            };
        };
        match info.state {
            AssetLoadState::ReadyToPublish => match self.queue.take_ready(request) {
                Ok(character) => {
                    self.ready = Some(character);
                    self.request = None;
                    AnimatedCharacterStatus::Ready
                }
                Err(AssetLoadTakeError::NotReady) => AnimatedCharacterStatus::Loading {
                    completed_work: info.progress.completed,
                    total_work: info.progress.total,
                },
                Err(error) => {
                    let message = format!("animated character could not be taken: {error}");
                    self.last_failure = Some(message.clone());
                    self.request = None;
                    AnimatedCharacterStatus::Failed { message }
                }
            },
            AssetLoadState::Failed => {
                let message = self.queue.failure(request).map_or_else(
                    || "unknown animated character load failure".to_owned(),
                    ToString::to_string,
                );
                self.last_failure = Some(message.clone());
                self.request = None;
                AnimatedCharacterStatus::Failed { message }
            }
            AssetLoadState::Queued
            | AssetLoadState::Reading
            | AssetLoadState::Decoding
            | AssetLoadState::Published => AnimatedCharacterStatus::Loading {
                completed_work: info.progress.completed,
                total_work: info.progress.total,
            },
        }
    }

    /// Returns load progress in `0.0..=1.0` for loading screens.
    #[must_use]
    #[allow(
        clippy::cast_precision_loss,
        reason = "loading UI is approximate while exact u64 counters remain authoritative"
    )]
    pub fn progress_fraction(&self) -> f32 {
        if self.ready.is_some() {
            return 1.0;
        }
        let Some(request) = self.request else {
            return 0.0;
        };
        let Some(info) = self.queue.info(request) else {
            return 0.0;
        };
        if info.progress.total == 0 {
            0.03
        } else {
            (info.progress.completed.min(info.progress.total) as f32
                / info.progress.total as f32)
                .clamp(0.03, 0.99)
        }
    }

    /// Takes the CPU-ready character and builds the GPU presenter.
    ///
    /// # Errors
    ///
    /// Returns when the load is not ready or bind-pose sampling fails.
    pub fn take_ready(&mut self) -> Result<AnimatedCharacter3d, AnimatedCharacterError> {
        let _ = self.poll();
        let prepared = self
            .ready
            .take()
            .ok_or(AnimatedCharacterError::NotReady)?;
        AnimatedCharacter3d::from_prepared(prepared, self.clip_index)
    }

    /// Blocks until CPU-ready or `timeout` elapses.
    ///
    /// # Errors
    ///
    /// Returns load failure, timeout, or bind-pose errors.
    pub fn wait_ready(
        &mut self,
        timeout: Duration,
    ) -> Result<AnimatedCharacter3d, AnimatedCharacterError> {
        let deadline = Instant::now() + timeout;
        loop {
            match self.poll() {
                AnimatedCharacterStatus::Ready => return self.take_ready(),
                AnimatedCharacterStatus::Failed { message } => {
                    return Err(AnimatedCharacterError::LoadFailed { message });
                }
                AnimatedCharacterStatus::Idle | AnimatedCharacterStatus::Loading { .. } => {
                    if Instant::now() >= deadline {
                        return Err(AnimatedCharacterError::Timeout { timeout });
                    }
                    thread::sleep(Duration::from_millis(2));
                }
            }
        }
    }
}

/// CPU animation + budgeted GPU residency for one skeletal playermodel.
pub struct AnimatedCharacter3d {
    asset: ImportedAsset,
    animation: Option<AnimationPlayer>,
    pose: AnimationSnapshot,
    gpu: GltfAnimationPreviewGpu,
}

impl AnimatedCharacter3d {
    fn from_prepared(
        prepared: PreparedAnimatedCharacter,
        clip_index: usize,
    ) -> Result<Self, AnimatedCharacterError> {
        let animation = (!prepared.asset.scene.animations().is_empty()).then(|| {
            AnimationPlayer::new(AnimationClipIndex::new(
                clip_index.min(prepared.asset.scene.animations().len().saturating_sub(1)),
            ))
        });
        let pose =
            sample_bind_pose(&prepared.asset.scene).map_err(AnimatedCharacterError::BindPose)?;
        Ok(Self {
            asset: prepared.asset,
            animation,
            pose,
            gpu: GltfAnimationPreviewGpu::new(prepared.textures),
        })
    }

    /// Sets flat key lighting applied when GPU residency is built.
    #[must_use]
    pub fn with_lighting(mut self, lighting: LambertLighting3d) -> Self {
        self.gpu = self.gpu.with_lighting(lighting);
        self
    }

    /// Returns whether textures and the skeletal renderer are resident.
    #[must_use]
    pub const fn is_ready(&self) -> bool {
        self.gpu.is_ready()
    }

    /// Approximate GPU residency progress (`0.0` uploading, `1.0` ready).
    #[must_use]
    pub fn gpu_progress_fraction(&self) -> f32 {
        if self.is_ready() { 1.0 } else { 0.5 }
    }

    /// Uploads textures within a frame budget and builds the skeletal renderer.
    ///
    /// # Errors
    ///
    /// Forwards GPU prepare failures.
    pub fn prepare_for_frame(
        &mut self,
        frame: &RenderFrame<'_>,
        budget: ModelUploadBudget3d,
    ) -> Result<bool, AnimatedCharacterError> {
        self.gpu
            .prepare_for_frame(frame, &self.asset.model, &self.asset.scene, budget)
            .map_err(AnimatedCharacterError::Gpu)
    }

    /// Advances clip 0 (or configured clip) when `playing` is true.
    pub fn advance(&mut self, delta_seconds: f32, playing: bool) {
        let Some(animation) = self.animation.as_mut() else {
            return;
        };
        if playing {
            animation.play();
        } else {
            animation.pause();
        }
        if animation
            .advance(&self.asset.scene, delta_seconds)
            .is_err()
        {
            return;
        }
        if let Ok(pose) = animation.snapshot(&self.asset.scene) {
            self.pose = pose;
        }
    }

    /// Finds a scene node by exact name.
    #[must_use]
    pub fn find_node(&self, name: &str) -> Option<NodeIndex> {
        self.asset
            .scene
            .nodes()
            .iter()
            .position(|node| node.name() == Some(name))
            .map(NodeIndex::new)
    }

    /// Returns a node translation in world space after applying `root_transform`.
    ///
    /// # Errors
    ///
    /// Returns when the node is missing from the sampled pose.
    pub fn node_world_position(
        &self,
        node: NodeIndex,
        root_transform: [f32; 16],
    ) -> Result<[f32; 3], AnimatedCharacterError> {
        let local = node_translation(&self.pose, node)?;
        Ok(transform_point(root_transform, local))
    }

    /// Midpoint of two named bones in world space (chase / FPS eye sockets).
    ///
    /// Optional `(min_y, max_y)` clamps the result relative to `root_transform`'s
    /// translation Y (feet).
    ///
    /// # Errors
    ///
    /// Returns when either bone is missing.
    pub fn camera_focus_from_bones(
        &self,
        left_name: &str,
        right_name: &str,
        root_transform: [f32; 16],
        clamp_above_feet: Option<(f32, f32)>,
    ) -> Result<[f32; 3], AnimatedCharacterError> {
        let left = self
            .find_node(left_name)
            .ok_or_else(|| AnimatedCharacterError::MissingNode(left_name.to_owned()))?;
        let right = self
            .find_node(right_name)
            .ok_or_else(|| AnimatedCharacterError::MissingNode(right_name.to_owned()))?;
        let left_pos = self.node_world_position(left, root_transform)?;
        let right_pos = self.node_world_position(right, root_transform)?;
        let mut focus = midpoint3(left_pos, right_pos);
        if let Some((min_above, max_above)) = clamp_above_feet {
            let feet_y = root_transform[13];
            focus[1] = focus[1].clamp(feet_y + min_above, feet_y + max_above);
        }
        Ok(focus)
    }

    /// Draws the current pose under `root_transform`.
    ///
    /// # Errors
    ///
    /// Returns when GPU residency is incomplete or draw fails.
    pub fn draw(
        &self,
        frame: &mut RenderFrame<'_>,
        camera: Camera3d,
        root_transform: [f32; 16],
        depth_load: DepthLoad,
    ) -> Result<(), AnimatedCharacterError> {
        self.gpu
            .draw_with_root_transform(
                frame,
                camera,
                &self.asset.scene,
                &self.pose,
                root_transform,
                depth_load,
            )
            .map_err(AnimatedCharacterError::Gpu)
    }

    /// Returns the imported asset.
    #[must_use]
    pub const fn asset(&self) -> &ImportedAsset {
        &self.asset
    }

    /// Returns the current animation snapshot.
    #[must_use]
    pub const fn pose(&self) -> &AnimationSnapshot {
        &self.pose
    }

    /// Escape hatch to the underlying preview GPU.
    pub const fn gpu_mut(&mut self) -> &mut GltfAnimationPreviewGpu {
        &mut self.gpu
    }

    /// Escape hatch to the animation player when a clip exists.
    pub const fn animation_mut(&mut self) -> Option<&mut AnimationPlayer> {
        self.animation.as_mut()
    }
}

fn import_animated_character(path: &Path) -> Result<ImportedAsset, String> {
    if !path.is_file() {
        return Err(format!("missing animated character at {}", path.display()));
    }
    let asset = import_scene_path_with_options(path, ImportOptions::skeletal_preview())
        .map_err(|error| error.to_string())?;
    if asset.scene.skins().is_empty() {
        return Err(format!("{} contains no skeleton", path.display()));
    }
    if asset.scene.animations().is_empty() {
        return Err(format!("{} contains no animation clips", path.display()));
    }
    Ok(asset)
}

fn node_translation(
    pose: &AnimationSnapshot,
    node: NodeIndex,
) -> Result<[f32; 3], AnimatedCharacterError> {
    let matrix = pose
        .world_matrices()
        .get(node.get())
        .ok_or(AnimatedCharacterError::MissingPoseNode(node))?;
    Ok([matrix[12], matrix[13], matrix[14]])
}

fn transform_point(matrix: [f32; 16], point: [f32; 3]) -> [f32; 3] {
    [
        matrix[0].mul_add(
            point[0],
            matrix[4].mul_add(point[1], matrix[8].mul_add(point[2], matrix[12])),
        ),
        matrix[1].mul_add(
            point[0],
            matrix[5].mul_add(point[1], matrix[9].mul_add(point[2], matrix[13])),
        ),
        matrix[2].mul_add(
            point[0],
            matrix[6].mul_add(point[1], matrix[10].mul_add(point[2], matrix[14])),
        ),
    ]
}

fn midpoint3(left: [f32; 3], right: [f32; 3]) -> [f32; 3] {
    [
        left[0].midpoint(right[0]),
        left[1].midpoint(right[1]),
        left[2].midpoint(right[2]),
    ]
}

/// Failure while loading or presenting an animated character.
#[derive(Debug)]
pub enum AnimatedCharacterError {
    /// Task-pool queue submission failed.
    Submit(AssetLoadSubmitError),
    /// Bind-pose sampling failed.
    BindPose(yuyib_gltf::AnimationSampleError),
    /// GPU prepare/draw failed.
    Gpu(GltfAnimationPreviewGpuError),
    /// CPU load is not ready yet.
    NotReady,
    /// Background load failed.
    LoadFailed {
        /// Failure message.
        message: String,
    },
    /// Load did not finish before the timeout.
    Timeout {
        /// Caller-supplied timeout.
        timeout: Duration,
    },
    /// Named scene node is missing.
    MissingNode(String),
    /// Sampled pose has no matrix for the node.
    MissingPoseNode(NodeIndex),
}

impl fmt::Display for AnimatedCharacterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Submit(error) => write!(formatter, "animated character submit: {error}"),
            Self::BindPose(error) => write!(formatter, "animated character bind pose: {error}"),
            Self::Gpu(error) => write!(formatter, "animated character gpu: {error}"),
            Self::NotReady => formatter.write_str("animated character is not ready"),
            Self::LoadFailed { message } => {
                write!(formatter, "animated character load failed: {message}")
            }
            Self::Timeout { timeout } => {
                write!(formatter, "animated character load timed out after {timeout:?}")
            }
            Self::MissingNode(name) => {
                write!(formatter, "animated character missing node `{name}`")
            }
            Self::MissingPoseNode(node) => {
                write!(
                    formatter,
                    "animated character pose missing node {}",
                    node.get()
                )
            }
        }
    }
}

impl Error for AnimatedCharacterError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Submit(error) => Some(error),
            Self::BindPose(error) => Some(error),
            Self::Gpu(error) => Some(error),
            Self::NotReady
            | Self::LoadFailed { .. }
            | Self::Timeout { .. }
            | Self::MissingNode(_)
            | Self::MissingPoseNode(_) => None,
        }
    }
}
