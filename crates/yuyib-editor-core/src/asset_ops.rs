//! Track / rename helpers for project-relative glTF sources and `.yasset` metadata.
//!
//! GUID identity is never derived from path or content hash. Scene `Model3d`
//! path rewrites live in [`crate::scene_asset_migration`].

use std::{
    collections::BTreeMap,
    error::Error,
    fmt, fs,
    path::{Component, Path, PathBuf},
    str::FromStr,
};

use serde::Serialize;
use serde_json::{Value, json};
use yuyib_authoring::{
    AssetGuid, CapabilityId, ContentHash, ImportSettingsSchemaId, SchemaVersion,
};

use crate::{
    ASSET_FORMAT, AssetDocument, DocumentError, DocumentRevision, ImportSettingsRecord,
    ProjectDocumentStore, build_asset_index,
};

const GLTF_IMPORTER: &str = "yuyib.gltf-import";
const GLTF_IMPORT_SETTINGS: &str = "yuyib.gltf-import-settings";
const DEPENDENCY_DIAGNOSTICS_EXT: &str = "yuyib.dependency_diagnostics";

/// Whether a logical dependency is required for a correct import.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetDependencyKind {
    /// Missing dependency blocks a correct import.
    Required,
    /// Missing dependency may fall back with diagnostics.
    Optional,
}

/// One URI discovered by an importer before host resolution.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AssetLogicalDependency {
    /// Document-relative or scheme-qualified URI from the source.
    pub uri: String,
    /// Required vs optional contract.
    pub kind: AssetDependencyKind,
}

/// Unresolved logical URI retained for Inspector / diagnostics.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct UnresolvedAssetDependency {
    /// Original URI from the source document.
    pub uri: String,
    /// Required / optional.
    pub kind: AssetDependencyKind,
    /// Stable machine-readable code.
    pub code: String,
    /// Human-readable detail.
    pub message: String,
}

/// Result of resolving and persisting tracked dependencies.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DependencyRefreshReport {
    /// Resolved dependency identities written into `.yasset`.
    pub dependencies: Vec<AssetGuid>,
    /// URIs that did not map to a tracked asset.
    pub unresolved: Vec<UnresolvedAssetDependency>,
}

/// Result of ensuring a source has persistent `.yasset` metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrackedAsset {
    /// Persistent identity (unchanged across rename / content edits).
    pub guid: AssetGuid,
    /// Project-relative source path written into the metadata document.
    pub source: String,
    /// Project-relative `.yasset` path.
    pub metadata_path: String,
    /// Whether a new metadata document was written.
    pub created: bool,
}

/// Failure while tracking or renaming a project asset.
#[derive(Debug)]
pub enum AssetOpsError {
    /// Relative path escapes the project or uses disallowed components.
    InvalidPath(String),
    /// Source file is missing on disk.
    MissingSource(String),
    /// Destination already exists.
    DestinationExists(String),
    /// Source extension is not a supported trackable type.
    UnsupportedSource(String),
    /// Metadata already exists but points at a different source.
    MetadataSourceMismatch {
        /// Project-relative `.yasset` path.
        metadata_path: String,
        /// Source recorded in metadata.
        recorded: String,
        /// Source the caller requested.
        requested: String,
    },
    /// No `.yasset` tracks the requested identity.
    NotTracked(String),
    /// Document store rejected the operation.
    Document(DocumentError),
    /// Filesystem operation failed.
    Io(std::io::Error),
}

impl fmt::Display for AssetOpsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPath(path) => {
                write!(formatter, "asset path must be confined and relative: {path}")
            }
            Self::MissingSource(path) => write!(formatter, "asset source not found: {path}"),
            Self::DestinationExists(path) => {
                write!(formatter, "asset destination already exists: {path}")
            }
            Self::UnsupportedSource(path) => {
                write!(
                    formatter,
                    "only .glb/.gltf sources can be tracked yet: {path}"
                )
            }
            Self::MetadataSourceMismatch {
                metadata_path,
                recorded,
                requested,
            } => write!(
                formatter,
                "metadata `{metadata_path}` records source `{recorded}`, not `{requested}`"
            ),
            Self::NotTracked(id) => write!(formatter, "asset is not tracked: {id}"),
            Self::Document(error) => write!(formatter, "{error}"),
            Self::Io(error) => write!(formatter, "{error}"),
        }
    }
}

