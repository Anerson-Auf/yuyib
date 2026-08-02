//! High-level two-stage glTF scene loading without blocking the window.

use std::{collections::HashSet, error::Error, fmt, path::Path, path::PathBuf, sync::Arc};

use yuyib_assets::{
    AssetLoadFailure, AssetLoadId, AssetLoadQueue, AssetLoadState, AssetLoadSubmitError,
    AssetLoadTakeError, Assets, CookCache, ImportDiagnostic, ImportError as RegistryImportError,
    ImportSource, ImporterRegistry, ImporterRegistryConfigError, ImporterRegistryLimits,
};
use yuyib_ecs::prelude::World;
use yuyib_game_3d::{
    ComputeModelBoundsError3d, Model3d, ModelBoundsRegistry3d, SceneBoundsError3d,
    SceneBoundsResult3d, SceneCollisionBuildLimits3d, SceneCollisionError3d, StaticSceneCollider3d,
    StaticSceneCollisionDraw3d, StaticSceneCollisionPrimitive3d, WorldTransform3d,
    build_static_scene_collider_3d, build_static_scene_collider_3d_from_draws_with,
    register_computed_model_bounds_3d, scene_bounds_3d,
};
use yuyib_gltf::{
    GltfAssetImporter, ImportOptions, ImportedAsset, ImportedScene, NodeIndex,
    import_scene_bytes_cached_at,
};
use yuyib_model::{Model, ModelHandle, ModelMaterialPolicy, ModelMaterialPolicyError};
use yuyib_model_assets::{
    ModelTextureLoadError, ModelTextureLoader, ModelTextureLoaderInitError, PreparedModelTextures,
};
use yuyib_scene::{SceneSelection, SceneSpawnError, SpawnedScene, spawn_scene};
use yuyib_tasks::{TaskPool, TaskPoolConfig, TaskPoolCreateError};

use crate::{
    Game3dScene, Game3dSceneError, Game3dSceneStats, Game3dShading, ModelUploadBudget3d,
    ModelUploadProgress3d,
};

/// Policies for one high-level glTF scene load.
#[derive(Clone, Debug)]
pub struct GltfSceneLoadConfig {
    import: ImportOptions,
    importer_registry_limits: ImporterRegistryLimits,
    selection: SceneSelection,
    workers: TaskPoolConfig,
    build_static_collider: bool,
    semantic_collision: GltfSceneCollisionConfig3d,
    prepare_textures: bool,
    material_policy: Option<ModelMaterialPolicy>,
    cook_cache: Option<CookCache>,
}

impl GltfSceneLoadConfig {
    /// Replaces the glTF subset and resource limits.
    #[must_use]
    pub const fn with_import_options(mut self, import: ImportOptions) -> Self {
        self.import = import;
        self
    }

    /// Replaces the source-import trust boundary used before glTF decoding.
    ///
    /// This is separate from [`ImportOptions`]: the registry limits encoded
    /// source bytes, while glTF import limits bound decoded buffers, images and
    /// geometry. Large trusted GLB files commonly need a larger
    /// [`ImporterRegistryLimits::max_source_bytes`] while retaining the other
    /// defaults.
    #[must_use]
    pub const fn with_importer_registry_limits(mut self, limits: ImporterRegistryLimits) -> Self {
        self.importer_registry_limits = limits;
        self
    }

    /// Selects the glTF root scene to spawn.
    #[must_use]
    pub const fn with_scene_selection(mut self, selection: SceneSelection) -> Self {
        self.selection = selection;
        self
    }

    /// Replaces the private bounded CPU worker-pool configuration.
    #[must_use]
    pub const fn with_task_pool(mut self, workers: TaskPoolConfig) -> Self {
        self.workers = workers;
        self
    }

    /// Enables or disables a static triangle-mesh collider build in the worker.
    #[must_use]
    pub const fn with_static_collider(mut self, enabled: bool) -> Self {
        self.build_static_collider = enabled;
        self
    }

    /// Adds worker-built semantic collision layers alongside the backward-
    /// compatible world collider selected by [`Self::with_static_collider`].
    #[must_use]
    pub fn with_semantic_collision(mut self, collision: GltfSceneCollisionConfig3d) -> Self {
        self.semantic_collision = collision;
        self
    }

    /// Enables or disables worker-side image decode/preparation.
    #[must_use]
    pub const fn with_texture_preparation(mut self, enabled: bool) -> Self {
        self.prepare_textures = enabled;
        self
    }

    /// Applies an explicit material override / fallback policy after import.
    ///
    /// The policy runs on the CPU model before texture preparation and GPU
    /// publication. Asset-specific repair belongs here rather than in renderer
    /// heuristics or silent `Material::default()` substitution.
    #[must_use]
    pub fn with_material_policy(mut self, policy: ModelMaterialPolicy) -> Self {
        self.material_policy = Some(policy);
        self
    }

    /// Enables the M3 disk cook cache for imported glTF assets.
    ///
    /// On a cache hit the worker skips glTF parse and reconstructs
    /// [`ImportedAsset`] from the cooked blob. Cached hits currently carry an
    /// empty import report (see `yuyib_gltf::import_scene_bytes_cached`).
    #[must_use]
    pub fn with_cook_cache(mut self, cache: CookCache) -> Self {
        self.cook_cache = Some(cache);
        self
    }
}

impl Default for GltfSceneLoadConfig {
    fn default() -> Self {
        Self {
            import: ImportOptions::default(),
            importer_registry_limits: ImporterRegistryLimits::default(),
            selection: SceneSelection::Default,
            workers: TaskPoolConfig::new(2, 8).expect("built-in task pool limits are positive"),
            build_static_collider: true,
            semantic_collision: GltfSceneCollisionConfig3d::default(),
            prepare_textures: true,
            material_policy: None,
            cook_cache: None,
        }
    }
}

/// Stable identifier for one imported-scene collision layer.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct GltfSceneColliderLayerId3d(String);

impl GltfSceneColliderLayerId3d {
    /// Creates a non-empty layer identifier.
    ///
    /// # Errors
    ///
    /// Returns [`GltfSceneCollisionConfigError3d::EmptyLayerId`] for an empty
    /// value. Byte-length limits are checked by the enclosing config.
    pub fn new(value: impl Into<String>) -> Result<Self, GltfSceneCollisionConfigError3d> {
        let value = value.into();
        if value.is_empty() {
            return Err(GltfSceneCollisionConfigError3d::EmptyLayerId);
        }
        Ok(Self(value))
    }

    /// Returns the application-owned stable identifier.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Supported deterministic source-name comparisons.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GltfSceneCollisionNameMatch3d {
    value: String,
    prefix: bool,
}

impl GltfSceneCollisionNameMatch3d {
    /// Matches one complete case-sensitive source name.
    ///
    /// # Errors
    ///
    /// Rejects an empty pattern.
    pub fn exact(value: impl Into<String>) -> Result<Self, GltfSceneCollisionConfigError3d> {
        Self::new(value.into(), false)
    }

    /// Matches a case-sensitive source-name prefix.
    ///
    /// # Errors
    ///
    /// Rejects an empty pattern.
    pub fn prefix(value: impl Into<String>) -> Result<Self, GltfSceneCollisionConfigError3d> {
        Self::new(value.into(), true)
    }

    fn new(value: String, prefix: bool) -> Result<Self, GltfSceneCollisionConfigError3d> {
        if value.is_empty() {
            return Err(GltfSceneCollisionConfigError3d::EmptyNamePattern);
        }
        Ok(Self { value, prefix })
    }

    fn matches(&self, candidate: Option<&str>) -> bool {
        candidate.is_some_and(|candidate| {
            if self.prefix {
                candidate.starts_with(&self.value)
            } else {
                candidate == self.value
            }
        })
    }
}

