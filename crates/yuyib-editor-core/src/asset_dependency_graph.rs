//! Reverse index over persisted [`AssetDocument::dependencies`] edges.
//!
//! Only GUID→GUID edges participate. Unresolved URI diagnostics never invent
//! identities and therefore never appear as reverse edges.

use std::{
    collections::BTreeMap,
    error::Error,
    fmt, fs,
    path::{Component, Path},
};

use serde::Serialize;
use yuyib_authoring::AssetGuid;

use crate::{ASSET_FORMAT, AssetDocument, DocumentError, ProjectDocumentStore};

const MAXIMUM_METADATA_FILES: usize = 4096;

/// Forward + reverse snapshot of tracked asset dependency edges.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct AssetDependencyGraph {
    /// `guid →` sorted dependents that list it in `AssetDocument.dependencies`.
    dependents: BTreeMap<AssetGuid, Vec<AssetGuid>>,
    /// `guid →` sorted forward dependency edges from that document.
    dependencies: BTreeMap<AssetGuid, Vec<AssetGuid>>,
}

impl AssetDependencyGraph {
    /// Returns assets that depend on `guid`, in stable GUID order.
    #[must_use]
    pub fn dependents_of(&self, guid: AssetGuid) -> &[AssetGuid] {
        self.dependents
            .get(&guid)
            .map_or(&[], Vec::as_slice)
    }

    /// Returns forward dependencies recorded for `guid`.
    #[must_use]
    pub fn dependencies_of(&self, guid: AssetGuid) -> &[AssetGuid] {
        self.dependencies
            .get(&guid)
            .map_or(&[], Vec::as_slice)
    }

    /// Number of assets that declare at least one forward edge.
    #[must_use]
    pub fn tracked_with_edges(&self) -> usize {
        self.dependencies
            .values()
            .filter(|edges| !edges.is_empty())
            .count()
    }
}

/// Ordered reverse-dependent cascade for a reimported root asset.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ReimportCascadePlan {
    /// Asset that was (or will be) reimported.
    pub root: AssetGuid,
    /// Transitive reverse dependents in BFS order (stable among siblings).
    /// Does not include `root`.
    pub dependents: Vec<AssetGuid>,
}

/// Plans which tracked assets must refresh when `root` is reimported.
///
/// Walks reverse edges only (`dependents_of`). Cycles are cut by a visited set.
/// Sibling order matches [`AssetDependencyGraph::dependents_of`] (sorted GUIDs).
#[must_use]
pub fn plan_reimport_cascade(
    graph: &AssetDependencyGraph,
    root: AssetGuid,
) -> ReimportCascadePlan {
    use std::collections::{BTreeSet, VecDeque};

    let mut dependents = Vec::new();
    let mut seen = BTreeSet::from([root]);
    let mut queue = VecDeque::new();
    for &dependent in graph.dependents_of(root) {
        queue.push_back(dependent);
    }
    while let Some(guid) = queue.pop_front() {
        if !seen.insert(guid) {
            continue;
        }
        dependents.push(guid);
        for &next in graph.dependents_of(guid) {
            if !seen.contains(&next) {
                queue.push_back(next);
            }
        }
    }
    ReimportCascadePlan { root, dependents }
}

/// Failure while scanning `.yasset` metadata for the dependency graph.
#[derive(Debug)]
pub enum AssetDependencyGraphError {
    /// Relative path escapes the project or uses disallowed components.
    InvalidPath(String),
    /// Document store rejected a load.
    Document(DocumentError),
    /// Filesystem walk failed.
    Io(std::io::Error),
}

impl fmt::Display for AssetDependencyGraphError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPath(path) => {
                write!(formatter, "path must be confined and relative: {path}")
            }
            Self::Document(error) => write!(formatter, "{error}"),
            Self::Io(error) => write!(formatter, "{error}"),
        }
    }
}

impl Error for AssetDependencyGraphError {}

impl From<DocumentError> for AssetDependencyGraphError {
    fn from(error: DocumentError) -> Self {
        Self::Document(error)
    }
}

/// Builds a reverse index from all valid `.yasset` documents under `asset_root`.
///
/// # Errors
///
/// Rejects an escaping asset root. Per-file parse failures are skipped.
pub fn build_asset_dependency_graph(
    store: &ProjectDocumentStore,
    asset_root: &str,
) -> Result<AssetDependencyGraph, AssetDependencyGraphError> {
    let asset_root = normalize_portable(asset_root);
    validate_relative(&asset_root)?;
    let absolute_root = store.root().join(&asset_root);
    let mut graph = AssetDependencyGraph::default();
    if !absolute_root.is_dir() {
        return Ok(graph);
    }

    let mut files = Vec::new();
    collect_yasset_files(&absolute_root, &absolute_root, &mut files);
    files.sort();
    if files.len() > MAXIMUM_METADATA_FILES {
        files.truncate(MAXIMUM_METADATA_FILES);
    }

    for relative in files {
        let project_relative = if asset_root.is_empty() {
            relative
        } else {
            format!("{asset_root}/{relative}")
        };
        let Ok(snapshot) = store.load_json::<AssetDocument>(&project_relative) else {
            continue;
        };
        if snapshot.value.format != ASSET_FORMAT {
            continue;
        }
        let owner = snapshot.value.guid;
        let mut forward = snapshot.value.dependencies;
        forward.sort();
        forward.dedup();
        for dependency in &forward {
            graph
                .dependents
                .entry(*dependency)
                .or_default()
                .push(owner);
        }
        if !forward.is_empty() {
            graph.dependencies.insert(owner, forward);
        }
    }

    for dependents in graph.dependents.values_mut() {
        dependents.sort();
        dependents.dedup();
    }

    Ok(graph)
}

