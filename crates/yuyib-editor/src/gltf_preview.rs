//! Production glTF preview session for the Editor viewport.
//!
//! Uses [`GltfSceneLoad`] — the same importer/cook path as runtime examples.
//! There is no editor-only decoder.

use std::{
    error::Error,
    fmt,
    path::{Path, PathBuf},
};

use yuyib_assets::{CookCache, ImporterRegistryLimits};
use uuid::Uuid;
use yuyib_authoring::{
    AssetGuid, CapabilityId, ContentHash, ImportSettingsSchemaId, PreviewAdapter, PreviewArtifact,
    PreviewCache, PreviewCacheError, PreviewCacheKey, PreviewCancellation, PreviewMaterialOverride,
    PreviewRequest, SchemaVersion,
};
use yuyib_editor_core::hash_asset_file;
use yuyib_game_3d::{
    LocalTransform3d, Model3d, SceneBounds3d, SceneBoundsResult3d, propagate_world_transforms,
};
use yuyib_gltf::{AnimationClipIndex, AnimationPlayer, LocalTransform};
use yuyib_gltf_authoring::{
    GLTF_IMPORT_SETTINGS_SCHEMA, GltfPreviewAdapter, default_settings_json, parse_import_settings,
};
use yuyib_render::RenderFrame;
use yuyib_render_3d::{
    DepthLoad, Game3dScene, Game3dSceneStats, GltfAnimationPreviewGpu, GltfSceneGpuProgress,
    GltfSceneLoad, GltfSceneLoadConfig, GltfSceneLoadStage, LoadedGltfScene,
    LoadedGltfSceneRenderError, ModelUploadBudget3d,
};
use yuyib_model::{
    MaterialFactorPatch, MaterialIndex, ModelTextureIndex, NormalTextureBinding, TextureBinding,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Trusted project assets may be large (city maps, characters).
const EDITOR_PREVIEW_MAX_SOURCE_BYTES: usize = 256 * 1024 * 1024;

/// Faster GPU residency for interactive editor preview than the default stream budget.
const EDITOR_UPLOAD_BUDGET: ModelUploadBudget3d = ModelUploadBudget3d {
    maximum_texture_slots: 32,
    target_texture_bytes: 64 * 1024 * 1024,
    maximum_primitives: 64,
    target_geometry_bytes: 64 * 1024 * 1024,
};

/// DNS namespace UUID for stable synthetic preview AssetGuids (untracked sources).
const PREVIEW_ASSET_NAMESPACE: Uuid = Uuid::from_bytes([
    0x79, 0x75, 0x79, 0x69, 0x62, 0x2e, 0x70, 0x72, 0x65, 0x76, 0x69, 0x65, 0x77, 0x2e, 0x61, 0x73,
]);

/// Multi-entry host parking for CPU-ready glTF preview scenes.
///
/// Keeps decoded [`LoadedGltfScene`] values under the adapter [`PreviewCachePolicy`]
/// so switching A→B→A can restore without re-running the importer. Only one
/// [`GltfPreviewSession`] is GPU-resident at a time.
pub struct HostGltfPreviewStore {
    cache: PreviewCache,
}

/// CPU-ready preview payload removed from an active session for host parking.
pub struct ParkedGltfPreview {
    key: PreviewCacheKey,
    asset: AssetGuid,
    /// Project-relative path that was parked (logging / diagnostics).
    pub relative_path: String,
    loaded: LoadedGltfScene,
    cpu_bytes: u64,
}

impl HostGltfPreviewStore {
    /// Empty store with the same policy as [`GltfPreviewAdapter`] (max 8 entries).
    #[must_use]
    pub fn new() -> Self {
        let policy = GltfPreviewAdapter::new().descriptor().cache();
        Self {
            cache: PreviewCache::new(policy),
        }
    }

    /// Drops every parked entry (project close / root change).
    pub fn clear(&mut self) {
        *self = Self::new();
    }

    /// Number of parked CPU scenes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.cache.len()
    }

    /// Whether the store has no parked scenes.
    #[must_use]
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.cache.is_empty()
    }

    /// Whether `key` is currently parked.
    #[must_use]
    pub fn contains(&self, key: &PreviewCacheKey) -> bool {
        self.cache.contains(key)
    }

    /// Inserts a parked CPU scene, evicting LRU entries per policy.
    ///
    /// # Errors
    ///
    /// Forwards [`PreviewCacheError`] when the artifact cannot fit the budget.
    pub fn park(&mut self, parked: ParkedGltfPreview) -> Result<(), PreviewCacheError> {
        let capability = CapabilityId::new("yuyib.gltf-preview").expect("stable capability id");
        let artifact = PreviewArtifact::new(
            parked.asset,
            capability,
            parked.cpu_bytes,
            0,
            parked.loaded,
        );
        self.cache.insert(parked.key, artifact)
    }

    /// Removes and returns a parked CPU scene (cache hit + consume).
    pub fn take(&mut self, key: &PreviewCacheKey) -> Option<LoadedGltfScene> {
        let artifact = self.cache.take(key)?;
        match artifact.downcast::<LoadedGltfScene>() {
            Ok(loaded) => Some(loaded),
            Err(_wrong_type) => None,
        }
    }
}

impl Default for HostGltfPreviewStore {
    fn default() -> Self {
        Self::new()
    }
}

/// Stable [`AssetGuid`] for preview cache keys: tracked guid when known, else UUID v5 of path.
#[must_use]
pub fn preview_asset_guid(tracked: Option<&str>, relative_path: &str) -> AssetGuid {
    if let Some(raw) = tracked {
        if let Ok(guid) = raw.parse::<AssetGuid>() {
            return guid;
        }
    }
    let normalized = relative_path.replace('\\', "/");
    AssetGuid::from_uuid(Uuid::new_v5(
        &PREVIEW_ASSET_NAMESPACE,
        normalized.as_bytes(),
    ))
}

/// One in-flight or resident glTF preview.
pub struct GltfPreviewSession {
    relative_path: String,
    absolute_path: PathBuf,
    asset: AssetGuid,
    import_settings: Value,
    last_cache_key: Option<PreviewCacheKey>,
    /// `None` draws every mesh; `Some(i)` hides `Model3d` nodes with other mesh indices.
    selected_mesh: Option<u32>,
    /// `None` draws every material; `Some(i)` hides meshes that do not use material `i`.
    selected_material: Option<u32>,
    /// `None` keeps bind/static pose; `Some(i)` plays that clip.
    selected_animation: Option<u32>,
    /// Preview-only factor patch for one source material; never persisted.
    material_override: Option<(u32, PreviewMaterialOverride)>,
    animation_player: Option<AnimationPlayer>,
    skeletal_gpu: Option<GltfAnimationPreviewGpu>,
    loading: Option<GltfSceneLoad>,
    ready: Option<LoadedGltfScene>,
    /// Disk cook root (`.yuyib_cook`); `None` disables cook lookup for this session.
    cook_root: Option<PathBuf>,
    /// Set when a production load finishes; `None` for parked restore / not yet loaded.
    last_cook_hit: Option<bool>,
    gpu_ready: bool,
    gpu_event_sent: bool,
    last_gpu: Option<GltfSceneGpuProgress>,
}

/// One mesh entry for Asset Preview selection UI.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PreviewMeshEntry {
    /// Zero-based source mesh index.
    pub index: u32,
    /// Optional glTF mesh name.
    pub name: Option<String>,
}

/// One material entry for Asset Preview selection UI.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PreviewMaterialEntry {
    /// Zero-based source material index.
    pub index: u32,
    /// Optional glTF material name.
    pub name: Option<String>,
}

/// One model texture slot for preview-only remap UI.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PreviewTextureEntry {
    /// Zero-based model texture index.
    pub index: u32,
    /// Optional label / URI basename.
    pub name: Option<String>,
}

/// One animation clip entry for Asset Preview selection UI.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct PreviewAnimationEntry {
    /// Zero-based source clip index.
    pub index: u32,
    /// Optional glTF animation name.
    pub name: Option<String>,
    /// Clip duration in seconds.
    pub duration_seconds: f32,
}