/// One typed imported-metadata predicate.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GltfSceneCollisionPredicate3d {
    /// Match only the mesh-owning glTF node name.
    NodeName(GltfSceneCollisionNameMatch3d),
    /// Match the mesh-owning node or any selected ancestor node name.
    NodeOrAncestorName(GltfSceneCollisionNameMatch3d),
    /// Match the imported model mesh name.
    MeshName(GltfSceneCollisionNameMatch3d),
    /// Match the primitive's imported material name.
    MaterialName(GltfSceneCollisionNameMatch3d),
}

/// Boolean combination used inside one flat metadata selector.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GltfSceneCollisionMatchMode3d {
    /// Every predicate must match. An empty selector matches all geometry.
    All,
    /// At least one predicate must match. An empty selector matches nothing.
    Any,
}

/// Bounded, non-recursive metadata selector for one collision layer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GltfSceneCollisionSelector3d {
    mode: GltfSceneCollisionMatchMode3d,
    predicates: Vec<GltfSceneCollisionPredicate3d>,
}

impl GltfSceneCollisionSelector3d {
    /// Selects all imported geometry.
    #[must_use]
    pub const fn all_geometry() -> Self {
        Self {
            mode: GltfSceneCollisionMatchMode3d::All,
            predicates: Vec::new(),
        }
    }

    /// Requires every supplied metadata predicate.
    #[must_use]
    pub fn all(predicates: impl IntoIterator<Item = GltfSceneCollisionPredicate3d>) -> Self {
        Self {
            mode: GltfSceneCollisionMatchMode3d::All,
            predicates: predicates.into_iter().collect(),
        }
    }

    /// Requires at least one supplied metadata predicate.
    #[must_use]
    pub fn any(predicates: impl IntoIterator<Item = GltfSceneCollisionPredicate3d>) -> Self {
        Self {
            mode: GltfSceneCollisionMatchMode3d::Any,
            predicates: predicates.into_iter().collect(),
        }
    }
}

/// Definition of one worker-built semantic collider.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GltfSceneColliderLayer3d {
    id: GltfSceneColliderLayerId3d,
    include: GltfSceneCollisionSelector3d,
    exclude: Option<GltfSceneCollisionSelector3d>,
    required: bool,
}

impl GltfSceneColliderLayer3d {
    /// Creates a required layer. A required selector that matches no valid
    /// triangles fails the scene load instead of silently publishing absence.
    #[must_use]
    pub const fn new(
        id: GltfSceneColliderLayerId3d,
        include: GltfSceneCollisionSelector3d,
    ) -> Self {
        Self {
            id,
            include,
            exclude: None,
            required: true,
        }
    }

    /// Excludes primitives matched by a second selector after inclusion.
    #[must_use]
    pub fn excluding(mut self, selector: GltfSceneCollisionSelector3d) -> Self {
        self.exclude = Some(selector);
        self
    }

    /// Allows this layer to be absent when no primitive matches.
    #[must_use]
    pub const fn optional(mut self) -> Self {
        self.required = false;
        self
    }
}

/// Trust/work limits for semantic imported-scene collision construction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GltfSceneCollisionLimits3d {
    /// Maximum semantic layers built for one loaded scene.
    pub maximum_layers: usize,
    /// Maximum predicates in either include or exclude selector.
    pub maximum_predicates_per_selector: usize,
    /// Maximum UTF-8 bytes in a layer id or name pattern.
    pub maximum_name_bytes: usize,
    /// Maximum retained triangles summed across semantic layers.
    pub maximum_total_triangles: usize,
    /// Per-layer geometry construction limits.
    pub per_layer: SceneCollisionBuildLimits3d,
}

impl Default for GltfSceneCollisionLimits3d {
    fn default() -> Self {
        Self {
            maximum_layers: 8,
            maximum_predicates_per_selector: 16,
            maximum_name_bytes: 256,
            maximum_total_triangles: 4_000_000,
            per_layer: SceneCollisionBuildLimits3d {
                maximum_source_draws: 100_000,
                maximum_primitives: 1_000_000,
                maximum_vertices: 4_000_000,
                maximum_triangles: 2_000_000,
            },
        }
    }
}

/// Validated semantic collision policy attached to a glTF load.
#[derive(Clone, Debug, Default)]
pub struct GltfSceneCollisionConfig3d {
    layers: Vec<GltfSceneColliderLayer3d>,
    limits: GltfSceneCollisionLimits3d,
}

impl GltfSceneCollisionConfig3d {
    /// Creates and validates semantic collision layers with default limits.
    ///
    /// # Errors
    ///
    /// Returns a typed config error for duplicate ids or exceeded limits.
    pub fn new(
        layers: impl IntoIterator<Item = GltfSceneColliderLayer3d>,
    ) -> Result<Self, GltfSceneCollisionConfigError3d> {
        let config = Self {
            layers: layers.into_iter().collect(),
            limits: GltfSceneCollisionLimits3d::default(),
        };
        config.validate()?;
        Ok(config)
    }

    /// Replaces and validates every semantic collision limit.
    ///
    /// # Errors
    ///
    /// Returns a typed config error for zero/exhausted limits.
    pub fn with_limits(
        mut self,
        limits: GltfSceneCollisionLimits3d,
    ) -> Result<Self, GltfSceneCollisionConfigError3d> {
        self.limits = limits;
        self.validate()?;
        Ok(self)
    }

    fn validate(&self) -> Result<(), GltfSceneCollisionConfigError3d> {
        for (field, value) in [
            ("maximum_layers", self.limits.maximum_layers),
            (
                "maximum_predicates_per_selector",
                self.limits.maximum_predicates_per_selector,
            ),
            ("maximum_name_bytes", self.limits.maximum_name_bytes),
            (
                "maximum_total_triangles",
                self.limits.maximum_total_triangles,
            ),
            (
                "per_layer.maximum_source_draws",
                self.limits.per_layer.maximum_source_draws,
            ),
            (
                "per_layer.maximum_primitives",
                self.limits.per_layer.maximum_primitives,
            ),
            (
                "per_layer.maximum_vertices",
                self.limits.per_layer.maximum_vertices,
            ),
            (
                "per_layer.maximum_triangles",
                self.limits.per_layer.maximum_triangles,
            ),
        ] {
            if value == 0 {
                return Err(GltfSceneCollisionConfigError3d::ZeroLimit(field));
            }
        }
        if self.layers.len() > self.limits.maximum_layers {
            return Err(GltfSceneCollisionConfigError3d::TooManyLayers {
                actual: self.layers.len(),
                limit: self.limits.maximum_layers,
            });
        }
        let mut ids = HashSet::new();
        for layer in &self.layers {
            if layer.id.0.len() > self.limits.maximum_name_bytes {
                return Err(GltfSceneCollisionConfigError3d::NameTooLong {
                    actual: layer.id.0.len(),
                    limit: self.limits.maximum_name_bytes,
                });
            }
            if !ids.insert(layer.id.0.as_str()) {
                return Err(GltfSceneCollisionConfigError3d::DuplicateLayerId(
                    layer.id.clone(),
                ));
            }
            for selector in std::iter::once(&layer.include).chain(layer.exclude.as_ref()) {
                if selector.predicates.len() > self.limits.maximum_predicates_per_selector {
                    return Err(GltfSceneCollisionConfigError3d::TooManyPredicates {
                        actual: selector.predicates.len(),
                        limit: self.limits.maximum_predicates_per_selector,
                    });
                }
                for predicate in &selector.predicates {
                    let matcher = match predicate {
                        GltfSceneCollisionPredicate3d::NodeName(matcher)
                        | GltfSceneCollisionPredicate3d::NodeOrAncestorName(matcher)
                        | GltfSceneCollisionPredicate3d::MeshName(matcher)
                        | GltfSceneCollisionPredicate3d::MaterialName(matcher) => matcher,
                    };
                    if matcher.value.len() > self.limits.maximum_name_bytes {
                        return Err(GltfSceneCollisionConfigError3d::NameTooLong {
                            actual: matcher.value.len(),
                            limit: self.limits.maximum_name_bytes,
                        });
                    }
                }
            }
        }
        Ok(())
    }
}

