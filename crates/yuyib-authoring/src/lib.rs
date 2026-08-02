//! Editor-neutral authoring contracts for Yuyib.
//!
//! Owns stable authoring contracts: schemas, commands, migrations, and preview
//! descriptors. No ECS world, renderer, window, or editor UI lives here.
//! Capability crates publish adapters; an editor consumes them without becoming
//! a runtime dependency.

#![forbid(unsafe_code)]

mod command;
mod descriptor;
mod identity;
mod migration;
mod preview;
mod preview_cache;
mod registry;
mod scene;
mod system;

pub use command::{
    CommandError, CommandHistory, CommandTransaction, DocumentCommand, Revision, TransactionError,
};
pub use descriptor::{
    AssetCoverageEvidence, CapabilityDescriptor, ComponentDescriptor, CoverageManifest,
    CoverageStatus, FieldDescriptor, FieldKind, ImportSettingsDescriptor,
};
pub use identity::{
    AssetGuid, CapabilityId, ComponentSchemaId, ContentHash, EntityGuid, ImportSettingsSchemaId,
    PluginId, ProjectGuid, SceneGuid, ScheduleId, SchemaVersion, StableIdError, SystemId,
};
pub use migration::{
    ComponentMigration, ImportSettingsMigration, MigrationEdgeDescriptor, MigrationError,
    MigrationKey, MigrationLimits, MigrationRegistry, MigrationTransformError,
};
pub use preview::{
    PreviewAdapter, PreviewArtifact, PreviewBudgets, PreviewCachePolicy, PreviewCancellation,
    PreviewDescriptor, PreviewDiagnostic, PreviewDiagnosticSeverity, PreviewFeatures, PreviewJob,
    PreviewJobError, PreviewJobState, PreviewMaterialOverride, PreviewOverlay, PreviewPollBudget,
    PreviewProgress, PreviewRenderPreset, PreviewRequest, PreviewSelection, PreviewUpdate,
};
pub use preview_cache::{PreviewCache, PreviewCacheError, PreviewCacheKey};
pub use registry::{AuthoringRegistry, CoverageGateError, RegistrationError};
pub use scene::{
    ComponentRecord, SCENE_FORMAT, SCENE_FORMAT_VERSION, SceneDocument, SceneEntityRecord,
    SceneFormatError,
};
pub use system::{SourceNavigation, SystemDescriptor};
