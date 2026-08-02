//! [`PreviewAdapter`] wrapping production [`GltfSceneLoad`].

use std::{
    collections::BTreeSet,
    num::{NonZeroU32, NonZeroU64},
    path::{Path, PathBuf},
    sync::Arc,
};

use yuyib_assets::{ImportDiagnostic, ImportDiagnosticSeverity, ImporterRegistryLimits};
use yuyib_authoring::{
    AuthoringRegistry, CapabilityId, ImportSettingsSchemaId, PreviewAdapter, PreviewArtifact,
    PreviewBudgets, PreviewCachePolicy, PreviewDescriptor, PreviewDiagnostic,
    PreviewDiagnosticSeverity, PreviewFeatures, PreviewJob, PreviewJobError, PreviewJobState,
    PreviewOverlay, PreviewPollBudget, PreviewProgress, PreviewRequest, PreviewSelection,
    PreviewUpdate, RegistrationError, SchemaVersion,
};
use yuyib_render_3d::{GltfSceneLoad, GltfSceneLoadConfig, GltfSceneLoadStage};

use crate::import_settings::{
    GLTF_IMPORT_SETTINGS_SCHEMA, parse_import_settings,
};

const PREVIEW_CAPABILITY: &str = "yuyib.gltf-preview";
const MAX_SOURCE_BYTES: usize = 256 * 1024 * 1024;

/// Production glTF preview adapter (no editor-only decoder).
pub struct GltfPreviewAdapter {
    descriptor: PreviewDescriptor,
}

impl GltfPreviewAdapter {
    /// Creates the bounded adapter descriptor used by coverage manifests.
    #[must_use]
    pub fn new() -> Self {
        let capability = CapabilityId::new(PREVIEW_CAPABILITY).expect("stable capability id");
        let features = PreviewFeatures {
            non_destructive_reimport: true,
            shared_render_presets: true,
            mesh_selection: true,
            material_selection: true,
            overlays: BTreeSet::from([
                PreviewOverlay::Bounds,
                PreviewOverlay::Collision,
                PreviewOverlay::Normals,
                PreviewOverlay::Tangents,
                PreviewOverlay::Uv,
            ]),
            ..PreviewFeatures::default()
        };
        let budgets = PreviewBudgets {
            max_cpu_bytes: NonZeroU64::new(512 * 1024 * 1024).expect("budget"),
            max_gpu_bytes: NonZeroU64::new(512 * 1024 * 1024).expect("budget"),
            max_diagnostics: NonZeroU32::new(256).expect("budget"),
        };
        let cache = PreviewCachePolicy {
            max_entries: NonZeroU32::new(8).expect("cache"),
            max_bytes: NonZeroU64::new(1024 * 1024 * 1024).expect("cache"),
            invalidate_on_content_hash_change: true,
            invalidate_on_import_settings_change: true,
        };
        Self {
            descriptor: PreviewDescriptor::new(capability, features, budgets, cache),
        }
    }

    /// Builds a policy-aware cache key for `request` (same policy as coverage).
    #[must_use]
    pub fn cache_key(&self, request: &PreviewRequest) -> yuyib_authoring::PreviewCacheKey {
        yuyib_authoring::PreviewCacheKey::from_request(request, self.descriptor.cache())
    }
}

impl Default for GltfPreviewAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl PreviewAdapter for GltfPreviewAdapter {
    fn descriptor(&self) -> &PreviewDescriptor {
        &self.descriptor
    }