impl Error for AssetOpsError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Document(error) => Some(error),
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<DocumentError> for AssetOpsError {
    fn from(value: DocumentError) -> Self {
        Self::Document(value)
    }
}

/// Creates sibling `.yasset` metadata for an untracked glTF under `asset_root`.
///
/// `source_under_asset_root` is relative to the project asset root (as shown in
/// [`crate::ProjectAssetIndex`]). Idempotent when sibling metadata already
/// tracks the same source.
///
/// # Errors
///
/// Rejects path escape, missing/unsupported sources, and conflicting metadata.
pub fn ensure_tracked_gltf(
    store: &ProjectDocumentStore,
    asset_root: &str,
    source_under_asset_root: &str,
) -> Result<TrackedAsset, AssetOpsError> {
    let asset_root = normalize_portable(asset_root);
    let source_under_root = normalize_portable(source_under_asset_root);
    validate_relative(&asset_root)?;
    validate_relative(&source_under_root)?;
    if !is_gltf_source(&source_under_root) {
        return Err(AssetOpsError::UnsupportedSource(source_under_root));
    }

    let project_source = join_portable(&asset_root, &source_under_root);
    validate_relative(&project_source)?;
    let absolute_source = resolve_existing_file(store, &project_source)?;
    let metadata_path = sibling_metadata_path(&project_source)?;

    if store_path_exists(store, &metadata_path)? {
        let snapshot = store.load_json::<AssetDocument>(&metadata_path)?;
        let metadata = snapshot.value;
        if normalize_portable(&metadata.source) != project_source {
            return Err(AssetOpsError::MetadataSourceMismatch {
                metadata_path,
                recorded: metadata.source,
                requested: project_source,
            });
        }
        return Ok(TrackedAsset {
            guid: metadata.guid,
            source: project_source,
            metadata_path,
            created: false,
        });
    }

    if let Some(existing) = find_metadata_for_source(store, &asset_root, &project_source)? {
        return Ok(TrackedAsset {
            guid: existing.document.guid,
            source: project_source,
            metadata_path: existing.metadata_path,
            created: false,
        });
    }

    let content_hash = hash_file(&absolute_source)?;
    let document = AssetDocument {
        format: ASSET_FORMAT.to_owned(),
        format_version: SchemaVersion::new(1).expect("version"),
        guid: AssetGuid::new(),
        source: project_source.clone(),
        content_hash: Some(content_hash),
        importer: CapabilityId::new(GLTF_IMPORTER).expect("importer"),
        importer_version: SchemaVersion::new(1).expect("version"),
        import_settings: ImportSettingsRecord {
            schema: ImportSettingsSchemaId::new(GLTF_IMPORT_SETTINGS).expect("schema"),
            version: SchemaVersion::new(1).expect("version"),
            payload: json!({}),
            extensions: BTreeMap::new(),
        },
        dependencies: Vec::new(),
        extensions: BTreeMap::new(),
    };
    let _revision: DocumentRevision = store.save_json(&metadata_path, &document, None)?;
    Ok(TrackedAsset {
        guid: document.guid,
        source: project_source,
        metadata_path,
        created: true,
    })
}

/// Writes validated import-settings payload into a tracked `.yasset`.
///
/// Callers must validate the payload against the importer settings schema before
/// invoking this helper. GUID and source path are preserved.
///
/// # Errors
///
/// Rejects untracked identities, path escape, and revision conflicts.
pub fn save_tracked_import_settings(
    store: &ProjectDocumentStore,
    asset_root: &str,
    identity: &str,
    payload: Value,
) -> Result<TrackedAsset, AssetOpsError> {
    let asset_root = normalize_portable(asset_root);
    validate_relative(&asset_root)?;
    let located = locate_tracked(store, &asset_root, identity)?;
    let mut document = located.document;
    document.import_settings.payload = payload;
    let _revision = store.save_json(
        &located.metadata_path,
        &document,
        Some(located.revision),
    )?;
    Ok(TrackedAsset {
        guid: document.guid,
        source: document.source,
        metadata_path: located.metadata_path,
        created: false,
    })
}

