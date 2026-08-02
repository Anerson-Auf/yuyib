//! Creates a minimal on-disk Yuyib Editor project layout.

use std::{
    error::Error,
    fmt, fs,
    path::{Path, PathBuf},
};

use serde_json::json;
use yuyib_authoring::{
    ComponentRecord, ComponentSchemaId, EntityGuid, SCENE_FORMAT_VERSION, SceneDocument,
    SceneEntityRecord, SchemaVersion,
};

use crate::{
    DocumentError, ProjectDocumentStore, ProjectManifest, ProjectProfile, ProjectScene,
    ProjectValidationError,
};

const STARTER_SCENE_PATH: &str = "scenes/main.yscene";
const STARTER_SCENE_NAME: &str = "Main";
const STARTER_SOURCE: &str = r#"//! Generated Yuyib project bootstrap.
//!
//! Scenes are data files (`.yscene`), not generated Rust. Editor Play launches
//! the engine `yuyib-play` runner which loads the pinned scene into a window.
//! Replace this file when you ship a custom game binary.

fn main() {
    println!("Yuyib project ready — use Editor Play (yuyib-play) to run scenes.");
}
"#;

fn starter_cargo_toml(package_name: &str) -> String {
    format!(
        r#"[package]
name = "{package_name}"
version = "0.1.0"
edition = "2021"
publish = false

# Keep this package out of any parent Cargo workspace that nests above it.
[workspace]

[[bin]]
name = "{package_name}"
path = "src/main.rs"
"#
    )
}

/// Options for [`scaffold_project`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScaffoldRequest {
    /// Absolute or relative parent directory that will contain the project folder.
    pub parent_directory: PathBuf,
    /// Folder and project display name.
    pub project_name: String,
    /// Default application/game composition.
    pub profile: ProjectProfile,
}

/// Result of a successful scaffold.
#[derive(Clone, Debug)]
pub struct ScaffoldedProject {
    /// Canonical project root.
    pub root: PathBuf,
    /// Written project manifest.
    pub manifest: ProjectManifest,
    /// Project-relative startup scene path.
    pub startup_scene_path: String,
}

/// Failure while creating a new project tree.
#[derive(Debug)]
pub enum ScaffoldError {
    /// Project name rejected by validation policy.
    InvalidName(String),
    /// Target path already exists.
    AlreadyExists(PathBuf),
    /// Selected path does not contain `project.yuyib`.
    MissingManifest(PathBuf),
    /// Selected parent contains more than one project folder.
    AmbiguousProjectRoot {
        /// Directory that was inspected.
        parent: PathBuf,
        /// Child directories that each contain `project.yuyib`.
        candidates: Vec<PathBuf>,
    },
    /// Underlying filesystem failure.
    Io(std::io::Error),
    /// Document store rejection.
    Document(DocumentError),
    /// Manifest failed validation before write.
    Manifest(ProjectValidationError),
    /// Scene schema construction failed.
    Scene(String),
}

impl fmt::Display for ScaffoldError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidName(name) => write!(formatter, "invalid project name {name:?}"),
            Self::AlreadyExists(path) => {
                write!(formatter, "project path already exists: {}", path.display())
            }
            Self::MissingManifest(path) => write!(
                formatter,
                "no project.yuyib in {} — select the project folder that contains project.yuyib",
                path.display()
            ),
            Self::AmbiguousProjectRoot { parent, candidates } => {
                write!(
                    formatter,
                    "multiple projects under {}: pick one of {}",
                    parent.display(),
                    candidates
                        .iter()
                        .map(|path| path.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
            Self::Io(error) => write!(formatter, "project scaffold I/O failed: {error}"),
            Self::Document(error) => write!(formatter, "project scaffold document failed: {error}"),
            Self::Manifest(error) => write!(formatter, "project scaffold manifest failed: {error}"),
            Self::Scene(error) => write!(formatter, "project scaffold scene failed: {error}"),
        }
    }
}

impl Error for ScaffoldError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Document(error) => Some(error),
            Self::Manifest(error) => Some(error),
            _ => None,
        }
    }
}