    fn start(&self, request: PreviewRequest) -> Result<Box<dyn PreviewJob>, PreviewJobError> {
        if request.import_settings_schema.as_str() != GLTF_IMPORT_SETTINGS_SCHEMA {
            return Err(PreviewJobError::new(format!(
                "unsupported import settings schema `{}` (expected `{GLTF_IMPORT_SETTINGS_SCHEMA}`)",
                request.import_settings_schema
            )));
        }
        if request.import_settings_version.get() != 1 {
            return Err(PreviewJobError::new(format!(
                "unsupported import settings version {} (expected 1)",
                request.import_settings_version.get()
            )));
        }
        if !request.overlays.is_empty() {
            let unsupported: Vec<_> = request
                .overlays
                .iter()
                .copied()
                .filter(|overlay| {
                    !matches!(
                        overlay,
                        PreviewOverlay::Bounds
                            | PreviewOverlay::Collision
                            | PreviewOverlay::Normals
                            | PreviewOverlay::Tangents
                            | PreviewOverlay::Uv
                    )
                })
                .collect();
            if !unsupported.is_empty() {
                return Err(PreviewJobError::new(format!(
                    "glTF preview overlays not registered yet: {unsupported:?} (Bounds/Collision/Normals/Tangents/UV are supported)"
                )));
            }
        }
        if let Some(selection) = &request.selection {
            match selection {
                PreviewSelection::Mesh(_) | PreviewSelection::Material(_) => {}
                PreviewSelection::AnimationClip(_) => {
                    return Err(PreviewJobError::new(
                        "glTF preview animation selection is not registered yet (Mesh/Material are supported)",
                    ));
                }
            }
        }

        let settings = parse_import_settings(&request.import_settings)
            .map_err(|error| PreviewJobError::new(error.to_string()))?;
        let path = PathBuf::from(&request.source);
        if !path.is_file() {
            return Err(PreviewJobError::new(format!(
                "glTF source is not a file: {}",
                path.display()
            )));
        }

        let config = GltfSceneLoadConfig::default()
            .with_import_options(settings.to_import_options())
            .with_importer_registry_limits(ImporterRegistryLimits {
                max_source_bytes: MAX_SOURCE_BYTES,
                ..ImporterRegistryLimits::default()
            })
            .with_static_collider(false);
        let loading = GltfSceneLoad::start(&path, config)
            .map_err(|error| PreviewJobError::new(error.to_string()))?;

        Ok(Box::new(GltfPreviewJob {
            request,
            loading: Some(loading),
            artifact: None,
            taken: false,
            path,
        }))
    }
}

struct GltfPreviewJob {
    request: PreviewRequest,
    loading: Option<GltfSceneLoad>,
    artifact: Option<PreviewArtifact>,
    taken: bool,
    path: PathBuf,
}

impl PreviewJob for GltfPreviewJob {
    fn poll(&mut self, budget: PreviewPollBudget) -> Result<PreviewUpdate, PreviewJobError> {
        if self.request.cancellation.is_cancelled() {
            self.loading = None;
            self.artifact = None;
            return Ok(PreviewUpdate {
                progress: PreviewProgress::new("cancelled", 0, Some(0))
                    .expect("cancelled progress"),
                diagnostics: Vec::new(),
                state: PreviewJobState::Cancelled,
            });
        }

        if self.artifact.is_some() {
            return Ok(PreviewUpdate {
                progress: PreviewProgress::new("ready", 1, Some(1)).expect("ready progress"),
                diagnostics: Vec::new(),
                state: PreviewJobState::Ready,
            });
        }

        let Some(loading) = &mut self.loading else {
            return Err(PreviewJobError::new("glTF preview job has no active load"));
        };

        // Cooperative bound: one importer update per poll (matches editor tick).
        let _ = budget;
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
            let message = loading
                .failure()
                .map(|error| error.to_string())
                .unwrap_or_else(|| "glTF import failed".to_owned());
            self.loading = None;
            return Err(PreviewJobError::new(message));
        }

        if progress.stage == GltfSceneLoadStage::Ready {
            let loaded = loading
                .take_ready()
                .map_err(|error| PreviewJobError::new(error.to_string()))?;
            self.loading = None;
            let diagnostics = map_diagnostics(loaded.diagnostics(), budget.max_diagnostics.get());
            let cpu_bytes = estimate_cpu_bytes(&self.path);
            let capability = CapabilityId::new(PREVIEW_CAPABILITY).expect("capability");
            self.artifact = Some(PreviewArtifact::new(
                self.request.asset,
                capability,
                cpu_bytes,
                0,
                loaded,
            ));
            return Ok(PreviewUpdate {
                progress: PreviewProgress::new("ready", 1, Some(1)).expect("ready progress"),
                diagnostics,
                state: PreviewJobState::Ready,
            });
        }

