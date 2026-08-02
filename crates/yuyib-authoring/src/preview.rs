use std::{
    any::Any,
    collections::BTreeSet,
    error::Error,
    fmt,
    num::{NonZeroU32, NonZeroU64},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{AssetGuid, CapabilityId, ContentHash, ImportSettingsSchemaId, SchemaVersion};

/// Hard memory and diagnostic limits for one preview job.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PreviewBudgets {
    /// Maximum decoded CPU bytes retained by the job.
    pub max_cpu_bytes: NonZeroU64,
    /// Maximum GPU bytes retained by the job.
    pub max_gpu_bytes: NonZeroU64,
    /// Maximum diagnostics retained for the job.
    pub max_diagnostics: NonZeroU32,
}

/// Bounded preview cache and invalidation policy.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PreviewCachePolicy {
    /// Maximum cached preview entries.
    pub max_entries: NonZeroU32,
    /// Maximum total cache bytes.
    pub max_bytes: NonZeroU64,
    /// Invalidate when source content changes without changing asset identity.
    pub invalidate_on_content_hash_change: bool,
    /// Invalidate when persisted import settings change.
    pub invalidate_on_import_settings_change: bool,
}

/// Optional geometric and shading debug visualization.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PreviewOverlay {
    /// Imported or generated collision shapes.
    Collision,
    /// Vertex normals.
    Normals,
    /// Vertex tangents.
    Tangents,
    /// UV coordinates.
    Uv,
    /// Local and aggregate bounds.
    Bounds,
}

/// A sub-resource selected for preview.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum PreviewSelection {
    /// Zero-based imported mesh index.
    Mesh(u32),
    /// Zero-based imported material index.
    Material(u32),
    /// Stable importer-provided animation clip name.
    AnimationClip(String),
}

/// Non-destructive material override for preview only.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct PreviewMaterialOverride {
    /// Stable material parameter names mapped to neutral JSON values.
    pub parameters: std::collections::BTreeMap<String, Value>,
}

/// Runtime render preset shared with Play Mode.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PreviewRenderPreset {
    /// Stable project-defined or engine-defined preset name.
    pub id: CapabilityId,
}

/// Explicit preview features supported by an adapter.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct PreviewFeatures {
    /// The importer can enumerate and preview individual meshes.
    pub mesh_selection: bool,
    /// The importer can enumerate and preview individual materials.
    pub material_selection: bool,
    /// The importer can enumerate and switch animation clips.
    pub animation_selection: bool,
    /// Supported visual debugging overlays.
    pub overlays: BTreeSet<PreviewOverlay>,
    /// Material overrides can be applied without changing authored data.
    pub material_override: bool,
    /// Preview accepts the same render presets as Play Mode.
    pub shared_render_presets: bool,
    /// Import settings can be changed and reimported without modifying source.
    pub non_destructive_reimport: bool,
}

/// Authoring preview contract for one engine capability.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PreviewDescriptor {
    capability: CapabilityId,
    features: PreviewFeatures,
    budgets: PreviewBudgets,
    cache: PreviewCachePolicy,
}

impl PreviewDescriptor {
    /// Creates a bounded preview descriptor.
    #[must_use]
    pub const fn new(
        capability: CapabilityId,
        features: PreviewFeatures,
        budgets: PreviewBudgets,
        cache: PreviewCachePolicy,
    ) -> Self {
        Self {
            capability,
            features,
            budgets,
            cache,
        }
    }

    /// Returns the capability providing the preview pipeline.
    #[must_use]
    pub const fn capability(&self) -> &CapabilityId {
        &self.capability
    }

    /// Returns supported inspection and override features.
    #[must_use]
    pub const fn features(&self) -> &PreviewFeatures {
        &self.features
    }

    /// Returns per-job hard budgets.
    #[must_use]
    pub const fn budgets(&self) -> PreviewBudgets {
        self.budgets
    }

    /// Returns cache bounds and invalidation rules.
    #[must_use]
    pub const fn cache(&self) -> PreviewCachePolicy {
        self.cache
    }
}

/// Thread-safe cooperative cancellation shared with importer/cooker work.
#[derive(Clone, Debug, Default)]
pub struct PreviewCancellation(Arc<AtomicBool>);

impl PreviewCancellation {
    /// Requests cancellation. Repeated calls are harmless.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    /// Reports whether cancellation has been requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

/// Immutable input for the source-import-cook-preview pipeline.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PreviewRequest {
    /// Persistent asset identity, independent from contents and path.
    pub asset: AssetGuid,
    /// Logical source URI or project-relative source path.
    pub source: String,
    /// Optional cache/invalidation hash of the current source contents.
    pub content_hash: Option<ContentHash>,
    /// Stable importer-settings schema.
    pub import_settings_schema: ImportSettingsSchemaId,
    /// Importer-settings schema version.
    pub import_settings_version: SchemaVersion,
    /// Opaque settings consumed by the same importer/cooker as runtime assets.
    pub import_settings: Value,
    /// Optional mesh, material, or animation selection.
    pub selection: Option<PreviewSelection>,
    /// Requested visual debugging overlays.
    pub overlays: BTreeSet<PreviewOverlay>,
    /// Optional non-persisted material override.
    pub material_override: Option<PreviewMaterialOverride>,
    /// Optional render preset also understood by Play Mode.
    pub render_preset: Option<PreviewRenderPreset>,
    /// Cooperative cancellation observed by every pipeline stage.
    #[serde(skip, default)]
    pub cancellation: PreviewCancellation,
}