/// Creates `parent/name/` with `project.yuyib`, `assets/`, `scenes/main.yscene`, and `src/main.rs`.
///
/// # Errors
///
/// Rejects empty/invalid names, existing targets, I/O failures, and document validation errors.
pub fn scaffold_project(request: ScaffoldRequest) -> Result<ScaffoldedProject, ScaffoldError> {
    let name = sanitize_project_name(&request.project_name)?;
    let root = request.parent_directory.join(&name);
    if root.exists() {
        return Err(ScaffoldError::AlreadyExists(root));
    }

    fs::create_dir_all(&root).map_err(ScaffoldError::Io)?;
    for relative in ["assets", "scenes", "src"] {
        fs::create_dir_all(root.join(relative)).map_err(ScaffoldError::Io)?;
    }

    let documents =
        ProjectDocumentStore::new(&root, 2 * 1024 * 1024).map_err(ScaffoldError::Document)?;

    let scene = starter_scene().map_err(ScaffoldError::Scene)?;
    documents
        .save_json(STARTER_SCENE_PATH, &scene, None)
        .map_err(ScaffoldError::Document)?;
    documents
        .save_text("src/main.rs", STARTER_SOURCE, None)
        .map_err(ScaffoldError::Document)?;

    let mut manifest = ProjectManifest::new(name.clone(), request.profile);
    let scene_guid = scene.scene_guid;
    manifest.scenes.push(ProjectScene {
        guid: scene_guid,
        path: STARTER_SCENE_PATH.to_owned(),
        name: STARTER_SCENE_NAME.to_owned(),
        extensions: Default::default(),
    });
    manifest.startup_scene = Some(scene_guid);
    let package_name = cargo_package_name(&name);
    manifest.development.cargo_package = Some(package_name.clone());
    manifest.validate().map_err(ScaffoldError::Manifest)?;
    documents
        .save_json("project.yuyib", &manifest, None)
        .map_err(ScaffoldError::Document)?;
    documents
        .save_text("Cargo.toml", &starter_cargo_toml(&package_name), None)
        .map_err(ScaffoldError::Document)?;

    Ok(ScaffoldedProject {
        root: documents.root().to_path_buf(),
        manifest,
        startup_scene_path: STARTER_SCENE_PATH.to_owned(),
    })
}

/// Ensures a minimal `Cargo.toml` exists when the manifest declares a Cargo package.
///
/// Existing projects created before scaffold wrote a package manifest are repaired
/// in place so scoped `cargo check` does not walk into a parent workspace and
/// suggest unrelated crates. Also inserts an empty `[workspace]` table when a
/// legacy Cargo.toml is missing one.
///
/// # Errors
///
/// Returns document I/O failures when creating or repairing the package file.
pub fn ensure_project_cargo_toml(
    documents: &ProjectDocumentStore,
    manifest: &ProjectManifest,
) -> Result<bool, ScaffoldError> {
    let Some(package) = manifest.development.cargo_package.as_deref() else {
        return Ok(false);
    };
    let cargo_path = documents.root().join("Cargo.toml");
    if !cargo_path.is_file() {
        documents
            .save_text("Cargo.toml", &starter_cargo_toml(package), None)
            .map_err(ScaffoldError::Document)?;
        return Ok(true);
    }
    let existing = fs::read_to_string(&cargo_path).map_err(ScaffoldError::Io)?;
    if existing.contains("[workspace]") {
        return Ok(false);
    }
    let repaired = if existing.trim_end().is_empty() {
        starter_cargo_toml(package)
    } else {
        format!(
            "{}\n\n# Keep this package out of any parent Cargo workspace.\n[workspace]\n",
            existing.trim_end()
        )
    };
    fs::write(&cargo_path, repaired).map_err(ScaffoldError::Io)?;
    Ok(true)
}