        Ok(PreviewUpdate {
            progress: PreviewProgress::new(
                stage,
                progress.completed_work,
                Some(progress.total_work.max(1)),
            )
            .map_err(|error| PreviewJobError::new(error.to_string()))?,
            diagnostics: Vec::new(),
            state: PreviewJobState::Running,
        })
    }

    fn take_artifact(&mut self) -> Result<PreviewArtifact, PreviewJobError> {
        if self.taken {
            return Err(PreviewJobError::new(
                "glTF preview artifact was already taken",
            ));
        }
        let artifact = self.artifact.take().ok_or_else(|| {
            PreviewJobError::new("glTF preview artifact is not ready")
        })?;
        self.taken = true;
        Ok(artifact)
    }
}

fn map_diagnostics(source: &[ImportDiagnostic], max: u32) -> Vec<PreviewDiagnostic> {
    source
        .iter()
        .take(max as usize)
        .map(|diagnostic| PreviewDiagnostic {
            code: diagnostic.code.clone(),
            severity: match diagnostic.severity {
                ImportDiagnosticSeverity::Info => PreviewDiagnosticSeverity::Info,
                ImportDiagnosticSeverity::Warning => PreviewDiagnosticSeverity::Warning,
            },
            message: diagnostic.message.clone(),
            subject: None,
        })
        .collect()
}

fn estimate_cpu_bytes(path: &Path) -> u64 {
    std::fs::metadata(path)
        .map(|meta| meta.len())
        .unwrap_or(0)
}