/// Resolves importer-discovered URIs into tracked [`AssetGuid`] edges and saves
/// them on the owning `.yasset`.
///
/// URIs resolve relative to the directory of `AssetDocument.source`. Absolute
/// paths, parent escapes, and non-relative schemes never become edges. Unknown
/// targets are recorded under `extensions.yuyib.dependency_diagnostics` — never
/// as synthetic GUIDs.
///
/// # Errors
///
/// Rejects untracked identities, path escape, and revision conflicts.
pub fn refresh_tracked_dependencies(
    store: &ProjectDocumentStore,
    asset_root: &str,
    identity: &str,
    logical: &[AssetLogicalDependency],
) -> Result<(TrackedAsset, DependencyRefreshReport), AssetOpsError> {
    let asset_root = normalize_portable(asset_root);
    validate_relative(&asset_root)?;
    let located = locate_tracked(store, &asset_root, identity)?;
    let source_dir = parent_portable(&located.document.source);
    let mut resolved = Vec::new();
    let mut unresolved = Vec::new();
    let mut seen_guid = std::collections::BTreeSet::new();

    for dependency in logical {
        let uri = dependency.uri.trim();
        if uri.is_empty() {
            continue;
        }
        match resolve_dependency_project_path(&source_dir, uri) {
            Ok(project_path) => match find_guid_for_project_source(store, &asset_root, &project_path)?
            {
                Some(guid) => {
                    if seen_guid.insert(guid) {
                        resolved.push(guid);
                    }
                }
                None => unresolved.push(UnresolvedAssetDependency {
                    uri: uri.to_owned(),
                    kind: dependency.kind,
                    code: "unresolved-external-dependency".to_owned(),
                    message: format!(
                        "no tracked `.yasset` for `{project_path}` (track the dependency first)"
                    ),
                }),
            },
            Err(message) => unresolved.push(UnresolvedAssetDependency {
                uri: uri.to_owned(),
                kind: dependency.kind,
                code: "invalid-dependency-uri".to_owned(),
                message,
            }),
        }
    }

    let mut document = located.document;
    document.dependencies = resolved.clone();
    if unresolved.is_empty() {
        document.extensions.remove(DEPENDENCY_DIAGNOSTICS_EXT);
    } else {
        document.extensions.insert(
            DEPENDENCY_DIAGNOSTICS_EXT.to_owned(),
            serde_json::to_value(&unresolved).map_err(|error| {
                AssetOpsError::InvalidPath(format!("dependency diagnostics serialize: {error}"))
            })?,
        );
    }
    let _revision = store.save_json(
        &located.metadata_path,
        &document,
        Some(located.revision),
    )?;

    Ok((
        TrackedAsset {
            guid: document.guid,
            source: document.source,
            metadata_path: located.metadata_path,
            created: false,
        },
        DependencyRefreshReport {
            dependencies: resolved,
            unresolved,
        },
    ))
}

fn parent_portable(path: &str) -> String {
    let normalized = normalize_portable(path);
    match normalized.rsplit_once('/') {
        Some((parent, _)) => parent.to_owned(),
        None => String::new(),
    }
}

