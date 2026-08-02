use std::{
    collections::hash_map::DefaultHasher,
    error::Error,
    fmt, fs,
    hash::{Hash, Hasher},
    path::{Component, Path, PathBuf},
};

use yuyib_authoring::AssetGuid;

use crate::{ASSET_FORMAT, AssetDocument, ProjectDocumentStore};

const MAXIMUM_ITEMS: usize = 256;

/// A deterministic snapshot of assets below one confined project directory.
#[derive(Debug)]
pub struct ProjectAssetIndex {
    /// Fingerprint of the indexed paths and filesystem metadata.
    pub revision: u64,
    /// Project-relative root that was indexed.
    pub root: String,
    /// Sorted index entries, bounded to 256 items.
    pub items: Vec<AssetIndexItem>,
    /// Non-fatal discovery and metadata failures.
    pub diagnostics: Vec<AssetIndexDiagnostic>,
}

/// One project file visible in [`ProjectAssetIndex`].
#[derive(Debug)]
pub struct AssetIndexItem {
    /// Persistent metadata identity, when the item has valid `.yasset` metadata.
    pub id: Option<AssetGuid>,
    /// Path relative to the indexed asset root, using `/` separators.
    pub path: String,
    /// File-stem label suitable for an asset browser.
    pub name: String,
    /// Coarse type used to select host behavior.
    pub kind: AssetKind,
    /// Whether the item has persistent asset metadata.
    pub tracking: AssetTracking,
    /// Editor action available for this item.
    pub open: Option<AssetOpenIntent>,
    /// Current preview route availability.
    pub preview: Option<AssetActionStatus>,
    /// Current reimport route availability.
    pub reimport: Option<AssetActionStatus>,
    /// Parsed metadata retained for tracked assets.
    pub metadata: Option<AssetDocument>,
}

/// Coarse asset-browser classification.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AssetKind {
    /// A Yuyib scene document.
    Scene,
    /// A Yuyib asset metadata document.
    AssetMetadata,
    /// A glTF or GLB source file.
    GltfSource,
    /// A common raster image source file.
    ImageSource,
    /// A file without a known Editor route.
    Other,
}

/// Persistent metadata state for an index item.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AssetTracking {
    /// Valid `.yasset` metadata supplies the persistent identity.
    Tracked(AssetGuid),
    /// A source file has no persistent Yuyib asset identity.
    UntrackedSource,
    /// A metadata file could not be interpreted safely.
    InvalidMetadata,
    /// The file is not part of the import-tracking model.
    NotApplicable,
}

/// Explicit host action for an index item.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AssetOpenIntent {
    /// Open the scene in scene authoring.
    Scene,
    /// Open the glTF source in the asset preview.
    GltfPreview,
}

/// Availability of a future host action.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AssetActionStatus {
    /// The host provides this action.
    Available,
    /// The host has no production route for this action.
    Unavailable {
        /// Stable host-facing explanation code.
        reason_code: &'static str,
    },
}

/// Non-fatal condition encountered while discovering the index.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AssetIndexDiagnostic {
    /// Path relative to the asset root, when one is available.
    pub path: String,
    /// Stable machine-readable condition code.
    pub code: AssetIndexDiagnosticCode,
    /// Human-readable detail for logs and diagnostics UI.
    pub message: String,
}

/// Stable diagnostic category emitted by [`build_asset_index`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AssetIndexDiagnosticCode {
    /// The asset root is absent.
    MissingAssetRoot,
    /// A symbolic link was skipped to preserve root confinement.
    SymbolicLinkSkipped,
    /// A directory entry could not be read.
    DirectoryReadFailed,
    /// A `.yasset` file was not valid metadata.
    MalformedAssetMetadata,
    /// A `.yasset` document had the wrong format discriminator.
    UnsupportedAssetMetadataFormat,
    /// More files were found than the bounded index can expose.
    ItemLimitReached,
}

/// Asset-index construction failure.
#[derive(Debug)]
pub enum AssetIndexError {
    /// The requested root is absolute or escapes the project directory.
    InvalidAssetRoot(PathBuf),
    /// The requested root itself is a symbolic link.
    SymbolicLinkAssetRoot(PathBuf),
}

impl fmt::Display for AssetIndexError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidAssetRoot(path) => write!(
                formatter,
                "asset root must be a confined project-relative path: {}",
                path.display()
            ),
            Self::SymbolicLinkAssetRoot(path) => {
                write!(
                    formatter,
                    "asset root must not be a symbolic link: {}",
                    path.display()
                )
            }
        }
    }
}

