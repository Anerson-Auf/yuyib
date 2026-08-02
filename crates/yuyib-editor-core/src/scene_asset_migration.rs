//! Bulk rewrite of authored `Model3d.model` path refs to `asset://{AssetGuid}`.
//!
//! Place-in-scene and field edits already canonicalize to GUID when tracked.
//! This module migrates older scenes that still persist filesystem paths.

use std::{
    collections::BTreeMap,
    error::Error,
    fmt, fs,
    path::{Component, Path},
    str::FromStr,
};

use serde::Serialize;
use serde_json::Value;
use yuyib_authoring::{AssetGuid, SceneDocument};

use crate::{ASSET_FORMAT, AssetDocument, DocumentError, ProjectDocumentStore};

const MODEL3D_SCHEMA: &str = "yuyib.model3d";
const MAXIMUM_METADATA_FILES: usize = 4096;

/// Request to rewrite model refs in an explicit scene list.
#[derive(Clone, Debug)]
pub struct SceneModelRefMigrationRequest {
    /// Project-relative `.yscene` paths to scan (never a silent full-disk walk).
    pub scene_paths: Vec<String>,
    /// When true, compute the report without writing.
    pub dry_run: bool,
    /// Paths the host refuses to write (e.g. open dirty authored scene).
    pub skip_paths: Vec<String>,
}

/// Aggregate result for UI confirmation and diagnostics.
#[derive(Clone, Debug, Default, Serialize)]
pub struct SceneModelRefMigrationReport {
    /// Echo of the dry-run flag.
    pub dry_run: bool,
    /// Number of scene files inspected.
    pub scenes_scanned: usize,
    /// Scenes that would change / did change.
    pub scenes_changed: usize,
    /// Model refs rewritten to `asset://{guid}`.
    pub refs_rewritten: usize,
    /// Model refs that already used a GUID identity.
    pub refs_already_guid: usize,
    /// Path refs whose source is not tracked.
    pub refs_skipped_untracked: usize,
    /// Builtin / empty / non-string refs left alone.
    pub refs_skipped_other: usize,
    /// Per-scene detail.
    pub scenes: Vec<SceneMigrationEntry>,
    /// Non-fatal host-facing messages.
    pub diagnostics: Vec<String>,
}

/// One scene's migration outcome.
#[derive(Clone, Debug, Serialize)]
pub struct SceneMigrationEntry {
    /// Project-relative scene path.
    pub path: String,
    /// `ok`, `unchanged`, `skipped_dirty`, `conflict`, or `error`.
    pub status: String,
    /// Whether the document payload differs from disk (or would).
    pub changed: bool,
    /// Successful path в†’ GUID rewrites.
    pub rewritten: Vec<RewrittenModelRef>,
    /// Refs that were inspected but not rewritten.
    pub skipped: Vec<SkippedModelRef>,
    /// Optional error / skip reason.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// One rewritten `Model3d.model` value.
#[derive(Clone, Debug, Serialize)]
pub struct RewrittenModelRef {
    /// Entity GUID that owns the component.
    pub entity_guid: String,
    /// Previous path / URI string.
    pub from: String,
    /// Canonical `asset://{guid}` value.
    pub to: String,
}

/// One inspected but unchanged model ref.
#[derive(Clone, Debug, Serialize)]
pub struct SkippedModelRef {
    /// Entity GUID that owns the component.
    pub entity_guid: String,
    /// Current value, when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// Stable skip reason.
    pub reason: String,
}

/// Failure while building the tracked source map or migrating scenes.
#[derive(Debug)]
pub enum SceneMigrationError {
    /// Relative path escapes the project or uses disallowed components.
    InvalidPath(String),
    /// Document store rejected the operation.
    Document(DocumentError),
    /// Scene JSON failed structural validation.
    Scene(String),
    /// Filesystem operation failed.
    Io(std::io::Error),
}

impl fmt::Display for SceneMigrationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPath(path) => {
                write!(formatter, "path must be confined and relative: {path}")
            }
            Self::Document(error) => write!(formatter, "{error}"),
            Self::Scene(message) => write!(formatter, "{message}"),
            Self::Io(error) => write!(formatter, "{error}"),
        }
    }
}

impl Error for SceneMigrationError {}

impl From<DocumentError> for SceneMigrationError {
    fn from(error: DocumentError) -> Self {
        Self::Document(error)
    }
}