fn resolve_dependency_project_path(source_dir: &str, uri: &str) -> Result<String, String> {
    let trimmed = uri.trim().replace('\\', "/");
    if trimmed.is_empty() {
        return Err("empty dependency URI".to_owned());
    }
    if trimmed.contains(':') {
        return Err(format!(
            "non-relative dependency URI is not resolved as a project file: {trimmed}"
        ));
    }
    let candidate = Path::new(&trimmed);
    if candidate.is_absolute()
        || candidate.components().any(|component| {
            matches!(
                component,
                Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(format!(
            "absolute dependency URI is not resolved as a project file: {trimmed}"
        ));
    }

    let mut parts = Vec::new();
    for segment in source_dir.split('/') {
        if segment.is_empty() || segment == "." {
            continue;
        }
        if segment == ".." {
            return Err(format!(
                "source directory is unsafe for dependency resolution: {source_dir}"
            ));
        }
        parts.push(segment.to_owned());
    }
    for segment in trimmed.split('/') {
        if segment.is_empty() || segment == "." {
            continue;
        }
        if segment == ".." {
            if parts.pop().is_none() {
                return Err(format!(
                    "dependency URI escapes the project via parent segments: {trimmed}"
                ));
            }
            continue;
        }
        parts.push(segment.to_owned());
    }
    let project_path = parts.join("/");
    if project_path.is_empty() {
        return Err(format!("dependency URI resolved empty: {trimmed}"));
    }
    Ok(project_path)
}

fn find_guid_for_project_source(
    store: &ProjectDocumentStore,
    asset_root: &str,
    project_source: &str,
) -> Result<Option<AssetGuid>, AssetOpsError> {
    let project_source = normalize_portable(project_source);
    if let Ok(sibling) = sibling_metadata_path(&project_source)
        && store_path_exists(store, &sibling)?
    {
        let snapshot = store.load_json::<AssetDocument>(&sibling)?;
        if normalize_portable(&snapshot.value.source) == project_source {
            return Ok(Some(snapshot.value.guid));
        }
    }
    if let Some(found) = find_metadata_for_source(store, asset_root, &project_source)? {
        return Ok(Some(found.document.guid));
    }
    Ok(None)
}

/// Renames a tracked glTF source while preserving its [`AssetGuid`].
///
/// Updates `AssetDocument.source`. When metadata lives as a sibling of the old
/// source, the `.yasset` file is renamed to the new sibling path as well.
///
/// # Errors
///
/// Rejects untracked identities, path escape, missing sources, and overwrites.
pub fn rename_tracked_gltf(
    store: &ProjectDocumentStore,
    asset_root: &str,
    identity: &str,
    new_source_under_asset_root: &str,
) -> Result<TrackedAsset, AssetOpsError> {
    let asset_root = normalize_portable(asset_root);
    let new_under_root = normalize_portable(new_source_under_asset_root);
    validate_relative(&asset_root)?;
    validate_relative(&new_under_root)?;
    if !is_gltf_source(&new_under_root) {
        return Err(AssetOpsError::UnsupportedSource(new_under_root));
    }

    let new_project_source = join_portable(&asset_root, &new_under_root);
    validate_relative(&new_project_source)?;

    let located = locate_tracked(store, &asset_root, identity)?;
    if normalize_portable(&located.document.source) == new_project_source {
        return Ok(TrackedAsset {
            guid: located.document.guid,
            source: new_project_source,
            metadata_path: located.metadata_path,
            created: false,
        });
    }

    if store_path_exists(store, &new_project_source)? {
        return Err(AssetOpsError::DestinationExists(new_project_source));
    }

    let old_absolute = resolve_existing_file(store, &located.document.source)?;
    let new_absolute = resolve_for_create(store, &new_project_source)?;
    if let Some(parent) = new_absolute.parent() {
        fs::create_dir_all(parent).map_err(AssetOpsError::Io)?;
    }
    fs::rename(&old_absolute, &new_absolute).map_err(AssetOpsError::Io)?;

    let mut document = located.document;
    let old_guid = document.guid;
    document.source = new_project_source.clone();
    document.content_hash = Some(hash_file(&new_absolute)?);

    let expected = Some(located.revision);
    let new_metadata_path =
        if is_sibling_metadata(&located.metadata_path, &located.old_source_project) {
            let candidate = sibling_metadata_path(&new_project_source)?;
            if candidate != located.metadata_path {
                if store_path_exists(store, &candidate)? {
                    let _ = fs::rename(&new_absolute, &old_absolute);
                    return Err(AssetOpsError::DestinationExists(candidate));
                }
                // Update contents in place first, then rename the sidecar so
                // optimistic create (expected=None) is never used on an
                // already-present destination path.
                store.save_json(&located.metadata_path, &document, expected)?;
                let old_meta_abs = resolve_existing_file(store, &located.metadata_path)?;
                let new_meta_abs = resolve_for_create(store, &candidate)?;
                fs::rename(&old_meta_abs, &new_meta_abs).map_err(AssetOpsError::Io)?;
                candidate
            } else {
                store.save_json(&located.metadata_path, &document, expected)?;
                located.metadata_path
            }
        } else {
            store.save_json(&located.metadata_path, &document, expected)?;
            located.metadata_path
        };

    debug_assert_eq!(document.guid, old_guid);
    Ok(TrackedAsset {
        guid: document.guid,
        source: new_project_source,
        metadata_path: new_metadata_path,
        created: false,
    })
}

struct LocatedTracked {
    document: AssetDocument,
    metadata_path: String,
    revision: DocumentRevision,
    old_source_project: String,
}

/// Locates a tracked asset by GUID (`asset://…` / bare UUID) or source path.
///
/// # Errors
///
/// Returns [`AssetOpsError::NotTracked`] when no `.yasset` matches.
pub fn resolve_tracked_asset(
    store: &ProjectDocumentStore,
    asset_root: &str,
    identity: &str,
) -> Result<TrackedAsset, AssetOpsError> {
    let asset_root = normalize_portable(asset_root);
    validate_relative(&asset_root)?;
    let located = locate_tracked(store, &asset_root, identity)?;
    Ok(TrackedAsset {
        guid: located.document.guid,
        source: located.document.source,
        metadata_path: located.metadata_path,
        created: false,
    })
}

/// Non-destructive reimport helper: refreshes `content_hash` on a tracked `.yasset`.
///
/// # Errors
///
/// Propagates locate / hash / document failures.
pub fn refresh_tracked_content_hash(
    store: &ProjectDocumentStore,
    asset_root: &str,
    identity: &str,
) -> Result<TrackedAsset, AssetOpsError> {
    let asset_root = normalize_portable(asset_root);
    validate_relative(&asset_root)?;
    let located = locate_tracked(store, &asset_root, identity)?;
    let absolute = resolve_existing_file(store, &located.document.source)?;
    let content_hash = hash_file(&absolute)?;
    let mut document = located.document;
    document.content_hash = Some(content_hash);
    let _revision = store.save_json(
        &located.metadata_path,
        &document,
        Some(located.revision),
    )?;
    Ok(TrackedAsset {
        guid: document.guid,
        source: document.source,
        metadata_path: located.metadata_path,
        created: false,
    })
}

fn locate_tracked(
    store: &ProjectDocumentStore,
    asset_root: &str,
    identity: &str,
) -> Result<LocatedTracked, AssetOpsError> {
    let identity = strip_asset_uri(identity);
    let identity = normalize_portable(identity);

    if let Ok(guid) = AssetGuid::from_str(&identity) {
        if let Some(found) = find_metadata_by_guid(store, asset_root, guid)? {
            return Ok(found);
        }
        return Err(AssetOpsError::NotTracked(identity));
    }

    let project_source = if identity == asset_root || identity.starts_with(&format!("{asset_root}/"))
    {
        identity.clone()
    } else {
        join_portable(asset_root, &identity)
    };
    validate_relative(&project_source)?;

    let sibling = sibling_metadata_path(&project_source)?;
    if store_path_exists(store, &sibling)? {
        let snapshot = store.load_json::<AssetDocument>(&sibling)?;
        if normalize_portable(&snapshot.value.source) == project_source {
            return Ok(LocatedTracked {
                old_source_project: project_source,
                document: snapshot.value,
                metadata_path: sibling,
                revision: snapshot.revision,
            });
        }
    }

    if let Some(found) = find_metadata_for_source(store, asset_root, &project_source)? {
        return Ok(found);
    }
    Err(AssetOpsError::NotTracked(identity))
}

fn find_metadata_for_source(
    store: &ProjectDocumentStore,
    asset_root: &str,
    project_source: &str,
) -> Result<Option<LocatedTracked>, AssetOpsError> {
    let index = build_asset_index(store, asset_root).map_err(|error| {
        AssetOpsError::InvalidPath(format!("asset root `{asset_root}`: {error}"))
    })?;
    for item in index.items {
        let Some(metadata) = item.metadata else {
            continue;
        };
        if normalize_portable(&metadata.source) != project_source {
            continue;
        }
        let metadata_path = item_project_path(asset_root, &item.path);
        let snapshot = store.load_json::<AssetDocument>(&metadata_path)?;
        return Ok(Some(LocatedTracked {
            old_source_project: project_source.to_owned(),
            document: snapshot.value,
            metadata_path,
            revision: snapshot.revision,
        }));
    }
    Ok(None)
}

fn find_metadata_by_guid(
    store: &ProjectDocumentStore,
    asset_root: &str,
    guid: AssetGuid,
) -> Result<Option<LocatedTracked>, AssetOpsError> {
    let index = build_asset_index(store, asset_root).map_err(|error| {
        AssetOpsError::InvalidPath(format!("asset root `{asset_root}`: {error}"))
    })?;
    for item in index.items {
        let Some(metadata) = item.metadata else {
            continue;
        };
        if metadata.guid != guid {
            continue;
        }
        let metadata_path = item_project_path(asset_root, &item.path);
        let snapshot = store.load_json::<AssetDocument>(&metadata_path)?;
        let old_source = snapshot.value.source.clone();
        return Ok(Some(LocatedTracked {
            document: snapshot.value,
            metadata_path,
            revision: snapshot.revision,
            old_source_project: old_source,
        }));
    }
    Ok(None)
}

fn item_project_path(asset_root: &str, item_path: &str) -> String {
    join_portable(asset_root, item_path)
}

fn sibling_metadata_path(project_source: &str) -> Result<String, AssetOpsError> {
    let path = Path::new(project_source);
    let parent = path
        .parent()
        .map(portable_path)
        .filter(|value| !value.is_empty())
        .unwrap_or_default();
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or_else(|| AssetOpsError::InvalidPath(project_source.to_owned()))?;
    let file_name = format!("{stem}.yasset");
    Ok(if parent.is_empty() {
        file_name
    } else {
        format!("{parent}/{file_name}")
    })
}

fn is_sibling_metadata(metadata_path: &str, project_source: &str) -> bool {
    sibling_metadata_path(project_source)
        .map(|expected| expected == normalize_portable(metadata_path))
        .unwrap_or(false)
}

fn is_gltf_source(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            let lower = extension.to_ascii_lowercase();
            lower == "glb" || lower == "gltf"
        })
}

