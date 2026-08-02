use std::{collections::BTreeMap, error::Error, fmt};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use yuyib_authoring::{
    AssetGuid, CapabilityId, ContentHash, ImportSettingsSchemaId, ProjectGuid, SceneGuid,
    SchemaVersion,
};

/// Stable project-manifest discriminator.
pub const PROJECT_FORMAT: &str = "yuyib.project";
/// Stable asset-metadata discriminator.
pub const ASSET_FORMAT: &str = "yuyib.asset";

/// High-level runtime composition authored by one project.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectProfile {
    /// Native desktop shell without a game schedule by default.
    Application,
    /// 2D game scene and schedule composition.
    Game2d,
    /// 3D game scene and schedule composition.
    Game3d,
}

/// Optional local-development commands used by the Editor host.
///
/// These values are project data, not arbitrary shell snippets. The Editor
/// still validates the Cargo package and confines the Play executable below
/// the project root before spawning either process.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProjectDevelopment {
    /// Cargo package accepted by the scoped `cargo check -p` action.
    pub cargo_package: Option<String>,
    /// Project-relative executable used by Play Mode after it has been built.
    pub play_executable: Option<String>,
    /// Literal process arguments passed to the Play executable without a shell.
    #[serde(default)]
    pub play_arguments: Vec<String>,
}

/// One project scene reference. Identity is independent from its file path.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProjectScene {
    /// Persistent scene identity.
    pub guid: SceneGuid,
    /// Project-relative authoring document path.
    pub path: String,
    /// Human-facing label.
    pub name: String,
    /// Future scene-reference fields preserved by older Editors.
    #[serde(flatten)]
    pub extensions: BTreeMap<String, Value>,
}

/// Versioned project entry point.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ProjectManifest {
    /// Stable project-manifest discriminator.
    pub format: String,
    /// Project-container schema version.
    pub format_version: SchemaVersion,
    /// Persistent identity independent from project-directory rename.
    pub project_guid: ProjectGuid,
    /// Human-facing project title.
    pub name: String,
    /// Default application/game composition.
    pub profile: ProjectProfile,
    /// Project-relative asset source root.
    pub asset_root: String,
    /// Project-relative Rust workspace or package root.
    pub code_root: String,
    /// Project scenes in deterministic palette order.
    pub scenes: Vec<ProjectScene>,
    /// Optional scene selected when Play starts.
    pub startup_scene: Option<SceneGuid>,
    /// Optional, non-shipping Editor process configuration.
    #[serde(default)]
    pub development: ProjectDevelopment,
    /// Forward-compatible project fields retained by older editors.
    #[serde(flatten)]
    pub extensions: BTreeMap<String, Value>,
}

impl ProjectManifest {
    /// Creates an empty version-one project.
    #[must_use]
    pub fn new(name: impl Into<String>, profile: ProjectProfile) -> Self {
        Self {
            format: PROJECT_FORMAT.to_owned(),
            format_version: SchemaVersion::INITIAL,
            project_guid: ProjectGuid::new(),
            name: name.into(),
            profile,
            asset_root: "assets".to_owned(),
            code_root: ".".to_owned(),
            scenes: Vec::new(),
            startup_scene: None,
            development: ProjectDevelopment::default(),
            extensions: BTreeMap::new(),
        }
    }

    /// Resolves the configured startup scene identity to its project-relative path.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectValidationError::MissingStartupScene`] when the selected
    /// identity is not declared by this manifest.
    pub fn startup_scene_path(&self) -> Result<Option<&str>, ProjectValidationError> {
        let Some(startup_scene) = self.startup_scene else {
            return Ok(None);
        };
        self.scenes
            .iter()
            .find(|scene| scene.guid == startup_scene)
            .map(|scene| Some(scene.path.as_str()))
            .ok_or(ProjectValidationError::MissingStartupScene(startup_scene))
    }

    /// Builds the Play process arguments, including the configured startup scene.
    ///
    /// # Errors
    ///
    /// Rejects conflicting user-provided scene arguments and an unresolved
    /// startup-scene identity.
    pub fn build_play_argv(&self) -> Result<Vec<String>, ProjectValidationError> {
        build_play_argv(self)
    }