/// Builds `source path в†’ AssetGuid` from `.yasset` metadata under `asset_root`.
///
/// Unlike the UI asset index, this scan is metadata-focused and allows up to
/// [`MAXIMUM_METADATA_FILES`] files so migration is not capped at 256.
///
/// # Errors
///
/// Rejects an escaping asset root or I/O failures while walking.
pub fn build_tracked_source_guid_map(
    store: &ProjectDocumentStore,
    asset_root: &str,
) -> Result<BTreeMap<String, AssetGuid>, SceneMigrationError> {
    let asset_root = normalize_portable(asset_root);
    validate_relative(&asset_root)?;
    let absolute_root = store.root().join(&asset_root);
    let mut map = BTreeMap::new();
    if !absolute_root.is_dir() {
        return Ok(map);
    }

    let mut files = Vec::new();
    let mut diagnostics = Vec::new();
    collect_yasset_files(&absolute_root, &absolute_root, &mut files, &mut diagnostics);
    files.sort();
    if files.len() > MAXIMUM_METADATA_FILES {
        files.truncate(MAXIMUM_METADATA_FILES);
        diagnostics.push(format!(
            "tracked metadata scan truncated to {MAXIMUM_METADATA_FILES} .yasset files"
        ));
    }
    let _ = diagnostics;

    for relative in files {
        let project_relative = if asset_root.is_empty() {
            relative.clone()
        } else {
            format!("{asset_root}/{relative}")
        };
        let Ok(snapshot) = store.load_json::<AssetDocument>(&project_relative) else {
            continue;
        };
        if snapshot.value.format != ASSET_FORMAT {
            continue;
        }
        let source = normalize_portable(&snapshot.value.source);
        if source.is_empty() {
            continue;
        }
        map.insert(source.clone(), snapshot.value.guid);
        if let Some(stripped) = source.strip_prefix(&format!("{asset_root}/")) {
            map.insert(stripped.to_owned(), snapshot.value.guid);
        }
    }
    Ok(map)
}

/// Rewrites tracked path model refs in the requested scenes.
///
/// # Errors
///
/// Returns only fatal store/path failures. Per-scene problems are recorded in
/// the report instead of aborting the whole batch.
pub fn migrate_scene_model_refs(
    store: &ProjectDocumentStore,
    asset_root: &str,
    request: &SceneModelRefMigrationRequest,
) -> Result<SceneModelRefMigrationReport, SceneMigrationError> {
    let source_map = build_tracked_source_guid_map(store, asset_root)?;
    let skip: std::collections::BTreeSet<String> = request
        .skip_paths
        .iter()
        .map(|path| normalize_portable(path))
        .collect();

    let mut report = SceneModelRefMigrationReport {
        dry_run: request.dry_run,
        ..SceneModelRefMigrationReport::default()
    };

    for raw_path in &request.scene_paths {
        let path = normalize_portable(raw_path);
        if path.is_empty() {
            report.diagnostics.push("empty scene path skipped".to_owned());
            continue;
        }
        report.scenes_scanned += 1;

        if skip.contains(&path) {
            report.scenes.push(SceneMigrationEntry {
                path,
                status: "skipped_dirty".to_owned(),
                changed: false,
                rewritten: Vec::new(),
                skipped: Vec::new(),
                message: Some("open scene has unsaved edits".to_owned()),
            });
            continue;
        }

        let snapshot = match store.load_json::<SceneDocument>(&path) {
            Ok(snapshot) => snapshot,
            Err(DocumentError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                report.scenes.push(SceneMigrationEntry {
                    path,
                    status: "error".to_owned(),
                    changed: false,
                    rewritten: Vec::new(),
                    skipped: Vec::new(),
                    message: Some("scene file not found".to_owned()),
                });
                continue;
            }
            Err(error) => {
                report.scenes.push(SceneMigrationEntry {
                    path,
                    status: "error".to_owned(),
                    changed: false,
                    rewritten: Vec::new(),
                    skipped: Vec::new(),
                    message: Some(error.to_string()),
                });
                continue;
            }
        };

        if let Err(error) = snapshot.value.validate() {
            report.scenes.push(SceneMigrationEntry {
                path,
                status: "error".to_owned(),
                changed: false,
                rewritten: Vec::new(),
                skipped: Vec::new(),
                message: Some(error.to_string()),
            });
            continue;
        }

        let mut document = snapshot.value;
        let outcome = rewrite_scene_document_model_refs(&mut document, &source_map, asset_root);
        report.refs_rewritten += outcome.rewritten.len();
        report.refs_already_guid += outcome.already_guid;
        report.refs_skipped_untracked += outcome.untracked;
        report.refs_skipped_other += outcome.other;

        if outcome.rewritten.is_empty() {
            report.scenes.push(SceneMigrationEntry {
                path,
                status: "unchanged".to_owned(),
                changed: false,
                rewritten: Vec::new(),
                skipped: outcome.skipped,
                message: None,
            });
            continue;
        }

        report.scenes_changed += 1;
        if request.dry_run {
            report.scenes.push(SceneMigrationEntry {
                path,
                status: "ok".to_owned(),
                changed: true,
                rewritten: outcome.rewritten,
                skipped: outcome.skipped,
                message: Some("dry_run".to_owned()),
            });
            continue;
        }

        match store.save_json(&path, &document, Some(snapshot.revision)) {
            Ok(_) => {
                report.scenes.push(SceneMigrationEntry {
                    path,
                    status: "ok".to_owned(),
                    changed: true,
                    rewritten: outcome.rewritten,
                    skipped: outcome.skipped,
                    message: None,
                });
            }
            Err(DocumentError::Conflict(_)) => {
                report.scenes.push(SceneMigrationEntry {
                    path,
                    status: "conflict".to_owned(),
                    changed: true,
                    rewritten: outcome.rewritten,
                    skipped: outcome.skipped,
                    message: Some("external revision conflict".to_owned()),
                });
            }
            Err(error) => {
                report.scenes.push(SceneMigrationEntry {
                    path,
                    status: "error".to_owned(),
                    changed: true,
                    rewritten: outcome.rewritten,
                    skipped: outcome.skipped,
                    message: Some(error.to_string()),
                });
            }
        }
    }

    Ok(report)
}