impl Error for AssetIndexError {}

/// Builds a bounded, deterministic index below `asset_root`.
///
/// The index never follows symbolic links and only exposes paths under the
/// store's canonical project root. Missing roots and malformed metadata remain
/// visible as diagnostics so hosts can surface them instead of silently losing
/// assets.
///
/// # Errors
///
/// Rejects an absolute, escaping, or symbolic-link asset root.
pub fn build_asset_index(
    store: &ProjectDocumentStore,
    asset_root: &str,
) -> Result<ProjectAssetIndex, AssetIndexError> {
    let relative_root = Path::new(asset_root);
    validate_asset_root(relative_root)?;
    let indexed_root = store.root().join(relative_root);
    match fs::symlink_metadata(&indexed_root) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(AssetIndexError::SymbolicLinkAssetRoot(
                relative_root.to_path_buf(),
            ));
        }
        Ok(metadata) if !metadata.is_dir() => {
            return Ok(ProjectAssetIndex {
                revision: 0,
                root: portable_path(relative_root),
                items: Vec::new(),
                diagnostics: vec![diagnostic(
                    "",
                    AssetIndexDiagnosticCode::MissingAssetRoot,
                    "asset root is not a directory",
                )],
            });
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ProjectAssetIndex {
                revision: 0,
                root: portable_path(relative_root),
                items: Vec::new(),
                diagnostics: vec![diagnostic(
                    "",
                    AssetIndexDiagnosticCode::MissingAssetRoot,
                    "asset root does not exist",
                )],
            });
        }
        Err(error) => {
            return Ok(ProjectAssetIndex {
                revision: 0,
                root: portable_path(relative_root),
                items: Vec::new(),
                diagnostics: vec![diagnostic(
                    "",
                    AssetIndexDiagnosticCode::DirectoryReadFailed,
                    error.to_string(),
                )],
            });
        }
    }

    let mut files = Vec::new();
    let mut diagnostics = Vec::new();
    collect_files(&indexed_root, &indexed_root, &mut files, &mut diagnostics);
    files.sort_by(|left, right| left.0.cmp(&right.0));

    let truncated = files.len() > MAXIMUM_ITEMS;
    files.truncate(MAXIMUM_ITEMS);
    let mut items = Vec::with_capacity(files.len());
    let mut revision = DefaultHasher::new();
    for (path, absolute) in files {
        path.hash(&mut revision);
        if let Ok(metadata) = fs::metadata(&absolute) {
            metadata.len().hash(&mut revision);
            metadata.modified().ok().hash(&mut revision);
        }
        items.push(build_item(store, &path, &absolute, &mut diagnostics));
    }
    attach_source_tracking(asset_root, &mut items);
    if truncated {
        diagnostics.push(diagnostic(
            "",
            AssetIndexDiagnosticCode::ItemLimitReached,
            "asset index is limited to 256 items",
        ));
    }

    Ok(ProjectAssetIndex {
        revision: revision.finish(),
        root: portable_path(relative_root),
        items,
        diagnostics,
    })
}

/// Joins `.yasset` GUID onto matching source entries (path-independent identity).
fn attach_source_tracking(asset_root: &str, items: &mut [AssetIndexItem]) {
    let mut by_source = std::collections::BTreeMap::<String, AssetGuid>::new();
    for item in items.iter() {
        let Some(metadata) = &item.metadata else {
            continue;
        };
        let source = normalize_portable(&metadata.source);
        by_source.insert(source.clone(), metadata.guid);
        // Also accept asset-root-relative sources written without the root prefix.
        if let Some(stripped) = source.strip_prefix(&format!("{}/", normalize_portable(asset_root)))
        {
            by_source.insert(stripped.to_owned(), metadata.guid);
        }
    }
    let root = normalize_portable(asset_root);
    for item in items.iter_mut() {
        if !matches!(
            item.kind,
            AssetKind::GltfSource | AssetKind::ImageSource | AssetKind::Other
        ) {
            continue;
        }
        if item.id.is_some() {
            continue;
        }
        let under_root = normalize_portable(&item.path);
        let project_source = if root.is_empty() {
            under_root.clone()
        } else {
            format!("{root}/{under_root}")
        };
        let Some(guid) = by_source
            .get(&project_source)
            .or_else(|| by_source.get(&under_root))
            .copied()
        else {
            continue;
        };
        item.id = Some(guid);
        item.tracking = AssetTracking::Tracked(guid);
    }
}