/// Invalid semantic collision configuration rejected before worker submission.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GltfSceneCollisionConfigError3d {
    /// Layer id was empty.
    EmptyLayerId,
    /// Source-name pattern was empty.
    EmptyNamePattern,
    /// A work limit was zero.
    ZeroLimit(&'static str),
    /// Layer count exceeded the configured bound.
    TooManyLayers {
        /// Configured layer count.
        actual: usize,
        /// Maximum accepted layer count.
        limit: usize,
    },
    /// Selector predicate count exceeded the configured bound.
    TooManyPredicates {
        /// Configured predicate count.
        actual: usize,
        /// Maximum accepted predicate count.
        limit: usize,
    },
    /// Identifier or pattern exceeded the configured UTF-8 byte bound.
    NameTooLong {
        /// Configured UTF-8 byte count.
        actual: usize,
        /// Maximum accepted UTF-8 byte count.
        limit: usize,
    },
    /// Two output layers used the same stable id.
    DuplicateLayerId(GltfSceneColliderLayerId3d),
}

impl fmt::Display for GltfSceneCollisionConfigError3d {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyLayerId => formatter.write_str("glTF collision layer id must not be empty"),
            Self::EmptyNamePattern => {
                formatter.write_str("glTF collision metadata pattern must not be empty")
            }
            Self::ZeroLimit(field) => {
                write!(formatter, "glTF collision limit {field} must be positive")
            }
            Self::TooManyLayers { actual, limit } => write!(
                formatter,
                "glTF collision config has {actual} layers, limit is {limit}"
            ),
            Self::TooManyPredicates { actual, limit } => write!(
                formatter,
                "glTF collision selector has {actual} predicates, limit is {limit}"
            ),
            Self::NameTooLong { actual, limit } => write!(
                formatter,
                "glTF collision metadata name has {actual} bytes, limit is {limit}"
            ),
            Self::DuplicateLayerId(id) => {
                write!(
                    formatter,
                    "duplicate glTF collision layer id {}",
                    id.as_str()
                )
            }
        }
    }
}

impl Error for GltfSceneCollisionConfigError3d {}

/// High-level scene-load phase suitable for a loading UI.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GltfSceneLoadStage {
    /// Accepted by the bounded task queue.
    Queued,
    /// Source bytes are being read.
    Reading,
    /// Import, ECS conversion, collision and texture decode are running.
    Processing,
    /// CPU scene data can be taken by the main thread.
    Ready,
    /// Worker import/preparation failed.
    Failed,
    /// The ready value has already been taken.
    Taken,
}

/// Observable CPU-side loading progress.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GltfSceneLoadProgress {
    /// Current high-level phase.
    pub stage: GltfSceneLoadStage,
    /// Completed importer-defined work units.
    pub completed_work: u64,
    /// Total importer-defined work units.
    pub total_work: u64,
}

impl GltfSceneLoadProgress {
    /// Returns normalized progress when a total is known.
    #[must_use]
    #[allow(
        clippy::cast_precision_loss,
        reason = "UI progress is approximate and counters remain available exactly"
    )]
    pub fn fraction(self) -> Option<f32> {
        if self.total_work == 0 {
            return None;
        }
        Some((self.completed_work.min(self.total_work) as f32) / self.total_work as f32)
    }
}

/// A single background glTF scene request with no exposed queue choreography.
pub struct GltfSceneLoad {
    queue: AssetLoadQueue<LoadedGltfScene, GltfSceneLoadError>,
    pool: Arc<TaskPool>,
    request: AssetLoadId,
}

impl GltfSceneLoad {
    /// Starts reading, importing, spawning and preparing one scene.
    ///
    /// `path` is read only by the owned bounded worker. External model textures
    /// remain confined below the document directory by `ModelTextureLoader`.
    ///
    /// # Errors
    ///
    /// Returns a start error when the asset root, worker pool, importer
    /// registration or bounded submission cannot be created.
    pub fn start(
        path: impl AsRef<Path>,
        config: GltfSceneLoadConfig,
    ) -> Result<Self, GltfSceneLoadStartError> {
        let pool =
            Arc::new(TaskPool::new(config.workers).map_err(GltfSceneLoadStartError::TaskPool)?);
        Self::start_on(path, config, pool)
    }

    /// Starts a scene request on an application-owned shared task pool.
    ///
    /// This is the scalable high-level path when an application streams more
    /// than one scene. The returned request retains an `Arc`, so the pool lives
    /// until every accepted scene job is complete.
    ///
    /// # Errors
    ///
    /// Returns a start error when the asset root is invalid or the shared
    /// bounded queue rejects the job.
    pub fn start_on(
        path: impl AsRef<Path>,
        config: GltfSceneLoadConfig,
        pool: Arc<TaskPool>,
    ) -> Result<Self, GltfSceneLoadStartError> {
        // Fail invalid trust-boundary configuration synchronously instead of
        // accepting a worker request which can only fail later.
        ImporterRegistry::<ImportedAsset>::new(config.importer_registry_limits)
            .map_err(GltfSceneLoadStartError::ImporterRegistryConfig)?;
        config
            .semantic_collision
            .validate()
            .map_err(GltfSceneLoadStartError::SemanticCollisionConfig)?;
        let path = path.as_ref().to_owned();
        let asset_root = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."))
            .to_owned();
        let texture_loader =
            ModelTextureLoader::new(&asset_root).map_err(GltfSceneLoadStartError::TextureLoader)?;
        let mut queue = AssetLoadQueue::new();
        let label = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("glTF scene")
            .to_owned();
        let request = queue
            .try_queue(pool.as_ref(), label, move |reporter| {
                reporter.set_total_work(5);
                reporter.reading();
                let bytes = std::fs::read(&path).map_err(|source| GltfSceneLoadError::Read {
                    path: path.clone(),
                    source,
                })?;
                reporter.advance(1);

                reporter.decoding();
                let max_source_bytes = config.importer_registry_limits.max_source_bytes;
                if bytes.len() > max_source_bytes {
                    return Err(GltfSceneLoadError::Import(RegistryImportError::SourceTooLarge {
                        actual: bytes.len(),
                        maximum: max_source_bytes,
                    }));
                }
                let (imported, mut diagnostics) = if let Some(cache) = config.cook_cache.as_ref() {
                    let (imported, cache_hit) = import_scene_bytes_cached_at(
                        &bytes,
                        &asset_root,
                        config.import,
                        cache,
                    )
                    .map_err(|error| GltfSceneLoadError::Cook(error.to_string()))?;
                    let mut diagnostics = Vec::new();
                    if cache_hit {
                        diagnostics.push(ImportDiagnostic {
                            code: "gltf-cook-cache-hit".to_owned(),
                            message: "loaded ImportedAsset from disk cook cache (parse skipped; deps verified)"
                                .to_owned(),
                            severity: yuyib_assets::ImportDiagnosticSeverity::Info,
                        });
                    }
                    (imported, diagnostics)
                } else {
                    let mut registry =
                        ImporterRegistry::<ImportedAsset>::new(config.importer_registry_limits)
                            .map_err(GltfSceneLoadError::ImporterRegistryConfig)?;
                    registry
                        .register(GltfAssetImporter::new(config.import))
                        .map_err(GltfSceneLoadError::RegisterImporter)?;
                    let imported_result = registry
                        .import(ImportSource::new(path.to_string_lossy().as_ref(), &bytes))
                        .map_err(GltfSceneLoadError::Import)?;
                    (imported_result.asset, imported_result.diagnostics)
                };
                reporter.advance(1);

                let mut world = World::new();
                let mut models = Assets::<Model>::new();
                let spawned = spawn_scene(&mut world, &mut models, &imported, config.selection)
                    .map_err(GltfSceneLoadError::Spawn)?;
                let model = spawned.model();
                if let Some(policy) = &config.material_policy {
                    let edited = models
                        .get_mut(model)
                        .ok_or(GltfSceneLoadError::MissingModel(model))?;
                    let report = policy
                        .apply(edited)
                        .map_err(GltfSceneLoadError::MaterialPolicy)?;
                    diagnostics.extend(report.into_diagnostics());
                }
                reporter.advance(1);

                let bounds =
                    scene_bounds_3d(&mut world, &models).map_err(GltfSceneLoadError::Bounds)?;
                let mut model_bounds = ModelBoundsRegistry3d::new();
                register_computed_model_bounds_3d(&mut model_bounds, &models, model)
                    .map_err(GltfSceneLoadError::ModelBounds)?;
                let collider = config
                    .build_static_collider
                    .then(|| build_static_scene_collider_3d(&mut world, &models))
                    .transpose()
                    .map_err(GltfSceneLoadError::Collision)?;
                let semantic_colliders = build_semantic_scene_colliders(
                    &world,
                    &models,
                    &spawned,
                    &imported.scene,
                    &config.semantic_collision,
                )?;
                reporter.advance(1);

                let prepared_textures = if config.prepare_textures {
                    let source = models
                        .get(model)
                        .ok_or(GltfSceneLoadError::MissingModel(model))?;
                    Some(
                        texture_loader
                            .prepare(source)
                            .map_err(GltfSceneLoadError::Textures)?,
                    )
                } else {
                    None
                };
                reporter.advance(1);

                Ok(LoadedGltfScene {
                    world,
                    models,
                    spawned,
                    bounds,
                    collider,
                    semantic_colliders,
                    prepared_textures,
                    diagnostics,
                    gpu_ready: false,
                    publication_started: false,
                    publication_shading: None,
                    model_bounds: Some(model_bounds),
                })
            })
            .map_err(GltfSceneLoadStartError::Submit)?;
        Ok(Self {
            queue,
            pool,
            request,
        })
    }