    /// Validates persistent identities and project-relative paths.
    ///
    /// # Errors
    ///
    /// Rejects empty labels, duplicate scene identities or paths, invalid
    /// relative paths, and a startup scene absent from `scenes`.
    pub fn validate(&self) -> Result<(), ProjectValidationError> {
        if self.format != PROJECT_FORMAT {
            return Err(ProjectValidationError::UnsupportedFormat(
                self.format.clone(),
            ));
        }
        if self.name.trim().is_empty() {
            return Err(ProjectValidationError::EmptyProjectName);
        }
        validate_relative_path(&self.asset_root)?;
        validate_relative_path(&self.code_root)?;
        if let Some(package) = &self.development.cargo_package
            && (package.is_empty()
                || !package
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')))
        {
            return Err(ProjectValidationError::InvalidCargoPackage(package.clone()));
        }
        if let Some(executable) = &self.development.play_executable {
            validate_relative_path(executable)?;
        }
        if self
            .development
            .play_arguments
            .iter()
            .any(|argument| argument.len() > 4_096 || argument.chars().any(char::is_control))
        {
            return Err(ProjectValidationError::InvalidPlayArgument);
        }
        let mut identities = std::collections::HashSet::new();
        let mut paths = std::collections::HashSet::new();
        for scene in &self.scenes {
            if scene.name.trim().is_empty() {
                return Err(ProjectValidationError::EmptySceneName(scene.guid));
            }
            validate_relative_path(&scene.path)?;
            if !identities.insert(scene.guid) {
                return Err(ProjectValidationError::DuplicateSceneGuid(scene.guid));
            }
            let path_key = portable_path_key(&scene.path);
            if !paths.insert(path_key) {
                return Err(ProjectValidationError::DuplicateScenePath(
                    scene.path.clone(),
                ));
            }
        }
        if let Some(startup) = self.startup_scene
            && !identities.contains(&startup)
        {
            return Err(ProjectValidationError::MissingStartupScene(startup));
        }
        Ok(())
    }
}

/// Builds Play process arguments from manifest development settings.
///
/// The startup scene is supplied exclusively by this helper so the host cannot
/// accidentally start a scene different from the manifest selection.
///
/// # Errors
///
/// Rejects a user-provided `--scene` argument and unresolved startup scene.
pub fn build_play_argv(manifest: &ProjectManifest) -> Result<Vec<String>, ProjectValidationError> {
    if manifest
        .development
        .play_arguments
        .iter()
        .any(|argument| argument == "--scene" || argument.starts_with("--scene="))
    {
        return Err(ProjectValidationError::PlayArgumentsContainScene);
    }

    let mut argv = manifest.development.play_arguments.clone();
    if let Some(scene_path) = manifest.startup_scene_path()? {
        argv.push("--scene".to_owned());
        argv.push(scene_path.to_owned());
    }
    Ok(argv)
}

/// Opaque, versioned importer settings retained across editor versions.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ImportSettingsRecord {
    /// Stable importer-settings schema.
    pub schema: ImportSettingsSchemaId,
    /// Persisted settings schema version.
    pub version: SchemaVersion,
    /// Known or unknown settings payload.
    pub payload: Value,
    /// Future import-settings envelope fields preserved by older Editors.
    #[serde(flatten)]
    pub extensions: BTreeMap<String, Value>,
}

/// Project asset metadata. GUID identity never derives from path or content.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct AssetDocument {
    /// Stable asset-metadata discriminator.
    pub format: String,
    /// Asset-document container version.
    pub format_version: SchemaVersion,
    /// Persistent identity preserved across source rename and content edits.
    pub guid: AssetGuid,
    /// Project-relative source path or logical URI.
    pub source: String,
    /// Optional content hash used only for invalidation.
    pub content_hash: Option<ContentHash>,
    /// Stable importer capability used to produce the cooked representation.
    pub importer: CapabilityId,
    /// Importer implementation/schema version participating in cache invalidation.
    pub importer_version: SchemaVersion,
    /// Versioned importer settings, opaque to older editors.
    pub import_settings: ImportSettingsRecord,
    /// Optional imported/cooked dependency identities.
    pub dependencies: Vec<AssetGuid>,
    /// Forward-compatible metadata retained by older editors.
    #[serde(flatten)]
    pub extensions: BTreeMap<String, Value>,
}