/// Outcome of a non-destructive reimport attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GltfPreviewReimport {
    /// Source hash + import settings match the resident load — decode skipped.
    CacheHit,
    /// A new production load was started; last-good draw is retained until ready.
    Started,
}

/// Non-terminal poll result for UI/host.process.
#[derive(Clone, Debug)]
pub struct GltfPreviewPoll {
    pub relative_path: String,
    pub stage: &'static str,
    pub completed: u64,
    pub total: u64,
    /// CPU import finished and scene is in memory (may still be uploading to GPU).
    pub cpu_ready: bool,
    /// Disk cook hit for the load that just became CPU-ready (`None` if unknown).
    pub cook_hit: Option<bool>,
    pub failed: Option<String>,
}

/// One frame of preview draw / upload work.
#[derive(Debug)]
#[allow(dead_code)]
pub enum GltfPreviewFrame {
    /// Still publishing textures/geometry to the GPU.
    Uploading(GltfSceneGpuProgress),
    /// Fully resident and drawn this frame.
    Drawn {
        stats: Game3dSceneStats,
        gpu: GltfSceneGpuProgress,
    },
}

impl GltfPreviewSession {
    /// Starts a production import of one project-relative glTF source.
    ///
    /// # Errors
    ///
    /// Returns when the path escapes the project root or the load cannot start.
    pub fn start(
        project_root: &Path,
        asset_root: &str,
        relative_path: impl Into<String>,
    ) -> Result<Self, GltfPreviewError> {
        Self::start_with_settings(
            project_root,
            asset_root,
            relative_path,
            &default_settings_json(),
        )
    }

    /// Starts import with authored `yuyib.gltf-import-settings` JSON.
    ///
    /// # Errors
    ///
    /// Returns path, settings, or load-start failures.
    pub fn start_with_settings(
        project_root: &Path,
        asset_root: &str,
        relative_path: impl Into<String>,
        import_settings: &Value,
    ) -> Result<Self, GltfPreviewError> {
        Self::start_with_settings_and_asset(
            project_root,
            asset_root,
            relative_path,
            import_settings,
            AssetGuid::new(),
            None,
        )
    }

    /// Starts import with a stable [`AssetGuid`] (tracked or synthetic) for cache keys.
    ///
    /// When `cook_root` is set, the load uses the production disk cook cache
    /// (same `.yuyib_cook` as `project.cook` / ypack hydrate).
    ///
    /// # Errors
    ///
    /// Returns path, settings, or load-start failures.
    pub fn start_with_settings_and_asset(
        project_root: &Path,
        asset_root: &str,
        relative_path: impl Into<String>,
        import_settings: &Value,
        asset: AssetGuid,
        cook_root: Option<&Path>,
    ) -> Result<Self, GltfPreviewError> {
        let relative_path = relative_path.into();
        let absolute_path = resolve_asset_path(project_root, asset_root, &relative_path)?;
        if !absolute_path.is_file() {
            return Err(GltfPreviewError::MissingFile(absolute_path));
        }
        let adapter = GltfPreviewAdapter::new();
        let content_hash = hash_asset_file(&absolute_path).ok();
        let cache_key = session_cache_key(
            &adapter,
            asset,
            &relative_path,
            content_hash,
            import_settings,
        );
        let cook_root = cook_root.map(Path::to_path_buf);
        let loading = start_load(&absolute_path, import_settings, cook_root.as_deref())?;
        Ok(Self {
            relative_path,
            absolute_path,
            asset,
            import_settings: import_settings.clone(),
            last_cache_key: Some(cache_key),
            selected_mesh: None,
            selected_material: None,
            selected_animation: None,
            material_override: None,
            animation_player: None,
            skeletal_gpu: None,
            loading: Some(loading),
            ready: None,
            cook_root,
            last_cook_hit: None,
            gpu_ready: false,
            gpu_event_sent: false,
            last_gpu: None,
        })
    }

    /// Restores a session from a parked CPU scene (skip importer). GPU upload runs on draw.
    ///
    /// # Errors
    ///
    /// Returns when the path escapes the project root or the file is missing.
    pub fn from_cpu_ready(
        project_root: &Path,
        asset_root: &str,
        relative_path: impl Into<String>,
        import_settings: &Value,
        asset: AssetGuid,
        loaded: LoadedGltfScene,
        cook_root: Option<&Path>,
    ) -> Result<Self, GltfPreviewError> {
        let relative_path = relative_path.into();
        let absolute_path = resolve_asset_path(project_root, asset_root, &relative_path)?;
        if !absolute_path.is_file() {
            return Err(GltfPreviewError::MissingFile(absolute_path));
        }
        let adapter = GltfPreviewAdapter::new();
        let content_hash = hash_asset_file(&absolute_path).ok();
        let cache_key = session_cache_key(
            &adapter,
            asset,
            &relative_path,
            content_hash,
            import_settings,
        );
        Ok(Self {
            relative_path,
            absolute_path,
            asset,
            import_settings: import_settings.clone(),
            last_cache_key: Some(cache_key),
            selected_mesh: None,
            selected_material: None,
            selected_animation: None,
            material_override: None,
            animation_player: None,
            skeletal_gpu: None,
            loading: None,
            ready: Some(loaded),
            cook_root: cook_root.map(Path::to_path_buf),
            last_cook_hit: None,
            gpu_ready: false,
            gpu_event_sent: false,
            last_gpu: None,
        })
    }

    /// Policy cache key for the current path / settings / content hash (for store lookup).
    #[must_use]
    pub fn cache_key_for(
        relative_path: &str,
        absolute_path: &Path,
        import_settings: &Value,
        asset: AssetGuid,
    ) -> PreviewCacheKey {
        let adapter = GltfPreviewAdapter::new();
        let content_hash = hash_asset_file(absolute_path).ok();
        session_cache_key(
            &adapter,
            asset,
            relative_path,
            content_hash,
            import_settings,
        )
    }

    /// Removes the CPU-ready scene for host parking. Clears GPU residency flags.
    #[must_use]
    pub fn park_cpu_ready(&mut self) -> Option<ParkedGltfPreview> {
        let key = self.last_cache_key.clone()?;
        let loaded = self.ready.take()?;
        self.gpu_ready = false;
        self.gpu_event_sent = false;
        self.last_gpu = None;
        self.skeletal_gpu = None;
        self.animation_player = None;
        let cpu_bytes = std::fs::metadata(&self.absolute_path)
            .map(|meta| meta.len())
            .unwrap_or(0);
        Some(ParkedGltfPreview {
            key,
            asset: self.asset,
            relative_path: self.relative_path.clone(),
            loaded,
            cpu_bytes,
        })
    }

    /// Persistent asset identity used for preview cache keys.
    #[must_use]
    pub const fn asset_guid(&self) -> AssetGuid {
        self.asset
    }

    /// Whether the last production load hit the disk cook cache.
    #[must_use]
    pub const fn last_cook_hit(&self) -> Option<bool> {
        self.last_cook_hit
    }

    /// Last policy cache key (present after start / restore).
    #[must_use]
    pub fn last_cache_key(&self) -> Option<&PreviewCacheKey> {
        self.last_cache_key.as_ref()
    }

    /// Non-destructive reimport: keeps the last-good CPU scene until the new
    /// load succeeds. Failed reimports leave the previous draw intact.
    ///
    /// When content hash and import settings still match the resident key under
    /// the adapter [`PreviewCachePolicy`], returns [`GltfPreviewReimport::CacheHit`]
    /// without starting another decode.
    ///
    /// # Errors
    ///
    /// Returns settings or load-start failures without clearing `ready`.
    pub fn reimport_with_settings(
        &mut self,
        import_settings: &Value,
    ) -> Result<GltfPreviewReimport, GltfPreviewError> {
        let adapter = GltfPreviewAdapter::new();
        let content_hash = hash_asset_file(&self.absolute_path).ok();
        let cache_key = session_cache_key(
            &adapter,
            self.asset,
            &self.relative_path,
            content_hash,
            import_settings,
        );
        if self.ready.is_some() && self.last_cache_key.as_ref() == Some(&cache_key) {
            return Ok(GltfPreviewReimport::CacheHit);
        }
        let loading = start_load(
            &self.absolute_path,
            import_settings,
            self.cook_root.as_deref(),
        )?;
        self.loading = Some(loading);
        self.import_settings = import_settings.clone();
        self.last_cache_key = Some(cache_key);
        self.last_cook_hit = None;
        // Keep ready / gpu_ready until the new load publishes successfully.
        Ok(GltfPreviewReimport::Started)
    }