    /// Polls the worker without blocking and returns current progress.
    pub fn update(&mut self) -> GltfSceneLoadProgress {
        self.queue.poll();
        self.progress()
    }

    /// Returns current progress without polling.
    #[must_use]
    pub fn progress(&self) -> GltfSceneLoadProgress {
        let Some(info) = self.queue.info(self.request) else {
            return GltfSceneLoadProgress {
                stage: GltfSceneLoadStage::Taken,
                completed_work: 0,
                total_work: 0,
            };
        };
        let stage = match info.state {
            AssetLoadState::Queued => GltfSceneLoadStage::Queued,
            AssetLoadState::Reading => GltfSceneLoadStage::Reading,
            AssetLoadState::Decoding => GltfSceneLoadStage::Processing,
            AssetLoadState::ReadyToPublish => GltfSceneLoadStage::Ready,
            AssetLoadState::Failed => GltfSceneLoadStage::Failed,
            AssetLoadState::Published => GltfSceneLoadStage::Taken,
        };
        GltfSceneLoadProgress {
            stage,
            completed_work: info.progress.completed,
            total_work: info.progress.total,
        }
    }

    /// Takes the prepared scene once it reaches [`GltfSceneLoadStage::Ready`].
    ///
    /// # Errors
    ///
    /// Returns an explicit not-ready, failed, already-taken or unknown request
    /// error without waiting for the worker.
    pub fn take_ready(&mut self) -> Result<LoadedGltfScene, AssetLoadTakeError> {
        self.queue.take_ready(self.request)
    }

    /// Returns the retained worker/import failure, if any.
    #[must_use]
    pub fn failure(&self) -> Option<&AssetLoadFailure<GltfSceneLoadError>> {
        self.queue.failure(self.request)
    }

    /// Returns the number of private worker threads owned by this request.
    #[must_use]
    pub fn worker_count(&self) -> usize {
        self.pool.config().workers()
    }
}

fn build_semantic_scene_colliders(
    world: &World,
    models: &Assets<Model>,
    spawned: &SpawnedScene,
    scene: &ImportedScene,
    config: &GltfSceneCollisionConfig3d,
) -> Result<Vec<(GltfSceneColliderLayerId3d, StaticSceneCollider3d)>, GltfSceneLoadError> {
    if config.layers.is_empty() {
        return Ok(Vec::new());
    }
    let draws = spawned
        .entities()
        .filter_map(|(source, entity)| {
            let model = world.get::<Model3d>(entity)?;
            Some((source, entity, *model))
        })
        .map(|(source, entity, model)| {
            let transform = world
                .get::<WorldTransform3d>(entity)
                .ok_or(GltfSceneLoadError::SemanticCollisionMissingTransform { node: source })?;
            Ok(StaticSceneCollisionDraw3d::new(
                source.get(),
                model.model,
                model.mesh,
                transform.column_major(),
            ))
        })
        .collect::<Result<Vec<_>, GltfSceneLoadError>>()?;
    let parents = imported_node_parents(scene);
    let mut result = Vec::with_capacity(config.layers.len());
    let mut total_triangles = 0_usize;
    for layer in &config.layers {
        let remaining_triangles = config
            .limits
            .maximum_total_triangles
            .saturating_sub(total_triangles);
        let mut layer_limits = config.limits.per_layer;
        layer_limits.maximum_triangles = layer_limits.maximum_triangles.min(remaining_triangles);
        let collider = build_static_scene_collider_3d_from_draws_with(
            &draws,
            models,
            layer_limits,
            |primitive| semantic_layer_selects(layer, primitive, scene, &parents),
        );
        let collider = match collider {
            Ok(collider) => collider,
            Err(SceneCollisionError3d::EmptyScene) if !layer.required => continue,
            Err(SceneCollisionError3d::LimitExceeded {
                resource: yuyib_game_3d::SceneCollisionLimitResource3d::Triangles,
                ..
            }) if remaining_triangles < config.limits.per_layer.maximum_triangles => {
                return Err(GltfSceneLoadError::SemanticCollisionTotalTriangles {
                    limit: config.limits.maximum_total_triangles,
                });
            }
            Err(source) => {
                return Err(GltfSceneLoadError::SemanticCollision {
                    layer: layer.id.clone(),
                    source,
                });
            }
        };
        total_triangles = total_triangles
            .checked_add(collider.triangle_count())
            .ok_or(GltfSceneLoadError::SemanticCollisionTotalTriangles {
                limit: config.limits.maximum_total_triangles,
            })?;
        if total_triangles > config.limits.maximum_total_triangles {
            return Err(GltfSceneLoadError::SemanticCollisionTotalTriangles {
                limit: config.limits.maximum_total_triangles,
            });
        }
        result.push((layer.id.clone(), collider));
    }
    Ok(result)
}

fn imported_node_parents(scene: &ImportedScene) -> Vec<Option<usize>> {
    let mut parents = vec![None; scene.nodes().len()];
    for (parent, node) in scene.nodes().iter().enumerate() {
        for child in node.children() {
            if let Some(slot) = parents.get_mut(child.get()) {
                *slot = Some(parent);
            }
        }
    }
    parents
}