fn collect_yasset_files(root: &Path, current: &Path, files: &mut Vec<String>) {
    let Ok(entries) = fs::read_dir(current) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            continue;
        };
        if metadata.file_type().is_symlink() {
            continue;
        }
        if metadata.is_dir() {
            collect_yasset_files(root, &path, files);
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

fn validate_relative(path: &str) -> Result<(), AssetDependencyGraphError> {
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
        return Err(AssetDependencyGraphError::InvalidPath(path.to_owned()));
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
    use crate::{
        AssetDependencyKind, AssetLogicalDependency, ImportSettingsRecord, ensure_tracked_gltf,
        refresh_tracked_dependencies,
    };
    use serde_json::json;
    use std::path::PathBuf;
    use yuyib_authoring::{
        CapabilityId, ImportSettingsSchemaId, SchemaVersion,
    };

    fn temporary_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "yuyib-dep-graph-{name}-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir(&root).expect("create temporary project");
        root
    }

    #[test]
    fn reverse_index_lists_dependents_in_stable_order() {
        let root = temporary_root("reverse");
        fs::create_dir_all(root.join("assets/models")).expect("dirs");
        fs::create_dir_all(root.join("assets/textures")).expect("tex");
        fs::write(root.join("assets/models/a.glb"), b"a").expect("a");
        fs::write(root.join("assets/models/b.glb"), b"b").expect("b");
        fs::write(root.join("assets/textures/albedo.png"), b"png").expect("png");
        let store = ProjectDocumentStore::new(&root, 256 * 1024).expect("store");

        let a = ensure_tracked_gltf(&store, "assets", "models/a.glb").expect("track a");
        let b = ensure_tracked_gltf(&store, "assets", "models/b.glb").expect("track b");

        let albedo_guid = AssetGuid::new();
        let albedo = AssetDocument {
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
                extensions: Default::default(),
            },
            dependencies: Vec::new(),
            extensions: Default::default(),
        };
        store
            .save_json("assets/textures/albedo.yasset", &albedo, None)
            .expect("albedo meta");

        refresh_tracked_dependencies(
            &store,
            "assets",
            &a.guid.to_string(),
            &[AssetLogicalDependency {
                uri: "../textures/albedo.png".to_owned(),
                kind: AssetDependencyKind::Optional,
            }],
        )
        .expect("deps a");
        refresh_tracked_dependencies(
            &store,
            "assets",
            &b.guid.to_string(),
            &[AssetLogicalDependency {
                uri: "../textures/albedo.png".to_owned(),
                kind: AssetDependencyKind::Optional,
            }],
        )
        .expect("deps b");

        let graph = build_asset_dependency_graph(&store, "assets").expect("graph");
        let mut expected = vec![a.guid, b.guid];
        expected.sort();
        assert_eq!(graph.dependents_of(albedo_guid), expected.as_slice());
        assert!(graph.dependents_of(a.guid).is_empty());
        assert_eq!(graph.dependencies_of(a.guid), &[albedo_guid]);
        assert_eq!(graph.tracked_with_edges(), 2);

        let plan = plan_reimport_cascade(&graph, albedo_guid);
        assert_eq!(plan.root, albedo_guid);
        assert_eq!(plan.dependents, expected);

        let leaf = plan_reimport_cascade(&graph, a.guid);
        assert!(leaf.dependents.is_empty());

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn cascade_walks_transitive_reverse_edges_and_cuts_cycles() {
        let root = temporary_root("cascade");
        fs::create_dir_all(root.join("assets/models")).expect("dirs");
        fs::write(root.join("assets/models/a.glb"), b"a").expect("a");
        fs::write(root.join("assets/models/b.glb"), b"b").expect("b");
        fs::write(root.join("assets/models/c.glb"), b"c").expect("c");
        let store = ProjectDocumentStore::new(&root, 256 * 1024).expect("store");
        let a = ensure_tracked_gltf(&store, "assets", "models/a.glb").expect("a");
        let b = ensure_tracked_gltf(&store, "assets", "models/b.glb").expect("b");
        let c = ensure_tracked_gltf(&store, "assets", "models/c.glb").expect("c");

        // c → b → a  (forward deps). Reverse cascade from a: b then c.
        refresh_tracked_dependencies(
            &store,
            "assets",
            &b.guid.to_string(),
            &[AssetLogicalDependency {
                uri: "a.glb".to_owned(),
                kind: AssetDependencyKind::Required,
            }],
        )
        .expect("b→a");
        refresh_tracked_dependencies(
            &store,
            "assets",
            &c.guid.to_string(),
            &[AssetLogicalDependency {
                uri: "b.glb".to_owned(),
                kind: AssetDependencyKind::Required,
            }],
        )
        .expect("c→b");
        // Cycle edge a → c would make reverse infinite without visited.
        refresh_tracked_dependencies(
            &store,
            "assets",
            &a.guid.to_string(),
            &[AssetLogicalDependency {
                uri: "c.glb".to_owned(),
                kind: AssetDependencyKind::Optional,
            }],
        )
        .expect("a→c cycle");

        let graph = build_asset_dependency_graph(&store, "assets").expect("graph");
        let plan = plan_reimport_cascade(&graph, a.guid);
        assert_eq!(plan.dependents, vec![b.guid, c.guid]);

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn empty_asset_root_yields_empty_graph() {
        let root = temporary_root("empty");
        let store = ProjectDocumentStore::new(&root, 64 * 1024).expect("store");
        let graph = build_asset_dependency_graph(&store, "assets").expect("graph");
        assert_eq!(graph, AssetDependencyGraph::default());
        fs::remove_dir_all(root).expect("cleanup");
    }
}