    /// Absolute path being imported.
    #[must_use]
    pub fn absolute_path(&self) -> &Path {
        &self.absolute_path
    }

    /// Relative project path.
    #[must_use]
    pub fn relative_path(&self) -> &str {
        &self.relative_path
    }

    /// Current mesh filter (`None` = entire model).
    #[must_use]
    pub const fn selected_mesh(&self) -> Option<u32> {
        self.selected_mesh
    }

    /// Current material filter (`None` = all materials).
    #[must_use]
    pub const fn selected_material(&self) -> Option<u32> {
        self.selected_material
    }

    /// Enumerates CPU-ready meshes for the preview picker.
    #[must_use]
    pub fn mesh_inventory(&self) -> Vec<PreviewMeshEntry> {
        let Some(ready) = &self.ready else {
            return Vec::new();
        };
        let Some(model) = ready.models().get(ready.model()) else {
            return Vec::new();
        };
        model
            .meshes()
            .iter()
            .enumerate()
            .map(|(index, mesh)| PreviewMeshEntry {
                index: index as u32,
                name: sanitize_mesh_name(mesh.name()),
            })
            .collect()
    }

    /// Enumerates CPU-ready materials for the preview picker.
    #[must_use]
    pub fn material_inventory(&self) -> Vec<PreviewMaterialEntry> {
        let Some(ready) = &self.ready else {
            return Vec::new();
        };
        let Some(model) = ready.models().get(ready.model()) else {
            return Vec::new();
        };
        model
            .materials()
            .iter()
            .enumerate()
            .map(|(index, material)| PreviewMaterialEntry {
                index: index as u32,
                name: sanitize_mesh_name(material.name()),
            })
            .collect()
    }

    /// Enumerates CPU-ready model textures for remap pickers.
    #[must_use]
    pub fn texture_inventory(&self) -> Vec<PreviewTextureEntry> {
        let Some(ready) = &self.ready else {
            return Vec::new();
        };
        let Some(model) = ready.models().get(ready.model()) else {
            return Vec::new();
        };
        model
            .textures()
            .iter()
            .enumerate()
            .map(|(index, texture)| {
                let name = texture
                    .label()
                    .map(str::to_owned)
                    .or_else(|| {
                        texture.uri().map(|uri| {
                            Path::new(uri)
                                .file_name()
                                .map(|name| name.to_string_lossy().into_owned())
                                .unwrap_or_else(|| uri.to_owned())
                        })
                    })
                    .or_else(|| Some(format!("texture_{index}")));
                PreviewTextureEntry {
                    index: index as u32,
                    name,
                }
            })
            .collect()
    }

    /// Filters draw to one mesh index, or clears the filter with `None`.
    ///
    /// Does not re-decode; toggles [`Model3d::visible`] on spawned nodes.
    /// When the preview is not CPU-ready yet, the index is stored and applied
    /// after import (out-of-range indices are cleared then).
    ///
    /// # Errors
    ///
    /// Returns when a CPU-ready inventory exists and `index` is out of range.
    pub fn set_mesh_selection(&mut self, index: Option<u32>) -> Result<(), GltfPreviewError> {
        if let Some(index) = index {
            let inventory = self.mesh_inventory();
            if !inventory.is_empty() && index as usize >= inventory.len() {
                return Err(GltfPreviewError::Selection(format!(
                    "mesh index {index} out of range ({} meshes)",
                    inventory.len()
                )));
            }
        }
        self.selected_mesh = index;
        if let Some(ready) = &mut self.ready {
            apply_preview_visibility(
                ready,
                self.selected_mesh,
                self.selected_material,
                self.selected_animation.is_some() && self.skeletal_gpu.is_some(),
            );
        }
        Ok(())
    }

    /// Filters draw to meshes that use one material, or clears with `None`.
    ///
    /// # Errors
    ///
    /// Returns when a CPU-ready inventory exists and `index` is out of range.
    pub fn set_material_selection(&mut self, index: Option<u32>) -> Result<(), GltfPreviewError> {
        if let Some(index) = index {
            let inventory = self.material_inventory();
            if !inventory.is_empty() && index as usize >= inventory.len() {
                return Err(GltfPreviewError::Selection(format!(
                    "material index {index} out of range ({} materials)",
                    inventory.len()
                )));
            }
        }
        self.selected_material = index;
        if let Some(ready) = &mut self.ready {
            apply_preview_visibility(
                ready,
                self.selected_mesh,
                self.selected_material,
                self.selected_animation.is_some() && self.skeletal_gpu.is_some(),
            );
        }
        Ok(())
    }

    /// Sets or clears a preview-only material factor override.
    ///
    /// A newly-ready scene receives the override before any GPU publication. If
    /// the resident scene was already published, a fresh production import is
    /// queued while the last-good scene remains drawable.
    pub fn set_material_override(
        &mut self,
        index: u32,
        override_: Option<PreviewMaterialOverride>,
    ) -> Result<(), GltfPreviewError> {
        if let Some(override_) = &override_ {
            let _ = material_factor_patch(override_)?;
            if !self.material_inventory().is_empty()
                && index as usize >= self.material_inventory().len()
            {
                return Err(GltfPreviewError::Selection(format!(
                    "material index {index} out of range ({} materials)",
                    self.material_inventory().len()
                )));
            }
        }
        self.material_override = override_.map(|override_| (index, override_));
        // Factor overrides (metallic/roughness) only render on the static PBR path.
        // Skeletal preview uses a reduced shader — pause animation while inspecting.
        if self.material_override.is_some() && self.selected_animation.is_some() {
            let _ = self.set_animation_selection(None);
        }
        if self.ready.is_some() {
            self.force_reimport()?;
        }
        Ok(())
    }

    /// Animation clip inventory from the imported scene (empty until CPU-ready).
    #[must_use]
    pub fn animation_inventory(&self) -> Vec<PreviewAnimationEntry> {
        let Some(ready) = &self.ready else {
            return Vec::new();
        };
        ready
            .imported_scene()
            .animations()
            .iter()
            .enumerate()
            .map(|(index, clip)| PreviewAnimationEntry {
                index: index as u32,
                name: sanitize_mesh_name(clip.name()),
                duration_seconds: clip.duration_seconds(),
            })
            .collect()
    }

    /// Selected animation clip index, if any.
    #[must_use]
    pub const fn selected_animation(&self) -> Option<u32> {
        self.selected_animation
    }

    /// Selects one animation clip, or clears playback with `None`.
    ///
    /// # Errors
    ///
    /// Returns when a CPU-ready inventory exists and `index` is out of range.
    pub fn set_animation_selection(&mut self, index: Option<u32>) -> Result<(), GltfPreviewError> {
        if let Some(index) = index {
            let inventory = self.animation_inventory();
            if !inventory.is_empty() && index as usize >= inventory.len() {
                return Err(GltfPreviewError::Selection(format!(
                    "animation index {index} out of range ({} clips)",
                    inventory.len()
                )));
            }
            self.animation_player = Some(AnimationPlayer::new(AnimationClipIndex::new(
                index as usize,
            )));
        } else {
            self.animation_player = None;
        }
        self.selected_animation = index;
        if let Some(ready) = &mut self.ready {
            apply_preview_visibility(
                ready,
                self.selected_mesh,
                self.selected_material,
                self.selected_animation.is_some() && self.skeletal_gpu.is_some(),
            );
        }
        Ok(())
    }

    /// Whether CPU import finished (scene may still be uploading).
    #[must_use]
    pub const fn is_cpu_ready(&self) -> bool {
        self.ready.is_some()
    }

    /// Whether a background import/reimport is in flight.
    #[must_use]
    pub const fn is_loading(&self) -> bool {
        self.loading.is_some()
    }