/// Resolves a user-selected path to a directory that directly contains `project.yuyib`.
///
/// Accepts the project root directory or the `project.yuyib` file itself. Does not
/// search parent/child trees — that silently opened unrelated folders (for example
/// monorepo test fixtures) and looked like a broken startup loop.
///
/// # Errors
///
/// Returns [`ScaffoldError::MissingManifest`] when the selection is not a project
/// root, or I/O failures while canonicalizing.
pub fn resolve_project_root(path: impl AsRef<Path>) -> Result<PathBuf, ScaffoldError> {
    let path = path.as_ref();
    let candidate =
        if path.is_file() && path.file_name().is_some_and(|name| name == "project.yuyib") {
            path.parent()
                .map(Path::to_path_buf)
                .ok_or_else(|| ScaffoldError::MissingManifest(path.to_path_buf()))?
        } else {
            path.to_path_buf()
        };

    if !candidate.join("project.yuyib").is_file() {
        return Err(ScaffoldError::MissingManifest(candidate));
    }
    candidate.canonicalize().map_err(ScaffoldError::Io)
}

/// Opens an existing project directory that already contains `project.yuyib`.
///
/// # Errors
///
/// Rejects missing roots/manifests and invalid project documents.
pub fn open_existing_project(
    root: impl AsRef<Path>,
    maximum_bytes: usize,
) -> Result<(ProjectDocumentStore, ProjectManifest), ScaffoldError> {
    let root = resolve_project_root(root)?;
    let documents =
        ProjectDocumentStore::new(root, maximum_bytes).map_err(ScaffoldError::Document)?;
    let snapshot = documents
        .load_json::<ProjectManifest>("project.yuyib")
        .map_err(ScaffoldError::Document)?;
    snapshot.value.validate().map_err(ScaffoldError::Manifest)?;
    let mut manifest = snapshot.value;
    if ensure_project_cargo_toml(&documents, &manifest)? {
        // Keep the in-memory package name aligned with the repaired Cargo.toml.
        if manifest.development.cargo_package.is_none() {
            manifest.development.cargo_package = Some(cargo_package_name(&manifest.name));
        }
    }
    Ok((documents, manifest))
}

fn sanitize_project_name(raw: &str) -> Result<String, ScaffoldError> {
    let name = raw.trim();
    if name.is_empty()
        || name.len() > 64
        || name.contains(['/', '\\', ':', '*', '?', '"', '<', '>', '|'])
        || name == "."
        || name == ".."
    {
        return Err(ScaffoldError::InvalidName(raw.to_owned()));
    }
    Ok(name.to_owned())
}

fn cargo_package_name(project_name: &str) -> String {
    let mut package = String::new();
    for byte in project_name.bytes() {
        let mapped = match byte {
            b'A'..=b'Z' => byte + 32,
            b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' => byte,
            b' ' | b'.' => b'_',
            _ => continue,
        };
        package.push(char::from(mapped));
    }
    if package.is_empty() {
        "yuyib_game".to_owned()
    } else if package.as_bytes()[0].is_ascii_digit() {
        format!("game_{package}")
    } else {
        package
    }
}