fn semantic_layer_selects(
    layer: &GltfSceneColliderLayer3d,
    primitive: StaticSceneCollisionPrimitive3d<'_>,
    scene: &ImportedScene,
    parents: &[Option<usize>],
) -> bool {
    semantic_selector_matches(&layer.include, primitive, scene, parents)
        && !layer
            .exclude
            .as_ref()
            .is_some_and(|exclude| semantic_selector_matches(exclude, primitive, scene, parents))
}

fn semantic_selector_matches(
    selector: &GltfSceneCollisionSelector3d,
    primitive: StaticSceneCollisionPrimitive3d<'_>,
    scene: &ImportedScene,
    parents: &[Option<usize>],
) -> bool {
    let matches = |predicate: &GltfSceneCollisionPredicate3d| match predicate {
        GltfSceneCollisionPredicate3d::NodeName(pattern) => pattern.matches(
            scene
                .nodes()
                .get(primitive.source_id)
                .and_then(yuyib_gltf::ImportedNode::name),
        ),
        GltfSceneCollisionPredicate3d::NodeOrAncestorName(pattern) => {
            let mut node = Some(primitive.source_id);
            let mut matched = false;
            while let Some(index) = node {
                if pattern.matches(
                    scene
                        .nodes()
                        .get(index)
                        .and_then(yuyib_gltf::ImportedNode::name),
                ) {
                    matched = true;
                    break;
                }
                node = parents.get(index).copied().flatten();
            }
            matched
        }
        GltfSceneCollisionPredicate3d::MeshName(pattern) => pattern.matches(primitive.mesh_name),
        GltfSceneCollisionPredicate3d::MaterialName(pattern) => {
            pattern.matches(primitive.material_name)
        }
    };
    match selector.mode {
        GltfSceneCollisionMatchMode3d::All => selector.predicates.iter().all(matches),
        GltfSceneCollisionMatchMode3d::Any => selector.predicates.iter().any(matches),
    }
}

/// Main-thread scene value produced by [`GltfSceneLoad`].
pub struct LoadedGltfScene {
    world: World,
    models: Assets<Model>,
    spawned: SpawnedScene,
    bounds: SceneBoundsResult3d,
    collider: Option<StaticSceneCollider3d>,
    semantic_colliders: Vec<(GltfSceneColliderLayerId3d, StaticSceneCollider3d)>,
    prepared_textures: Option<PreparedModelTextures>,
    diagnostics: Vec<ImportDiagnostic>,
    gpu_ready: bool,
    publication_started: bool,
    publication_shading: Option<Game3dShading>,
    model_bounds: Option<ModelBoundsRegistry3d>,
}

/// A loaded scene rejected a CPU model edit at an unsafe lifecycle point.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LoadedGltfSceneModelEditError {
    /// GPU publication has started; changing material metadata would desync
    /// already-created pipelines or bindings.
    PublicationStarted,
    /// Internal typed model storage lost the handle published with the scene.
    MissingModel,
}

impl fmt::Display for LoadedGltfSceneModelEditError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PublicationStarted => formatter
                .write_str("loaded glTF model can only be edited before GPU publication starts"),
            Self::MissingModel => formatter.write_str("loaded glTF scene model is missing"),
        }
    }
}

impl Error for LoadedGltfSceneModelEditError {}

/// Material policy could not be applied to a loaded scene.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LoadedGltfSceneMaterialPolicyError {
    /// Scene lifecycle rejected the CPU edit.
    Lifecycle(LoadedGltfSceneModelEditError),
    /// Declarative policy failed validation.
    Policy(ModelMaterialPolicyError),
}

impl fmt::Display for LoadedGltfSceneMaterialPolicyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Lifecycle(error) => write!(formatter, "{error}"),
            Self::Policy(error) => write!(formatter, "{error}"),
        }
    }
}

impl Error for LoadedGltfSceneMaterialPolicyError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Lifecycle(error) => Some(error),
            Self::Policy(error) => Some(error),
        }
    }
}

impl From<LoadedGltfSceneModelEditError> for LoadedGltfSceneMaterialPolicyError {
    fn from(value: LoadedGltfSceneModelEditError) -> Self {
        Self::Lifecycle(value)
    }
}

impl LoadedGltfScene {
    /// ECS world containing the selected imported scene.
    #[must_use]
    pub const fn world(&self) -> &World {
        &self.world
    }

    /// Mutable ECS world for game-specific entities and components.
    #[must_use]
    pub const fn world_mut(&mut self) -> &mut World {
        &mut self.world
    }

    /// Typed model storage referenced by the spawned ECS components.
    #[must_use]
    pub const fn models(&self) -> &Assets<Model> {
        &self.models
    }

    /// Returns the scene's CPU model for validated post-import edits.
    ///
    /// Call this after loading and before the first [`Self::prepare_for_frame`].
    /// The returned [`Model`] exposes validated material-edit operations while
    /// keeping geometry storage private. Once publication begins the method
    /// rejects edits instead of leaving GPU caches stale.
    ///
    /// Prefer [`GltfSceneLoadConfig::with_material_policy`] for declarative
    /// asset repair so importer diagnostics and policy diagnostics share one
    /// retained report.
    ///
    /// # Errors
    ///
    /// Returns [`LoadedGltfSceneModelEditError::PublicationStarted`] after the
    /// first GPU preparation call, or `MissingModel` if an internal invariant
    /// was broken.
    pub fn model_mut_before_publication(
        &mut self,
    ) -> Result<&mut Model, LoadedGltfSceneModelEditError> {
        if self.publication_started {
            return Err(LoadedGltfSceneModelEditError::PublicationStarted);
        }
        let handle = self.spawned.model();
        self.models
            .get_mut(handle)
            .ok_or(LoadedGltfSceneModelEditError::MissingModel)
    }

    /// Applies a material policy before GPU publication and retains diagnostics.
    ///
    /// # Errors
    ///
    /// Returns a lifecycle error when publication has started, or a policy
    /// error when a named material or mesh/primitive reference is invalid.
    pub fn apply_material_policy(
        &mut self,
        policy: &ModelMaterialPolicy,
    ) -> Result<(), LoadedGltfSceneMaterialPolicyError> {
        let model = self.model_mut_before_publication()?;
        let report = policy
            .apply(model)
            .map_err(LoadedGltfSceneMaterialPolicyError::Policy)?;
        self.diagnostics.extend(report.into_diagnostics());
        Ok(())
    }

    /// Importer and material-policy diagnostics retained with the scene.
    #[must_use]
    pub fn diagnostics(&self) -> &[ImportDiagnostic] {
        &self.diagnostics
    }

    /// Formats retained diagnostics for logs, loading UI or smoke scripts.
    ///
    /// This is the high-level observation path for import/policy issues. Empty
    /// when the importer and policy produced no diagnostics.
    #[must_use]
    pub fn diagnostics_summary(&self) -> String {
        if self.diagnostics.is_empty() {
            return String::new();
        }
        let mut summary = String::new();
        for diagnostic in &self.diagnostics {
            use std::fmt::Write as _;
            let _ = writeln!(
                summary,
                "{:?}: {} — {}",
                diagnostic.severity, diagnostic.code, diagnostic.message
            );
        }
        summary
    }

    /// High-level material binding inventory for the current CPU model.
    ///
    /// Prefer this over manually walking meshes when answering which primitives
    /// still use a broken fallback material such as `material_0`.
    #[must_use]
    pub fn material_usage(
        &self,
    ) -> Result<yuyib_model::ModelMaterialUsage, LoadedGltfSceneModelEditError> {
        let handle = self.spawned.model();
        self.models
            .get(handle)
            .map(yuyib_model::Model::material_usage)
            .ok_or(LoadedGltfSceneModelEditError::MissingModel)
    }