    /// Whether GPU residency completed at least once.
    #[must_use]
    pub const fn is_gpu_ready(&self) -> bool {
        self.gpu_ready
    }

    /// Emits a one-shot GPU-ready payload for host.process after the first full upload.
    pub fn take_gpu_ready_event(&mut self) -> Option<GltfSceneGpuProgress> {
        if self.gpu_ready && !self.gpu_event_sent {
            self.gpu_event_sent = true;
            self.last_gpu
        } else {
            None
        }
    }

    /// Last observed GPU upload counters.
    #[must_use]
    pub const fn last_gpu(&self) -> Option<GltfSceneGpuProgress> {
        self.last_gpu
    }

    /// Polls the worker and promotes a finished load into residency.
    pub fn poll(&mut self) -> GltfPreviewPoll {
        if self.loading.is_some() {
            return self.poll_active_load();
        }
        if self.ready.is_some() {
            return GltfPreviewPoll {
                relative_path: self.relative_path.clone(),
                stage: if self.gpu_ready {
                    "ready"
                } else {
                    "gpu_upload"
                },
                completed: 1,
                total: 1,
                cpu_ready: true,
                cook_hit: self.last_cook_hit,
                failed: None,
            };
        }
        GltfPreviewPoll {
            relative_path: self.relative_path.clone(),
            stage: "idle",
            completed: 0,
            total: 0,
            cpu_ready: false,
            cook_hit: self.last_cook_hit,
            failed: Some("preview session has no active load".to_owned()),
        }
    }

    fn poll_active_load(&mut self) -> GltfPreviewPoll {
        let had_ready = self.ready.is_some();
        let Some(loading) = &mut self.loading else {
            return GltfPreviewPoll {
                relative_path: self.relative_path.clone(),
                stage: "idle",
                completed: 0,
                total: 0,
                cpu_ready: had_ready,
                cook_hit: self.last_cook_hit,
                failed: Some("preview session has no active load".to_owned()),
            };
        };
        let progress = loading.update();
        let stage = match progress.stage {
            GltfSceneLoadStage::Queued => "queued",
            GltfSceneLoadStage::Reading => "reading",
            GltfSceneLoadStage::Processing => "processing",
            GltfSceneLoadStage::Ready => "ready_to_publish",
            GltfSceneLoadStage::Failed => "failed",
            GltfSceneLoadStage::Taken => "taken",
        };
        if progress.stage == GltfSceneLoadStage::Failed {
            let failed = loading
                .failure()
                .map(|error| error.to_string())
                .unwrap_or_else(|| "glTF import failed".to_owned());
            self.loading = None;
            // Non-destructive: keep last-good `ready` for draw.
            return GltfPreviewPoll {
                relative_path: self.relative_path.clone(),
                stage,
                completed: progress.completed_work,
                total: progress.total_work.max(1),
                cpu_ready: self.ready.is_some(),
                cook_hit: self.last_cook_hit,
                failed: Some(failed),
            };
        }
        if progress.stage == GltfSceneLoadStage::Ready {
            match loading.take_ready() {
                Ok(loaded) => {
                    self.loading = None;
                    let mut loaded = loaded;
                    let cook_hit = loaded_had_cook_hit(&loaded);
                    if let Err(error) = self.apply_material_override(&mut loaded) {
                        return GltfPreviewPoll {
                            relative_path: self.relative_path.clone(),
                            stage: "failed",
                            completed: progress.completed_work,
                            total: progress.total_work.max(1),
                            cpu_ready: self.ready.is_some(),
                            cook_hit: self.last_cook_hit,
                            failed: Some(error.to_string()),
                        };
                    }
                    self.last_cook_hit = Some(cook_hit);
                    self.skeletal_gpu = loaded
                        .take_skeletal_prepared()
                        .map(GltfAnimationPreviewGpu::new);
                    self.ready = Some(loaded);
                    self.gpu_ready = false;
                    self.gpu_event_sent = false;
                    self.last_gpu = None;
                    if let Some(index) = self.selected_mesh {
                        let count = self.mesh_inventory().len();
                        if index as usize >= count {
                            self.selected_mesh = None;
                        }
                    }
                    if let Some(index) = self.selected_material {
                        let count = self.material_inventory().len();
                        if index as usize >= count {
                            self.selected_material = None;
                        }
                    }
                    let animations = self.animation_inventory();
                    if let Some(index) = self.selected_animation {
                        if index as usize >= animations.len() {
                            self.selected_animation = None;
                            self.animation_player = None;
                        }
                    } else if self.material_override.is_none()
                        && let Some(first) = animations.first()
                    {
                        // Auto-play first clip so skeletal assets animate on open.
                        // Skip when a material factor override is active — skeletal
                        // path cannot show metallic/roughness.
                        self.selected_animation = Some(first.index);
                        self.animation_player = Some(AnimationPlayer::new(
                            AnimationClipIndex::new(first.index as usize),
                        ));
                    }
                    if let Some(ready) = &mut self.ready {
                        apply_preview_visibility(
                            ready,
                            self.selected_mesh,
                            self.selected_material,
                            self.selected_animation.is_some() && self.skeletal_gpu.is_some(),
                        );
                    }
                    return GltfPreviewPoll {
                        relative_path: self.relative_path.clone(),
                        stage: "gpu_upload",
                        completed: progress.completed_work.max(1),
                        total: progress.total_work.max(1),
                        cpu_ready: true,
                        cook_hit: Some(cook_hit),
                        failed: None,
                    };
                }
                Err(error) => {
                    self.loading = None;
                    return GltfPreviewPoll {
                        relative_path: self.relative_path.clone(),
                        stage: "failed",
                        completed: progress.completed_work,
                        total: progress.total_work.max(1),
                        cpu_ready: self.ready.is_some(),
                        cook_hit: self.last_cook_hit,
                        failed: Some(error.to_string()),
                    };
                }
            }
        }
        GltfPreviewPoll {
            relative_path: self.relative_path.clone(),
            stage,
            completed: progress.completed_work,
            total: progress.total_work.max(1),
            cpu_ready: self.ready.is_some(),
            cook_hit: self.last_cook_hit,
            failed: None,
        }
    }

    /// Aggregate AABB of the CPU-ready imported scene, if any.
    #[must_use]
    pub fn bounds(&self) -> Option<SceneBounds3d> {
        let ready = self.ready.as_ref()?;
        match ready.bounds() {
            SceneBoundsResult3d::Bounds(bounds) => Some(bounds),
            SceneBoundsResult3d::Empty => None,
        }
    }

    /// Vertex-normal shafts for Asset Preview overlay (world space, capped).
    #[must_use]
    pub fn normal_overlay_parts(&self) -> Vec<crate::editor_gizmo::GizmoDrawPart> {
        let length = self
            .bounds()
            .map(|bounds| crate::editor_gizmo::normal_shaft_length_for_radius(bounds.radius()))
            .unwrap_or(0.12);
        self.normal_overlay_parts_with_length(length)
    }

    /// Same as [`Self::normal_overlay_parts`] with an explicit shaft length.
    #[must_use]
    pub fn normal_overlay_parts_with_length(
        &self,
        length: f32,
    ) -> Vec<crate::editor_gizmo::GizmoDrawPart> {
        let Some(ready) = &self.ready else {
            return Vec::new();
        };
        crate::editor_gizmo::model_normal_overlay_parts(ready.world(), ready.models(), length)
    }

    /// Collision mesh wireframe for Asset Preview overlay (sampled, capped).
    #[must_use]
    pub fn collision_overlay_parts(&self) -> Vec<crate::editor_gizmo::GizmoDrawPart> {
        let thickness = self
            .bounds()
            .map(|bounds| {
                crate::editor_gizmo::collision_edge_thickness_for_radius(bounds.radius())
            })
            .unwrap_or(0.008);
        self.collision_overlay_parts_with_thickness(thickness)
    }

