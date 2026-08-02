//! Production glTF preview session for the Editor viewport.
//!
//! Uses [`GltfSceneLoad`] — the same importer/cook path as runtime examples.
//! There is no editor-only decoder.

use std::{
    error::Error,
    fmt,
    path::{Path, PathBuf},
};

use yuyib_assets::ImporterRegistryLimits;
use yuyib_authoring::{
    AssetGuid, ContentHash, ImportSettingsSchemaId, PreviewCacheKey, PreviewCancellation,
    PreviewMaterialOverride, PreviewRequest, SchemaVersion,
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
        let relative_path = relative_path.into();
        let absolute_path = resolve_asset_path(project_root, asset_root, &relative_path)?;
        if !absolute_path.is_file() {
            return Err(GltfPreviewError::MissingFile(absolute_path));
        }
        let adapter = GltfPreviewAdapter::new();
        let asset = AssetGuid::new();
        let content_hash = hash_asset_file(&absolute_path).ok();
        let cache_key = session_cache_key(
            &adapter,
            asset,
            &relative_path,
            content_hash,
            import_settings,
        );
        let loading = start_load(&absolute_path, import_settings)?;
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
            gpu_ready: false,
            gpu_event_sent: false,
            last_gpu: None,
        })
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
        let loading = start_load(&self.absolute_path, import_settings)?;
        self.loading = Some(loading);
        self.import_settings = import_settings.clone();
        self.last_cache_key = Some(cache_key);
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
                failed: None,
            };
        }
        GltfPreviewPoll {
            relative_path: self.relative_path.clone(),
            stage: "idle",
            completed: 0,
            total: 0,
            cpu_ready: false,
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
                failed: Some(failed),
            };
        }
        if progress.stage == GltfSceneLoadStage::Ready {
            match loading.take_ready() {
                Ok(loaded) => {
                    self.loading = None;
                    let mut loaded = loaded;
                    if let Err(error) = self.apply_material_override(&mut loaded) {
                        return GltfPreviewPoll {
                            relative_path: self.relative_path.clone(),
                            stage: "failed",
                            completed: progress.completed_work,
                            total: progress.total_work.max(1),
                            cpu_ready: self.ready.is_some(),
                            failed: Some(error.to_string()),
                        };
                    }
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
        self.loading = Some(start_load(&self.absolute_path, &self.import_settings)?);
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
) -> Result<GltfSceneLoad, GltfPreviewError> {
    let settings = parse_import_settings(import_settings)
        .map_err(|error| GltfPreviewError::Settings(error.to_string()))?;
    let options = settings.to_import_options();
    let config = GltfSceneLoadConfig::default()
        .with_import_options(options)
        .with_importer_registry_limits(ImporterRegistryLimits {
            max_source_bytes: EDITOR_PREVIEW_MAX_SOURCE_BYTES,
            ..ImporterRegistryLimits::default()
        })
        .with_static_collider(false);
    GltfSceneLoad::start(absolute_path, config)
        .map_err(|error| GltfPreviewError::Start(error.to_string()))
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