    /// Formats [`Self::material_usage`] for logs and demos.
    #[must_use]
    pub fn material_usage_summary(&self) -> Result<String, LoadedGltfSceneModelEditError> {
        Ok(self.material_usage()?.summary())
    }

    /// High-level texture binding inventory for the current CPU model.
    #[must_use]
    pub fn texture_usage(
        &self,
    ) -> Result<yuyib_model::ModelTextureUsage, LoadedGltfSceneModelEditError> {
        let handle = self.spawned.model();
        self.models
            .get(handle)
            .map(yuyib_model::Model::texture_usage)
            .ok_or(LoadedGltfSceneModelEditError::MissingModel)
    }

    /// Formats [`Self::texture_usage`] for logs and demos.
    #[must_use]
    pub fn texture_usage_summary(&self) -> Result<String, LoadedGltfSceneModelEditError> {
        Ok(self.texture_usage()?.summary())
    }

    /// Result of mapping the selected glTF scene to ECS.
    #[must_use]
    pub const fn spawned(&self) -> &SpawnedScene {
        &self.spawned
    }

    /// Shared model handle used by imported mesh nodes.
    #[must_use]
    pub const fn model(&self) -> ModelHandle {
        self.spawned.model()
    }

    /// Renderer-neutral scene bounds calculated in the worker.
    #[must_use]
    pub const fn bounds(&self) -> SceneBoundsResult3d {
        self.bounds
    }

    /// Optional static triangle collider requested by the load policy.
    #[must_use]
    pub const fn collider(&self) -> Option<&StaticSceneCollider3d> {
        self.collider.as_ref()
    }

    /// Returns one worker-built semantic collider by typed layer id.
    #[must_use]
    pub fn collider_layer(
        &self,
        id: &GltfSceneColliderLayerId3d,
    ) -> Option<&StaticSceneCollider3d> {
        self.semantic_colliders
            .iter()
            .find_map(|(candidate, collider)| (candidate == id).then_some(collider))
    }

    /// Iterates worker-built semantic colliders in config order.
    pub fn collider_layers(
        &self,
    ) -> impl Iterator<Item = (&GltfSceneColliderLayerId3d, &StaticSceneCollider3d)> {
        self.semantic_colliders
            .iter()
            .map(|(id, collider)| (id, collider))
    }

    /// Publishes decoded textures and geometry with the engine's balanced default budget.
    ///
    /// # Errors
    ///
    /// Returns a high-level scene error for unsupported shading, stale models,
    /// GPU upload or material incompatibility.
    pub fn prepare_for_frame(
        &mut self,
        frame: &yuyib_render::RenderFrame<'_>,
        renderer: &mut Game3dScene,
    ) -> Result<GltfSceneGpuProgress, Game3dSceneError> {
        self.prepare_for_frame_with_budget(frame, renderer, ModelUploadBudget3d::default())
    }

    /// Publishes decoded textures and geometry within an explicit frame budget.
    ///
    /// Use this only when profiling shows the balanced defaults are unsuitable;
    /// [`Self::prepare_for_frame`] is the intended high-level path.
    ///
    /// # Errors
    ///
    /// Returns a high-level scene error for unsupported shading, stale models,
    /// GPU upload or material incompatibility.
    pub fn prepare_for_frame_with_budget(
        &mut self,
        frame: &yuyib_render::RenderFrame<'_>,
        renderer: &mut Game3dScene,
        budget: ModelUploadBudget3d,
    ) -> Result<GltfSceneGpuProgress, Game3dSceneError> {
        self.publication_started = true;
        if let Some(bounds) = self.model_bounds.take() {
            renderer.extend_model_bounds(bounds);
        }
        let current_shading = renderer.shading();
        if let Some(prepared) = self.publication_shading
            && prepared != current_shading
        {
            return Err(Game3dSceneError::PreparedShadingChanged {
                prepared,
                current: current_shading,
            });
        }
        if self.publication_shading.is_none()
            && matches!(current_shading, Game3dShading::Lambert | Game3dShading::Pbr)
        {
            self.publication_shading = Some(current_shading);
        }
        if self.gpu_ready {
            return Ok(renderer
                .prepare_model_for_frame_with_budget(frame, &self.models, self.model(), budget)?
                .into());
        }
        if let Some(prepared) = self.prepared_textures.take() {
            renderer.queue_prepared_model(self.model(), prepared);
        }
        let progress = renderer.prepare_model_for_frame_with_budget(
            frame,
            &self.models,
            self.model(),
            budget,
        )?;
        self.gpu_ready = progress.ready;
        Ok(progress.into())
    }

    /// Extracts and draws the resident scene through the standard facade.
    ///
    /// # Errors
    ///
    /// Returns [`LoadedGltfSceneRenderError::NotGpuReady`] until bounded
    /// publication completes, or forwards high-level scene rendering failures.
    pub fn render(
        &mut self,
        frame: &mut yuyib_render::RenderFrame<'_>,
        renderer: &mut Game3dScene,
    ) -> Result<Game3dSceneStats, LoadedGltfSceneRenderError> {
        if !self.gpu_ready {
            return Err(LoadedGltfSceneRenderError::NotGpuReady);
        }
        renderer
            .render(frame, &mut self.world, &self.models)
            .map_err(|error| LoadedGltfSceneRenderError::Scene(Box::new(error)))
    }
}

/// Bounded render-thread residency progress.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct GltfSceneGpuProgress {
    /// Whether the model is fully resident.
    pub ready: bool,
    /// Texture slots already uploaded.
    pub completed_texture_slots: usize,
    /// Total decoded texture slots.
    pub total_texture_slots: usize,
    /// Unique decoded texture bytes uploaded by this call.
    pub uploaded_texture_bytes: u64,
    /// Whether this frame uploaded one texture larger than the byte target.
    pub uploaded_oversized_texture: bool,
    /// Mesh primitives already uploaded.
    pub completed_primitives: usize,
    /// Total mesh primitives in the model.
    pub total_primitives: usize,
    /// Source geometry bytes represented by resident primitives.
    pub completed_geometry_bytes: u64,
    /// Total source geometry bytes in the model.
    pub total_geometry_bytes: u64,
    /// Whether this frame uploaded one primitive larger than the byte target.
    pub uploaded_oversized_primitive: bool,
}

impl From<ModelUploadProgress3d> for GltfSceneGpuProgress {
    fn from(progress: ModelUploadProgress3d) -> Self {
        Self {
            ready: progress.ready,
            completed_texture_slots: progress.completed_texture_slots,
            total_texture_slots: progress.total_texture_slots,
            uploaded_texture_bytes: progress.uploaded_texture_bytes,
            uploaded_oversized_texture: progress.uploaded_oversized_texture,
            completed_primitives: progress.completed_primitives,
            total_primitives: progress.total_primitives,
            completed_geometry_bytes: progress.completed_geometry_bytes,
            total_geometry_bytes: progress.total_geometry_bytes,
            uploaded_oversized_primitive: progress.uploaded_oversized_primitive,
        }
    }
}

impl GltfSceneGpuProgress {
    /// Returns normalized GPU publication progress.
    #[must_use]
    #[allow(
        clippy::cast_precision_loss,
        reason = "UI progress is approximate and counters remain available exactly"
    )]
    pub fn fraction(self) -> f32 {
        let completed = self
            .completed_texture_slots
            .saturating_add(self.completed_primitives);
        let total = self
            .total_texture_slots
            .saturating_add(self.total_primitives);
        if total == 0 {
            return if self.ready { 1.0 } else { 0.0 };
        }
        completed.min(total) as f32 / total as f32
    }
}