    /// Same as [`Self::collision_overlay_parts`] with explicit edge thickness.
    #[must_use]
    pub fn collision_overlay_parts_with_thickness(
        &self,
        thickness: f32,
    ) -> Vec<crate::editor_gizmo::GizmoDrawPart> {
        let Some(ready) = &self.ready else {
            return Vec::new();
        };
        crate::editor_gizmo::model_collision_overlay_parts(ready.world(), ready.models(), thickness)
    }

    /// Tangent shafts for Asset Preview overlay (world space, capped).
    #[must_use]
    pub fn tangent_overlay_parts_with_length(
        &self,
        length: f32,
    ) -> Vec<crate::editor_gizmo::GizmoDrawPart> {
        let Some(ready) = &self.ready else {
            return Vec::new();
        };
        crate::editor_gizmo::model_tangent_overlay_parts(ready.world(), ready.models(), length)
    }

    /// UV0 vertex markers for Asset Preview overlay.
    #[must_use]
    pub fn uv_overlay_parts_with_size(
        &self,
        size: f32,
    ) -> Vec<crate::editor_gizmo::GizmoDrawPart> {
        let Some(ready) = &self.ready else {
            return Vec::new();
        };
        crate::editor_gizmo::model_uv_overlay_parts(ready.world(), ready.models(), size)
    }

    /// Frames the editor orbit camera on the imported bounds.
    ///
    /// Returns suggested near/far planes so large maps are not clipped by the
    /// default `far = 1000` camera.
    pub fn frame_orbit(
        &self,
        target: &mut [f32; 3],
        radius: &mut f32,
        near: &mut f32,
        far: &mut f32,
    ) {
        let framed_radius = match self.bounds() {
            Some(bounds) => {
                *target = bounds.centre();
                bounds.radius().max(1.5) * 2.2
            }
            None => {
                *target = [0.0, 0.0, 0.0];
                3.0
            }
        };
        *radius = framed_radius;
        *near = (framed_radius * 0.001).max(0.05);
        *far = (framed_radius * 20.0).max(*near * 200.0);
    }

    /// Advances texture/geometry residency without drawing the imported scene.
    ///
    /// Used while the foundation/authored preview must remain visible.
    ///
    /// # Errors
    ///
    /// Propagates GPU publication failures.
    pub fn advance_gpu_upload(
        &mut self,
        frame: &RenderFrame<'_>,
        renderer: &mut Game3dScene,
    ) -> Result<Option<GltfSceneGpuProgress>, Box<dyn Error>> {
        let Some(ready) = &mut self.ready else {
            return Ok(None);
        };
        let progress =
            ready.prepare_for_frame_with_budget(frame, renderer, EDITOR_UPLOAD_BUDGET)?;
        self.last_gpu = Some(progress);
        if progress.ready {
            self.gpu_ready = true;
        }
        Ok(Some(progress))
    }

    /// Uploads and draws the resident glTF through the production scene facade.
    ///
    /// When an animation clip is selected, advances the player by `delta_seconds`
    /// and draws via the skeletal path (when skins/morphs exist) or by applying
    /// TRS locals onto the ECS hierarchy.
    ///
    /// # Errors
    ///
    /// Propagates GPU publication and render failures.
    pub fn render(
        &mut self,
        frame: &mut RenderFrame<'_>,
        renderer: &mut Game3dScene,
        delta_seconds: f32,
    ) -> Result<Option<GltfPreviewFrame>, Box<dyn Error>> {
        let Some(progress) = self.advance_gpu_upload(frame, renderer)? else {
            return Ok(None);
        };
        if !progress.ready {
            return Ok(Some(GltfPreviewFrame::Uploading(progress)));
        }

        let use_skeletal = self.selected_animation.is_some() && self.skeletal_gpu.is_some();
        if use_skeletal {
            let mut skeletal = self.skeletal_gpu.take();
            let prepare_result = {
                let ready = self.ready.as_ref().ok_or("preview scene missing")?;
                let model = ready
                    .models()
                    .get(ready.model())
                    .ok_or("preview model missing for skeletal animation")?;
                let scene = ready.imported_scene();
                skeletal
                    .as_mut()
                    .expect("use_skeletal implies skeletal_gpu")
                    .prepare_for_frame(frame, model, scene, EDITOR_UPLOAD_BUDGET)
            };
            self.skeletal_gpu = skeletal;
            if !prepare_result? {
                return Ok(Some(GltfPreviewFrame::Uploading(progress)));
            }
        }

        let pose = {
            let delta = delta_seconds.clamp(0.0, 0.1);
            match (self.animation_player.as_mut(), self.ready.as_ref()) {
                (Some(player), Some(ready)) => {
                    let scene = ready.imported_scene();
                    player.advance(scene, delta)?;
                    Some(player.snapshot(scene)?)
                }
                _ => None,
            }
        };

        let camera = *renderer.camera_mut();
        let stats = {
            let ready = self.ready.as_mut().ok_or("preview scene missing")?;
            apply_preview_visibility(
                ready,
                self.selected_mesh,
                self.selected_material,
                use_skeletal,
            );
            if let Some(pose) = pose.as_ref()
                && !use_skeletal
            {
                apply_animation_locals(ready, pose)?;
            }
            match ready.render(frame, renderer) {
                Ok(stats) => stats,
                Err(LoadedGltfSceneRenderError::NotGpuReady) => {
                    return Ok(Some(GltfPreviewFrame::Uploading(progress)));
                }
                Err(error) => return Err(Box::new(error)),
            }
        };

        if use_skeletal
            && let (Some(pose), Some(skeletal), Some(ready)) =
                (pose.as_ref(), self.skeletal_gpu.as_ref(), self.ready.as_ref())
        {
            skeletal.draw(
                frame,
                camera,
                ready.imported_scene(),
                pose,
                DepthLoad::Load,
            )?;
        }

        Ok(Some(GltfPreviewFrame::Drawn {
            stats,
            gpu: progress,
        }))
    }

    fn force_reimport(&mut self) -> Result<(), GltfPreviewError> {
        self.loading = Some(start_load(
            &self.absolute_path,
            &self.import_settings,
            self.cook_root.as_deref(),
        )?);
        self.last_cook_hit = None;
        self.gpu_ready = false;
        self.gpu_event_sent = false;
        self.last_gpu = None;
        Ok(())
    }

    fn apply_material_override(&self, loaded: &mut LoadedGltfScene) -> Result<(), GltfPreviewError> {
        let Some((index, override_)) = &self.material_override else {
            return Ok(());
        };
        let patch = material_factor_patch(override_)?;
        let texture_remaps = material_texture_remaps(override_)?;
        let model = loaded
            .model_mut_before_publication()
            .map_err(|error| GltfPreviewError::Override(error.to_string()))?;
        let texture_count = model.textures().len();
        for remap in &texture_remaps {
            if let Some(texture_index) = remap.index
                && texture_index as usize >= texture_count
            {
                return Err(GltfPreviewError::Override(format!(
                    "texture index {texture_index} out of range ({texture_count} textures)"
                )));
            }
        }
        let current = model
            .materials()
            .get(*index as usize)
            .cloned()
            .ok_or_else(|| {
                GltfPreviewError::Selection(format!(
                    "material index {index} out of range ({} materials)",
                    model.materials().len()
                ))
            })?;
        let mut replacement = patch.apply_to(current);
        for remap in texture_remaps {
            replacement = apply_texture_remap(replacement, remap);
        }
        model
            .replace_material(MaterialIndex::new(*index as usize), replacement)
            .map_err(|error| GltfPreviewError::Override(error.to_string()))?;
        Ok(())
    }
}

/// Preview start / path validation failure.
#[derive(Debug)]
pub enum GltfPreviewError {
    /// Relative path escaped the project or asset root.
    EscapesProject { relative: String, detail: String },
    /// Source file is missing on disk.
    MissingFile(PathBuf),
    /// Authored import settings JSON was rejected.
    Settings(String),
    /// Mesh (or future) selection rejected.
    Selection(String),
    /// Preview-only material factor patch was invalid.
    Override(String),
    /// [`GltfSceneLoad::start`] rejected the request.
    Start(String),
}