/// Registers [`GltfPreviewAdapter`] on an Asset-covered `yuyib.gltf-preview`.
///
/// # Errors
///
/// Forwards [`AuthoringRegistry::register_preview`] failures.
pub fn register_gltf_preview(
    registry: &mut AuthoringRegistry,
    adapter: Arc<GltfPreviewAdapter>,
) -> Result<(), RegistrationError> {
    let _ = (
        ImportSettingsSchemaId::new(GLTF_IMPORT_SETTINGS_SCHEMA),
        SchemaVersion::new(1),
    );
    registry.register_preview(adapter)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use yuyib_authoring::{
        AssetCoverageEvidence, AssetGuid, AuthoringRegistry, CapabilityDescriptor, CoverageStatus,
        ImportSettingsSchemaId, PluginId, PreviewCancellation, SchemaVersion,
    };

    fn registry_with_asset_preview() -> AuthoringRegistry {
        let mut registry = AuthoringRegistry::new();
        let capability = CapabilityId::new(PREVIEW_CAPABILITY).expect("id");
        let settings = ImportSettingsSchemaId::new(GLTF_IMPORT_SETTINGS_SCHEMA).expect("settings");
        registry
            .register_capability(
                CapabilityDescriptor::new(
                    capability.clone(),
                    "glTF authoring preview",
                    CoverageStatus::Asset,
                    PluginId::new("yuyib.gltf").expect("plugin"),
                )
                .with_documentation("crates/yuyib-gltf-authoring")
                .with_asset_evidence(AssetCoverageEvidence::new(
                    settings,
                    capability,
                    ["gltf-flat-only-material"],
                )),
            )
            .expect("register capability");
        registry
    }

    #[test]
    fn registers_adapter_when_capability_is_asset() {
        let mut registry = registry_with_asset_preview();
        register_gltf_preview(&mut registry, Arc::new(GltfPreviewAdapter::new()))
            .expect("register adapter");
        let capability = CapabilityId::new(PREVIEW_CAPABILITY).expect("id");
        assert!(registry.preview_adapter(&capability).is_some());
        assert!(registry.preview_descriptor(&capability).is_some());
    }

    #[test]
    fn start_rejects_missing_file() {
        let adapter = GltfPreviewAdapter::new();
        let request = PreviewRequest {
            asset: AssetGuid::new(),
            source: "definitely/missing/preview.glb".to_owned(),
            content_hash: None,
            import_settings_schema: ImportSettingsSchemaId::new(GLTF_IMPORT_SETTINGS_SCHEMA)
                .expect("schema"),
            import_settings_version: SchemaVersion::new(1).expect("version"),
            import_settings: json!({}),
            selection: None,
            overlays: Default::default(),
            material_override: None,
            render_preset: None,
            cancellation: PreviewCancellation::default(),
        };
        let Err(error) = adapter.start(request) else {
            panic!("missing file must reject start");
        };
        assert!(error.message().contains("not a file"));
    }

    #[test]
    fn descriptor_advertises_mesh_selection() {
        let adapter = GltfPreviewAdapter::new();
        assert!(adapter.descriptor().features().mesh_selection);
        assert!(adapter.descriptor().features().material_selection);
    }

    #[test]
    fn start_allows_mesh_selection_before_file_check() {
        let adapter = GltfPreviewAdapter::new();
        let request = PreviewRequest {
            asset: AssetGuid::new(),
            source: "definitely/missing/preview.glb".to_owned(),
            content_hash: None,
            import_settings_schema: ImportSettingsSchemaId::new(GLTF_IMPORT_SETTINGS_SCHEMA)
                .expect("schema"),
            import_settings_version: SchemaVersion::new(1).expect("version"),
            import_settings: json!({}),
            selection: Some(PreviewSelection::Mesh(0)),
            overlays: Default::default(),
            material_override: None,
            render_preset: None,
            cancellation: PreviewCancellation::default(),
        };
        let Err(error) = adapter.start(request) else {
            panic!("missing file must still reject after Mesh is accepted");
        };
        assert!(error.message().contains("not a file"));
        assert!(!error.message().contains("selection is not registered"));
    }

    #[test]
    fn start_allows_material_selection_before_file_check() {
        let adapter = GltfPreviewAdapter::new();
        let request = PreviewRequest {
            asset: AssetGuid::new(),
            source: "definitely/missing/preview.glb".to_owned(),
            content_hash: None,
            import_settings_schema: ImportSettingsSchemaId::new(GLTF_IMPORT_SETTINGS_SCHEMA)
                .expect("schema"),
            import_settings_version: SchemaVersion::new(1).expect("version"),
            import_settings: json!({}),
            selection: Some(PreviewSelection::Material(0)),
            overlays: Default::default(),
            material_override: None,
            render_preset: None,
            cancellation: PreviewCancellation::default(),
        };
        let Err(error) = adapter.start(request) else {
            panic!("missing file must still reject after Material is accepted");
        };
        assert!(error.message().contains("not a file"));
        assert!(!error.message().contains("selection is not registered"));
    }

    #[test]
    fn start_rejects_animation_selection() {
        let adapter = GltfPreviewAdapter::new();
        let request = PreviewRequest {
            asset: AssetGuid::new(),
            source: "definitely/missing/preview.glb".to_owned(),
            content_hash: None,
            import_settings_schema: ImportSettingsSchemaId::new(GLTF_IMPORT_SETTINGS_SCHEMA)
                .expect("schema"),
            import_settings_version: SchemaVersion::new(1).expect("version"),
            import_settings: json!({}),
            selection: Some(PreviewSelection::AnimationClip("idle".to_owned())),
            overlays: Default::default(),
            material_override: None,
            render_preset: None,
            cancellation: PreviewCancellation::default(),
        };
        let Err(error) = adapter.start(request) else {
            panic!("animation selection must reject start");
        };
        assert!(error.message().contains("animation selection"));
    }

    #[test]
    fn descriptor_advertises_bounds_overlay() {
        let adapter = GltfPreviewAdapter::new();
        assert!(
            adapter
                .descriptor()
                .features()
                .overlays
                .contains(&PreviewOverlay::Bounds)
        );
        assert!(
            adapter
                .descriptor()
                .features()
                .overlays
                .contains(&PreviewOverlay::Collision)
        );
        assert!(
            adapter
                .descriptor()
                .features()
                .overlays
                .contains(&PreviewOverlay::Normals)
        );
        assert!(
            adapter
                .descriptor()
                .features()
                .overlays
                .contains(&PreviewOverlay::Tangents)
        );
        assert!(
            adapter
                .descriptor()
                .features()
                .overlays
                .contains(&PreviewOverlay::Uv)
        );
    }

    #[test]
    fn cache_key_misses_when_content_hash_changes() {
        let adapter = GltfPreviewAdapter::new();
        let asset = AssetGuid::new();
        let base = PreviewRequest {
            asset,
            source: "assets/a.glb".to_owned(),
            content_hash: Some(
                yuyib_authoring::ContentHash::new("blake3:aa").expect("hash"),
            ),
            import_settings_schema: ImportSettingsSchemaId::new(GLTF_IMPORT_SETTINGS_SCHEMA)
                .expect("schema"),
            import_settings_version: SchemaVersion::new(1).expect("version"),
            import_settings: json!({}),
            selection: None,
            overlays: Default::default(),
            material_override: None,
            render_preset: None,
            cancellation: PreviewCancellation::default(),
        };
        let mut changed = base.clone();
        changed.content_hash =
            Some(yuyib_authoring::ContentHash::new("blake3:bb").expect("hash"));
        assert_ne!(adapter.cache_key(&base), adapter.cache_key(&changed));
    }

    #[test]
    fn start_accepts_all_overlay_variants_before_file_check() {
        let adapter = GltfPreviewAdapter::new();
        let request = PreviewRequest {
            asset: AssetGuid::new(),
            source: "definitely/missing/preview.glb".to_owned(),
            content_hash: None,
            import_settings_schema: ImportSettingsSchemaId::new(GLTF_IMPORT_SETTINGS_SCHEMA)
                .expect("schema"),
            import_settings_version: SchemaVersion::new(1).expect("version"),
            import_settings: json!({}),
            selection: None,
            overlays: BTreeSet::from([
                PreviewOverlay::Bounds,
                PreviewOverlay::Collision,
                PreviewOverlay::Normals,
                PreviewOverlay::Tangents,
                PreviewOverlay::Uv,
            ]),
            material_override: None,
            render_preset: None,
            cancellation: PreviewCancellation::default(),
        };
        let Err(error) = adapter.start(request) else {
            panic!("missing file must still reject");
        };
        assert!(error.message().contains("not a file"));
        assert!(!error.message().contains("overlays not registered"));
    }

    #[test]
    fn start_allows_bounds_overlay_before_file_check() {
        let adapter = GltfPreviewAdapter::new();
        let request = PreviewRequest {
            asset: AssetGuid::new(),
            source: "definitely/missing/preview.glb".to_owned(),
            content_hash: None,
            import_settings_schema: ImportSettingsSchemaId::new(GLTF_IMPORT_SETTINGS_SCHEMA)
                .expect("schema"),
            import_settings_version: SchemaVersion::new(1).expect("version"),
            import_settings: json!({}),
            selection: None,
            overlays: BTreeSet::from([
                PreviewOverlay::Bounds,
                PreviewOverlay::Collision,
                PreviewOverlay::Normals,
                PreviewOverlay::Tangents,
                PreviewOverlay::Uv,
            ]),
            material_override: None,
            render_preset: None,
            cancellation: PreviewCancellation::default(),
        };
        let Err(error) = adapter.start(request) else {
            panic!("missing file must still reject after all overlays are accepted");
        };
        assert!(error.message().contains("not a file"));
        assert!(!error.message().contains("overlays not registered"));
    }
}