/// Failure before a background scene request can be accepted.
#[derive(Debug)]
pub enum GltfSceneLoadStartError {
    /// Importer-registry trust-boundary limits are invalid.
    ImporterRegistryConfig(ImporterRegistryConfigError),
    /// Semantic collision layers or limits are invalid.
    SemanticCollisionConfig(GltfSceneCollisionConfigError3d),
    /// External texture root is missing or invalid.
    TextureLoader(ModelTextureLoaderInitError),
    /// Worker threads could not be created.
    TaskPool(TaskPoolCreateError),
    /// The bounded worker queue rejected the request.
    Submit(AssetLoadSubmitError),
}

impl fmt::Display for GltfSceneLoadStartError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ImporterRegistryConfig(error) => {
                write!(formatter, "invalid scene importer limits: {error}")
            }
            Self::SemanticCollisionConfig(error) => {
                write!(formatter, "invalid semantic collision config: {error}")
            }
            Self::TextureLoader(error) => {
                write!(formatter, "cannot create texture resolver: {error}")
            }
            Self::TaskPool(error) => write!(formatter, "cannot create scene workers: {error}"),
            Self::Submit(error) => write!(formatter, "cannot submit scene load: {error}"),
        }
    }
}

impl Error for GltfSceneLoadStartError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::ImporterRegistryConfig(error) => Some(error),
            Self::SemanticCollisionConfig(error) => Some(error),
            Self::TextureLoader(error) => Some(error),
            Self::TaskPool(error) => Some(error),
            Self::Submit(error) => Some(error),
        }
    }
}

/// Structured background read/import/scene-preparation failure.
#[derive(Debug)]
pub enum GltfSceneLoadError {
    /// Source document read failed.
    Read {
        /// Source path.
        path: PathBuf,
        /// Underlying I/O error.
        source: std::io::Error,
    },
    /// Importer-registry limits became invalid before worker processing.
    ImporterRegistryConfig(ImporterRegistryConfigError),
    /// Built-in importer registration failed.
    RegisterImporter(yuyib_assets::ImporterRegistrationError),
    /// Typed importer registry rejected or failed the source.
    Import(RegistryImportError),
    /// Disk cook cache lookup, encode, or decode failed.
    Cook(String),
    /// Selected scene could not be represented in ECS.
    Spawn(SceneSpawnError),
    /// Scene bounds could not be calculated.
    Bounds(SceneBoundsError3d),
    /// Model-local culling bounds could not be calculated.
    ModelBounds(ComputeModelBoundsError3d),
    /// Static collision preparation failed.
    Collision(SceneCollisionError3d),
    /// A mesh node selected for semantic collision lost its world transform.
    SemanticCollisionMissingTransform {
        /// Imported node missing the required derived transform.
        node: NodeIndex,
    },
    /// One named semantic collider could not be built atomically.
    SemanticCollision {
        /// Stable application-owned output layer id.
        layer: GltfSceneColliderLayerId3d,
        /// Geometry or explicit per-layer limit failure.
        source: SceneCollisionError3d,
    },
    /// Sum of successful semantic layer triangles exceeded its global bound.
    SemanticCollisionTotalTriangles {
        /// Configured maximum across all semantic colliders.
        limit: usize,
    },
    /// Spawned model handle unexpectedly became stale.
    MissingModel(ModelHandle),
    /// Image decode/preparation failed.
    Textures(ModelTextureLoadError),
    /// Explicit material override / fallback policy failed.
    MaterialPolicy(ModelMaterialPolicyError),
}

impl fmt::Display for GltfSceneLoadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read { path, source } => {
                write!(
                    formatter,
                    "cannot read glTF scene {}: {source}",
                    path.display()
                )
            }
            Self::ImporterRegistryConfig(error) => {
                write!(formatter, "invalid scene importer limits: {error}")
            }
            Self::RegisterImporter(error) => {
                write!(formatter, "cannot register glTF importer: {error}")
            }
            Self::Import(error) => write!(formatter, "cannot import glTF scene: {error}"),
            Self::Cook(message) => write!(formatter, "glTF cook cache failed: {message}"),
            Self::Spawn(error) => write!(formatter, "cannot spawn glTF scene: {error}"),
            Self::Bounds(error) => write!(formatter, "cannot calculate scene bounds: {error}"),
            Self::ModelBounds(error) => {
                write!(formatter, "cannot calculate model-local bounds: {error}")
            }
            Self::Collision(error) => {
                write!(formatter, "cannot build static scene collision: {error}")
            }
            Self::SemanticCollisionMissingTransform { node } => write!(
                formatter,
                "semantic collision node {} has no resolved world transform",
                node.get()
            ),
            Self::SemanticCollision { layer, source } => write!(
                formatter,
                "cannot build semantic collision layer {}: {source}",
                layer.as_str()
            ),
            Self::SemanticCollisionTotalTriangles { limit } => write!(
                formatter,
                "semantic collision layers exceed total triangle limit {limit}"
            ),
            Self::MissingModel(model) => {
                write!(formatter, "spawned model handle is stale: {model:?}")
            }
            Self::Textures(error) => write!(formatter, "cannot prepare model textures: {error}"),
            Self::MaterialPolicy(error) => {
                write!(formatter, "cannot apply material policy: {error}")
            }
        }
    }
}

impl Error for GltfSceneLoadError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Read { source, .. } => Some(source),
            Self::ImporterRegistryConfig(error) => Some(error),
            Self::RegisterImporter(error) => Some(error),
            Self::Import(error) => Some(error),
            Self::Cook(_) => None,
            Self::Spawn(error) => Some(error),
            Self::Bounds(error) => Some(error),
            Self::ModelBounds(error) => Some(error),
            Self::Collision(error) => Some(error),
            Self::SemanticCollision { source, .. } => Some(source),
            Self::Textures(error) => Some(error),
            Self::MaterialPolicy(error) => Some(error),
            Self::SemanticCollisionMissingTransform { .. }
            | Self::SemanticCollisionTotalTriangles { .. }
            | Self::MissingModel(_) => None,
        }
    }
}

/// Drawing was attempted before residency or failed inside `Game3dScene`.
#[derive(Debug)]
pub enum LoadedGltfSceneRenderError {
    /// [`LoadedGltfScene::prepare_for_frame`] has not completed.
    NotGpuReady,
    /// High-level scene extraction/rendering failed.
    Scene(Box<Game3dSceneError>),
}

impl fmt::Display for LoadedGltfSceneRenderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotGpuReady => formatter.write_str("glTF scene GPU residency is not ready"),
            Self::Scene(error) => write!(formatter, "cannot render loaded glTF scene: {error}"),
        }
    }
}