fn hash_file(path: &Path) -> Result<ContentHash, AssetOpsError> {
    let bytes = fs::read(path).map_err(AssetOpsError::Io)?;
    let digest = blake3::hash(&bytes);
    ContentHash::new(format!("blake3:{digest}"))
        .map_err(|error| AssetOpsError::InvalidPath(error.to_string()))
}

/// Blake3 content hash of a source file (`blake3:…`), for preview cache keys.
///
/// # Errors
///
/// Returns I/O or hash-format failures.
pub fn hash_asset_file(path: &Path) -> Result<ContentHash, AssetOpsError> {
    hash_file(path)
}

fn strip_asset_uri(identity: &str) -> &str {
    identity.strip_prefix("asset://").unwrap_or(identity)
}

fn validate_relative(path: &str) -> Result<(), AssetOpsError> {
    let candidate = Path::new(path);
    if path.is_empty()
        || candidate.is_absolute()
        || candidate.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(AssetOpsError::InvalidPath(path.to_owned()));
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

fn join_portable(root: &str, child: &str) -> String {
    let root = normalize_portable(root);
    let child = normalize_portable(child);
    if root.is_empty() {
        child
    } else if child.is_empty() {
        root
    } else {
        format!("{root}/{child}")
    }
}

fn portable_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn store_path_exists(store: &ProjectDocumentStore, relative: &str) -> Result<bool, AssetOpsError> {
    validate_relative(relative)?;
    let path = store.root().join(relative);
    match fs::symlink_metadata(&path) {
        Ok(metadata) => Ok(metadata.is_file()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(AssetOpsError::Io(error)),
    }
}

fn resolve_existing_file(
    store: &ProjectDocumentStore,
    relative: &str,
) -> Result<PathBuf, AssetOpsError> {
    validate_relative(relative)?;
    let path = store.root().join(relative);
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err(AssetOpsError::InvalidPath(format!(
                "symbolic links are rejected: {relative}"
            )))
        }
        Ok(metadata) if metadata.is_file() => Ok(path),
        Ok(_) => Err(AssetOpsError::MissingSource(relative.to_owned())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Err(AssetOpsError::MissingSource(relative.to_owned()))
        }
        Err(error) => Err(AssetOpsError::Io(error)),
    }
}