/// Work bound supplied for one non-blocking job poll.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PreviewPollBudget {
    /// Maximum pipeline work items advanced in this poll.
    pub max_work_items: NonZeroU32,
    /// Maximum wall-clock milliseconds spent in this poll.
    pub max_millis: NonZeroU32,
    /// Maximum new diagnostics returned in this poll.
    pub max_diagnostics: NonZeroU32,
}

/// Bounded progress through a named importer/cooker/preview stage.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct PreviewProgress {
    /// Stable stage label such as `decode`, `cook`, or `upload`.
    pub stage: String,
    /// Completed units.
    pub completed: u64,
    /// Total units when known. Must not be less than `completed`.
    pub total: Option<u64>,
}

impl PreviewProgress {
    /// Constructs validated progress.
    ///
    /// # Errors
    ///
    /// Returns [`PreviewJobError`] when completed units exceed a known total.
    pub fn new(
        stage: impl Into<String>,
        completed: u64,
        total: Option<u64>,
    ) -> Result<Self, PreviewJobError> {
        if total.is_some_and(|total| completed > total) {
            return Err(PreviewJobError::new("preview progress exceeds its total"));
        }
        Ok(Self {
            stage: stage.into(),
            completed,
            total,
        })
    }
}

/// Severity of an importer, cooker, or viewport diagnostic.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PreviewDiagnosticSeverity {
    /// Informational imported metadata.
    Info,
    /// Recoverable mismatch or fallback.
    Warning,
    /// Preview cannot faithfully represent part or all of the asset.
    Error,
}

/// Structured preview diagnostic suitable for filtering and source attribution.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PreviewDiagnostic {
    /// Stable diagnostic code.
    pub code: String,
    /// Severity.
    pub severity: PreviewDiagnosticSeverity,
    /// Human-readable explanation.
    pub message: String,
    /// Optional importer-defined object path, material, mesh, or channel.
    pub subject: Option<String>,
}

/// Current terminal or non-terminal state of a preview job.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PreviewJobState {
    /// More bounded polls are required.
    Running,
    /// A neutral/cooked asset and viewport representation are ready.
    Ready,
    /// Cancellation completed and resources were released.
    Cancelled,
}

/// One bounded update emitted by a preview job.
#[derive(Clone, Debug, PartialEq)]
pub struct PreviewUpdate {
    /// Current stage progress.
    pub progress: PreviewProgress,
    /// New bounded diagnostics from this poll.
    pub diagnostics: Vec<PreviewDiagnostic>,
    /// Current job state.
    pub state: PreviewJobState,
}

/// Owned neutral/cooked result published by a completed preview job.
///
/// The payload is adapter-defined and deliberately type-erased at the editor
/// boundary. It must be produced by the runtime importer/cooker path, is moved
/// exactly once to the viewport adapter, and is released when this value is
/// dropped. GPU upload may therefore stay on the render thread instead of
/// leaking process-local handles into a persisted document.
pub struct PreviewArtifact {
    asset: AssetGuid,
    capability: CapabilityId,
    cpu_bytes: u64,
    gpu_bytes: u64,
    payload: Box<dyn Any + Send>,
}

impl PreviewArtifact {
    /// Publishes one adapter-owned artifact with its accounted memory usage.
    #[must_use]
    pub fn new<T: Any + Send>(
        asset: AssetGuid,
        capability: CapabilityId,
        cpu_bytes: u64,
        gpu_bytes: u64,
        payload: T,
    ) -> Self {
        Self {
            asset,
            capability,
            cpu_bytes,
            gpu_bytes,
            payload: Box::new(payload),
        }
    }

    /// Returns the persistent asset identity represented by this artifact.
    #[must_use]
    pub const fn asset(&self) -> AssetGuid {
        self.asset
    }

    /// Returns the capability whose viewport adapter understands the payload.
    #[must_use]
    pub const fn capability(&self) -> &CapabilityId {
        &self.capability
    }

    /// Returns accounted decoded CPU bytes.
    #[must_use]
    pub const fn cpu_bytes(&self) -> u64 {
        self.cpu_bytes
    }

    /// Returns accounted GPU bytes, including zero before render-thread upload.
    #[must_use]
    pub const fn gpu_bytes(&self) -> u64 {
        self.gpu_bytes
    }