impl Error for LoadedGltfSceneRenderError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Scene(error) => Some(error),
            Self::NotGpuReady => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    fn triangle_glb() -> Vec<u8> {
        let mut binary = Vec::new();
        binary.extend([0_u16, 1, 2].into_iter().flat_map(u16::to_le_bytes));
        binary.extend([0_u8; 2]);
        for position in [[0.0_f32, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]] {
            binary.extend(position.into_iter().flat_map(f32::to_le_bytes));
        }
        let json = br#"{"asset":{"version":"2.0"},"scene":0,"scenes":[{"nodes":[0]}],"nodes":[{"name":"street_level","children":[1]},{"name":"road_node","mesh":0}],"buffers":[{"byteLength":44}],"bufferViews":[{"buffer":0,"byteOffset":0,"byteLength":6},{"buffer":0,"byteOffset":8,"byteLength":36}],"accessors":[{"bufferView":0,"componentType":5123,"count":3,"type":"SCALAR"},{"bufferView":1,"componentType":5126,"count":3,"type":"VEC3","min":[0,0,0],"max":[1,1,0]}],"materials":[{"name":"road_surface"}],"meshes":[{"name":"road_mesh","primitives":[{"attributes":{"POSITION":1},"indices":0,"material":0}]}]}"#;
        let mut padded_json = json.to_vec();
        while !padded_json.len().is_multiple_of(4) {
            padded_json.push(b' ');
        }
        while !binary.len().is_multiple_of(4) {
            binary.push(0);
        }
        let total = 12 + 8 + padded_json.len() + 8 + binary.len();
        let mut glb = Vec::with_capacity(total);
        glb.extend(b"glTF");
        glb.extend(2_u32.to_le_bytes());
        glb.extend(
            u32::try_from(total)
                .expect("fixture length fits u32")
                .to_le_bytes(),
        );
        glb.extend(
            u32::try_from(padded_json.len())
                .expect("fixture JSON length fits u32")
                .to_le_bytes(),
        );
        glb.extend(0x4E4F_534A_u32.to_le_bytes());
        glb.extend(padded_json);
        glb.extend(
            u32::try_from(binary.len())
                .expect("fixture binary length fits u32")
                .to_le_bytes(),
        );
        glb.extend(0x004E_4942_u32.to_le_bytes());
        glb.extend(binary);
        glb
    }

    #[test]
    fn progress_fraction_is_bounded_and_explicit() {
        let progress = GltfSceneLoadProgress {
            stage: GltfSceneLoadStage::Processing,
            completed_work: 7,
            total_work: 5,
        };
        assert_eq!(progress.fraction(), Some(1.0));
        assert_eq!(
            GltfSceneLoadProgress {
                stage: GltfSceneLoadStage::Queued,
                completed_work: 0,
                total_work: 0,
            }
            .fraction(),
            None
        );
    }

    #[test]
    fn gpu_progress_handles_textureless_scene() {
        let fraction = GltfSceneGpuProgress {
            ready: true,
            ..GltfSceneGpuProgress::default()
        }
        .fraction();
        assert!((fraction - 1.0).abs() <= f32::EPSILON);
    }

    #[test]
    fn gpu_progress_remains_monotonic_across_texture_and_geometry_phases() {
        let textures = GltfSceneGpuProgress {
            completed_texture_slots: 2,
            total_texture_slots: 4,
            total_primitives: 8,
            ..GltfSceneGpuProgress::default()
        };
        let geometry = GltfSceneGpuProgress {
            completed_texture_slots: 4,
            total_texture_slots: 4,
            completed_primitives: 1,
            total_primitives: 8,
            ..GltfSceneGpuProgress::default()
        };

        assert!(textures.fraction() < geometry.fraction());
        assert!(geometry.fraction() < 1.0);
    }

    #[test]
    fn invalid_registry_limits_fail_before_worker_submission() {
        let pool = Arc::new(
            TaskPool::new(TaskPoolConfig::new(1, 1).expect("valid test pool limits"))
                .expect("create test worker"),
        );
        let config =
            GltfSceneLoadConfig::default().with_importer_registry_limits(ImporterRegistryLimits {
                max_source_bytes: 0,
                ..ImporterRegistryLimits::default()
            });
        let error = GltfSceneLoad::start_on("ignored.glb", config, pool)
            .err()
            .expect("zero source limit must fail synchronously");

        assert!(matches!(
            error,
            GltfSceneLoadStartError::ImporterRegistryConfig(
                ImporterRegistryConfigError::ZeroLimit("max_source_bytes")
            )
        ));
    }

    #[test]
    fn high_level_load_builds_scene_bounds_and_collider_off_thread() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("test clock follows Unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "yuyib-streamed-scene-{}-{unique}.glb",
            std::process::id()
        ));
        std::fs::write(&path, triangle_glb()).expect("write temporary GLB fixture");
        let mut loading = GltfSceneLoad::start(&path, GltfSceneLoadConfig::default())
            .expect("start high-level scene load");
        for _ in 0..10_000 {
            match loading.update().stage {
                GltfSceneLoadStage::Ready => {
                    let loaded = loading.take_ready().expect("take ready scene once");
                    assert!(loaded.models().get(loaded.model()).is_some());
                    assert!(matches!(loaded.bounds(), SceneBoundsResult3d::Bounds(_)));
                    assert!(loaded.collider().is_some());
                    std::fs::remove_file(&path).expect("remove temporary GLB fixture");
                    return;
                }
                GltfSceneLoadStage::Failed => {
                    panic!("scene load failed: {:?}", loading.failure())
                }
                GltfSceneLoadStage::Queued
                | GltfSceneLoadStage::Reading
                | GltfSceneLoadStage::Processing
                | GltfSceneLoadStage::Taken => std::thread::yield_now(),
            }
        }
        let _ = std::fs::remove_file(path);
        panic!("high-level scene load did not finish");
    }

    #[test]
    fn high_level_load_builds_required_and_optional_semantic_collision_layers() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("test clock follows Unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "yuyib-semantic-collision-{}-{unique}.glb",
            std::process::id()
        ));
        std::fs::write(&path, triangle_glb()).expect("write semantic GLB fixture");
        let ground_id = GltfSceneColliderLayerId3d::new("ground").expect("valid stable layer id");
        let optional_id =
            GltfSceneColliderLayerId3d::new("optional_water").expect("valid stable layer id");
        let street =
            GltfSceneCollisionNameMatch3d::exact("street_level").expect("valid node-name matcher");
        let absent =
            GltfSceneCollisionNameMatch3d::prefix("water_").expect("valid absent mesh matcher");
        let collision = GltfSceneCollisionConfig3d::new([
            GltfSceneColliderLayer3d::new(
                ground_id.clone(),
                GltfSceneCollisionSelector3d::any([
                    GltfSceneCollisionPredicate3d::NodeOrAncestorName(street),
                ]),
            ),
            GltfSceneColliderLayer3d::new(
                optional_id.clone(),
                GltfSceneCollisionSelector3d::any([GltfSceneCollisionPredicate3d::MeshName(
                    absent,
                )]),
            )
            .optional(),
        ])
        .expect("bounded semantic layer config");
        let config = GltfSceneLoadConfig::default()
            .with_static_collider(false)
            .with_semantic_collision(collision);
        let mut loading = GltfSceneLoad::start(&path, config).expect("start semantic scene load");
        for _ in 0..10_000 {
            match loading.update().stage {
                GltfSceneLoadStage::Ready => {
                    let loaded = loading.take_ready().expect("take semantic scene once");
                    assert!(loaded.collider().is_none());
                    assert_eq!(
                        loaded
                            .collider_layer(&ground_id)
                            .expect("required ground layer")
                            .triangle_count(),
                        1
                    );
                    assert!(loaded.collider_layer(&optional_id).is_none());
                    assert_eq!(loaded.collider_layers().count(), 1);
                    std::fs::remove_file(&path).expect("remove semantic GLB fixture");
                    return;
                }
                GltfSceneLoadStage::Failed => {
                    panic!("semantic scene load failed: {:?}", loading.failure())
                }
                GltfSceneLoadStage::Queued
                | GltfSceneLoadStage::Reading
                | GltfSceneLoadStage::Processing
                | GltfSceneLoadStage::Taken => std::thread::yield_now(),
            }
        }
        let _ = std::fs::remove_file(path);
        panic!("semantic scene load did not finish");
    }

    #[test]
    fn semantic_collision_config_rejects_duplicate_layer_ids() {
        let id = GltfSceneColliderLayerId3d::new("world").expect("valid layer id");
        let layer =
            GltfSceneColliderLayer3d::new(id.clone(), GltfSceneCollisionSelector3d::all_geometry());
        assert!(matches!(
            GltfSceneCollisionConfig3d::new([layer.clone(), layer]),
            Err(GltfSceneCollisionConfigError3d::DuplicateLayerId(duplicate))
                if duplicate == id
        ));
    }
}