/// Project manifest validation failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProjectValidationError {
    /// Project manifest discriminator is unsupported.
    UnsupportedFormat(String),
    /// Project title is blank.
    EmptyProjectName,
    /// A scene title is blank.
    EmptySceneName(SceneGuid),
    /// A path is absolute, empty, or contains a parent/root/prefix component.
    InvalidRelativePath(String),
    /// Persistent scene identity occurs more than once.
    DuplicateSceneGuid(SceneGuid),
    /// Two scene identities map to the same authoring path.
    DuplicateScenePath(String),
    /// Startup identity is not declared by this project.
    MissingStartupScene(SceneGuid),
    /// Cargo package contains characters outside its non-shell identifier subset.
    InvalidCargoPackage(String),
    /// A Play argument is unreasonably large or contains control characters.
    InvalidPlayArgument,
    /// Play arguments attempt to override the manifest startup scene.
    PlayArgumentsContainScene,
}

impl fmt::Display for ProjectValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedFormat(format) => {
                write!(formatter, "unsupported project format {format:?}")
            }
            Self::EmptyProjectName => formatter.write_str("project name must not be empty"),
            Self::EmptySceneName(guid) => write!(formatter, "scene {guid} has an empty name"),
            Self::InvalidRelativePath(path) => {
                write!(
                    formatter,
                    "project path must be confined and relative: {path}"
                )
            }
            Self::DuplicateSceneGuid(guid) => write!(formatter, "duplicate scene GUID {guid}"),
            Self::DuplicateScenePath(path) => write!(formatter, "duplicate scene path {path}"),
            Self::MissingStartupScene(guid) => {
                write!(formatter, "startup scene {guid} is not declared")
            }
            Self::InvalidCargoPackage(package) => {
                write!(formatter, "invalid scoped Cargo package {package:?}")
            }
            Self::InvalidPlayArgument => formatter.write_str(
                "Play arguments must contain at most 4096 bytes and no control characters",
            ),
            Self::PlayArgumentsContainScene => {
                formatter.write_str("Play arguments must not contain --scene")
            }
        }
    }
}

impl Error for ProjectValidationError {}

fn validate_relative_path(path: &str) -> Result<(), ProjectValidationError> {
    let candidate = std::path::Path::new(path);
    if path.is_empty()
        || candidate.is_absolute()
        || candidate.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
    {
        return Err(ProjectValidationError::InvalidRelativePath(path.to_owned()));
    }
    Ok(())
}