    /// Moves and downcasts the adapter payload.
    ///
    /// # Errors
    ///
    /// Returns the intact artifact when the requested type is not the type
    /// published by the adapter.
    pub fn downcast<T: Any + Send>(self) -> Result<T, Self> {
        let Self {
            asset,
            capability,
            cpu_bytes,
            gpu_bytes,
            payload,
        } = self;
        match payload.downcast::<T>() {
            Ok(payload) => Ok(*payload),
            Err(payload) => Err(Self {
                asset,
                capability,
                cpu_bytes,
                gpu_bytes,
                payload,
            }),
        }
    }

    /// Splits the artifact into type-erased parts for cache storage.
    #[must_use]
    pub fn into_any_payload(self) -> Box<dyn Any + Send> {
        self.payload
    }

    /// Rebuilds an artifact from cache storage parts.
    #[must_use]
    pub fn from_parts(
        asset: AssetGuid,
        capability: CapabilityId,
        cpu_bytes: u64,
        gpu_bytes: u64,
        payload: Box<dyn Any + Send>,
    ) -> Self {
        Self {
            asset,
            capability,
            cpu_bytes,
            gpu_bytes,
            payload,
        }
    }
}

impl fmt::Debug for PreviewArtifact {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreviewArtifact")
            .field("asset", &self.asset)
            .field("capability", &self.capability)
            .field("cpu_bytes", &self.cpu_bytes)
            .field("gpu_bytes", &self.gpu_bytes)
            .finish_non_exhaustive()
    }
}

/// A cancellable, incrementally polled preview pipeline job.
pub trait PreviewJob: Send {
    /// Advances work within the supplied limits.
    ///
    /// Implementations must observe [`PreviewRequest::cancellation`] between
    /// bounded work items and must not return more diagnostics than requested.
    ///
    /// # Errors
    ///
    /// Returns an adapter error while retaining enough state for cleanup or a
    /// subsequent cancellation poll.
    fn poll(&mut self, budget: PreviewPollBudget) -> Result<PreviewUpdate, PreviewJobError>;

    /// Takes the completed neutral/cooked artifact exactly once.
    ///
    /// This is valid only after a poll returned the ready job state. Calling
    /// it before readiness or more than once must return an error.
    ///
    /// # Errors
    ///
    /// Returns a lifecycle or adapter error without using an implicit side
    /// channel to publish renderer state.
    fn take_artifact(&mut self) -> Result<PreviewArtifact, PreviewJobError>;
}

/// Adapter that reuses the runtime importer/cooker and renderer representation.
pub trait PreviewAdapter: Send + Sync {
    /// Returns the machine-readable preview capability and limits.
    fn descriptor(&self) -> &PreviewDescriptor;

    /// Starts a cancellable pipeline for one authored request.
    ///
    /// # Errors
    ///
    /// Returns an error when settings or requested features are unsupported.
    fn start(&self, request: PreviewRequest) -> Result<Box<dyn PreviewJob>, PreviewJobError>;
}

/// Preview adapter or job failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreviewJobError {
    message: String,
}

impl PreviewJobError {
    /// Creates a preview error.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// Returns the diagnostic message.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for PreviewJobError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for PreviewJobError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_is_shared_and_progress_is_validated() {
        let first = PreviewCancellation::default();
        let second = first.clone();
        assert!(!second.is_cancelled());
        first.cancel();
        assert!(second.is_cancelled());
        assert!(PreviewProgress::new("decode", 4, Some(3)).is_err());
        assert!(PreviewProgress::new("decode", 3, Some(3)).is_ok());
    }

    #[test]
    fn request_serialization_excludes_process_local_cancellation() {
        let request = PreviewRequest {
            asset: AssetGuid::new(),
            source: "assets/map.glb".to_owned(),
            content_hash: Some(ContentHash::new("sha256:12ab").expect("hash")),
            import_settings_schema: ImportSettingsSchemaId::new("yuyib.gltf-import").expect("id"),
            import_settings_version: SchemaVersion::new(1).expect("version"),
            import_settings: serde_json::json!({"generate_tangents": true}),
            selection: Some(PreviewSelection::AnimationClip("idle".to_owned())),
            overlays: BTreeSet::from([PreviewOverlay::Bounds]),
            material_override: None,
            render_preset: Some(PreviewRenderPreset {
                id: CapabilityId::new("project.cyberpunk").expect("preset id"),
            }),
            cancellation: PreviewCancellation::default(),
        };
        let json = serde_json::to_string(&request).expect("serialize request");
        assert!(!json.contains("cancellation"));
    }

    #[test]
    fn completed_artifact_has_explicit_single_owner_and_typed_handoff() {
        let asset = AssetGuid::new();
        let capability = CapabilityId::new("yuyib.preview-test").expect("capability");
        let artifact = PreviewArtifact::new(asset, capability.clone(), 12, 34, vec![1_u8, 2, 3]);
        assert_eq!(artifact.asset(), asset);
        assert_eq!(artifact.capability(), &capability);
        assert_eq!(artifact.cpu_bytes(), 12);
        assert_eq!(artifact.gpu_bytes(), 34);
        assert_eq!(
            artifact.downcast::<Vec<u8>>().expect("typed payload"),
            vec![1, 2, 3]
        );
    }
}