impl fmt::Display for GltfPreviewError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EscapesProject { relative, detail } => {
                write!(
                    formatter,
                    "glTF path `{relative}` is outside the project asset root: {detail}"
                )
            }
            Self::MissingFile(path) => write!(formatter, "glTF file not found: {}", path.display()),
            Self::Settings(message) => {
                write!(formatter, "invalid glTF import settings: {message}")
            }
            Self::Selection(message) => write!(formatter, "invalid preview selection: {message}"),
            Self::Override(message) => write!(formatter, "invalid material override: {message}"),
            Self::Start(message) => write!(formatter, "could not start glTF preview: {message}"),
        }
    }
}

fn material_factor_patch(
    override_: &PreviewMaterialOverride,
) -> Result<MaterialFactorPatch, GltfPreviewError> {
    let mut base_color_factor = None;
    let mut metallic_factor = None;
    let mut roughness_factor = None;
    let mut emissive_factor = None;
    let mut double_sided = None;
    for (key, value) in &override_.parameters {
        match key.as_str() {
            "base_color_factor" => base_color_factor = Some(parse_factor::<4>(key, value)?),
            "metallic_factor" => metallic_factor = Some(parse_number(key, value)?),
            "roughness_factor" => roughness_factor = Some(parse_number(key, value)?),
            "emissive_factor" => emissive_factor = Some(parse_factor::<3>(key, value)?),
            "double_sided" => double_sided = Some(value.as_bool().ok_or_else(|| {
                GltfPreviewError::Override(format!("`{key}` must be a boolean"))
            })?),
            "base_color_texture"
            | "metallic_roughness_texture"
            | "emissive_texture"
            | "normal_texture" => {}
            _ => return Err(GltfPreviewError::Override(format!("unknown parameter `{key}`"))),
        }
    }
    let mut patch = MaterialFactorPatch::new();
    if let Some(value) = base_color_factor {
        patch = patch.with_base_color_factor(value);
    }
    if let Some(value) = metallic_factor {
        patch = patch.with_metallic_factor(value);
    }
    if let Some(value) = roughness_factor {
        patch = patch.with_roughness_factor(value);
    }
    if let Some(value) = emissive_factor {
        patch = patch.with_emissive_factor(value);
    }
    if let Some(value) = double_sided {
        patch = patch.with_double_sided(value);
    }
    Ok(patch)
}