fn resolve_for_create(
    store: &ProjectDocumentStore,
    relative: &str,
) -> Result<PathBuf, AssetOpsError> {
    validate_relative(relative)?;
    Ok(store.root().join(relative))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AssetTracking;

    fn temporary_root(name: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("yuyib-asset-ops-{name}-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).expect("create temporary project");
        root
    }

    #[test]
    fn track_then_rename_preserves_guid() {
        let root = temporary_root("track-rename");
        fs::create_dir_all(root.join("assets/models")).expect("dirs");
        fs::write(root.join("assets/models/hero.glb"), b"glb-bytes").expect("glb");
        let store = ProjectDocumentStore::new(&root, 64 * 1024).expect("store");

        let tracked = ensure_tracked_gltf(&store, "assets", "models/hero.glb").expect("track");
        assert!(tracked.created);
        assert_eq!(tracked.source, "assets/models/hero.glb");
        assert_eq!(tracked.metadata_path, "assets/models/hero.yasset");

        let again = ensure_tracked_gltf(&store, "assets", "models/hero.glb").expect("idempotent");
        assert!(!again.created);
        assert_eq!(again.guid, tracked.guid);

        let renamed = rename_tracked_gltf(
            &store,
            "assets",
            &tracked.guid.to_string(),
            "models/hero_v2.glb",
        )
        .expect("rename");
        assert_eq!(renamed.guid, tracked.guid);
        assert_eq!(renamed.source, "assets/models/hero_v2.glb");
        assert_eq!(renamed.metadata_path, "assets/models/hero_v2.yasset");
        assert!(root.join("assets/models/hero_v2.glb").is_file());
        assert!(!root.join("assets/models/hero.glb").exists());
        assert!(root.join("assets/models/hero_v2.yasset").is_file());
        assert!(!root.join("assets/models/hero.yasset").exists());

        let index = build_asset_index(&store, "assets").expect("index");
        let source = index
            .items
            .iter()
            .find(|item| item.path == "models/hero_v2.glb")
            .expect("renamed source in index");
        assert_eq!(source.id, Some(tracked.guid));
        assert_eq!(source.tracking, AssetTracking::Tracked(tracked.guid));

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn track_rejects_escape_and_overwrite_destination() {
        let root = temporary_root("reject");
        fs::create_dir_all(root.join("assets")).expect("dirs");
        fs::write(root.join("assets/a.glb"), b"a").expect("a");
        fs::write(root.join("assets/b.glb"), b"b").expect("b");
        let store = ProjectDocumentStore::new(&root, 64 * 1024).expect("store");

        assert!(ensure_tracked_gltf(&store, "assets", "../escape.glb").is_err());
        let tracked = ensure_tracked_gltf(&store, "assets", "a.glb").expect("track a");
        let err = rename_tracked_gltf(&store, "assets", &tracked.guid.to_string(), "b.glb")
            .expect_err("overwrite");
        assert!(matches!(err, AssetOpsError::DestinationExists(_)));

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn save_import_settings_preserves_guid() {
        let root = temporary_root("settings");
        fs::create_dir_all(root.join("assets")).expect("dirs");
        fs::write(root.join("assets/a.glb"), b"a").expect("glb");
        let store = ProjectDocumentStore::new(&root, 64 * 1024).expect("store");
        let tracked = ensure_tracked_gltf(&store, "assets", "a.glb").expect("track");
        let saved = save_tracked_import_settings(
            &store,
            "assets",
            &tracked.guid.to_string(),
            json!({ "policy": "skeletal_preview" }),
        )
        .expect("save settings");
        assert_eq!(saved.guid, tracked.guid);
        let snapshot = store
            .load_json::<AssetDocument>(&tracked.metadata_path)
            .expect("reload");
        assert_eq!(
            snapshot.value.import_settings.payload.get("policy"),
            Some(&json!("skeletal_preview"))
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn refresh_dependencies_resolves_tracked_and_records_unresolved() {
        let root = temporary_root("deps");
        fs::create_dir_all(root.join("assets/models")).expect("dirs");
        fs::create_dir_all(root.join("assets/textures")).expect("texdir");
        fs::write(root.join("assets/models/hero.glb"), b"glb").expect("glb");
        fs::write(root.join("assets/textures/albedo.png"), b"png").expect("png");
        let store = ProjectDocumentStore::new(&root, 256 * 1024).expect("store");
        let tracked = ensure_tracked_gltf(&store, "assets", "models/hero.glb").expect("track");

        let albedo_guid = AssetGuid::new();
        let albedo_meta = AssetDocument {
            format: ASSET_FORMAT.to_owned(),
            format_version: SchemaVersion::new(1).expect("version"),
            guid: albedo_guid,
            source: "assets/textures/albedo.png".to_owned(),
            content_hash: None,
            importer: CapabilityId::new("yuyib.image-import").expect("importer"),
            importer_version: SchemaVersion::new(1).expect("version"),
            import_settings: ImportSettingsRecord {
                schema: ImportSettingsSchemaId::new("yuyib.image-import-settings")
                    .expect("schema"),
                version: SchemaVersion::new(1).expect("version"),
                payload: json!({}),
                extensions: BTreeMap::new(),
            },
            dependencies: Vec::new(),
            extensions: BTreeMap::new(),
        };
        store
            .save_json("assets/textures/albedo.yasset", &albedo_meta, None)
            .expect("write albedo meta");

        let (_asset, report) = refresh_tracked_dependencies(
            &store,
            "assets",
            &tracked.guid.to_string(),
            &[
                AssetLogicalDependency {
                    uri: "../textures/albedo.png".to_owned(),
                    kind: AssetDependencyKind::Optional,
                },
                AssetLogicalDependency {
                    uri: "missing.bin".to_owned(),
                    kind: AssetDependencyKind::Required,
                },
                AssetLogicalDependency {
                    uri: "http://example.com/x.bin".to_owned(),
                    kind: AssetDependencyKind::Required,
                },
                AssetLogicalDependency {
                    uri: "../../../escape.bin".to_owned(),
                    kind: AssetDependencyKind::Required,
                },
            ],
        )
        .expect("refresh");

        assert_eq!(report.dependencies, vec![albedo_guid]);
        assert_eq!(report.unresolved.len(), 3);
        assert!(
            report
                .unresolved
                .iter()
                .any(|item| item.code == "unresolved-external-dependency"
                    && item.uri == "missing.bin")
        );
        assert!(
            report
                .unresolved
                .iter()
                .any(|item| item.code == "invalid-dependency-uri"
                    && item.uri.starts_with("http://"))
        );
        assert!(
            report
                .unresolved
                .iter()
                .any(|item| item.code == "invalid-dependency-uri"
                    && item.uri.contains("escape"))
        );

        let snapshot = store
            .load_json::<AssetDocument>(&tracked.metadata_path)
            .expect("reload");
        assert_eq!(snapshot.value.dependencies, vec![albedo_guid]);
        assert!(
            snapshot
                .value
                .extensions
                .get("yuyib.dependency_diagnostics")
                .is_some()
        );

        fs::remove_dir_all(root).expect("cleanup");
    }
}