fn normalize_portable(path: &str) -> String {
    path.replace('\\', "/")
        .split('/')
        .filter(|segment| !segment.is_empty() && *segment != ".")
        .collect::<Vec<_>>()
        .join("/")
}

fn collect_files(
    root: &Path,
    directory: &Path,
    files: &mut Vec<(String, PathBuf)>,
    diagnostics: &mut Vec<AssetIndexDiagnostic>,
) {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) => {
            diagnostics.push(diagnostic(
                relative_path(root, directory).as_deref().unwrap_or(""),
                AssetIndexDiagnosticCode::DirectoryReadFailed,
                error.to_string(),
            ));
            return;
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                diagnostics.push(diagnostic(
                    "",
                    AssetIndexDiagnosticCode::DirectoryReadFailed,
                    error.to_string(),
                ));
                continue;
            }
        };
        let path = entry.path();
        let relative = relative_path(root, &path).unwrap_or_else(|| path.display().to_string());
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) => {
                diagnostics.push(diagnostic(
                    &relative,
                    AssetIndexDiagnosticCode::DirectoryReadFailed,
                    error.to_string(),
                ));
                continue;
            }
        };
        if metadata.file_type().is_symlink() {
            diagnostics.push(diagnostic(
                &relative,
                AssetIndexDiagnosticCode::SymbolicLinkSkipped,
                "symbolic links are not indexed",
            ));
        } else if metadata.is_dir() {
            collect_files(root, &path, files, diagnostics);
        } else if metadata.is_file() {
            files.push((relative, path));
        }
    }
}

fn build_item(
    store: &ProjectDocumentStore,
    path: &str,
    absolute: &Path,
    diagnostics: &mut Vec<AssetIndexDiagnostic>,
) -> AssetIndexItem {
    let extension = absolute
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase);
    let name = absolute
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or(path)
        .to_owned();
    match extension.as_deref() {
        Some("yscene") => AssetIndexItem {
            id: None,
            path: path.to_owned(),
            name,
            kind: AssetKind::Scene,
            tracking: AssetTracking::NotApplicable,
            open: Some(AssetOpenIntent::Scene),
            preview: None,
            reimport: None,
            metadata: None,
        },
        Some("yasset") => metadata_item(store, path, absolute, name, diagnostics),
        Some("gltf" | "glb") => source_item(path, name, AssetKind::GltfSource, true),
        Some("png" | "jpg" | "jpeg" | "webp" | "bmp" | "gif" | "tga") => {
            source_item(path, name, AssetKind::ImageSource, false)
        }
        _ => source_item(path, name, AssetKind::Other, false),
    }
}

fn metadata_item(
    store: &ProjectDocumentStore,
    path: &str,
    absolute: &Path,
    name: String,
    diagnostics: &mut Vec<AssetIndexDiagnostic>,
) -> AssetIndexItem {
    let project_relative = absolute
        .strip_prefix(store.root())
        .expect("indexed asset path must remain beneath the project root");
    match store.load_json::<AssetDocument>(project_relative) {
        Ok(snapshot) if snapshot.value.format == ASSET_FORMAT => {
            let metadata = snapshot.value;
            AssetIndexItem {
                id: Some(metadata.guid),
                path: path.to_owned(),
                name,
                kind: AssetKind::AssetMetadata,
                tracking: AssetTracking::Tracked(metadata.guid),
                open: None,
                preview: None,
                reimport: None,
                metadata: Some(metadata),
            }
        }
        Ok(snapshot) => {
            diagnostics.push(diagnostic(
                path,
                AssetIndexDiagnosticCode::UnsupportedAssetMetadataFormat,
                format!("unsupported asset format {:?}", snapshot.value.format),
            ));
            invalid_metadata_item(path, name)
        }
        Err(error) => {
            diagnostics.push(diagnostic(
                path,
                AssetIndexDiagnosticCode::MalformedAssetMetadata,
                error.to_string(),
            ));
            invalid_metadata_item(path, name)
        }
    }
}

fn invalid_metadata_item(path: &str, name: String) -> AssetIndexItem {
    AssetIndexItem {
        id: None,
        path: path.to_owned(),
        name,
        kind: AssetKind::AssetMetadata,
        tracking: AssetTracking::InvalidMetadata,
        open: None,
        preview: None,
        reimport: None,
        metadata: None,
    }
}

fn source_item(path: &str, name: String, kind: AssetKind, is_gltf: bool) -> AssetIndexItem {
    let available = is_gltf.then_some(AssetActionStatus::Available);
    AssetIndexItem {
        id: None,
        path: path.to_owned(),
        name,
        kind,
        tracking: AssetTracking::UntrackedSource,
        open: is_gltf.then_some(AssetOpenIntent::GltfPreview),
        preview: available,
        reimport: available,
        metadata: None,
    }
}