#[derive(Default)]
struct RewriteOutcome {
    rewritten: Vec<RewrittenModelRef>,
    skipped: Vec<SkippedModelRef>,
    already_guid: usize,
    untracked: usize,
    other: usize,
}

/// Mutates one in-memory scene: tracked paths become `asset://{guid}`.
fn rewrite_scene_document_model_refs(
    document: &mut SceneDocument,
    source_map: &BTreeMap<String, AssetGuid>,
    asset_root: &str,
) -> RewriteOutcome {
    let asset_root = normalize_portable(asset_root);
    let mut outcome = RewriteOutcome::default();

    for entity in &mut document.entities {
        let entity_guid = entity.guid.to_string();
        for component in &mut entity.components {
            if component.schema().as_str() != MODEL3D_SCHEMA {
                continue;
            }
            let version = component.version();
            let mut payload = component.payload().clone();
            let Some(model_value) = payload.get("model").cloned() else {
                outcome.other += 1;
                outcome.skipped.push(SkippedModelRef {
                    entity_guid: entity_guid.clone(),
                    value: None,
                    reason: "missing_model".to_owned(),
                });
                continue;
            };

            let Some(raw) = model_value.as_str() else {
                outcome.other += 1;
                outcome.skipped.push(SkippedModelRef {
                    entity_guid: entity_guid.clone(),
                    value: None,
                    reason: "non_string_model".to_owned(),
                });
                continue;
            };

            let trimmed = raw.trim();
            if trimmed.is_empty() {
                outcome.other += 1;
                outcome.skipped.push(SkippedModelRef {
                    entity_guid: entity_guid.clone(),
                    value: Some(trimmed.to_owned()),
                    reason: "empty".to_owned(),
                });
                continue;
            }
            if trimmed.to_ascii_lowercase().starts_with("builtin:") {
                outcome.other += 1;
                outcome.skipped.push(SkippedModelRef {
                    entity_guid: entity_guid.clone(),
                    value: Some(trimmed.to_owned()),
                    reason: "builtin".to_owned(),
                });
                continue;
            }

            let without_scheme = trimmed
                .strip_prefix("asset://")
                .unwrap_or(trimmed)
                .trim_start_matches(['/', '\\']);
            if looks_like_asset_guid(without_scheme) {
                if AssetGuid::from_str(without_scheme).is_ok() {
                    let canonical = format!("asset://{without_scheme}");
                    if trimmed != canonical {
                        if let Some(object) = payload.as_object_mut() {
                            object.insert("model".to_owned(), Value::String(canonical.clone()));
                        }
                        component.replace_payload(version, payload);
                        outcome.rewritten.push(RewrittenModelRef {
                            entity_guid: entity_guid.clone(),
                            from: trimmed.to_owned(),
                            to: canonical,
                        });
                    } else {
                        outcome.already_guid += 1;
                        outcome.skipped.push(SkippedModelRef {
                            entity_guid: entity_guid.clone(),
                            value: Some(trimmed.to_owned()),
                            reason: "already_guid".to_owned(),
                        });
                    }
                } else {
                    outcome.other += 1;
                    outcome.skipped.push(SkippedModelRef {
                        entity_guid: entity_guid.clone(),
                        value: Some(trimmed.to_owned()),
                        reason: "invalid_guid".to_owned(),
                    });
                }
                continue;
            }

            let normalized = normalize_portable(without_scheme);
            let with_root =
                if asset_root.is_empty() || normalized.starts_with(&format!("{asset_root}/")) {
                    normalized.clone()
                } else {
                    format!("{asset_root}/{normalized}")
                };
            let under_root = normalized
                .strip_prefix(&format!("{asset_root}/"))
                .unwrap_or(normalized.as_str())
                .to_owned();

            let found = source_map
                .get(&normalized)
                .or_else(|| source_map.get(&with_root))
                .or_else(|| source_map.get(&under_root))
                .copied();

            if let Some(guid) = found {
                let to = format!("asset://{guid}");
                if let Some(object) = payload.as_object_mut() {
                    object.insert("model".to_owned(), Value::String(to.clone()));
                }
                component.replace_payload(version, payload);
                outcome.rewritten.push(RewrittenModelRef {
                    entity_guid: entity_guid.clone(),
                    from: trimmed.to_owned(),
                    to,
                });
            } else {
                outcome.untracked += 1;
                outcome.skipped.push(SkippedModelRef {
                    entity_guid: entity_guid.clone(),
                    value: Some(trimmed.to_owned()),
                    reason: "untracked".to_owned(),
                });
            }
        }
    }

    outcome
}