fn portable_path_key(path: &str) -> String {
    path.replace('\\', "/")
        .split('/')
        .filter(|segment| !segment.is_empty() && *segment != ".")
        .map(str::to_lowercase)
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_identity_is_independent_from_source_and_hash() {
        let guid = AssetGuid::new();
        let mut asset = AssetDocument {
            format: ASSET_FORMAT.to_owned(),
            format_version: SchemaVersion::new(1).expect("version"),
            guid,
            source: "models/hero.glb".to_owned(),
            content_hash: Some(ContentHash::new("blake3:01").expect("hash")),
            importer: CapabilityId::new("yuyib.gltf-import").expect("importer"),
            importer_version: SchemaVersion::new(1).expect("version"),
            import_settings: ImportSettingsRecord {
                schema: ImportSettingsSchemaId::new("yuyib.gltf-import").expect("schema"),
                version: SchemaVersion::new(1).expect("version"),
                payload: serde_json::json!({}),
                extensions: BTreeMap::new(),
            },
            dependencies: Vec::new(),
            extensions: BTreeMap::new(),
        };
        asset.source = "characters/hero-renamed.glb".to_owned();
        asset.content_hash = Some(ContentHash::new("blake3:02").expect("hash"));
        assert_eq!(asset.guid, guid);
    }

    #[test]
    fn manifest_rejects_duplicate_identity_and_path_traversal() {
        let guid = SceneGuid::new();
        let mut manifest = ProjectManifest::new("Editor test", ProjectProfile::Game3d);
        manifest.scenes = vec![
            ProjectScene {
                guid,
                path: "scenes/main.yscene".to_owned(),
                name: "Main".to_owned(),
                extensions: BTreeMap::new(),
            },
            ProjectScene {
                guid,
                path: "scenes/copy.yscene".to_owned(),
                name: "Copy".to_owned(),
                extensions: BTreeMap::new(),
            },
        ];
        assert_eq!(
            manifest.validate(),
            Err(ProjectValidationError::DuplicateSceneGuid(guid))
        );
        manifest.scenes.truncate(1);
        manifest.asset_root = "../outside".to_owned();
        assert!(matches!(
            manifest.validate(),
            Err(ProjectValidationError::InvalidRelativePath(_))
        ));
    }

    #[test]
    fn manifest_rejects_portable_path_aliases() {
        let mut manifest = ProjectManifest::new("Portable", ProjectProfile::Game3d);
        manifest.scenes = vec![
            ProjectScene {
                guid: SceneGuid::new(),
                path: "Scenes/./Main.yscene".to_owned(),
                name: "Main".to_owned(),
                extensions: BTreeMap::new(),
            },
            ProjectScene {
                guid: SceneGuid::new(),
                path: "scenes\\main.yscene".to_owned(),
                name: "Alias".to_owned(),
                extensions: BTreeMap::new(),
            },
        ];
        assert!(matches!(
            manifest.validate(),
            Err(ProjectValidationError::DuplicateScenePath(_))
        ));
    }

    #[test]
    fn development_profile_is_optional_but_never_a_shell_command() {
        let legacy = serde_json::json!({
            "format": PROJECT_FORMAT,
            "format_version": 1,
            "project_guid": ProjectGuid::new(),
            "name": "Legacy",
            "profile": "game3d",
            "asset_root": "assets",
            "code_root": ".",
            "scenes": [],
            "startup_scene": null
        });
        let parsed: ProjectManifest = serde_json::from_value(legacy).expect("legacy manifest");
        assert_eq!(parsed.development, ProjectDevelopment::default());

        let mut manifest = ProjectManifest::new("Unsafe", ProjectProfile::Game3d);
        manifest.development.cargo_package = Some("game --workspace".to_owned());
        assert!(matches!(
            manifest.validate(),
            Err(ProjectValidationError::InvalidCargoPackage(_))
        ));
    }

    #[test]
    fn startup_scene_path_and_play_argv_resolve_manifest_selection() {
        let scene_guid = SceneGuid::new();
        let mut manifest = ProjectManifest::new("Playable", ProjectProfile::Game3d);
        manifest.scenes.push(ProjectScene {
            guid: scene_guid,
            path: "scenes/main.yscene".to_owned(),
            name: "Main".to_owned(),
            extensions: BTreeMap::new(),
        });
        manifest.startup_scene = Some(scene_guid);
        manifest.development.play_arguments = vec!["--fullscreen".to_owned()];

        assert_eq!(
            manifest.startup_scene_path(),
            Ok(Some("scenes/main.yscene"))
        );
        assert_eq!(
            manifest.build_play_argv(),
            Ok(vec![
                "--fullscreen".to_owned(),
                "--scene".to_owned(),
                "scenes/main.yscene".to_owned(),
            ])
        );
    }

    #[test]
    fn play_argv_rejects_scene_override_and_missing_startup_scene() {
        let mut manifest = ProjectManifest::new("Playable", ProjectProfile::Game3d);
        manifest.development.play_arguments = vec!["--scene=other.yscene".to_owned()];
        assert_eq!(
            manifest.build_play_argv(),
            Err(ProjectValidationError::PlayArgumentsContainScene)
        );

        let missing = SceneGuid::new();
        manifest.development.play_arguments.clear();
        manifest.startup_scene = Some(missing);
        assert_eq!(
            manifest.startup_scene_path(),
            Err(ProjectValidationError::MissingStartupScene(missing))
        );
    }
}