fn validate_asset_root(path: &Path) -> Result<(), AssetIndexError> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(AssetIndexError::InvalidAssetRoot(path.to_path_buf()));
    }
    Ok(())
}

fn relative_path(root: &Path, path: &Path) -> Option<String> {
    path.strip_prefix(root).ok().map(portable_path)
}

fn portable_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn diagnostic(
    path: &str,
    code: AssetIndexDiagnosticCode,
    message: impl Into<String>,
) -> AssetIndexDiagnostic {
    AssetIndexDiagnostic {
        path: path.to_owned(),
        code,
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, fs};

    use serde_json::json;
    use yuyib_authoring::{CapabilityId, ContentHash, ImportSettingsSchemaId, SchemaVersion};

    use super::*;
    use crate::ImportSettingsRecord;

    fn temporary_root(name: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("yuyib-asset-index-{name}-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).expect("create temporary project");
        root
    }

    fn asset_document() -> AssetDocument {
        AssetDocument {
            format: ASSET_FORMAT.to_owned(),
            format_version: SchemaVersion::new(1).expect("version"),
            guid: AssetGuid::new(),
            source: "models/hero.glb".to_owned(),
            content_hash: Some(ContentHash::new("blake3:01").expect("hash")),
            importer: CapabilityId::new("yuyib.gltf-import").expect("importer"),
            importer_version: SchemaVersion::new(1).expect("version"),
            import_settings: ImportSettingsRecord {
                schema: ImportSettingsSchemaId::new("yuyib.gltf-import").expect("schema"),
                version: SchemaVersion::new(1).expect("version"),
                payload: json!({}),
                extensions: BTreeMap::new(),
            },
            dependencies: Vec::new(),
            extensions: BTreeMap::new(),
        }
    }

    #[test]
    fn index_is_sorted_classifies_sources_and_preserves_invalid_metadata() {
        let root = temporary_root("classification");
        fs::create_dir_all(root.join("assets/models")).expect("asset directory");
        fs::write(root.join("assets/models/hero.glb"), []).expect("glb fixture");
        fs::write(root.join("assets/main.yscene"), "{}").expect("scene fixture");
        fs::write(root.join("assets/broken.yasset"), "{").expect("invalid metadata");
        let store = ProjectDocumentStore::new(&root, 4096).expect("store");

        let index = build_asset_index(&store, "assets").expect("index");

        assert_eq!(
            index
                .items
                .iter()
                .map(|item| &item.path)
                .collect::<Vec<_>>(),
            vec!["broken.yasset", "main.yscene", "models/hero.glb"]
        );
        assert_eq!(index.items[1].open, Some(AssetOpenIntent::Scene));
        assert_eq!(index.items[2].id, None);
        assert_eq!(index.items[2].open, Some(AssetOpenIntent::GltfPreview));
        assert_eq!(index.items[2].preview, Some(AssetActionStatus::Available));
        assert_eq!(index.items[2].reimport, Some(AssetActionStatus::Available));
        assert!(index.diagnostics.iter().any(|diagnostic| {
            diagnostic.path == "broken.yasset"
                && diagnostic.code == AssetIndexDiagnosticCode::MalformedAssetMetadata
        }));

        fs::remove_dir_all(root).expect("remove temporary project");
    }

    #[test]
    fn index_parses_tracked_metadata_and_reports_missing_root() {
        let root = temporary_root("metadata");
        fs::create_dir(root.join("assets")).expect("asset directory");
        let store = ProjectDocumentStore::new(&root, 4096).expect("store");
        let metadata = asset_document();
        store
            .save_json("assets/hero.yasset", &metadata, None)
            .expect("metadata fixture");

        let index = build_asset_index(&store, "assets").expect("index");
        assert_eq!(index.items.len(), 1);
        assert_eq!(index.items[0].id, Some(metadata.guid));
        assert_eq!(
            index.items[0].tracking,
            AssetTracking::Tracked(metadata.guid)
        );
        assert_eq!(
            index.items[0].metadata.as_ref().map(|item| item.guid),
            Some(metadata.guid)
        );

        let missing = build_asset_index(&store, "missing").expect("missing root");
        assert!(missing.items.is_empty());
        assert_eq!(
            missing.diagnostics[0].code,
            AssetIndexDiagnosticCode::MissingAssetRoot
        );

        fs::remove_dir_all(root).expect("remove temporary project");
    }
}