fn collect_yasset_files(
    root: &Path,
    current: &Path,
    files: &mut Vec<String>,
    diagnostics: &mut Vec<String>,
) {
    let entries = match fs::read_dir(current) {
        Ok(entries) => entries,
        Err(error) => {
            diagnostics.push(format!("{}: {error}", portable_path(current)));
            return;
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) => {
                diagnostics.push(format!("{}: {error}", portable_path(&path)));
                continue;
            }
        };
        if metadata.file_type().is_symlink() {
            continue;
        }
        if metadata.is_dir() {
            collect_yasset_files(root, &path, files, diagnostics);
            continue;
        }
        if !metadata.is_file() {
            continue;
        }
        let Some(extension) = path.extension().and_then(|extension| extension.to_str()) else {
            continue;
        };
        if !extension.eq_ignore_ascii_case("yasset") {
            continue;
        }
        let Ok(relative) = path.strip_prefix(root) else {
            continue;
        };
        files.push(portable_path(relative));
    }
}

fn looks_like_asset_guid(value: &str) -> bool {
    let value = value.trim();
    if value.len() != 36 {
        return false;
    }
    let mut parts = value.split('-');
    matches!(
        (
            parts.next().map(str::len),
            parts.next().map(str::len),
            parts.next().map(str::len),
            parts.next().map(str::len),
            parts.next().map(str::len),
            parts.next(),
        ),
        (Some(8), Some(4), Some(4), Some(4), Some(12), None)
    ) && value
        .bytes()
        .all(|byte| byte == b'-' || byte.is_ascii_hexdigit())
}

fn validate_relative(path: &str) -> Result<(), SceneMigrationError> {
    let candidate = Path::new(path);
    if path.is_empty() {
        return Ok(());
    }
    if candidate.is_absolute()
        || candidate.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(SceneMigrationError::InvalidPath(path.to_owned()));
    }
    Ok(())
}

fn normalize_portable(path: &str) -> String {
    path.replace('\\', "/")
        .split('/')
        .filter(|segment| !segment.is_empty() && *segment != ".")
        .collect::<Vec<_>>()
        .join("/")
}