#[derive(Clone, Copy, Debug)]
struct TextureRemap {
    slot: TextureRemapSlot,
    /// `None` clears the slot; `Some(i)` binds model texture `i`.
    index: Option<u32>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TextureRemapSlot {
    BaseColor,
    MetallicRoughness,
    Emissive,
    Normal,
}

fn material_texture_remaps(
    override_: &PreviewMaterialOverride,
) -> Result<Vec<TextureRemap>, GltfPreviewError> {
    let mut remaps = Vec::new();
    for (key, value) in &override_.parameters {
        let slot = match key.as_str() {
            "base_color_texture" => TextureRemapSlot::BaseColor,
            "metallic_roughness_texture" => TextureRemapSlot::MetallicRoughness,
            "emissive_texture" => TextureRemapSlot::Emissive,
            "normal_texture" => TextureRemapSlot::Normal,
            _ => continue,
        };
        let index = if value.is_null() {
            None
        } else {
            let number = value
                .as_u64()
                .or_else(|| value.as_f64().map(|value| value as u64));
            let Some(number) = number.filter(|value| *value <= u32::MAX as u64) else {
                return Err(GltfPreviewError::Override(format!(
                    "`{key}` must be null or a texture index"
                )));
            };
            Some(number as u32)
        };
        remaps.push(TextureRemap { slot, index });
    }
    Ok(remaps)
}

fn apply_texture_remap(
    material: yuyib_model::Material,
    remap: TextureRemap,
) -> yuyib_model::Material {
    match (remap.slot, remap.index) {
        (TextureRemapSlot::BaseColor, None) => material.without_base_color_texture(),
        (TextureRemapSlot::BaseColor, Some(index)) => {
            let tex_coord = material
                .base_color_texture()
                .map(|binding| binding.tex_coord_set())
                .unwrap_or(0);
            material.with_base_color_texture(TextureBinding::new(
                ModelTextureIndex::new(index as usize),
                tex_coord,
            ))
        }
        (TextureRemapSlot::MetallicRoughness, None) => {
            material.without_metallic_roughness_texture()
        }
        (TextureRemapSlot::MetallicRoughness, Some(index)) => {
            let tex_coord = material
                .metallic_roughness_texture()
                .map(|binding| binding.tex_coord_set())
                .unwrap_or(0);
            material.with_metallic_roughness_texture(TextureBinding::new(
                ModelTextureIndex::new(index as usize),
                tex_coord,
            ))
        }
        (TextureRemapSlot::Emissive, None) => material.without_emissive_texture(),
        (TextureRemapSlot::Emissive, Some(index)) => {
            let tex_coord = material
                .emissive_texture()
                .map(|binding| binding.tex_coord_set())
                .unwrap_or(0);
            material.with_emissive_texture(TextureBinding::new(
                ModelTextureIndex::new(index as usize),
                tex_coord,
            ))
        }
        (TextureRemapSlot::Normal, None) => material.without_normal_texture(),
        (TextureRemapSlot::Normal, Some(index)) => {
            let (tex_coord, scale) = material
                .normal_texture()
                .map(|binding| (binding.binding().tex_coord_set(), binding.scale()))
                .unwrap_or((0, 1.0));
            material.with_normal_texture(NormalTextureBinding::new(
                TextureBinding::new(ModelTextureIndex::new(index as usize), tex_coord),
                scale,
            ))
        }
    }
}

fn parse_number(key: &str, value: &Value) -> Result<f32, GltfPreviewError> {
    let value = value
        .as_f64()
        .filter(|value| value.is_finite() && *value >= f32::MIN as f64 && *value <= f32::MAX as f64)
        .ok_or_else(|| GltfPreviewError::Override(format!("`{key}` must be a finite number")))?;
    Ok(value as f32)
}

fn parse_factor<const N: usize>(key: &str, value: &Value) -> Result<[f32; N], GltfPreviewError> {
    let values = value
        .as_array()
        .filter(|values| values.len() == N)
        .ok_or_else(|| GltfPreviewError::Override(format!("`{key}` must be an array of {N} numbers")))?;
    let mut factor = [0.0; N];
    for (slot, value) in factor.iter_mut().zip(values) {
        *slot = parse_number(key, value)?;
    }
    Ok(factor)
}

impl Error for GltfPreviewError {}

fn start_load(
    absolute_path: &Path,
    import_settings: &Value,
    cook_root: Option<&Path>,
) -> Result<GltfSceneLoad, GltfPreviewError> {
    let settings = parse_import_settings(import_settings)
        .map_err(|error| GltfPreviewError::Settings(error.to_string()))?;
    let options = settings.to_import_options();
    let mut config = GltfSceneLoadConfig::default()
        .with_import_options(options)
        .with_importer_registry_limits(ImporterRegistryLimits {
            max_source_bytes: EDITOR_PREVIEW_MAX_SOURCE_BYTES,
            ..ImporterRegistryLimits::default()
        })
        .with_static_collider(false);
    if let Some(root) = cook_root {
        config = config.with_cook_cache(CookCache::new(root));
    }
    GltfSceneLoad::start(absolute_path, config)
        .map_err(|error| GltfPreviewError::Start(error.to_string()))
}

fn loaded_had_cook_hit(loaded: &LoadedGltfScene) -> bool {
    loaded
        .diagnostics()
        .iter()
        .any(|diagnostic| diagnostic.code == "gltf-cook-cache-hit")
}

fn session_cache_key(
    adapter: &GltfPreviewAdapter,
    asset: AssetGuid,
    relative_path: &str,
    content_hash: Option<ContentHash>,
    import_settings: &Value,
) -> PreviewCacheKey {
    let request = PreviewRequest {
        asset,
        source: relative_path.to_owned(),
        content_hash,
        import_settings_schema: ImportSettingsSchemaId::new(GLTF_IMPORT_SETTINGS_SCHEMA)
            .expect("schema"),
        import_settings_version: SchemaVersion::new(1).expect("version"),
        import_settings: import_settings.clone(),
        selection: None,
        overlays: Default::default(),
        material_override: None,
        render_preset: None,
        cancellation: PreviewCancellation::default(),
    };
    adapter.cache_key(&request)
}

fn apply_preview_visibility(
    ready: &mut LoadedGltfScene,
    selected_mesh: Option<u32>,
    selected_material: Option<u32>,
    hide_skinned_for_skeletal_draw: bool,
) {
    let skinned_meshes: std::collections::HashSet<usize> = if hide_skinned_for_skeletal_draw {
        ready
            .imported_scene()
            .skinned_primitives()
            .iter()
            .map(|primitive| primitive.mesh())
            .chain(
                ready
                    .imported_scene()
                    .morph_primitives()
                    .iter()
                    .map(|primitive| primitive.mesh()),
            )
            .collect()
    } else {
        std::collections::HashSet::new()
    };
    let entities: Vec<_> = ready.spawned().entities().map(|(_, entity)| entity).collect();
    let visibility: Vec<(yuyib_ecs::bevy_ecs::entity::Entity, bool)> = {
        let models = ready.models();
        let world = ready.world();
        entities
            .iter()
            .filter_map(|&entity| {
                let model3d = world.get::<Model3d>(entity)?;
                if hide_skinned_for_skeletal_draw
                    && model3d
                        .mesh
                        .is_some_and(|mesh| skinned_meshes.contains(&mesh))
                {
                    return Some((entity, false));
                }
                let mesh_ok = match selected_mesh {
                    None => true,
                    Some(index) => model3d.mesh == Some(index as usize),
                };
                if !mesh_ok {
                    return Some((entity, false));
                }
                let material_ok = match selected_material {
                    None => true,
                    Some(material_index) => {
                        let model = models.get(model3d.model)?;
                        let mesh_indices: Vec<usize> = match model3d.mesh {
                            Some(index) => vec![index],
                            None => (0..model.meshes().len()).collect(),
                        };
                        mesh_indices.iter().any(|&mesh_index| {
                            model.meshes().get(mesh_index).is_some_and(|mesh| {
                                mesh.primitives().iter().any(|primitive| {
                                    primitive.material().is_some_and(|material| {
                                        material.get() == material_index as usize
                                    })
                                })
                            })
                        })
                    }
                };
                Some((entity, material_ok))
            })
            .collect()
    };
    let world = ready.world_mut();
    for (entity, visible) in visibility {
        if let Some(mut model) = world.get_mut::<Model3d>(entity) {
            model.visible = visible;
        }
    }
}

fn apply_animation_locals(
    ready: &mut LoadedGltfScene,
    pose: &yuyib_gltf::AnimationSnapshot,
) -> Result<(), Box<dyn Error>> {
    let mappings: Vec<_> = ready.spawned().entities().collect();
    {
        let world = ready.world_mut();
        for (node, entity) in mappings {
            let Some(local) = pose.local_transforms().get(node.get()) else {
                continue;
            };
            match local {
                LocalTransform::Trs {
                    translation,
                    rotation,
                    scale,
                } => {
                    world.entity_mut(entity).insert(
                        LocalTransform3d::IDENTITY
                            .with_translation(*translation)
                            .with_rotation(*rotation)
                            .with_scale(*scale),
                    );
                }
                LocalTransform::Matrix { .. } => {
                    // Matrix-authored nodes stay on the imported bind transform.
                }
            }
        }
    }
    propagate_world_transforms(ready.world_mut())?;
    Ok(())
}

fn sanitize_mesh_name(name: Option<&str>) -> Option<String> {
    let name = name?.trim();
    if name.is_empty() || name.contains('\u{FFFD}') {
        return None;
    }
    Some(name.to_owned())
}

fn resolve_asset_path(
    project_root: &Path,
    asset_root: &str,
    relative_path: &str,
) -> Result<PathBuf, GltfPreviewError> {
    let relative = Path::new(relative_path);
    if relative.is_absolute()
        || relative.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
    {
        return Err(GltfPreviewError::EscapesProject {
            relative: relative_path.to_owned(),
            detail: "absolute or parent components are rejected".to_owned(),
        });
    }
    let joined = if asset_root.is_empty() {
        project_root.join(relative)
    } else {
        project_root.join(asset_root).join(relative)
    };
    let canonical_root = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf());
    match joined.canonicalize() {
        Ok(canonical) if canonical.starts_with(&canonical_root) => Ok(canonical),
        Ok(_) => Err(GltfPreviewError::EscapesProject {
            relative: relative_path.to_owned(),
            detail: "resolved path left the project root".to_owned(),
        }),
        Err(_) => Ok(joined),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use yuyib_gltf_authoring::default_settings_json;

    fn temporary_root(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "yuyib_preview_store_{label}_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("assets/models")).expect("dirs");
        root
    }

    fn valid_triangle_glb() -> Vec<u8> {
        let mut binary = Vec::new();
        binary.extend([0_u16, 1, 2].into_iter().flat_map(u16::to_le_bytes));
        binary.extend([0_u8; 2]);
        for position in [[0.0_f32, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]] {
            binary.extend(position.into_iter().flat_map(f32::to_le_bytes));
        }
        let json = br#"{"asset":{"version":"2.0"},"scene":0,"scenes":[{"nodes":[0]}],"nodes":[{"name":"root","mesh":0}],"buffers":[{"byteLength":44}],"bufferViews":[{"buffer":0,"byteOffset":0,"byteLength":6},{"buffer":0,"byteOffset":8,"byteLength":36}],"accessors":[{"bufferView":0,"componentType":5123,"count":3,"type":"SCALAR"},{"bufferView":1,"componentType":5126,"count":3,"type":"VEC3","min":[0,0,0],"max":[1,1,0]}],"meshes":[{"primitives":[{"attributes":{"POSITION":1},"indices":0}]}]}"#;
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
        glb.extend(u32::try_from(total).expect("glb size").to_le_bytes());
        glb.extend(u32::try_from(padded_json.len()).expect("json size").to_le_bytes());
        glb.extend(0x4E4F_534A_u32.to_le_bytes());
        glb.extend(padded_json);
        glb.extend(u32::try_from(binary.len()).expect("bin size").to_le_bytes());
        glb.extend(0x004E_4942_u32.to_le_bytes());
        glb.extend(binary);
        glb
    }

    /// Triangle + two TRS translation clips (`bob`, `shift`) for selection tests.
    /// Requires skeletal / skeletal_preview import policy (Strict rejects animations).
    fn animated_triangle_glb() -> Vec<u8> {
        let mut binary = Vec::new();
        binary.extend([0_u16, 1, 2].into_iter().flat_map(u16::to_le_bytes));
        binary.extend([0_u8; 2]);
        for position in [[0.0_f32, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]] {
            binary.extend(position.into_iter().flat_map(f32::to_le_bytes));
        }
        binary.extend([0.0_f32, 1.0].into_iter().flat_map(f32::to_le_bytes));
        for translation in [[0.0_f32, 0.0, 0.0], [0.0, 1.0, 0.0]] {
            binary.extend(translation.into_iter().flat_map(f32::to_le_bytes));
        }
        assert_eq!(binary.len(), 76);
        let json = br#"{"asset":{"version":"2.0"},"scene":0,"scenes":[{"nodes":[0]}],"nodes":[{"name":"root","mesh":0}],"buffers":[{"byteLength":76}],"bufferViews":[{"buffer":0,"byteOffset":0,"byteLength":6},{"buffer":0,"byteOffset":8,"byteLength":36},{"buffer":0,"byteOffset":44,"byteLength":8},{"buffer":0,"byteOffset":52,"byteLength":24}],"accessors":[{"bufferView":0,"componentType":5123,"count":3,"type":"SCALAR"},{"bufferView":1,"componentType":5126,"count":3,"type":"VEC3","min":[0,0,0],"max":[1,1,0]},{"bufferView":2,"componentType":5126,"count":2,"type":"SCALAR","min":[0],"max":[1]},{"bufferView":3,"componentType":5126,"count":2,"type":"VEC3"}],"meshes":[{"primitives":[{"attributes":{"POSITION":1},"indices":0}]}],"animations":[{"name":"bob","samplers":[{"input":2,"output":3,"interpolation":"LINEAR"}],"channels":[{"sampler":0,"target":{"node":0,"path":"translation"}}]},{"name":"shift","samplers":[{"input":2,"output":3,"interpolation":"LINEAR"}],"channels":[{"sampler":0,"target":{"node":0,"path":"translation"}}]}]}"#;
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
        glb.extend(u32::try_from(total).expect("glb size").to_le_bytes());
        glb.extend(u32::try_from(padded_json.len()).expect("json size").to_le_bytes());
        glb.extend(0x4E4F_534A_u32.to_le_bytes());
        glb.extend(padded_json);
        glb.extend(u32::try_from(binary.len()).expect("bin size").to_le_bytes());
        glb.extend(0x004E_4942_u32.to_le_bytes());
        glb.extend(binary);
        glb
    }

    fn poll_until_cpu_ready(session: &mut GltfPreviewSession, label: &str) {
        let mut last_stage = String::new();
        for _ in 0..50_000 {
            let poll = session.poll();
            last_stage = poll.stage.to_owned();
            if poll.cpu_ready {
                return;
            }
            if let Some(failed) = poll.failed {
                panic!("{label} failed at {last_stage}: {failed}");
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        panic!("{label} did not become cpu_ready (last stage={last_stage})");
    }

    #[test]
    fn preview_asset_guid_is_stable_for_path() {
        let a = preview_asset_guid(None, "models/hero.glb");
        let b = preview_asset_guid(None, "models/hero.glb");
        let c = preview_asset_guid(None, "models/other.glb");
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn preview_asset_guid_prefers_tracked() {
        let tracked = AssetGuid::new();
        let from_tracked = preview_asset_guid(Some(&tracked.to_string()), "models/hero.glb");
        assert_eq!(from_tracked, tracked);
        assert_ne!(from_tracked, preview_asset_guid(None, "models/hero.glb"));
    }

    #[test]
    fn host_store_park_take_roundtrip_skips_reimport() {
        let root = temporary_root("roundtrip");
        let glb = valid_triangle_glb();
        let rel_a = "models/a.glb";
        let rel_b = "models/b.glb";
        fs::write(root.join("assets").join(rel_a), &glb).expect("a");
        fs::write(root.join("assets").join(rel_b), &glb).expect("b");

        let settings = default_settings_json();
        let asset_a = preview_asset_guid(None, rel_a);
        let asset_b = preview_asset_guid(None, rel_b);

        let mut session_a = GltfPreviewSession::start_with_settings_and_asset(
            &root,
            "assets",
            rel_a,
            &settings,
            asset_a,
            None,
        )
        .expect("start a");
        poll_until_cpu_ready(&mut session_a, "A");
        assert!(session_a.is_cpu_ready());

        let mut store = HostGltfPreviewStore::new();
        let parked = session_a.park_cpu_ready().expect("park a");
        assert_eq!(parked.relative_path, rel_a);
        store.park(parked).expect("insert a");
        assert_eq!(store.len(), 1);

        let mut session_b = GltfPreviewSession::start_with_settings_and_asset(
            &root,
            "assets",
            rel_b,
            &settings,
            asset_b,
            None,
        )
        .expect("start b");
        poll_until_cpu_ready(&mut session_b, "B");
        let parked_b = session_b.park_cpu_ready().expect("park b");
        store.park(parked_b).expect("insert b");
        assert_eq!(store.len(), 2);

        let abs_a = root.join("assets").join(rel_a);
        let key_a = GltfPreviewSession::cache_key_for(rel_a, &abs_a, &settings, asset_a);
        let loaded = store.take(&key_a).expect("A→B→A hit");
        let restored = GltfPreviewSession::from_cpu_ready(
            &root,
            "assets",
            rel_a,
            &settings,
            asset_a,
            loaded,
            None,
        )
        .expect("restore");
        assert!(restored.is_cpu_ready());
        assert!(!restored.is_loading());
        assert_eq!(store.len(), 1);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn preview_load_reports_cook_hit_after_disk_seed() {
        let root = temporary_root("cook_hit");
        let rel = "models/hero.glb";
        let abs = root.join("assets").join(rel);
        fs::write(&abs, valid_triangle_glb()).expect("write");
        let cook_root = root.join(".yuyib_cook");
        let settings = default_settings_json();
        let asset = preview_asset_guid(None, rel);

        // First load seeds the cook cache (miss).
        let mut miss = GltfPreviewSession::start_with_settings_and_asset(
            &root,
            "assets",
            rel,
            &settings,
            asset,
            Some(&cook_root),
        )
        .expect("start miss");
        poll_until_cpu_ready(&mut miss, "cook miss");
        assert_eq!(miss.last_cook_hit(), Some(false));
        drop(miss);

        // Second load must hit disk cook without re-parse.
        let mut hit = GltfPreviewSession::start_with_settings_and_asset(
            &root,
            "assets",
            rel,
            &settings,
            asset,
            Some(&cook_root),
        )
        .expect("start hit");
        poll_until_cpu_ready(&mut hit, "cook hit");
        assert_eq!(hit.last_cook_hit(), Some(true));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn host_store_take_misses_when_content_hash_changes() {
        let root = temporary_root("hash");
        let rel = "models/hero.glb";
        fs::write(root.join("assets").join(rel), valid_triangle_glb()).expect("write");
        let settings = default_settings_json();
        let asset = preview_asset_guid(None, rel);

        let mut session = GltfPreviewSession::start_with_settings_and_asset(
            &root,
            "assets",
            rel,
            &settings,
            asset,
            None,
        )
        .expect("start");
        poll_until_cpu_ready(&mut session, "hash fixture");
        assert!(session.is_cpu_ready());
        let key_before = session.last_cache_key().expect("key").clone();
        let mut store = HostGltfPreviewStore::new();
        store
            .park(session.park_cpu_ready().expect("park"))
            .expect("insert");
        assert!(store.contains(&key_before));

        // Mutate bytes → content hash changes → lookup key misses.
        let mut mutated = valid_triangle_glb();
        mutated.push(0);
        fs::write(root.join("assets").join(rel), mutated).expect("mutate");
        let key_after = GltfPreviewSession::cache_key_for(
            rel,
            &root.join("assets").join(rel),
            &settings,
            asset,
        );
        assert_ne!(key_before, key_after);
        assert!(store.take(&key_after).is_none());
        assert!(store.contains(&key_before));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn animation_inventory_and_selection_roundtrip() {
        let root = temporary_root("anim_sel");
        let rel = "models/animated.glb";
        fs::write(root.join("assets").join(rel), animated_triangle_glb()).expect("write");
        let settings = serde_json::json!({ "policy": "skeletal_preview" });
        let asset = preview_asset_guid(None, rel);

        let mut session = GltfPreviewSession::start_with_settings_and_asset(
            &root,
            "assets",
            rel,
            &settings,
            asset,
            None,
        )
        .expect("start");
        poll_until_cpu_ready(&mut session, "animated");

        let inventory = session.animation_inventory();
        assert_eq!(inventory.len(), 2);
        assert_eq!(inventory[0].index, 0);
        assert_eq!(inventory[0].name.as_deref(), Some("bob"));
        assert!((inventory[0].duration_seconds - 1.0).abs() < 1.0e-5);
        assert_eq!(inventory[1].index, 1);
        assert_eq!(inventory[1].name.as_deref(), Some("shift"));

        // CPU-ready auto-selects the first clip when inventory is non-empty.
        assert_eq!(session.selected_animation(), Some(0));

        session
            .set_animation_selection(None)
            .expect("clear clip");
        assert_eq!(session.selected_animation(), None);

        session
            .set_animation_selection(Some(1))
            .expect("select shift");
        assert_eq!(session.selected_animation(), Some(1));

        assert!(matches!(
            session.set_animation_selection(Some(99)),
            Err(GltfPreviewError::Selection(message))
                if message.contains("out of range")
        ));
        assert_eq!(session.selected_animation(), Some(1));

        let _ = fs::remove_dir_all(&root);
    }
}