fn starter_scene() -> Result<SceneDocument, String> {
    let version = SchemaVersion::new(SCENE_FORMAT_VERSION).map_err(|error| error.to_string())?;
    let transform_schema =
        ComponentSchemaId::new("yuyib.transform3d").map_err(|error| error.to_string())?;
    let model_schema =
        ComponentSchemaId::new("yuyib.model3d").map_err(|error| error.to_string())?;
    let mut scene = SceneDocument::new(version);
    let entity = EntityGuid::new();
    scene.entities.push(SceneEntityRecord {
        guid: entity,
        name: Some("Cube".to_owned()),
        components: vec![
            ComponentRecord::new(
                transform_schema,
                SchemaVersion::new(1).map_err(|error| error.to_string())?,
                json!({
                    "translation": [0.0, 0.0, 0.0],
                    "rotation": [0.0, 0.0, 0.0, 1.0],
                    "scale": [1.0, 1.0, 1.0]
                }),
            ),
            ComponentRecord::new(
                model_schema,
                SchemaVersion::new(1).map_err(|error| error.to_string())?,
                json!({
                    "model": "builtin:cube",
                    "mesh": null,
                    "visible": true,
                    "render_order": 0
                }),
            ),
        ],
        extensions: Default::default(),
    });
    scene.validate().map_err(|error| error.to_string())?;
    Ok(scene)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scaffold_writes_manifest_scene_and_source() {
        let parent = std::env::temp_dir().join(format!(
            "yuyib-scaffold-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        ));
        fs::create_dir_all(&parent).expect("parent");
        let project = scaffold_project(ScaffoldRequest {
            parent_directory: parent.clone(),
            project_name: "Neon District".to_owned(),
            profile: ProjectProfile::Game3d,
        })
        .expect("scaffold");

        assert!(project.root.join("project.yuyib").is_file());
        assert!(project.root.join("scenes/main.yscene").is_file());
        assert!(project.root.join("src/main.rs").is_file());
        assert!(project.root.join("Cargo.toml").is_file());
        assert!(project.root.join("assets").is_dir());
        assert_eq!(
            resolve_project_root(&project.root).expect("root resolves"),
            project.root
        );
        assert_eq!(
            resolve_project_root(project.root.join("project.yuyib")).expect("manifest resolves"),
            project.root
        );
        assert!(matches!(
            resolve_project_root(&parent),
            Err(ScaffoldError::MissingManifest(_))
        ));
        assert_eq!(project.manifest.name, "Neon District");
        assert_eq!(
            project.manifest.development.cargo_package.as_deref(),
            Some("neon_district")
        );
        let cargo = fs::read_to_string(project.root.join("Cargo.toml")).expect("cargo");
        assert!(cargo.contains("name = \"neon_district\""));
        assert!(cargo.contains("[workspace]"));
        assert_eq!(project.startup_scene_path, STARTER_SCENE_PATH);

        fs::remove_dir_all(parent).expect("cleanup");
    }

    #[test]
    fn open_existing_accepts_editor_tests_prj2_fixture() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../editor_tests/prj2");
        let (documents, manifest) =
            open_existing_project(&root, 2 * 1024 * 1024).expect("open prj2");
        assert_eq!(manifest.name, "prj2");
        assert!(documents.root().join("project.yuyib").is_file());
        assert!(documents.root().join("scenes/main.yscene").is_file());
    }

    #[test]
    fn open_existing_repairs_missing_cargo_toml() {
        let parent = std::env::temp_dir().join(format!(
            "yuyib-scaffold-repair-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        ));
        fs::create_dir_all(&parent).expect("parent");
        let project = scaffold_project(ScaffoldRequest {
            parent_directory: parent.clone(),
            project_name: "prj".to_owned(),
            profile: ProjectProfile::Game3d,
        })
        .expect("scaffold");
        fs::remove_file(project.root.join("Cargo.toml")).expect("remove cargo");
        let (documents, manifest) =
            open_existing_project(&project.root, 2 * 1024 * 1024).expect("open");
        assert_eq!(manifest.development.cargo_package.as_deref(), Some("prj"));
        assert!(documents.root().join("Cargo.toml").is_file());
        let cargo = fs::read_to_string(documents.root().join("Cargo.toml")).expect("cargo");
        assert!(cargo.contains("[workspace]"));
        fs::remove_dir_all(parent).expect("cleanup");
    }

    #[test]
    fn open_existing_injects_workspace_table_into_legacy_cargo_toml() {
        let parent = std::env::temp_dir().join(format!(
            "yuyib-scaffold-workspace-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        ));
        fs::create_dir_all(&parent).expect("parent");
        let project = scaffold_project(ScaffoldRequest {
            parent_directory: parent.clone(),
            project_name: "legacy".to_owned(),
            profile: ProjectProfile::Game3d,
        })
        .expect("scaffold");
        fs::write(
            project.root.join("Cargo.toml"),
            "[package]\nname = \"legacy\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .expect("legacy cargo");
        let (documents, _) = open_existing_project(&project.root, 2 * 1024 * 1024).expect("open");
        let cargo = fs::read_to_string(documents.root().join("Cargo.toml")).expect("cargo");
        assert!(cargo.contains("[workspace]"));
        fs::remove_dir_all(parent).expect("cleanup");
    }
}