fn portable_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ensure_tracked_gltf;
    use serde_json::json;
    use std::path::PathBuf;
    use yuyib_authoring::{
        ComponentRecord, ComponentSchemaId, EntityGuid, SCENE_FORMAT, SCENE_FORMAT_VERSION,
        SceneEntityRecord, SchemaVersion,
    };

    fn temporary_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "yuyib-scene-mig-{name}-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir(&root).expect("create temporary project");
        root
    }

    fn model_component(model: &str) -> ComponentRecord {
        ComponentRecord::new(
            ComponentSchemaId::new(MODEL3D_SCHEMA).expect("schema"),
            SchemaVersion::new(1).expect("version"),
            json!({ "model": model }),
        )
    }

    fn write_scene(store: &ProjectDocumentStore, path: &str, models: &[&str]) {
        let entities = models
            .iter()
            .enumerate()
            .map(|(index, model)| SceneEntityRecord {
                guid: EntityGuid::new(),
                name: Some(format!("Entity{index}")),
                components: vec![model_component(model)],
                extensions: Default::default(),
            })
            .collect();
        let document = SceneDocument {
            format: SCENE_FORMAT.to_owned(),
            format_version: SchemaVersion::new(SCENE_FORMAT_VERSION).expect("version"),
            scene_guid: yuyib_authoring::SceneGuid::new(),
            entities,
            extensions: Default::default(),
        };
        store
            .save_json(path, &document, None)
            .expect("write scene");
    }

    #[test]
    fn migrates_tracked_path_preserves_guid_and_untracked() {
        let root = temporary_root("migrate");
        fs::create_dir_all(root.join("assets/models")).expect("dirs");
        fs::create_dir_all(root.join("scenes")).expect("scenes");
        fs::write(root.join("assets/models/hero.glb"), b"glb").expect("glb");
        fs::write(root.join("assets/models/prop.glb"), b"prop").expect("prop");
        let store = ProjectDocumentStore::new(&root, 256 * 1024).expect("store");
        let tracked = ensure_tracked_gltf(&store, "assets", "models/hero.glb").expect("track");

        write_scene(
            &store,
            "scenes/main.yscene",
            &[
                "assets/models/hero.glb",
                "models/prop.glb",
                "builtin:cube",
                &format!("asset://{}", tracked.guid),
            ],
        );

        let report = migrate_scene_model_refs(
            &store,
            "assets",
            &SceneModelRefMigrationRequest {
                scene_paths: vec!["scenes/main.yscene".to_owned()],
                dry_run: false,
                skip_paths: Vec::new(),
            },
        )
        .expect("migrate");

        assert_eq!(report.scenes_changed, 1);
        assert_eq!(report.refs_rewritten, 1);
        assert_eq!(report.refs_skipped_untracked, 1);
        assert_eq!(report.refs_already_guid, 1);
        assert!(report.refs_skipped_other >= 1);

        let reloaded = store
            .load_json::<SceneDocument>("scenes/main.yscene")
            .expect("reload");
        let models: Vec<_> = reloaded
            .value
            .entities
            .iter()
            .map(|entity| {
                entity.components[0]
                    .payload()
                    .get("model")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned()
            })
            .collect();
        assert_eq!(models[0], format!("asset://{}", tracked.guid));
        assert_eq!(models[1], "models/prop.glb");
        assert_eq!(models[2], "builtin:cube");
        assert_eq!(models[3], format!("asset://{}", tracked.guid));

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn dry_run_does_not_write() {
        let root = temporary_root("dry");
        fs::create_dir_all(root.join("assets")).expect("dirs");
        fs::create_dir_all(root.join("scenes")).expect("scenes");
        fs::write(root.join("assets/a.glb"), b"a").expect("glb");
        let store = ProjectDocumentStore::new(&root, 256 * 1024).expect("store");
        let tracked = ensure_tracked_gltf(&store, "assets", "a.glb").expect("track");
        write_scene(&store, "scenes/main.yscene", &["assets/a.glb"]);

        let report = migrate_scene_model_refs(
            &store,
            "assets",
            &SceneModelRefMigrationRequest {
                scene_paths: vec!["scenes/main.yscene".to_owned()],
                dry_run: true,
                skip_paths: Vec::new(),
            },
        )
        .expect("dry");
        assert_eq!(report.scenes_changed, 1);
        assert_eq!(
            report.scenes[0].rewritten[0].to,
            format!("asset://{}", tracked.guid)
        );

        let reloaded = store
            .load_json::<SceneDocument>("scenes/main.yscene")
            .expect("reload");
        let model = reloaded.value.entities[0].components[0]
            .payload()
            .get("model")
            .and_then(Value::as_str)
            .unwrap();
        assert_eq!(model, "assets/a.glb");

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn skips_dirty_paths() {
        let root = temporary_root("dirty");
        fs::create_dir_all(root.join("scenes")).expect("scenes");
        let store = ProjectDocumentStore::new(&root, 64 * 1024).expect("store");
        write_scene(&store, "scenes/main.yscene", &["builtin:cube"]);
        let report = migrate_scene_model_refs(
            &store,
            "assets",
            &SceneModelRefMigrationRequest {
                scene_paths: vec!["scenes/main.yscene".to_owned()],
                dry_run: false,
                skip_paths: vec!["scenes/main.yscene".to_owned()],
            },
        )
        .expect("migrate");
        assert_eq!(report.scenes[0].status, "skipped_dirty");
        fs::remove_dir_all(root).expect("cleanup");
    }
}
