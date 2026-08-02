//! Headless project services shared by the native Yuyib Editor and tooling.
//!
//! This crate owns versioned project/asset documents, confined project paths,
//! external-change detection, and process-isolated Play/Cargo tools. It has no
//! window, `WebView`, ECS, renderer, or GPU dependency.

#![forbid(unsafe_code)]

mod asset_dependency_graph;
mod asset_index;
mod asset_ops;
mod document;
mod process;
mod project;
mod scaffold;
mod scene_asset_migration;

pub use asset_dependency_graph::{
    AssetDependencyGraph, AssetDependencyGraphError, ReimportCascadePlan,
    build_asset_dependency_graph, plan_reimport_cascade,
};
pub use asset_index::{
    AssetActionStatus, AssetIndexDiagnostic, AssetIndexDiagnosticCode, AssetIndexError,
    AssetIndexItem, AssetKind, AssetOpenIntent, AssetTracking, ProjectAssetIndex,
    build_asset_index,
};
pub use asset_ops::{
    AssetDependencyKind, AssetLogicalDependency, AssetOpsError, DependencyRefreshReport,
    TrackedAsset, UnresolvedAssetDependency, ensure_tracked_gltf, hash_asset_file,
    refresh_tracked_content_hash, refresh_tracked_dependencies, rename_tracked_gltf,
    resolve_tracked_asset, save_tracked_import_settings,
};
pub use document::{
    DocumentConflict, DocumentError, DocumentRevision, DocumentRevisionParseError,
    DocumentSnapshot, ProjectDocumentStore,
};
pub use process::{ManagedProcess, ManagedProcessError, ProcessKind, ProcessPoll};
pub use project::{
    ASSET_FORMAT, AssetDocument, ImportSettingsRecord, PROJECT_FORMAT, ProjectDevelopment,
    ProjectManifest, ProjectProfile, ProjectScene, ProjectValidationError, build_play_argv,
};
pub use scaffold::{
    ScaffoldError, ScaffoldRequest, ScaffoldedProject, ensure_project_cargo_toml,
    open_existing_project, resolve_project_root, scaffold_project,
};
pub use scene_asset_migration::{
    RewrittenModelRef, SceneMigrationEntry, SceneMigrationError, SceneModelRefMigrationReport,
    SceneModelRefMigrationRequest, SkippedModelRef, build_tracked_source_guid_map,
    migrate_scene_model_refs,
};
