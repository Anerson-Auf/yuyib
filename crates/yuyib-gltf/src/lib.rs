//! Static glTF 2.0 import for [`yuyib_model::Model`].
//!
//! The importer accepts textual `.gltf` and binary `.glb` documents, resolves
//! local external buffers relative to the document, and supports data-URI and
//! GLB binary buffers. It imports triangle primitives with `POSITION`, optional
//! `NORMAL`, `TANGENT` and `TEXCOORD_0`, plus basic metallic-roughness material
//! factors, referenced image URIs and encoded image buffer views.
//!
//! The default strict policy rejects rather than silently drops non-triangle
//! primitive modes, sparse accessors, morph targets, skins, animations, Draco
//! compression, data-URI images, and texture references
//! that do not use UV set zero. [`ImportPolicy::StaticPreview`] is an explicit
//! opt-in for a static preview: it drops skinning data, extra UV sets and
//! animations, while retaining the same geometry, material and resource limits.
//! [`ImportPolicy::SkeletalPreview`] additionally permits a model preview to
//! skip non-triangle helper primitives, and records every skipped primitive in
//! an [`ImportReport`].
//! Node transforms are not baked: each source mesh becomes one
//! [`yuyib_model::Mesh`]. Both glTF TRS and finite affine matrix transforms are
//! preserved exactly in the imported scene graph.

#![forbid(unsafe_code)]

mod cook;
mod mixer;

pub use cook::{
    GLTF_IMPORTED_COOK_SCHEMA, GLTF_IMPORTED_COOKER_ID, cook_key_for_gltf_source,
    decode_imported_asset, dependency_fingerprints_match, encode_imported_asset,
    fingerprint_gltf_dependencies, gltf_imported_cooker_identity, import_options_fingerprint,
    import_scene_bytes_cached, import_scene_bytes_cached_at,
};
pub use mixer::{
    AnimationCrossFadeChange, AnimationCrossFadeDuration, AnimationCrossFadeDurationError,
    AnimationCrossFadeError, AnimationCrossFadeMixer, blend_animation_snapshots,
};

use std::{
    collections::HashSet,
    error::Error,
    fmt, fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use serde::{Deserialize, Serialize};
use yuyib_assets::{
    AssetImporter, ImportDependency, ImportDependencyKind, ImportDiagnostic,
    ImportDiagnosticSeverity, ImportMatch, ImportProbe, ImportSource, ImporterDescriptor,
    ImporterOutput,
};
use yuyib_model::{
    AlphaMode, Material, MaterialIndex, Mesh, MeshPrimitive, Model, ModelTexture,
    ModelTextureAddressMode, ModelTextureIndex, ModelTextureMagFilter, ModelTextureMinFilter,
    ModelTextureSampler, NormalTextureBinding, SpecularGlossinessMaterial, TextureBinding,
};

/// Limits applied before importing untrusted glTF assets.
///
/// A limit is deliberately applied to the aggregate decoded buffer data and
/// to the final geometry counts. It does not claim to sandbox the JSON parser.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ImportLimits {
    /// Maximum total bytes of all decoded buffer payloads.
    pub max_buffer_bytes: usize,
    /// Maximum position vertices over the complete model.
    pub max_vertices: usize,
    /// Maximum indices over the complete model.
    pub max_indices: usize,
    /// Maximum aggregate encoded bytes copied from GLB image buffer views.
    pub max_embedded_image_bytes: usize,
    /// Maximum joints across all imported skins.
    pub max_skin_joints: usize,
    /// Maximum keyframes across all imported animation channels.
    pub max_animation_keyframes: usize,
}

impl Default for ImportLimits {
    fn default() -> Self {
        Self {
            max_buffer_bytes: 256 * 1024 * 1024,
            max_vertices: 16 * 1024 * 1024,
            max_indices: 48 * 1024 * 1024,
            max_embedded_image_bytes: 256 * 1024 * 1024,
            max_skin_joints: 16 * 1024,
            max_animation_keyframes: 4 * 1024 * 1024,
        }
    }
}

/// Controls limits and the accepted glTF subset.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ImportOptions {
    /// Resource and geometry limits.
    pub limits: ImportLimits,
    /// Explicit handling policy for optional source data.
    pub policy: ImportPolicy,
}

impl ImportOptions {
    /// Creates the opt-in static-preview configuration with default limits.
    ///
    /// This is intended for inspecting a model that carries rigging data but
    /// does not need to be animated. The imported mesh is its source bind pose;
    /// no skeleton deformation or animation is applied.
    #[must_use]
    pub fn static_preview() -> Self {
        Self {
            policy: ImportPolicy::StaticPreview,
            ..Self::default()
        }
    }

    /// Creates the opt-in rigged-model configuration with default limits.
    ///
    /// It imports one four-joint skin set and translation/rotation/scale
    /// animation tracks. Rendering remains a separate concern; callers obtain
    /// CPU joint palette matrices from [`AnimationPlayer`].
    #[must_use]
    pub fn skeletal() -> Self {
        Self {
            policy: ImportPolicy::Skeletal,
            ..Self::default()
        }
    }

    /// Creates the high-level configuration for previewing a rigged model.
    ///
    /// It has the same skeletal contract as [`Self::skeletal`], but explicitly
    /// omits `LINES`, `LINE_STRIP`, `LINE_LOOP` and `POINTS` primitives. Those
    /// primitives cannot be represented by the triangle-only `Model` API.
    /// Nothing is dropped silently: [`ImportedAsset::report`] lists the source
    /// mesh, primitive and topology for each omission.
    #[must_use]
    pub fn skeletal_preview() -> Self {
        Self {
            policy: ImportPolicy::SkeletalPreview,
            ..Self::default()
        }
    }

    /// Replaces only the source-data handling policy.
    ///
    /// This low-level form is useful when callers also set custom
    /// [`ImportLimits`] using a struct literal.
    #[must_use]
    pub const fn with_policy(mut self, policy: ImportPolicy) -> Self {
        self.policy = policy;
        self
    }
}

/// Registry adapter for self-contained GLB or data-URI glTF scene assets.
///
/// External files are deliberately not opened by this plugin. A host resolver
/// must supply a future dependency-aware source bundle or use the explicit
/// path-based glTF APIs under its own filesystem policy.
#[derive(Clone, Copy, Debug, Default)]
pub struct GltfAssetImporter {
    options: ImportOptions,
}

impl GltfAssetImporter {
    /// Creates a registry plugin with explicit glTF limits and import policy.
    #[must_use]
    pub const fn new(options: ImportOptions) -> Self {
        Self { options }
    }

    /// Returns this plugin's glTF import configuration.
    #[must_use]
    pub const fn options(self) -> ImportOptions {
        self.options
    }
}

impl AssetImporter<ImportedAsset> for GltfAssetImporter {
    type Error = ImportError;

    fn descriptor(&self) -> ImporterDescriptor {
        ImporterDescriptor::new("yuyib.gltf", env!("CARGO_PKG_VERSION"))
            .with_extension("glb")
            .with_extension("gltf")
            .with_media_type("model/gltf-binary")
            .with_media_type("model/gltf+json")
    }

    fn probe(&self, probe: ImportProbe<'_>) -> ImportMatch {
        if probe.prefix.starts_with(b"glTF") {
            return ImportMatch::Exact;
        }
        let first = probe
            .prefix
            .iter()
            .copied()
            .find(|byte| !byte.is_ascii_whitespace());
        if first == Some(b'{') && probe.extension == Some("gltf") {
            ImportMatch::Preferred
        } else if matches!(probe.extension, Some("gltf" | "glb")) {
            ImportMatch::Possible
        } else {
            ImportMatch::Unsupported
        }
    }

    fn import(
        &self,
        source: ImportSource<'_>,
    ) -> Result<ImporterOutput<ImportedAsset>, Self::Error> {
        let dependencies = discover_external_dependencies(source.bytes())?;
        let asset = import_scene_bytes_embedded(source.bytes(), self.options)?;
        let skipped = asset.report().skipped_primitive_count();
        let mut output = ImporterOutput::new(asset);
        output.dependencies = dependencies;
        output.cpu_bytes = u64::try_from(source.bytes().len()).ok();
        if skipped > 0 {
            output.diagnostics.push(ImportDiagnostic {
                code: "gltf-skipped-primitives".to_owned(),
                message: format!("preview policy skipped {skipped} non-triangle primitives"),
                severity: ImportDiagnosticSeverity::Warning,
            });
        }
        push_model_material_diagnostics(&output.asset.model, &mut output.diagnostics);
        push_model_texture_diagnostics(&output.asset.model, &mut output.diagnostics);
        Ok(output)
    }
}

fn push_model_texture_diagnostics(model: &Model, diagnostics: &mut Vec<ImportDiagnostic>) {
    let usage = model.texture_usage();
    for entry in usage.textures() {
        let label = entry
            .label
            .as_deref()
            .map_or_else(|| format!("#{}", entry.index.get()), ToOwned::to_owned);
        if entry.label.is_none() {
            diagnostics.push(ImportDiagnostic {
                code: "gltf-unnamed-texture".to_owned(),
                message: format!("texture {} has no source label", entry.index.get()),
                severity: ImportDiagnosticSeverity::Info,
            });
        }
        if entry.external {
            diagnostics.push(ImportDiagnostic {
                code: "gltf-external-texture-uri".to_owned(),
                message: format!(
                    "texture `{label}` uses an external URI; bytes are not embedded in the document"
                ),
                severity: ImportDiagnosticSeverity::Info,
            });
        }
        if entry.empty_embedded {
            diagnostics.push(ImportDiagnostic {
                code: "gltf-empty-embedded-texture".to_owned(),
                message: format!("texture `{label}` has an empty embedded image blob"),
                severity: ImportDiagnosticSeverity::Warning,
            });
        }
        if entry.material_users.is_empty() {
            diagnostics.push(ImportDiagnostic {
                code: "gltf-unused-texture".to_owned(),
                message: format!("texture `{label}` is not referenced by any material"),
                severity: ImportDiagnosticSeverity::Info,
            });
        }
    }

    for issue in usage.missing_uv_bindings() {
        diagnostics.push(ImportDiagnostic {
            code: "gltf-missing-uv-set".to_owned(),
            message: format!(
                "mesh {} primitive {} material #{} binds texture {} to TEXCOORD_{}, which is absent on the primitive",
                issue.primitive.mesh,
                issue.primitive.primitive,
                issue.material_index,
                issue.texture.get(),
                issue.tex_coord_set
            ),
            severity: ImportDiagnosticSeverity::Warning,
        });
        if issue.tex_coord_set != 0 {
            diagnostics.push(ImportDiagnostic {
                code: "gltf-texcoord-set-nonzero".to_owned(),
                message: format!(
                    "mesh {} primitive {} material #{} uses TEXCOORD_{} (non-zero); some high-level paths only sample UV0",
                    issue.primitive.mesh,
                    issue.primitive.primitive,
                    issue.material_index,
                    issue.tex_coord_set
                ),
                severity: ImportDiagnosticSeverity::Warning,
            });
        }
    }

    for (material_index, material) in model.materials().iter().enumerate() {
        let mut bindings = Vec::new();
        if let Some(binding) = material.base_color_texture() {
            bindings.push(binding);
        }
        if let Some(normal) = material.normal_texture() {
            bindings.push(normal.binding());
        }
        if let Some(binding) = material.metallic_roughness_texture() {
            bindings.push(binding);
        }
        if let Some(binding) = material.emissive_texture() {
            bindings.push(binding);
        }
        if let Some(workflow) = material.specular_glossiness() {
            if let Some(binding) = workflow.diffuse_texture() {
                bindings.push(binding);
            }
            if let Some(binding) = workflow.specular_glossiness_texture() {
                bindings.push(binding);
            }
        }
        for binding in bindings {
            if binding.tex_coord_set() == 0 {
                continue;
            }
            let already = usage.missing_uv_bindings().iter().any(|issue| {
                issue.material_index == material_index
                    && issue.texture == binding.texture()
                    && issue.tex_coord_set == binding.tex_coord_set()
            });
            if already {
                continue;
            }
            diagnostics.push(ImportDiagnostic {
                code: "gltf-texcoord-set-nonzero".to_owned(),
                message: format!(
                    "material #{material_index} binds texture {} to TEXCOORD_{}",
                    binding.texture().get(),
                    binding.tex_coord_set()
                ),
                severity: ImportDiagnosticSeverity::Info,
            });
        }
    }
}

fn push_model_material_diagnostics(model: &Model, diagnostics: &mut Vec<ImportDiagnostic>) {
    for (index, material) in model.materials().iter().enumerate() {
        let label = material
            .name()
            .map_or_else(|| format!("#{index}"), ToOwned::to_owned);
        if material.name().is_none() {
            diagnostics.push(ImportDiagnostic {
                code: "gltf-unnamed-material".to_owned(),
                message: format!("material {index} has no source name"),
                severity: ImportDiagnosticSeverity::Info,
            });
        }
        let has_texture = material.base_color_texture().is_some()
            || material.normal_texture().is_some()
            || material.metallic_roughness_texture().is_some()
            || material.emissive_texture().is_some()
            || material.specular_glossiness().is_some_and(|workflow| {
                workflow.diffuse_texture().is_some()
                    || workflow.specular_glossiness_texture().is_some()
            });
        if !has_texture {
            diagnostics.push(ImportDiagnostic {
                code: "gltf-factor-only-material".to_owned(),
                message: format!(
                    "material `{label}` has no texture maps; only factors/emissive are available"
                ),
                severity: ImportDiagnosticSeverity::Info,
            });
        }
    }

    for (mesh, mesh_value) in model.meshes().iter().enumerate() {
        let mesh_label = mesh_value
            .name()
            .map_or_else(|| format!("#{mesh}"), ToOwned::to_owned);
        if mesh_value.primitives().is_empty() {
            diagnostics.push(ImportDiagnostic {
                code: "gltf-empty-mesh".to_owned(),
                message: format!("mesh `{mesh_label}` has no primitives"),
                severity: ImportDiagnosticSeverity::Warning,
            });
            continue;
        }
        for (primitive, primitive_value) in mesh_value.primitives().iter().enumerate() {
            if primitive_value.normals().is_none() {
                diagnostics.push(ImportDiagnostic {
                    code: "gltf-missing-normals".to_owned(),
                    message: format!(
                        "mesh `{mesh_label}` primitive {primitive} has no NORMAL attribute"
                    ),
                    severity: ImportDiagnosticSeverity::Info,
                });
            }
            if (0u8..8).all(|set| primitive_value.tex_coords(set).is_none()) {
                diagnostics.push(ImportDiagnostic {
                    code: "gltf-missing-uv0".to_owned(),
                    message: format!(
                        "mesh `{mesh_label}` primitive {primitive} has no TEXCOORD attributes"
                    ),
                    severity: ImportDiagnosticSeverity::Info,
                });
            }
            match primitive_value.material() {
                None => diagnostics.push(ImportDiagnostic {
                    code: "gltf-unbound-material".to_owned(),
                    message: format!(
                        "mesh `{mesh_label}` primitive {primitive} has no material binding"
                    ),
                    severity: ImportDiagnosticSeverity::Warning,
                }),
                Some(material) if material.get() >= model.materials().len() => {
                    diagnostics.push(ImportDiagnostic {
                        code: "gltf-invalid-material-index".to_owned(),
                        message: format!(
                            "mesh `{mesh_label}` primitive {primitive} references missing material {}",
                            material.get()
                        ),
                        severity: ImportDiagnosticSeverity::Warning,
                    });
                }
                Some(_) => {}
            }
        }
    }
}

/// Selects how the importer handles source data that a static mesh does not need.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum ImportPolicy {
    /// Preserve the normal import contract and reject unsupported source data.
    #[default]
    Strict,
    /// Import only a static preview of a rigged source asset.
    ///
    /// `JOINTS_0`, `WEIGHTS_0`, skin definitions and animations are
    /// intentionally ignored. Authored `TEXCOORD_0` through `TEXCOORD_7` and
    /// material bindings retain their original set numbers. The result keeps
    /// the bind-pose mesh and ordinary node transforms, but contains no
    /// animation or skeleton data. Other attributes, including vertex colours,
    /// morph targets and additional joint/weight sets, still fail explicitly.
    StaticPreview,
    /// Import a constrained skeleton and transform animations.
    ///
    /// This permits `JOINTS_0`, `WEIGHTS_0`, skins and translation, rotation or
    /// scale tracks. UV sets through `TEXCOORD_7` are retained. It still
    /// rejects morph targets, extra joint/weight sets, cubic-spline tracks and
    /// animated matrix nodes.
    Skeletal,
    /// Import a constrained skeleton for an interactive model preview.
    ///
    /// This has the same skinning and animation support as [`Self::Skeletal`],
    /// but intentionally skips non-triangle helper geometry. The caller must
    /// inspect [`ImportedAsset::report`] if it needs to know what did not make
    /// it into the triangle renderer. Position morph targets and linear/step
    /// morph-weight animation are retained for preview rendering alongside
    /// skeletal TRS tracks. Morph normal/tangent deltas are currently ignored
    /// by the unlit character path. Vertex colours and unrelated attributes
    /// still fail.
    SkeletalPreview,
}

/// A source primitive deliberately omitted by an opt-in preview import.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SkippedPrimitive {
    mesh: usize,
    primitive: usize,
    mode: gltf::mesh::Mode,
}

impl SkippedPrimitive {
    /// Returns the zero-based source mesh index.
    #[must_use]
    pub const fn mesh(self) -> usize {
        self.mesh
    }

    /// Returns the zero-based primitive index inside [`Self::mesh`].
    #[must_use]
    pub const fn primitive(self) -> usize {
        self.primitive
    }

    /// Returns the topology which could not enter the triangle renderer.
    #[must_use]
    pub const fn mode(self) -> gltf::mesh::Mode {
        self.mode
    }
}

/// Explicit account of source data omitted by an opt-in preview policy.
///
/// Strict and regular skeletal imports always return an empty report or fail.
/// A non-empty report means that the triangle scene is usable, but some source
/// helper geometry such as a Blender line or point cloud was intentionally not
/// imported.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ImportReport {
    primitives: Vec<SkippedPrimitive>,
}

impl ImportReport {
    /// Returns every source primitive intentionally omitted by this import.
    #[must_use]
    pub fn skipped_primitives(&self) -> &[SkippedPrimitive] {
        &self.primitives
    }

    /// Returns how many non-triangle primitives the preview omitted.
    #[must_use]
    pub fn skipped_primitive_count(&self) -> usize {
        self.primitives.len()
    }

    /// Returns true when every source primitive entered the triangle model.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.primitives.is_empty()
    }
}

/// Imports one `.gltf` or `.glb` file into renderer-neutral model data.
///
/// External buffer paths must remain inside the directory containing `path`.
/// This prevents a model from reading arbitrary parent paths. Texture image
/// URIs are preserved as source-relative strings; image bytes are not decoded
/// by this crate.
///
/// # Errors
///
/// Returns [`ImportError`] for I/O, malformed glTF, unsupported features,
/// out-of-budget data or invalid resulting model geometry.
pub fn import_path(path: impl AsRef<Path>) -> Result<Model, ImportError> {
    import_path_with_options(path, ImportOptions::default())
}

/// Imports one `.gltf` or `.glb` file using explicit resource limits.
///
/// # Errors
///
/// Returns [`ImportError`] when parsing, resource resolution or model
/// conversion fails.
pub fn import_path_with_options(
    path: impl AsRef<Path>,
    options: ImportOptions,
) -> Result<Model, ImportError> {
    let path = path.as_ref();
    let bytes = fs::read(path).map_err(|source| ImportError::ReadDocument {
        path: path.to_owned(),
        source,
    })?;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    import_bytes_with_base_path(&bytes, parent, options)
}

/// Imports `.gltf` or `.glb` bytes with an explicit base directory for external buffers.
///
/// The base directory is used only for `buffers[*].uri`; no URI is fetched
/// from the network.
///
/// # Errors
///
/// Returns [`ImportError`] for malformed glTF, I/O, unsupported features or
/// model validation failures.
pub fn import_bytes_with_base_path(
    bytes: &[u8],
    base_path: impl AsRef<Path>,
    options: ImportOptions,
) -> Result<Model, ImportError> {
    let gltf = gltf::Gltf::from_slice(bytes).map_err(ImportError::Parse)?;
    validate_document(&gltf.document, options.policy)?;
    let buffers = load_buffers(&gltf, base_path.as_ref(), options.limits)?;
    Ok(convert_document(&gltf.document, &buffers, options)?.model)
}

/// Imports a model together with its scene graph from one glTF file.
///
/// Unlike [`import_path`], this entry point preserves every node's local TRS
/// or affine matrix, child links, mesh references and camera/directional-light metadata. It
/// retains all source scenes rather than choosing and flattening the default
/// scene.
///
/// # Errors
///
/// Returns [`ImportError`] for the model failures described by [`import_path`]
/// and for unsupported punctual-light kinds or invalid local transforms.
pub fn import_scene_path(path: impl AsRef<Path>) -> Result<ImportedAsset, ImportError> {
    import_scene_path_with_options(path, ImportOptions::default())
}

/// Imports a model and scene graph using explicit resource limits.
///
/// # Errors
///
/// Returns [`ImportError`] when parsing, resource resolution or asset
/// conversion fails.
pub fn import_scene_path_with_options(
    path: impl AsRef<Path>,
    options: ImportOptions,
) -> Result<ImportedAsset, ImportError> {
    let path = path.as_ref();
    let bytes = fs::read(path).map_err(|source| ImportError::ReadDocument {
        path: path.to_owned(),
        source,
    })?;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    import_scene_bytes_with_base_path(&bytes, parent, options)
}

/// Imports glTF bytes and their scene graph using `base_path` for buffers.
///
/// No node transform is baked into mesh vertices. Nodes expressed with a glTF
/// matrix retain its exact column-major affine representation instead of being
/// heuristically decomposed into TRS.
///
/// # Errors
///
/// Returns [`ImportError`] for malformed, unsupported or out-of-budget data.
pub fn import_scene_bytes_with_base_path(
    bytes: &[u8],
    base_path: impl AsRef<Path>,
    options: ImportOptions,
) -> Result<ImportedAsset, ImportError> {
    let gltf = gltf::Gltf::from_slice(bytes).map_err(ImportError::Parse)?;
    validate_document(&gltf.document, options.policy)?;
    let buffers = load_buffers(&gltf, base_path.as_ref(), options.limits)?;
    let converted = convert_document(&gltf.document, &buffers, options)?;
    let scene = convert_scene(&gltf.document, &buffers, options, &converted.mesh_mapping)?;
    Ok(ImportedAsset {
        model: converted.model,
        scene,
        report: converted.report,
    })
}

/// Imports a self-contained GLB or data-URI glTF without filesystem access.
///
/// This is the safe byte-only entry point used by [`GltfAssetImporter`]. A
/// non-data external buffer URI returns [`ImportError::ExternalBufferRequiresResolver`]
/// instead of being opened relative to the process working directory.
///
/// # Errors
///
/// Returns [`ImportError`] for malformed, unsupported, external-dependent or
/// out-of-budget data.
pub fn import_scene_bytes_embedded(
    bytes: &[u8],
    options: ImportOptions,
) -> Result<ImportedAsset, ImportError> {
    let gltf = gltf::Gltf::from_slice(bytes).map_err(ImportError::Parse)?;
    validate_document(&gltf.document, options.policy)?;
    let buffers = load_embedded_buffers(&gltf, options.limits)?;
    let converted = convert_document(&gltf.document, &buffers, options)?;
    let scene = convert_scene(&gltf.document, &buffers, options, &converted.mesh_mapping)?;
    Ok(ImportedAsset {
        model: converted.model,
        scene,
        report: converted.report,
    })
}

/// Lists external buffer/image URIs without opening those files.
///
/// `data:` URIs are ignored (embedded). Buffer URIs are [`ImportDependencyKind::Required`];
/// image URIs are [`ImportDependencyKind::Optional`]. Duplicate URIs keep the first
/// (stronger) kind in document order.
///
/// # Errors
///
/// Returns [`ImportError::Parse`] when the glTF/GLB container cannot be parsed.
pub fn discover_external_dependencies(bytes: &[u8]) -> Result<Vec<ImportDependency>, ImportError> {
    let gltf = gltf::Gltf::from_slice(bytes).map_err(ImportError::Parse)?;
    let mut dependencies = Vec::new();
    let mut seen = HashSet::new();

    for buffer in gltf.buffers() {
        let gltf::buffer::Source::Uri(uri) = buffer.source() else {
            continue;
        };
        if uri.starts_with("data:") || !seen.insert(uri.to_owned()) {
            continue;
        }
        dependencies.push(ImportDependency {
            uri: uri.to_owned(),
            kind: ImportDependencyKind::Required,
        });
    }

    for image in gltf.images() {
        let gltf::image::Source::Uri { uri, .. } = image.source() else {
            continue;
        };
        if uri.starts_with("data:") || !seen.insert(uri.to_owned()) {
            continue;
        }
        dependencies.push(ImportDependency {
            uri: uri.to_owned(),
            kind: ImportDependencyKind::Optional,
        });
    }

    Ok(dependencies)
}

/// A loaded model and its renderer-neutral scene graph.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ImportedAsset {
    /// Static mesh and material data.
    pub model: Model,
    /// Scene hierarchy and node instances that reference model meshes.
    pub scene: ImportedScene,
    /// Explicit omissions made by an opt-in preview policy.
    #[serde(skip)]
    pub report: ImportReport,
}

impl ImportedAsset {
    /// Returns the explicit report of source primitives omitted for preview.
    #[must_use]
    pub const fn report(&self) -> &ImportReport {
        &self.report
    }
}

/// Scene graph metadata from one glTF document.
///
/// glTF permits several root scenes. They are all retained in source order;
/// [`Self::default_scene`] only identifies the author-selected scene, if one
/// exists. Node indices refer to [`Self::nodes`] and mesh indices refer to the
/// associated [`Model::meshes`].
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ImportedScene {
    scenes: Vec<ImportedRootScene>,
    default_scene: Option<SceneIndex>,
    nodes: Vec<ImportedNode>,
    cameras: Vec<ImportedCamera>,
    directional_lights: Vec<ImportedDirectionalLight>,
    skins: Vec<ImportedSkin>,
    animations: Vec<ImportedAnimationClip>,
    skinned_primitives: Vec<ImportedSkinnedPrimitive>,
    morph_primitives: Vec<ImportedMorphPrimitive>,
}

impl ImportedScene {
    /// Returns every declared root scene in source order.
    #[must_use]
    pub fn scenes(&self) -> &[ImportedRootScene] {
        &self.scenes
    }

    /// Returns the selected default source scene, if the document declared one.
    #[must_use]
    pub const fn default_scene(&self) -> Option<SceneIndex> {
        self.default_scene
    }

    /// Returns all nodes indexed by [`NodeIndex`], including unreferenced nodes.
    #[must_use]
    pub fn nodes(&self) -> &[ImportedNode] {
        &self.nodes
    }

    /// Returns cameras indexed by [`CameraIndex`].
    #[must_use]
    pub fn cameras(&self) -> &[ImportedCamera] {
        &self.cameras
    }

    /// Returns directional lights indexed by [`DirectionalLightIndex`].
    #[must_use]
    pub fn directional_lights(&self) -> &[ImportedDirectionalLight] {
        &self.directional_lights
    }

    /// Returns imported skeleton definitions.
    #[must_use]
    pub fn skins(&self) -> &[ImportedSkin] {
        &self.skins
    }

    /// Returns imported animation clips.
    #[must_use]
    pub fn animations(&self) -> &[ImportedAnimationClip] {
        &self.animations
    }

    /// Returns per-vertex skinning data for mesh primitives.
    #[must_use]
    pub fn skinned_primitives(&self) -> &[ImportedSkinnedPrimitive] {
        &self.skinned_primitives
    }

    /// Returns position-delta morph targets retained for preview animation.
    #[must_use]
    pub fn morph_primitives(&self) -> &[ImportedMorphPrimitive] {
        &self.morph_primitives
    }
}

/// A zero-based index into [`ImportedScene::scenes`].
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct SceneIndex(usize);

impl SceneIndex {
    /// Creates an index from a validated zero-based source position.
    #[must_use]
    pub const fn new(index: usize) -> Self {
        Self(index)
    }

    /// Returns the zero-based source position.
    #[must_use]
    pub const fn get(self) -> usize {
        self.0
    }
}

/// A zero-based index into [`ImportedScene::nodes`].
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct NodeIndex(usize);

impl NodeIndex {
    /// Creates an index from a validated zero-based source position.
    #[must_use]
    pub const fn new(index: usize) -> Self {
        Self(index)
    }

    /// Returns the zero-based source position.
    #[must_use]
    pub const fn get(self) -> usize {
        self.0
    }
}

/// A zero-based index into [`ImportedScene::cameras`].
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct CameraIndex(usize);

impl CameraIndex {
    /// Creates an index from a validated zero-based source position.
    #[must_use]
    pub const fn new(index: usize) -> Self {
        Self(index)
    }

    /// Returns the zero-based source position.
    #[must_use]
    pub const fn get(self) -> usize {
        self.0
    }
}

/// A zero-based index into [`ImportedScene::directional_lights`].
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct DirectionalLightIndex(usize);

impl DirectionalLightIndex {
    /// Creates an index from a validated zero-based source position.
    #[must_use]
    pub const fn new(index: usize) -> Self {
        Self(index)
    }

    /// Returns the zero-based source position.
    #[must_use]
    pub const fn get(self) -> usize {
        self.0
    }
}

/// A zero-based index into [`ImportedScene::skins`].
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct SkinIndex(usize);

impl SkinIndex {
    /// Creates an index from a validated zero-based source position.
    #[must_use]
    pub const fn new(index: usize) -> Self {
        Self(index)
    }

    /// Returns the zero-based source position.
    #[must_use]
    pub const fn get(self) -> usize {
        self.0
    }
}

/// A zero-based index into [`ImportedScene::animations`].
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct AnimationClipIndex(usize);

impl AnimationClipIndex {
    /// Creates an index from a validated zero-based source position.
    #[must_use]
    pub const fn new(index: usize) -> Self {
        Self(index)
    }

    /// Returns the zero-based source position.
    #[must_use]
    pub const fn get(self) -> usize {
        self.0
    }
}

/// One glTF root scene.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ImportedRootScene {
    name: Option<String>,
    roots: Vec<NodeIndex>,
}

impl ImportedRootScene {
    /// Returns the optional source/debug name.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// Returns top-level node references in source order.
    #[must_use]
    pub fn roots(&self) -> &[NodeIndex] {
        &self.roots
    }
}

/// One node instance preserving its local transform and source links.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ImportedNode {
    name: Option<String>,
    local_transform: LocalTransform,
    mesh: Option<usize>,
    camera: Option<CameraIndex>,
    directional_light: Option<DirectionalLightIndex>,
    skin: Option<SkinIndex>,
    morph_weights: Vec<f32>,
    children: Vec<NodeIndex>,
}

impl ImportedNode {
    /// Returns the optional source/debug name.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// Returns the exact source local transform.
    #[must_use]
    pub const fn local_transform(&self) -> LocalTransform {
        self.local_transform
    }

    /// Returns the source mesh index into the associated [`Model::meshes`].
    #[must_use]
    pub const fn mesh(&self) -> Option<usize> {
        self.mesh
    }

    /// Returns the optional source camera.
    #[must_use]
    pub const fn camera(&self) -> Option<CameraIndex> {
        self.camera
    }

    /// Returns the optional source directional light.
    #[must_use]
    pub const fn directional_light(&self) -> Option<DirectionalLightIndex> {
        self.directional_light
    }

    /// Returns the skeleton bound to this mesh node, if skeletal import was enabled.
    #[must_use]
    pub const fn skin(&self) -> Option<SkinIndex> {
        self.skin
    }

    /// Returns initial morph weights inherited from the node or its mesh.
    #[must_use]
    pub fn morph_weights(&self) -> &[f32] {
        &self.morph_weights
    }

    /// Returns child node references in source order.
    #[must_use]
    pub fn children(&self) -> &[NodeIndex] {
        &self.children
    }
}

/// Exact local transform from a glTF node.
///
/// Matrices retain their column-major source representation. Importers must
/// not heuristically decompose them: a matrix may contain shear that cannot
/// be represented by a translation, quaternion and per-axis scale.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum LocalTransform {
    /// Translation, unit quaternion rotation and per-axis scale.
    Trs {
        /// Local translation in glTF coordinate units.
        translation: [f32; 3],
        /// Local quaternion rotation in [x, y, z, w] order.
        rotation: [f32; 4],
        /// Local per-axis scale.
        scale: [f32; 3],
    },
    /// A finite affine matrix in glTF/WGPU column-major order.
    Matrix {
        /// All sixteen source matrix elements, without decomposition.
        column_major: [f32; 16],
    },
}

/// One skeleton definition copied from a glTF skin.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ImportedSkin {
    name: Option<String>,
    skeleton_root: Option<NodeIndex>,
    joints: Vec<NodeIndex>,
    inverse_bind_matrices: Vec<[f32; 16]>,
}

impl ImportedSkin {
    /// Returns the optional source/debug name.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// Returns the optional declared skeleton root.
    #[must_use]
    pub const fn skeleton_root(&self) -> Option<NodeIndex> {
        self.skeleton_root
    }

    /// Returns joint nodes in palette order.
    #[must_use]
    pub fn joints(&self) -> &[NodeIndex] {
        &self.joints
    }

    /// Returns inverse bind matrices in the same order as [`Self::joints`].
    #[must_use]
    pub fn inverse_bind_matrices(&self) -> &[[f32; 16]] {
        &self.inverse_bind_matrices
    }
}

/// Four joint indices and normalized weights for one source vertex.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ImportedSkinVertex {
    joints: [u16; 4],
    weights: [f32; 4],
}

impl ImportedSkinVertex {
    /// Returns indices into the skin's joint list.
    #[must_use]
    pub const fn joints(&self) -> [u16; 4] {
        self.joints
    }

    /// Returns normalized four-joint weights.
    #[must_use]
    pub const fn weights(&self) -> [f32; 4] {
        self.weights
    }
}

/// Skinning vertex data for one mesh primitive.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ImportedSkinnedPrimitive {
    mesh: usize,
    primitive: usize,
    vertices: Vec<ImportedSkinVertex>,
}

impl ImportedSkinnedPrimitive {
    /// Returns the model mesh index.
    #[must_use]
    pub const fn mesh(&self) -> usize {
        self.mesh
    }

    /// Returns the primitive index within [`Self::mesh`].
    #[must_use]
    pub const fn primitive(&self) -> usize {
        self.primitive
    }

    /// Returns one four-joint binding per position vertex.
    #[must_use]
    pub fn vertices(&self) -> &[ImportedSkinVertex] {
        &self.vertices
    }
}

/// Position deltas for one glTF morph target.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ImportedMorphTarget {
    position_deltas: Vec<[f32; 3]>,
}

impl ImportedMorphTarget {
    /// Returns one additive position delta per primitive vertex.
    #[must_use]
    pub fn position_deltas(&self) -> &[[f32; 3]] {
        &self.position_deltas
    }
}

/// Morph targets associated with one converted model primitive.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ImportedMorphPrimitive {
    mesh: usize,
    primitive: usize,
    targets: Vec<ImportedMorphTarget>,
}

impl ImportedMorphPrimitive {
    /// Returns the converted model mesh index.
    #[must_use]
    pub const fn mesh(&self) -> usize {
        self.mesh
    }

    /// Returns the primitive index within [`Self::mesh`].
    #[must_use]
    pub const fn primitive(&self) -> usize {
        self.primitive
    }

    /// Returns additive position targets in source order.
    #[must_use]
    pub fn targets(&self) -> &[ImportedMorphTarget] {
        &self.targets
    }
}

/// One source animation clip.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ImportedAnimationClip {
    name: Option<String>,
    duration_seconds: f32,
    tracks: Vec<ImportedAnimationTrack>,
    morph_tracks: Vec<ImportedMorphAnimationTrack>,
}

impl ImportedAnimationClip {
    /// Returns the optional source/debug name.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// Returns the final keyframe time, or zero for an empty clip.
    #[must_use]
    pub const fn duration_seconds(&self) -> f32 {
        self.duration_seconds
    }

    /// Returns transform tracks in source channel order.
    #[must_use]
    pub fn tracks(&self) -> &[ImportedAnimationTrack] {
        &self.tracks
    }

    /// Returns morph-weight tracks in source channel order.
    #[must_use]
    pub fn morph_tracks(&self) -> &[ImportedMorphAnimationTrack] {
        &self.morph_tracks
    }
}

/// One node's morph weights sampled at animation keyframes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ImportedMorphAnimationTrack {
    node: NodeIndex,
    interpolation: AnimationInterpolation,
    times_seconds: Vec<f32>,
    weights: Vec<Vec<f32>>,
}

impl ImportedMorphAnimationTrack {
    /// Returns the animated mesh node.
    #[must_use]
    pub const fn node(&self) -> NodeIndex {
        self.node
    }

    /// Returns source interpolation.
    #[must_use]
    pub const fn interpolation(&self) -> AnimationInterpolation {
        self.interpolation
    }

    /// Returns monotonically increasing key times.
    #[must_use]
    pub fn times_seconds(&self) -> &[f32] {
        &self.times_seconds
    }

    /// Returns one complete target-weight vector per key time.
    #[must_use]
    pub fn weights(&self) -> &[Vec<f32>] {
        &self.weights
    }
}

/// One transform property animated on a node.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum AnimationProperty {
    /// Local translation.
    Translation,
    /// Local quaternion rotation.
    Rotation,
    /// Local scale.
    Scale,
}

/// How keyframes are sampled between their timestamps.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum AnimationInterpolation {
    /// Keep the previous value until the next key.
    Step,
    /// Interpolate linearly; rotations use the shortest quaternion arc.
    Linear,
}

/// A typed transform track with monotonically increasing timestamps.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ImportedAnimationTrack {
    node: NodeIndex,
    property: AnimationProperty,
    interpolation: AnimationInterpolation,
    times_seconds: Vec<f32>,
    values: Vec<AnimationValue>,
}

impl ImportedAnimationTrack {
    /// Returns the node modified by this track.
    #[must_use]
    pub const fn node(&self) -> NodeIndex {
        self.node
    }

    /// Returns the modified local property.
    #[must_use]
    pub const fn property(&self) -> AnimationProperty {
        self.property
    }

    /// Returns the source interpolation mode.
    #[must_use]
    pub const fn interpolation(&self) -> AnimationInterpolation {
        self.interpolation
    }

    /// Returns monotonically increasing keyframe times in seconds.
    #[must_use]
    pub fn times_seconds(&self) -> &[f32] {
        &self.times_seconds
    }

    /// Returns values matching [`Self::times_seconds`].
    #[must_use]
    pub fn values(&self) -> &[AnimationValue] {
        &self.values
    }
}

/// A typed TRS animation sample.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum AnimationValue {
    /// Translation or scale.
    Vector3([f32; 3]),
    /// Unit quaternion in glTF [x, y, z, w] order.
    Rotation([f32; 4]),
}

/// Current playback state of an [`AnimationPlayer`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AnimationPlayState {
    /// Time is advanced by [`AnimationPlayer::advance`].
    Playing,
    /// Time and pose remain unchanged until playback resumes.
    Paused,
    /// Playback is reset to the beginning and not advanced.
    Stopped,
}

/// Runtime controls for one imported animation clip.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AnimationPlayer {
    clip: AnimationClipIndex,
    time_seconds: f32,
    speed: f32,
    looping: bool,
    state: AnimationPlayState,
}

impl AnimationPlayer {
    /// Starts a player at the beginning of the selected clip.
    #[must_use]
    pub const fn new(clip: AnimationClipIndex) -> Self {
        Self {
            clip,
            time_seconds: 0.0,
            speed: 1.0,
            looping: true,
            state: AnimationPlayState::Playing,
        }
    }

    /// Selects whether time wraps at the final keyframe.
    #[must_use]
    pub const fn with_looping(mut self, looping: bool) -> Self {
        self.looping = looping;
        self
    }

    /// Returns the selected clip.
    #[must_use]
    pub const fn clip(&self) -> AnimationClipIndex {
        self.clip
    }

    /// Returns the current sample time in seconds.
    #[must_use]
    pub const fn time_seconds(&self) -> f32 {
        self.time_seconds
    }

    /// Returns the positive playback speed multiplier.
    #[must_use]
    pub const fn speed(&self) -> f32 {
        self.speed
    }

    /// Returns whether the player wraps at clip end.
    #[must_use]
    pub const fn looping(&self) -> bool {
        self.looping
    }

    /// Returns the current play/pause/stop state.
    #[must_use]
    pub const fn state(&self) -> AnimationPlayState {
        self.state
    }

    /// Sets the positive finite playback speed.
    ///
    /// # Errors
    ///
    /// Returns an error when `speed` is negative, NaN or infinite.
    pub fn set_speed(&mut self, speed: f32) -> Result<(), AnimationSampleError> {
        if !speed.is_finite() || speed < 0.0 {
            return Err(AnimationSampleError::InvalidPlaybackSpeed);
        }
        self.speed = speed;
        Ok(())
    }

    /// Starts or resumes playback.
    pub fn play(&mut self) {
        self.state = AnimationPlayState::Playing;
    }

    /// Pauses playback without changing the sampled pose.
    pub fn pause(&mut self) {
        self.state = AnimationPlayState::Paused;
    }

    /// Stops playback and resets time to zero.
    pub fn stop(&mut self) {
        self.state = AnimationPlayState::Stopped;
        self.time_seconds = 0.0;
    }

    /// Advances the player using the selected clip duration.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid frame delta or a missing selected clip.
    pub fn advance(
        &mut self,
        scene: &ImportedScene,
        delta_seconds: f32,
    ) -> Result<(), AnimationSampleError> {
        if !delta_seconds.is_finite() || delta_seconds < 0.0 {
            return Err(AnimationSampleError::InvalidDelta);
        }
        if self.state != AnimationPlayState::Playing {
            return Ok(());
        }
        let clip = scene
            .animations()
            .get(self.clip.get())
            .ok_or(AnimationSampleError::InvalidClip(self.clip))?;
        let duration = clip.duration_seconds();
        if duration <= f32::EPSILON {
            self.time_seconds = 0.0;
            if !self.looping {
                self.state = AnimationPlayState::Stopped;
            }
            return Ok(());
        }
        self.time_seconds += delta_seconds * self.speed;
        if self.looping {
            self.time_seconds %= duration;
        } else if self.time_seconds >= duration {
            self.time_seconds = duration;
            self.state = AnimationPlayState::Paused;
        }
        Ok(())
    }

    /// Samples local transforms, world transforms and skin palettes at current time.
    ///
    /// # Errors
    ///
    /// Returns an error for a missing clip, invalid hierarchy or non-invertible
    /// skinned mesh transform.
    pub fn snapshot(
        &self,
        scene: &ImportedScene,
    ) -> Result<AnimationSnapshot, AnimationSampleError> {
        sample_animation(scene, self.clip, self.time_seconds)
    }
}

/// A sampled pose ready for a future GPU skinning upload.
#[derive(Clone, Debug, PartialEq)]
pub struct AnimationSnapshot {
    local_transforms: Vec<LocalTransform>,
    world_matrices: Vec<[f32; 16]>,
    skin_palettes: Vec<SkinPalette>,
    morph_weights: Vec<Vec<f32>>,
}

impl AnimationSnapshot {
    /// Returns one sampled local transform per source node.
    #[must_use]
    pub fn local_transforms(&self) -> &[LocalTransform] {
        &self.local_transforms
    }

    /// Returns one resolved world matrix per source node.
    #[must_use]
    pub fn world_matrices(&self) -> &[[f32; 16]] {
        &self.world_matrices
    }

    /// Returns palette matrices per skinned mesh-node instance.
    #[must_use]
    pub fn skin_palettes(&self) -> &[SkinPalette] {
        &self.skin_palettes
    }

    /// Returns sampled morph weights indexed by source node.
    #[must_use]
    pub fn morph_weights(&self, node: NodeIndex) -> Option<&[f32]> {
        self.morph_weights.get(node.get()).map(Vec::as_slice)
    }
}

/// Joint matrices for one node that uses a skin.
#[derive(Clone, Debug, PartialEq)]
pub struct SkinPalette {
    skin: SkinIndex,
    mesh_node: NodeIndex,
    matrices: Vec<[f32; 16]>,
}

impl SkinPalette {
    /// Returns the source skin used by this palette.
    #[must_use]
    pub const fn skin(&self) -> SkinIndex {
        self.skin
    }

    /// Returns the mesh node whose local space this palette uses.
    #[must_use]
    pub const fn mesh_node(&self) -> NodeIndex {
        self.mesh_node
    }

    /// Returns GPU-ready column-major joint matrices in skin joint order.
    #[must_use]
    pub fn matrices(&self) -> &[[f32; 16]] {
        &self.matrices
    }
}

/// Sampling or pose construction failed without silently changing an animation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AnimationSampleError {
    /// The requested clip index does not exist in the imported scene.
    InvalidClip(AnimationClipIndex),
    /// Playback speed was negative, NaN or infinite.
    InvalidPlaybackSpeed,
    /// Frame delta was negative, NaN or infinite.
    InvalidDelta,
    /// An animation track targeted a matrix-authored node.
    MatrixAnimationTarget(NodeIndex),
    /// Scene parent links form a cycle or assign a node two parents.
    InvalidHierarchy,
    /// A skinned mesh-node world matrix cannot be inverted.
    NonInvertibleMeshTransform(NodeIndex),
}

impl fmt::Display for AnimationSampleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidClip(clip) => {
                write!(formatter, "animation clip {} does not exist", clip.get())
            }
            Self::InvalidPlaybackSpeed => {
                formatter.write_str("animation speed must be finite and non-negative")
            }
            Self::InvalidDelta => {
                formatter.write_str("animation delta must be finite and non-negative")
            }
            Self::MatrixAnimationTarget(node) => write!(
                formatter,
                "animation cannot modify matrix node {}",
                node.get()
            ),
            Self::InvalidHierarchy => formatter.write_str("scene hierarchy is not a tree"),
            Self::NonInvertibleMeshTransform(node) => write!(
                formatter,
                "skinned mesh node {} has a non-invertible world transform",
                node.get()
            ),
        }
    }
}

impl Error for AnimationSampleError {}

/// Samples one imported clip at an explicit time.
///
/// This lower-level function does not advance time. For play/pause/loop state,
/// use [`AnimationPlayer`].
///
/// # Errors
///
/// Returns an error for an absent clip or invalid scene hierarchy.
pub fn sample_animation(
    scene: &ImportedScene,
    clip: AnimationClipIndex,
    time_seconds: f32,
) -> Result<AnimationSnapshot, AnimationSampleError> {
    if !time_seconds.is_finite() || time_seconds < 0.0 {
        return Err(AnimationSampleError::InvalidDelta);
    }
    let clip = scene
        .animations()
        .get(clip.get())
        .ok_or(AnimationSampleError::InvalidClip(clip))?;
    let mut local_transforms = scene
        .nodes()
        .iter()
        .map(ImportedNode::local_transform)
        .collect::<Vec<_>>();
    let mut morph_weights = scene
        .nodes()
        .iter()
        .map(|node| node.morph_weights().to_vec())
        .collect::<Vec<_>>();
    for track in clip.tracks() {
        let node = track.node().get();
        let transform = local_transforms
            .get_mut(node)
            .ok_or(AnimationSampleError::InvalidHierarchy)?;
        let sampled = sample_track(track, time_seconds);
        match (transform, track.property(), sampled) {
            (
                LocalTransform::Trs { translation, .. },
                AnimationProperty::Translation,
                AnimationValue::Vector3(value),
            ) => *translation = value,
            (
                LocalTransform::Trs { rotation, .. },
                AnimationProperty::Rotation,
                AnimationValue::Rotation(value),
            ) => *rotation = value,
            (
                LocalTransform::Trs { scale, .. },
                AnimationProperty::Scale,
                AnimationValue::Vector3(value),
            ) => *scale = value,
            (LocalTransform::Matrix { .. }, _, _) => {
                return Err(AnimationSampleError::MatrixAnimationTarget(track.node()));
            }
            _ => return Err(AnimationSampleError::InvalidHierarchy),
        }
    }
    for track in clip.morph_tracks() {
        let weights = morph_weights
            .get_mut(track.node().get())
            .ok_or(AnimationSampleError::InvalidHierarchy)?;
        *weights = sample_morph_track(track, time_seconds);
    }
    snapshot_from_local_transforms(scene, local_transforms, morph_weights)
}

/// Builds the imported model's bind pose without requiring an animation clip.
///
/// Use this for a rigged source that supplies a skeleton but no authored
/// motion. The returned matrices and skin palettes have the same GPU-ready
/// contract as [`sample_animation`].
///
/// # Errors
///
/// Returns an error for an invalid node hierarchy or non-invertible skinned
/// mesh transform.
pub fn sample_bind_pose(scene: &ImportedScene) -> Result<AnimationSnapshot, AnimationSampleError> {
    snapshot_from_local_transforms(
        scene,
        scene
            .nodes()
            .iter()
            .map(ImportedNode::local_transform)
            .collect(),
        scene
            .nodes()
            .iter()
            .map(|node| node.morph_weights().to_vec())
            .collect(),
    )
}

fn snapshot_from_local_transforms(
    scene: &ImportedScene,
    local_transforms: Vec<LocalTransform>,
    morph_weights: Vec<Vec<f32>>,
) -> Result<AnimationSnapshot, AnimationSampleError> {
    let world_matrices = resolve_world_matrices(scene, &local_transforms)?;
    let skin_palettes = build_skin_palettes(scene, &world_matrices)?;
    Ok(AnimationSnapshot {
        local_transforms,
        world_matrices,
        skin_palettes,
        morph_weights,
    })
}

fn sample_morph_track(track: &ImportedMorphAnimationTrack, time_seconds: f32) -> Vec<f32> {
    let times = track.times_seconds();
    let values = track.weights();
    if times.is_empty() || time_seconds <= times[0] {
        return values[0].clone();
    }
    let last = times.len() - 1;
    if time_seconds >= times[last] {
        return values[last].clone();
    }
    let right = times.partition_point(|time| *time <= time_seconds);
    let left = right - 1;
    if track.interpolation() == AnimationInterpolation::Step {
        return values[left].clone();
    }
    let factor = (time_seconds - times[left]) / (times[right] - times[left]);
    values[left]
        .iter()
        .zip(&values[right])
        .map(|(from, to)| from + (to - from) * factor)
        .collect()
}

fn sample_track(track: &ImportedAnimationTrack, time_seconds: f32) -> AnimationValue {
    let times = &track.times_seconds;
    let values = &track.values;
    if times.is_empty() || time_seconds <= times[0] {
        return values[0];
    }
    let last = times.len() - 1;
    if time_seconds >= times[last] {
        return values[last];
    }
    let right = times.partition_point(|time| *time <= time_seconds);
    let left = right - 1;
    if track.interpolation == AnimationInterpolation::Step {
        return values[left];
    }
    let factor = (time_seconds - times[left]) / (times[right] - times[left]);
    match (values[left], values[right]) {
        (AnimationValue::Vector3(from), AnimationValue::Vector3(to)) => {
            AnimationValue::Vector3(lerp_vec3(from, to, factor))
        }
        (AnimationValue::Rotation(from), AnimationValue::Rotation(to)) => {
            AnimationValue::Rotation(slerp_quaternion(from, to, factor))
        }
        _ => values[left],
    }
}

fn resolve_world_matrices(
    scene: &ImportedScene,
    local_transforms: &[LocalTransform],
) -> Result<Vec<[f32; 16]>, AnimationSampleError> {
    if local_transforms.len() != scene.nodes().len() {
        return Err(AnimationSampleError::InvalidHierarchy);
    }
    let mut parents = vec![None; scene.nodes().len()];
    for (parent, node) in scene.nodes().iter().enumerate() {
        for child in node.children() {
            let child = child.get();
            let entry = parents
                .get_mut(child)
                .ok_or(AnimationSampleError::InvalidHierarchy)?;
            if entry.replace(parent).is_some() {
                return Err(AnimationSampleError::InvalidHierarchy);
            }
        }
    }
    let local = local_transforms
        .iter()
        .copied()
        .map(local_transform_matrix)
        .collect::<Vec<_>>();
    let mut states = vec![0_u8; local.len()];
    let mut resolved = vec![identity_matrix(); local.len()];
    for node in 0..local.len() {
        resolve_world_matrix(node, &parents, &local, &mut states, &mut resolved)?;
    }
    Ok(resolved)
}

fn resolve_world_matrix(
    node: usize,
    parents: &[Option<usize>],
    local: &[[f32; 16]],
    states: &mut [u8],
    resolved: &mut [[f32; 16]],
) -> Result<[f32; 16], AnimationSampleError> {
    match states[node] {
        2 => return Ok(resolved[node]),
        1 => return Err(AnimationSampleError::InvalidHierarchy),
        _ => {}
    }
    states[node] = 1;
    let matrix = if let Some(parent) = parents[node] {
        multiply_matrices(
            resolve_world_matrix(parent, parents, local, states, resolved)?,
            local[node],
        )
    } else {
        local[node]
    };
    states[node] = 2;
    resolved[node] = matrix;
    Ok(matrix)
}

fn build_skin_palettes(
    scene: &ImportedScene,
    world_matrices: &[[f32; 16]],
) -> Result<Vec<SkinPalette>, AnimationSampleError> {
    let mut palettes = Vec::new();
    for (node_index, node) in scene.nodes().iter().enumerate() {
        let Some(skin_index) = node.skin() else {
            continue;
        };
        if node.mesh().is_none() {
            continue;
        }
        let skin = scene
            .skins()
            .get(skin_index.get())
            .ok_or(AnimationSampleError::InvalidHierarchy)?;
        let inverse_mesh = invert_affine(world_matrices[node_index]).ok_or(
            AnimationSampleError::NonInvertibleMeshTransform(NodeIndex::new(node_index)),
        )?;
        let matrices = skin
            .joints()
            .iter()
            .zip(skin.inverse_bind_matrices())
            .map(|(joint, inverse_bind)| {
                multiply_matrices(
                    multiply_matrices(inverse_mesh, world_matrices[joint.get()]),
                    *inverse_bind,
                )
            })
            .collect();
        palettes.push(SkinPalette {
            skin: skin_index,
            mesh_node: NodeIndex::new(node_index),
            matrices,
        });
    }
    Ok(palettes)
}

fn local_transform_matrix(transform: LocalTransform) -> [f32; 16] {
    match transform {
        LocalTransform::Trs {
            translation,
            rotation,
            scale,
        } => trs_matrix(translation, rotation, scale),
        LocalTransform::Matrix { column_major } => column_major,
    }
}

fn trs_matrix(translation: [f32; 3], rotation: [f32; 4], scale: [f32; 3]) -> [f32; 16] {
    let [x, y, z, w] = rotation;
    let xx = x * x;
    let yy = y * y;
    let zz = z * z;
    let xy = x * y;
    let xz = x * z;
    let yz = y * z;
    let wx = w * x;
    let wy = w * y;
    let wz = w * z;
    [
        (1.0 - 2.0 * (yy + zz)) * scale[0],
        (2.0 * (xy + wz)) * scale[0],
        (2.0 * (xz - wy)) * scale[0],
        0.0,
        (2.0 * (xy - wz)) * scale[1],
        (1.0 - 2.0 * (xx + zz)) * scale[1],
        (2.0 * (yz + wx)) * scale[1],
        0.0,
        (2.0 * (xz + wy)) * scale[2],
        (2.0 * (yz - wx)) * scale[2],
        (1.0 - 2.0 * (xx + yy)) * scale[2],
        0.0,
        translation[0],
        translation[1],
        translation[2],
        1.0,
    ]
}

fn multiply_matrices(left: [f32; 16], right: [f32; 16]) -> [f32; 16] {
    let mut output = [0.0; 16];
    for column in 0..4 {
        for row in 0..4 {
            output[column * 4 + row] = (0..4)
                .map(|inner| left[inner * 4 + row] * right[column * 4 + inner])
                .sum();
        }
    }
    output
}

fn invert_affine(matrix: [f32; 16]) -> Option<[f32; 16]> {
    let determinant = matrix[0] * (matrix[5] * matrix[10] - matrix[9] * matrix[6])
        - matrix[4] * (matrix[1] * matrix[10] - matrix[9] * matrix[2])
        + matrix[8] * (matrix[1] * matrix[6] - matrix[5] * matrix[2]);
    if !determinant.is_finite() || determinant.abs() <= 1.0e-8 {
        return None;
    }
    let inverse = 1.0 / determinant;
    let inverse_linear = [
        (matrix[5] * matrix[10] - matrix[9] * matrix[6]) * inverse,
        (matrix[8] * matrix[6] - matrix[4] * matrix[10]) * inverse,
        (matrix[4] * matrix[9] - matrix[8] * matrix[5]) * inverse,
        (matrix[9] * matrix[2] - matrix[1] * matrix[10]) * inverse,
        (matrix[0] * matrix[10] - matrix[8] * matrix[2]) * inverse,
        (matrix[8] * matrix[1] - matrix[0] * matrix[9]) * inverse,
        (matrix[1] * matrix[6] - matrix[5] * matrix[2]) * inverse,
        (matrix[4] * matrix[2] - matrix[0] * matrix[6]) * inverse,
        (matrix[0] * matrix[5] - matrix[4] * matrix[1]) * inverse,
    ];
    let translation = [matrix[12], matrix[13], matrix[14]];
    Some([
        inverse_linear[0],
        inverse_linear[1],
        inverse_linear[2],
        0.0,
        inverse_linear[3],
        inverse_linear[4],
        inverse_linear[5],
        0.0,
        inverse_linear[6],
        inverse_linear[7],
        inverse_linear[8],
        0.0,
        -(inverse_linear[0] * translation[0]
            + inverse_linear[3] * translation[1]
            + inverse_linear[6] * translation[2]),
        -(inverse_linear[1] * translation[0]
            + inverse_linear[4] * translation[1]
            + inverse_linear[7] * translation[2]),
        -(inverse_linear[2] * translation[0]
            + inverse_linear[5] * translation[1]
            + inverse_linear[8] * translation[2]),
        1.0,
    ])
}

fn lerp_vec3(from: [f32; 3], to: [f32; 3], factor: f32) -> [f32; 3] {
    [
        from[0] + (to[0] - from[0]) * factor,
        from[1] + (to[1] - from[1]) * factor,
        from[2] + (to[2] - from[2]) * factor,
    ]
}

fn slerp_quaternion(from: [f32; 4], mut to: [f32; 4], factor: f32) -> [f32; 4] {
    let mut dot = from
        .iter()
        .zip(to)
        .map(|(left, right)| left * right)
        .sum::<f32>();
    if dot < 0.0 {
        dot = -dot;
        for value in &mut to {
            *value = -*value;
        }
    }
    if dot > 0.9995 {
        return normalize_quaternion([
            from[0] + (to[0] - from[0]) * factor,
            from[1] + (to[1] - from[1]) * factor,
            from[2] + (to[2] - from[2]) * factor,
            from[3] + (to[3] - from[3]) * factor,
        ]);
    }
    let angle = dot.clamp(-1.0, 1.0).acos();
    let sin_angle = angle.sin();
    let from_weight = ((1.0 - factor) * angle).sin() / sin_angle;
    let to_weight = (factor * angle).sin() / sin_angle;
    normalize_quaternion([
        from[0] * from_weight + to[0] * to_weight,
        from[1] * from_weight + to[1] * to_weight,
        from[2] * from_weight + to[2] * to_weight,
        from[3] * from_weight + to[3] * to_weight,
    ])
}

fn normalize_quaternion(value: [f32; 4]) -> [f32; 4] {
    let length = value
        .iter()
        .map(|component| component * component)
        .sum::<f32>()
        .sqrt();
    if length <= f32::EPSILON {
        [0.0, 0.0, 0.0, 1.0]
    } else {
        value.map(|component| component / length)
    }
}

/// One camera definition referenced by nodes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ImportedCamera {
    name: Option<String>,
    projection: CameraProjection,
}

impl ImportedCamera {
    /// Returns the optional source/debug name.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// Returns the source camera projection.
    #[must_use]
    pub const fn projection(&self) -> &CameraProjection {
        &self.projection
    }
}

/// A camera projection copied without choosing a renderer aspect ratio.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum CameraProjection {
    /// A finite orthographic frustum.
    Orthographic {
        /// Horizontal magnification.
        xmag: f32,
        /// Vertical magnification.
        ymag: f32,
        /// Near clipping plane.
        znear: f32,
        /// Far clipping plane.
        zfar: f32,
    },
    /// A perspective frustum.
    Perspective {
        /// Vertical field of view in radians.
        yfov_radians: f32,
        /// Optional source aspect ratio. None asks the application to supply it.
        aspect_ratio: Option<f32>,
        /// Near clipping plane.
        znear: f32,
        /// Optional far clipping plane; None represents an infinite far plane.
        zfar: Option<f32>,
    },
}

/// One `KHR_lights_punctual` directional light definition.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ImportedDirectionalLight {
    name: Option<String>,
    color: [f32; 3],
    illuminance_lux: f32,
}

impl ImportedDirectionalLight {
    /// Returns the optional source/debug name.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// Returns the source linear RGB color multiplier.
    #[must_use]
    pub const fn color(&self) -> [f32; 3] {
        self.color
    }

    /// Returns the source directional illuminance in lux.
    #[must_use]
    pub const fn illuminance_lux(&self) -> f32 {
        self.illuminance_lux
    }
}

fn convert_scene(
    document: &gltf::Document,
    buffers: &[Vec<u8>],
    options: ImportOptions,
    mesh_mapping: &[SourceMeshMapping],
) -> Result<ImportedScene, ImportError> {
    let (directional_lights, light_indices) = convert_directional_lights(document)?;
    let (skins, skinned_primitives, morph_primitives, animations) =
        if is_skeletal_policy(options.policy) {
            (
                convert_skins(document, buffers, options.limits)?,
                convert_skinned_primitives(document, buffers, mesh_mapping)?,
                if options.policy == ImportPolicy::SkeletalPreview {
                    convert_morph_primitives(document, buffers, mesh_mapping)?
                } else {
                    Vec::new()
                },
                convert_animations(document, buffers, options.limits, options.policy)?,
            )
        } else {
            (Vec::new(), Vec::new(), Vec::new(), Vec::new())
        };
    let cameras = document
        .cameras()
        .map(|camera| convert_camera(&camera))
        .collect::<Result<Vec<_>, ImportError>>()?;
    let nodes = document
        .nodes()
        .map(|node| {
            let local_transform = convert_local_transform(node.index(), &node.transform())?;
            let morph_weights = node
                .weights()
                .or_else(|| node.mesh().and_then(|mesh| mesh.weights()))
                .map_or_else(
                    || {
                        vec![
                            0.0;
                            node.mesh()
                                .and_then(|mesh| mesh.primitives().next())
                                .map(|primitive| primitive.morph_targets().count())
                                .unwrap_or_default()
                        ]
                    },
                    <[f32]>::to_vec,
                );
            let directional_light = node
                .light()
                .map(|light| {
                    light_indices
                        .get(light.index())
                        .copied()
                        .flatten()
                        .ok_or(ImportError::UnsupportedFeature("point or spot lights"))
                })
                .transpose()?;
            Ok(ImportedNode {
                name: node.name().map(str::to_owned),
                local_transform,
                mesh: node.mesh().and_then(|mesh| {
                    mesh_mapping
                        .get(mesh.index())
                        .and_then(|mapping| mapping.output_mesh)
                }),
                camera: node.camera().map(|camera| CameraIndex::new(camera.index())),
                directional_light,
                skin: if is_skeletal_policy(options.policy) {
                    node.skin().map(|skin| SkinIndex::new(skin.index()))
                } else {
                    None
                },
                morph_weights,
                children: node
                    .children()
                    .map(|child| NodeIndex::new(child.index()))
                    .collect(),
            })
        })
        .collect::<Result<Vec<_>, ImportError>>()?;
    let scenes = document
        .scenes()
        .map(|scene| ImportedRootScene {
            name: scene.name().map(str::to_owned),
            roots: scene
                .nodes()
                .map(|node| NodeIndex::new(node.index()))
                .collect(),
        })
        .collect();
    Ok(ImportedScene {
        scenes,
        default_scene: document
            .default_scene()
            .map(|scene| SceneIndex::new(scene.index())),
        nodes,
        cameras,
        directional_lights,
        skins,
        animations,
        skinned_primitives,
        morph_primitives,
    })
}

fn convert_local_transform(
    node: usize,
    transform: &gltf::scene::Transform,
) -> Result<LocalTransform, ImportError> {
    match *transform {
        gltf::scene::Transform::Decomposed {
            translation,
            rotation,
            scale,
        } => {
            if !all_finite(&translation) || !all_finite(&rotation) || !all_finite(&scale) {
                return Err(ImportError::InvalidNodeTransform {
                    node,
                    issue: NodeTransformIssue::NonFinite,
                });
            }
            let length_squared = rotation.iter().map(|value| value * value).sum::<f32>();
            if !length_squared.is_finite() || (length_squared - 1.0).abs() > 1.0e-3 {
                return Err(ImportError::InvalidNodeTransform {
                    node,
                    issue: NodeTransformIssue::NonUnitQuaternion,
                });
            }
            Ok(LocalTransform::Trs {
                translation,
                rotation,
                scale,
            })
        }
        gltf::scene::Transform::Matrix { matrix } => {
            let column_major = [
                matrix[0][0],
                matrix[0][1],
                matrix[0][2],
                matrix[0][3],
                matrix[1][0],
                matrix[1][1],
                matrix[1][2],
                matrix[1][3],
                matrix[2][0],
                matrix[2][1],
                matrix[2][2],
                matrix[2][3],
                matrix[3][0],
                matrix[3][1],
                matrix[3][2],
                matrix[3][3],
            ];
            if !all_finite(&column_major) {
                return Err(ImportError::InvalidNodeTransform {
                    node,
                    issue: NodeTransformIssue::NonFinite,
                });
            }
            if !is_affine_matrix(column_major) {
                return Err(ImportError::InvalidNodeTransform {
                    node,
                    issue: NodeTransformIssue::NonAffineMatrix,
                });
            }
            Ok(LocalTransform::Matrix { column_major })
        }
    }
}

fn is_affine_matrix(matrix: [f32; 16]) -> bool {
    const AFFINE_EPSILON: f32 = 1.0e-6;
    matrix[3].abs() <= AFFINE_EPSILON
        && matrix[7].abs() <= AFFINE_EPSILON
        && matrix[11].abs() <= AFFINE_EPSILON
        && (matrix[15] - 1.0).abs() <= AFFINE_EPSILON
}

fn convert_camera(camera: &gltf::Camera<'_>) -> Result<ImportedCamera, ImportError> {
    let projection = match camera.projection() {
        gltf::camera::Projection::Orthographic(projection) => CameraProjection::Orthographic {
            xmag: projection.xmag(),
            ymag: projection.ymag(),
            znear: projection.znear(),
            zfar: projection.zfar(),
        },
        gltf::camera::Projection::Perspective(projection) => CameraProjection::Perspective {
            yfov_radians: projection.yfov(),
            aspect_ratio: projection.aspect_ratio(),
            znear: projection.znear(),
            zfar: projection.zfar(),
        },
    };
    if !camera_projection_is_finite(projection) {
        return Err(ImportError::InvalidCamera {
            camera: camera.index(),
        });
    }
    Ok(ImportedCamera {
        name: camera.name().map(str::to_owned),
        projection,
    })
}

fn camera_projection_is_finite(projection: CameraProjection) -> bool {
    match projection {
        CameraProjection::Orthographic {
            xmag,
            ymag,
            znear,
            zfar,
        } => [xmag, ymag, znear, zfar]
            .iter()
            .all(|value| value.is_finite()),
        CameraProjection::Perspective {
            yfov_radians,
            aspect_ratio,
            znear,
            zfar,
        } => {
            [yfov_radians, znear].iter().all(|value| value.is_finite())
                && aspect_ratio.is_none_or(f32::is_finite)
                && zfar.is_none_or(f32::is_finite)
        }
    }
}

fn convert_directional_lights(
    document: &gltf::Document,
) -> Result<
    (
        Vec<ImportedDirectionalLight>,
        Vec<Option<DirectionalLightIndex>>,
    ),
    ImportError,
> {
    let Some(lights) = document.lights() else {
        return Ok((Vec::new(), Vec::new()));
    };
    let mut imported = Vec::new();
    let mut indices = Vec::new();
    for light in lights {
        if !matches!(light.kind(), gltf::khr_lights_punctual::Kind::Directional) {
            indices.push(None);
            continue;
        }
        if !all_finite(&light.color()) || !light.intensity().is_finite() || light.intensity() < 0.0
        {
            return Err(ImportError::InvalidDirectionalLight {
                light: light.index(),
            });
        }
        let index = DirectionalLightIndex::new(imported.len());
        imported.push(ImportedDirectionalLight {
            name: light.name().map(str::to_owned),
            color: light.color(),
            illuminance_lux: light.intensity(),
        });
        indices.push(Some(index));
    }
    Ok((imported, indices))
}

fn convert_skins(
    document: &gltf::Document,
    buffers: &[Vec<u8>],
    limits: ImportLimits,
) -> Result<Vec<ImportedSkin>, ImportError> {
    let mut total_joints = 0_usize;
    document
        .skins()
        .map(|skin| {
            let joints = skin
                .joints()
                .map(|node| NodeIndex::new(node.index()))
                .collect::<Vec<_>>();
            total_joints =
                total_joints
                    .checked_add(joints.len())
                    .ok_or(ImportError::LimitExceeded {
                        resource: "skin joints",
                        limit: limits.max_skin_joints,
                    })?;
            if total_joints > limits.max_skin_joints {
                return Err(ImportError::LimitExceeded {
                    resource: "skin joints",
                    limit: limits.max_skin_joints,
                });
            }
            let inverse_bind_matrices = if skin.inverse_bind_matrices().is_some() {
                skin.reader(|buffer| Some(buffers[buffer.index()].as_slice()))
                    .read_inverse_bind_matrices()
                    .ok_or(ImportError::UnsupportedFeature("inverse bind matrices"))?
                    .map(matrix_columns_to_flat)
                    .collect::<Vec<_>>()
            } else {
                vec![identity_matrix(); joints.len()]
            };
            if inverse_bind_matrices.len() != joints.len()
                || inverse_bind_matrices
                    .iter()
                    .any(|matrix| !all_finite(matrix))
            {
                return Err(ImportError::UnsupportedFeature(
                    "invalid inverse bind matrices",
                ));
            }
            Ok(ImportedSkin {
                name: skin.name().map(str::to_owned),
                skeleton_root: skin.skeleton().map(|node| NodeIndex::new(node.index())),
                joints,
                inverse_bind_matrices,
            })
        })
        .collect()
}

fn convert_skinned_primitives(
    document: &gltf::Document,
    buffers: &[Vec<u8>],
    mesh_mapping: &[SourceMeshMapping],
) -> Result<Vec<ImportedSkinnedPrimitive>, ImportError> {
    let mut imported = Vec::new();
    for mesh in document.meshes() {
        for primitive in mesh.primitives() {
            let Some(output_mesh) = mesh_mapping
                .get(mesh.index())
                .and_then(|mapping| mapping.output_mesh)
            else {
                continue;
            };
            let Some(output_primitive) = mesh_mapping[mesh.index()]
                .primitive_mapping
                .get(primitive.index())
                .copied()
                .flatten()
            else {
                continue;
            };
            let reader = primitive.reader(|buffer| Some(buffers[buffer.index()].as_slice()));
            let joints = reader
                .read_joints(0)
                .map(|values| values.into_u16().collect::<Vec<_>>());
            let weights = reader
                .read_weights(0)
                .map(|values| values.into_f32().collect::<Vec<_>>());
            match (joints, weights) {
                (None, None) => {}
                (Some(joints), Some(weights)) => {
                    if joints.len() != weights.len() {
                        return Err(ImportError::UnsupportedFeature(
                            "skin attribute count mismatch",
                        ));
                    }
                    let position_count = reader
                        .read_positions()
                        .ok_or(ImportError::UnsupportedFeature(
                            "skinned primitive positions",
                        ))?
                        .count();
                    if joints.len() != position_count {
                        return Err(ImportError::UnsupportedFeature(
                            "skin vertex count mismatch",
                        ));
                    }
                    let vertices = joints
                        .into_iter()
                        .zip(weights)
                        .map(|(joints, mut weights)| {
                            let total = weights.iter().sum::<f32>();
                            if weights
                                .iter()
                                .any(|weight| !weight.is_finite() || *weight < 0.0)
                                || !total.is_finite()
                                || total <= f32::EPSILON
                            {
                                return Err(ImportError::UnsupportedFeature(
                                    "invalid skin weights",
                                ));
                            }
                            for weight in &mut weights {
                                *weight /= total;
                            }
                            Ok(ImportedSkinVertex { joints, weights })
                        })
                        .collect::<Result<Vec<_>, ImportError>>()?;
                    imported.push(ImportedSkinnedPrimitive {
                        mesh: output_mesh,
                        primitive: output_primitive,
                        vertices,
                    });
                }
                _ => {
                    return Err(ImportError::UnsupportedFeature(
                        "incomplete skin attributes",
                    ));
                }
            }
        }
    }
    Ok(imported)
}

fn convert_morph_primitives(
    document: &gltf::Document,
    buffers: &[Vec<u8>],
    mesh_mapping: &[SourceMeshMapping],
) -> Result<Vec<ImportedMorphPrimitive>, ImportError> {
    let mut imported = Vec::new();
    for mesh in document.meshes() {
        for primitive in mesh.primitives() {
            let Some(output_mesh) = mesh_mapping
                .get(mesh.index())
                .and_then(|mapping| mapping.output_mesh)
            else {
                continue;
            };
            let Some(output_primitive) = mesh_mapping[mesh.index()]
                .primitive_mapping
                .get(primitive.index())
                .copied()
                .flatten()
            else {
                continue;
            };
            if primitive.morph_targets().next().is_none() {
                continue;
            }
            let reader = primitive.reader(|buffer| Some(buffers[buffer.index()].as_slice()));
            let vertex_count = reader
                .read_positions()
                .ok_or(ImportError::MissingPositions {
                    mesh: mesh.index(),
                    primitive: primitive.index(),
                })?
                .count();
            let targets = reader
                .read_morph_targets()
                .map(|(positions, _normals, _tangents)| {
                    let position_deltas =
                        positions.map_or_else(|| vec![[0.0; 3]; vertex_count], Iterator::collect);
                    if position_deltas.len() != vertex_count
                        || position_deltas.iter().any(|delta| !all_finite(delta))
                    {
                        return Err(ImportError::UnsupportedFeature(
                            "invalid morph target position deltas",
                        ));
                    }
                    Ok(ImportedMorphTarget { position_deltas })
                })
                .collect::<Result<Vec<_>, ImportError>>()?;
            imported.push(ImportedMorphPrimitive {
                mesh: output_mesh,
                primitive: output_primitive,
                targets,
            });
        }
    }
    Ok(imported)
}

#[allow(
    clippy::too_many_lines,
    clippy::match_same_arms,
    reason = "All glTF animation channel validation stays together so the accepted subset is auditable."
)]
fn convert_animations(
    document: &gltf::Document,
    buffers: &[Vec<u8>],
    limits: ImportLimits,
    policy: ImportPolicy,
) -> Result<Vec<ImportedAnimationClip>, ImportError> {
    let mut total_keyframes = 0_usize;
    document
        .animations()
        .map(|animation| {
            let mut duration_seconds = 0.0_f32;
            let mut targets = HashSet::new();
            let tracks = animation
                .channels()
                .filter(|channel| {
                    channel.target().property() != gltf::animation::Property::MorphTargetWeights
                })
                .map(|channel| {
                    let target = channel.target();
                    let property = match target.property() {
                        gltf::animation::Property::Translation => AnimationProperty::Translation,
                        gltf::animation::Property::Rotation => AnimationProperty::Rotation,
                        gltf::animation::Property::Scale => AnimationProperty::Scale,
                        gltf::animation::Property::MorphTargetWeights => {
                            return Err(ImportError::UnsupportedFeature("morph target animation"));
                        }
                    };
                    let node = NodeIndex::new(target.node().index());
                    if matches!(
                        target.node().transform(),
                        gltf::scene::Transform::Matrix { .. }
                    ) {
                        return Err(ImportError::UnsupportedFeature("animated matrix node"));
                    }
                    if !targets.insert((node.get(), property as u8)) {
                        return Err(ImportError::UnsupportedFeature(
                            "duplicate animation channel",
                        ));
                    }
                    let interpolation = match channel.sampler().interpolation() {
                        gltf::animation::Interpolation::Step => AnimationInterpolation::Step,
                        gltf::animation::Interpolation::Linear => AnimationInterpolation::Linear,
                        gltf::animation::Interpolation::CubicSpline => {
                            return Err(ImportError::UnsupportedFeature("cubic spline animation"));
                        }
                    };
                    let reader = channel.reader(|buffer| Some(buffers[buffer.index()].as_slice()));
                    let times_seconds = reader
                        .read_inputs()
                        .ok_or(ImportError::UnsupportedFeature("animation input"))?
                        .collect::<Vec<_>>();
                    if times_seconds.iter().any(|time| !time.is_finite())
                        || times_seconds.windows(2).any(|times| times[0] >= times[1])
                    {
                        return Err(ImportError::UnsupportedFeature("animation timestamps"));
                    }
                    total_keyframes = total_keyframes.checked_add(times_seconds.len()).ok_or(
                        ImportError::LimitExceeded {
                            resource: "animation keyframes",
                            limit: limits.max_animation_keyframes,
                        },
                    )?;
                    if total_keyframes > limits.max_animation_keyframes {
                        return Err(ImportError::LimitExceeded {
                            resource: "animation keyframes",
                            limit: limits.max_animation_keyframes,
                        });
                    }
                    let values: Vec<AnimationValue> = match (property, reader.read_outputs()) {
                        (
                            AnimationProperty::Translation,
                            Some(gltf::animation::util::ReadOutputs::Translations(values)),
                        ) => values.map(AnimationValue::Vector3).collect(),
                        (
                            AnimationProperty::Scale,
                            Some(gltf::animation::util::ReadOutputs::Scales(values)),
                        ) => values.map(AnimationValue::Vector3).collect(),
                        (
                            AnimationProperty::Rotation,
                            Some(gltf::animation::util::ReadOutputs::Rotations(values)),
                        ) => values.into_f32().map(AnimationValue::Rotation).collect(),
                        _ => return Err(ImportError::UnsupportedFeature("animation output")),
                    };
                    if values.len() != times_seconds.len() {
                        return Err(ImportError::UnsupportedFeature("animation keyframe count"));
                    }
                    if values.iter().any(|value| match value {
                        AnimationValue::Vector3(value) => !all_finite(value),
                        AnimationValue::Rotation(value) => {
                            !all_finite(value)
                                || (value
                                    .iter()
                                    .map(|component| component * component)
                                    .sum::<f32>()
                                    - 1.0)
                                    .abs()
                                    > 1.0e-3
                        }
                    }) {
                        return Err(ImportError::UnsupportedFeature("invalid animation values"));
                    }
                    if let Some(last) = times_seconds.last() {
                        duration_seconds = duration_seconds.max(*last);
                    }
                    Ok(ImportedAnimationTrack {
                        node,
                        property,
                        interpolation,
                        times_seconds,
                        values,
                    })
                })
                .collect::<Result<Vec<_>, ImportError>>()?;
            let morph_tracks = if policy == ImportPolicy::SkeletalPreview {
                animation
                    .channels()
                    .filter(|channel| {
                        channel.target().property() == gltf::animation::Property::MorphTargetWeights
                    })
                    .map(|channel| {
                        let target = channel.target();
                        let node = NodeIndex::new(target.node().index());
                        let target_count = target
                            .node()
                            .mesh()
                            .and_then(|mesh| mesh.primitives().next())
                            .map(|primitive| primitive.morph_targets().count())
                            .filter(|count| *count != 0)
                            .ok_or(ImportError::UnsupportedFeature(
                                "morph animation target count",
                            ))?;
                        let interpolation = match channel.sampler().interpolation() {
                            gltf::animation::Interpolation::Step => AnimationInterpolation::Step,
                            gltf::animation::Interpolation::Linear => {
                                AnimationInterpolation::Linear
                            }
                            gltf::animation::Interpolation::CubicSpline => {
                                return Err(ImportError::UnsupportedFeature(
                                    "cubic spline morph animation",
                                ));
                            }
                        };
                        let reader =
                            channel.reader(|buffer| Some(buffers[buffer.index()].as_slice()));
                        let times_seconds = reader
                            .read_inputs()
                            .ok_or(ImportError::UnsupportedFeature("morph animation input"))?
                            .collect::<Vec<_>>();
                        if times_seconds.iter().any(|time| !time.is_finite())
                            || times_seconds.windows(2).any(|times| times[0] >= times[1])
                        {
                            return Err(ImportError::UnsupportedFeature(
                                "morph animation timestamps",
                            ));
                        }
                        total_keyframes = total_keyframes.checked_add(times_seconds.len()).ok_or(
                            ImportError::LimitExceeded {
                                resource: "animation keyframes",
                                limit: limits.max_animation_keyframes,
                            },
                        )?;
                        if total_keyframes > limits.max_animation_keyframes {
                            return Err(ImportError::LimitExceeded {
                                resource: "animation keyframes",
                                limit: limits.max_animation_keyframes,
                            });
                        }
                        let flat = match reader.read_outputs() {
                            Some(gltf::animation::util::ReadOutputs::MorphTargetWeights(
                                values,
                            )) => values.into_f32().collect::<Vec<_>>(),
                            _ => {
                                return Err(ImportError::UnsupportedFeature(
                                    "morph animation output",
                                ));
                            }
                        };
                        if flat.len() != times_seconds.len().saturating_mul(target_count)
                            || flat.iter().any(|weight| !weight.is_finite())
                        {
                            return Err(ImportError::UnsupportedFeature(
                                "morph animation keyframe count",
                            ));
                        }
                        let weights = flat
                            .chunks_exact(target_count)
                            .map(<[f32]>::to_vec)
                            .collect();
                        if let Some(last) = times_seconds.last() {
                            duration_seconds = duration_seconds.max(*last);
                        }
                        Ok(ImportedMorphAnimationTrack {
                            node,
                            interpolation,
                            times_seconds,
                            weights,
                        })
                    })
                    .collect::<Result<Vec<_>, ImportError>>()?
            } else {
                Vec::new()
            };
            Ok(ImportedAnimationClip {
                name: animation.name().map(str::to_owned),
                duration_seconds,
                tracks,
                morph_tracks,
            })
        })
        .collect()
}

fn all_finite(values: &[f32]) -> bool {
    values.iter().all(|value| value.is_finite())
}

const fn identity_matrix() -> [f32; 16] {
    [
        1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
    ]
}

const fn matrix_columns_to_flat(matrix: [[f32; 4]; 4]) -> [f32; 16] {
    [
        matrix[0][0],
        matrix[0][1],
        matrix[0][2],
        matrix[0][3],
        matrix[1][0],
        matrix[1][1],
        matrix[1][2],
        matrix[1][3],
        matrix[2][0],
        matrix[2][1],
        matrix[2][2],
        matrix[2][3],
        matrix[3][0],
        matrix[3][1],
        matrix[3][2],
        matrix[3][3],
    ]
}

fn validate_document(document: &gltf::Document, policy: ImportPolicy) -> Result<(), ImportError> {
    for mesh in document.meshes() {
        for primitive in mesh.primitives() {
            if primitive.mode() != gltf::mesh::Mode::Triangles
                && !skips_non_triangle_primitives(policy)
            {
                return Err(ImportError::UnsupportedPrimitiveMode {
                    mesh: mesh.index(),
                    primitive: primitive.index(),
                    mode: primitive.mode(),
                });
            }
            if primitive.morph_targets().next().is_some() && policy != ImportPolicy::SkeletalPreview
            {
                return Err(ImportError::UnsupportedFeature("morph targets"));
            }
            for (semantic, accessor) in primitive.attributes() {
                if accessor.sparse().is_some()
                    && !is_ignored_static_preview_attribute(policy, &semantic)
                {
                    return Err(ImportError::UnsupportedFeature("sparse accessors"));
                }
            }
            if primitive
                .indices()
                .is_some_and(|accessor| accessor.sparse().is_some())
            {
                return Err(ImportError::UnsupportedFeature("sparse accessors"));
            }
            for (semantic, _) in primitive.attributes() {
                if !is_imported_attribute(policy, &semantic)
                    && !is_ignored_static_preview_attribute(policy, &semantic)
                {
                    return Err(ImportError::UnsupportedFeature("vertex attribute"));
                }
            }
        }
    }
    if policy == ImportPolicy::Strict && document.skins().next().is_some() {
        return Err(ImportError::UnsupportedFeature("skins"));
    }
    if policy == ImportPolicy::Strict && document.animations().next().is_some() {
        return Err(ImportError::UnsupportedFeature("animations"));
    }
    Ok(())
}

fn is_imported_attribute(policy: ImportPolicy, semantic: &gltf::Semantic) -> bool {
    matches!(
        semantic,
        gltf::Semantic::Positions
            | gltf::Semantic::Normals
            | gltf::Semantic::Tangents
            | gltf::Semantic::TexCoords(0..=7)
    ) || (is_skeletal_policy(policy)
        && matches!(
            semantic,
            gltf::Semantic::Joints(0) | gltf::Semantic::Weights(0)
        ))
}

fn is_ignored_static_preview_attribute(policy: ImportPolicy, semantic: &gltf::Semantic) -> bool {
    matches!(
        policy,
        ImportPolicy::StaticPreview | ImportPolicy::Skeletal | ImportPolicy::SkeletalPreview
    ) && matches!(
        semantic,
        gltf::Semantic::Joints(0) | gltf::Semantic::Weights(0)
    )
}

const fn is_skeletal_policy(policy: ImportPolicy) -> bool {
    matches!(
        policy,
        ImportPolicy::Skeletal | ImportPolicy::SkeletalPreview
    )
}

const fn skips_non_triangle_primitives(policy: ImportPolicy) -> bool {
    matches!(policy, ImportPolicy::SkeletalPreview)
}

fn load_buffers(
    gltf: &gltf::Gltf,
    base_path: &Path,
    limits: ImportLimits,
) -> Result<Vec<Vec<u8>>, ImportError> {
    let base = fs::canonicalize(base_path).map_err(|source| ImportError::ReadBasePath {
        path: base_path.to_owned(),
        source,
    })?;
    let mut total = 0_usize;
    gltf.document
        .buffers()
        .map(|buffer| {
            let data = match buffer.source() {
                gltf::buffer::Source::Bin => {
                    gltf.blob.clone().ok_or(ImportError::MissingGlbBinary)?
                }
                gltf::buffer::Source::Uri(uri) => load_uri_buffer(uri, &base)?,
            };
            total = total
                .checked_add(data.len())
                .ok_or(ImportError::LimitExceeded {
                    resource: "buffer bytes",
                    limit: limits.max_buffer_bytes,
                })?;
            if total > limits.max_buffer_bytes {
                return Err(ImportError::LimitExceeded {
                    resource: "buffer bytes",
                    limit: limits.max_buffer_bytes,
                });
            }
            if data.len() < buffer.length() {
                return Err(ImportError::BufferTooShort {
                    buffer: buffer.index(),
                    expected: buffer.length(),
                    actual: data.len(),
                });
            }
            Ok(data)
        })
        .collect()
}

fn load_embedded_buffers(
    gltf: &gltf::Gltf,
    limits: ImportLimits,
) -> Result<Vec<Vec<u8>>, ImportError> {
    let mut total = 0_usize;
    gltf.document
        .buffers()
        .map(|buffer| {
            let data = match buffer.source() {
                gltf::buffer::Source::Bin => {
                    gltf.blob.clone().ok_or(ImportError::MissingGlbBinary)?
                }
                gltf::buffer::Source::Uri(uri) if uri.starts_with("data:") => {
                    load_uri_buffer(uri, Path::new("."))?
                }
                gltf::buffer::Source::Uri(uri) => {
                    return Err(ImportError::ExternalBufferRequiresResolver(uri.to_owned()));
                }
            };
            total = total
                .checked_add(data.len())
                .ok_or(ImportError::LimitExceeded {
                    resource: "buffer bytes",
                    limit: limits.max_buffer_bytes,
                })?;
            if total > limits.max_buffer_bytes {
                return Err(ImportError::LimitExceeded {
                    resource: "buffer bytes",
                    limit: limits.max_buffer_bytes,
                });
            }
            if data.len() < buffer.length() {
                return Err(ImportError::BufferTooShort {
                    buffer: buffer.index(),
                    expected: buffer.length(),
                    actual: data.len(),
                });
            }
            Ok(data)
        })
        .collect()
}

pub(crate) fn load_uri_buffer(uri: &str, base: &Path) -> Result<Vec<u8>, ImportError> {
    if let Some(payload) = uri.strip_prefix("data:") {
        return decode_data_uri(payload);
    }
    let relative = Path::new(uri);
    if relative.is_absolute() {
        return Err(ImportError::UnsafeExternalPath(uri.to_owned()));
    }
    let candidate =
        fs::canonicalize(base.join(relative)).map_err(|source| ImportError::ReadBuffer {
            path: base.join(relative),
            source,
        })?;
    if !candidate.starts_with(base) {
        return Err(ImportError::UnsafeExternalPath(uri.to_owned()));
    }
    fs::read(&candidate).map_err(|source| ImportError::ReadBuffer {
        path: candidate,
        source,
    })
}

fn decode_data_uri(payload: &str) -> Result<Vec<u8>, ImportError> {
    let (_, encoded) = payload
        .split_once(',')
        .ok_or(ImportError::MalformedDataUri)?;
    let (metadata, _) = payload
        .split_once(',')
        .ok_or(ImportError::MalformedDataUri)?;
    if !metadata.ends_with(";base64") {
        return Err(ImportError::UnsupportedDataUriEncoding);
    }
    base64::Engine::decode(&base64::engine::general_purpose::STANDARD, encoded)
        .map_err(ImportError::DecodeDataUri)
}

struct ConvertedDocument {
    model: Model,
    mesh_mapping: Vec<SourceMeshMapping>,
    report: ImportReport,
}

/// Maps one source mesh and its primitives to the compact triangle-only model.
#[derive(Clone, Debug)]
struct SourceMeshMapping {
    output_mesh: Option<usize>,
    primitive_mapping: Vec<Option<usize>>,
}

#[allow(
    clippy::too_many_lines,
    reason = "The complete mesh conversion sequence is intentionally co-located for invariant review."
)]
fn convert_document(
    document: &gltf::Document,
    buffers: &[Vec<u8>],
    options: ImportOptions,
) -> Result<ConvertedDocument, ImportError> {
    let textures = convert_textures(document, buffers, options.limits)?;
    let materials = convert_materials(document)?;
    let mut vertices = 0_usize;
    let mut indices = 0_usize;
    let mut report = ImportReport::default();
    let mut mesh_mapping = Vec::new();
    let mut meshes = Vec::new();
    for mesh in document.meshes() {
        let mut primitive_mapping = vec![None; mesh.primitives().len()];
        let mut primitives = Vec::new();
        for primitive in mesh.primitives() {
            if primitive.mode() != gltf::mesh::Mode::Triangles {
                debug_assert!(skips_non_triangle_primitives(options.policy));
                report.primitives.push(SkippedPrimitive {
                    mesh: mesh.index(),
                    primitive: primitive.index(),
                    mode: primitive.mode(),
                });
                continue;
            }
            let reader = primitive.reader(|buffer| Some(buffers[buffer.index()].as_slice()));
            let positions = reader
                .read_positions()
                .ok_or(ImportError::MissingPositions {
                    mesh: mesh.index(),
                    primitive: primitive.index(),
                })?
                .collect::<Vec<_>>();
            vertices = vertices
                .checked_add(positions.len())
                .ok_or(ImportError::LimitExceeded {
                    resource: "vertices",
                    limit: options.limits.max_vertices,
                })?;
            if vertices > options.limits.max_vertices {
                return Err(ImportError::LimitExceeded {
                    resource: "vertices",
                    limit: options.limits.max_vertices,
                });
            }
            let index_values = reader
                .read_indices()
                .ok_or(ImportError::MissingIndices {
                    mesh: mesh.index(),
                    primitive: primitive.index(),
                })?
                .into_u32()
                .collect::<Vec<_>>();
            indices =
                indices
                    .checked_add(index_values.len())
                    .ok_or(ImportError::LimitExceeded {
                        resource: "indices",
                        limit: options.limits.max_indices,
                    })?;
            if indices > options.limits.max_indices {
                return Err(ImportError::LimitExceeded {
                    resource: "indices",
                    limit: options.limits.max_indices,
                });
            }
            let mut output = MeshPrimitive::new(positions, index_values).map_err(|source| {
                ImportError::InvalidPrimitive {
                    mesh: mesh.index(),
                    primitive: primitive.index(),
                    source,
                }
            })?;
            if let Some(normals) = reader.read_normals() {
                output = output.with_normals(normals.collect()).map_err(|source| {
                    ImportError::InvalidPrimitive {
                        mesh: mesh.index(),
                        primitive: primitive.index(),
                        source,
                    }
                })?;
            }
            if let Some(tangents) = reader.read_tangents() {
                output = output.with_tangents(tangents.collect()).map_err(|source| {
                    ImportError::InvalidPrimitive {
                        mesh: mesh.index(),
                        primitive: primitive.index(),
                        source,
                    }
                })?;
            }
            for set in 0_u32..8 {
                if let Some(tex_coords) = reader.read_tex_coords(set) {
                    output = output
                        .with_tex_coords(
                            u8::try_from(set).expect("bounded glTF UV set fits u8"),
                            tex_coords.into_f32().collect(),
                        )
                        .map_err(|source| ImportError::InvalidPrimitive {
                            mesh: mesh.index(),
                            primitive: primitive.index(),
                            source,
                        })?;
                }
            }
            if let Some(material) = primitive.material().index() {
                output = output.with_material(MaterialIndex::new(material));
            }
            primitive_mapping[primitive.index()] = Some(primitives.len());
            primitives.push(output);
        }
        let output_mesh = if primitives.is_empty() {
            None
        } else {
            let output_mesh = meshes.len();
            let converted =
                Mesh::new(mesh.name().map(str::to_owned), primitives).map_err(|source| {
                    ImportError::InvalidMesh {
                        mesh: mesh.index(),
                        source,
                    }
                })?;
            meshes.push(converted);
            Some(output_mesh)
        };
        mesh_mapping.push(SourceMeshMapping {
            output_mesh,
            primitive_mapping,
        });
    }
    if meshes.is_empty() {
        return Err(ImportError::NoRenderableTrianglePrimitives {
            skipped: report.skipped_primitive_count(),
        });
    }
    let model = Model::new(meshes, materials, textures).map_err(ImportError::InvalidModel)?;
    Ok(ConvertedDocument {
        model,
        mesh_mapping,
        report,
    })
}

fn convert_textures(
    document: &gltf::Document,
    buffers: &[Vec<u8>],
    limits: ImportLimits,
) -> Result<Vec<ModelTexture>, ImportError> {
    let mut embedded_image_bytes = 0_usize;
    document
        .textures()
        .map(|texture| {
            let image = texture.source();
            let result = match image.source() {
                gltf::image::Source::Uri { uri, .. } => {
                    if uri.starts_with("data:") {
                        return Err(ImportError::UnsupportedFeature("data URI images"));
                    }
                    ModelTexture::new(uri)
                }
                gltf::image::Source::View { view, mime_type } => {
                    let buffer = view.buffer().index();
                    let offset = view.offset();
                    let length = view.length();
                    let end = offset.checked_add(length).ok_or(
                        ImportError::EmbeddedImageRangeOverflow {
                            image: image.index(),
                            offset,
                            length,
                        },
                    )?;
                    let bytes = buffers
                        .get(buffer)
                        .and_then(|bytes| bytes.get(offset..end))
                        .ok_or(ImportError::EmbeddedImageOutOfBounds {
                            image: image.index(),
                            buffer,
                            offset,
                            length,
                        })?;
                    embedded_image_bytes = embedded_image_bytes.checked_add(bytes.len()).ok_or(
                        ImportError::LimitExceeded {
                            resource: "embedded image bytes",
                            limit: limits.max_embedded_image_bytes,
                        },
                    )?;
                    if embedded_image_bytes > limits.max_embedded_image_bytes {
                        return Err(ImportError::LimitExceeded {
                            resource: "embedded image bytes",
                            limit: limits.max_embedded_image_bytes,
                        });
                    }
                    ModelTexture::embedded(mime_type, Arc::<[u8]>::from(bytes))
                }
            };
            let result = result.with_sampler(convert_sampler(&texture.sampler()));
            Ok(if let Some(name) = texture.name().or(image.name()) {
                result.with_label(name)
            } else {
                result
            })
        })
        .collect()
}

fn convert_sampler(sampler: &gltf::texture::Sampler<'_>) -> ModelTextureSampler {
    ModelTextureSampler {
        address_mode_u: match sampler.wrap_s() {
            gltf::texture::WrappingMode::Repeat => ModelTextureAddressMode::Repeat,
            gltf::texture::WrappingMode::MirroredRepeat => ModelTextureAddressMode::MirroredRepeat,
            gltf::texture::WrappingMode::ClampToEdge => ModelTextureAddressMode::ClampToEdge,
        },
        address_mode_v: match sampler.wrap_t() {
            gltf::texture::WrappingMode::Repeat => ModelTextureAddressMode::Repeat,
            gltf::texture::WrappingMode::MirroredRepeat => ModelTextureAddressMode::MirroredRepeat,
            gltf::texture::WrappingMode::ClampToEdge => ModelTextureAddressMode::ClampToEdge,
        },
        mag_filter: match sampler
            .mag_filter()
            .unwrap_or(gltf::texture::MagFilter::Linear)
        {
            gltf::texture::MagFilter::Nearest => ModelTextureMagFilter::Nearest,
            gltf::texture::MagFilter::Linear => ModelTextureMagFilter::Linear,
        },
        min_filter: match sampler
            .min_filter()
            .unwrap_or(gltf::texture::MinFilter::Linear)
        {
            gltf::texture::MinFilter::Nearest => ModelTextureMinFilter::Nearest,
            gltf::texture::MinFilter::Linear => ModelTextureMinFilter::Linear,
            gltf::texture::MinFilter::NearestMipmapNearest => {
                ModelTextureMinFilter::NearestMipmapNearest
            }
            gltf::texture::MinFilter::LinearMipmapNearest => {
                ModelTextureMinFilter::LinearMipmapNearest
            }
            gltf::texture::MinFilter::NearestMipmapLinear => {
                ModelTextureMinFilter::NearestMipmapLinear
            }
            gltf::texture::MinFilter::LinearMipmapLinear => {
                ModelTextureMinFilter::LinearMipmapLinear
            }
        },
    }
}

fn convert_materials(document: &gltf::Document) -> Result<Vec<Material>, ImportError> {
    document
        .materials()
        .map(|material| {
            let emissive_strength = material.emissive_strength().unwrap_or(1.0);
            if !emissive_strength.is_finite() || emissive_strength < 0.0 {
                return Err(ImportError::UnsupportedFeature("invalid emissive strength"));
            }
            let emissive_factor = material
                .emissive_factor()
                .map(|channel| channel * emissive_strength);
            if emissive_factor
                .iter()
                .any(|factor| !factor.is_finite() || *factor < 0.0)
            {
                return Err(ImportError::UnsupportedFeature("invalid emissive factor"));
            }
            let pbr = material.pbr_metallic_roughness();
            let alpha_mode = match material.alpha_mode() {
                gltf::material::AlphaMode::Opaque => AlphaMode::Opaque,
                gltf::material::AlphaMode::Mask => {
                    let cutoff = material.alpha_cutoff().unwrap_or(0.5);
                    if !cutoff.is_finite() || !(0.0..=1.0).contains(&cutoff) {
                        return Err(ImportError::UnsupportedFeature("invalid alpha cutoff"));
                    }
                    AlphaMode::Mask { cutoff }
                }
                gltf::material::AlphaMode::Blend => AlphaMode::Blend,
            };
            let mut output = Material::new()
                .with_base_color_factor(pbr.base_color_factor())
                .with_metallic_roughness(pbr.metallic_factor(), pbr.roughness_factor())
                .with_emissive_factor(emissive_factor)
                .with_double_sided(material.double_sided())
                .with_alpha_mode(alpha_mode);
            if let Some(name) = material.name() {
                output = output.with_name(name);
            }
            if let Some(specular_glossiness) = material.pbr_specular_glossiness() {
                let mut workflow = SpecularGlossinessMaterial::new(
                    specular_glossiness.diffuse_factor(),
                    specular_glossiness.specular_factor(),
                    specular_glossiness.glossiness_factor(),
                );
                if let Some(info) = specular_glossiness.diffuse_texture() {
                    workflow = workflow.with_diffuse_texture(convert_texture_info(&info)?);
                }
                if let Some(info) = specular_glossiness.specular_glossiness_texture() {
                    workflow =
                        workflow.with_specular_glossiness_texture(convert_texture_info(&info)?);
                }
                output = output.with_specular_glossiness(workflow);
            } else if let Some(info) = pbr.base_color_texture() {
                output = output.with_base_color_texture(convert_texture_info(&info)?);
            }
            if let Some(info) = material.normal_texture() {
                let binding = convert_texture_parts(info.texture().index(), info.tex_coord())?;
                output =
                    output.with_normal_texture(NormalTextureBinding::new(binding, info.scale()));
            }
            if let Some(info) = pbr.metallic_roughness_texture() {
                output = output.with_metallic_roughness_texture(convert_texture_info(&info)?);
            }
            if let Some(info) = material.emissive_texture() {
                output = output.with_emissive_texture(convert_texture_info(&info)?);
            }
            Ok(output)
        })
        .collect()
}

fn convert_texture_info(info: &gltf::texture::Info<'_>) -> Result<TextureBinding, ImportError> {
    convert_texture_parts(info.texture().index(), info.tex_coord())
}

fn convert_texture_parts(texture: usize, tex_coord: u32) -> Result<TextureBinding, ImportError> {
    if tex_coord > 7 {
        return Err(ImportError::UnsupportedTextureCoordinateSet(tex_coord));
    }
    Ok(TextureBinding::new(
        ModelTextureIndex::new(texture),
        u8::try_from(tex_coord).expect("validated glTF UV set fits u8"),
    ))
}

/// A local node transform could not be represented without changing it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NodeTransformIssue {
    /// Translation, rotation or scale contained NaN or infinity.
    NonFinite,
    /// Rotation was not a unit quaternion within a small import tolerance.
    NonUnitQuaternion,
    /// A node matrix had a projective final row instead of an affine one.
    NonAffineMatrix,
}

/// A glTF import failed without silently changing source semantics.
#[derive(Debug)]
pub enum ImportError {
    /// The source document could not be read.
    ReadDocument {
        /// Document path.
        path: PathBuf,
        /// Underlying I/O error.
        source: std::io::Error,
    },
    /// The external resource base could not be resolved.
    ReadBasePath {
        /// Requested base directory.
        path: PathBuf,
        /// Underlying I/O error.
        source: std::io::Error,
    },
    /// An external buffer could not be read.
    ReadBuffer {
        /// Resolved buffer path.
        path: PathBuf,
        /// Underlying I/O error.
        source: std::io::Error,
    },
    /// glTF JSON or GLB container parsing failed.
    Parse(gltf::Error),
    /// A GLB document referenced its missing BIN chunk.
    MissingGlbBinary,
    /// An external buffer URI could escape its source directory.
    UnsafeExternalPath(String),
    /// Byte-only import requires the host to resolve an external buffer.
    ExternalBufferRequiresResolver(String),
    /// A data URI did not use the expected `metadata,contents` layout.
    MalformedDataUri,
    /// A data URI used an encoding other than Base64.
    UnsupportedDataUriEncoding,
    /// Base64 decoding failed.
    DecodeDataUri(base64::DecodeError),
    /// A declared buffer length exceeds loaded bytes.
    BufferTooShort {
        /// Zero-based glTF buffer index.
        buffer: usize,
        /// Declared byte length.
        expected: usize,
        /// Bytes actually loaded.
        actual: usize,
    },
    /// An embedded image buffer-view range overflowed host address space.
    EmbeddedImageRangeOverflow {
        /// Zero-based glTF image index.
        image: usize,
        /// Declared byte offset inside the source buffer.
        offset: usize,
        /// Declared byte length.
        length: usize,
    },
    /// An embedded image buffer-view range was outside the loaded source buffer.
    EmbeddedImageOutOfBounds {
        /// Zero-based glTF image index.
        image: usize,
        /// Zero-based glTF buffer index.
        buffer: usize,
        /// Declared byte offset inside the source buffer.
        offset: usize,
        /// Declared byte length.
        length: usize,
    },
    /// A configured resource limit was exceeded.
    LimitExceeded {
        /// Human-readable limited resource.
        resource: &'static str,
        /// Configured maximum.
        limit: usize,
    },
    /// The feature cannot be represented faithfully by this static importer.
    UnsupportedFeature(&'static str),
    /// A primitive used a non-triangle topology.
    UnsupportedPrimitiveMode {
        /// Zero-based mesh index.
        mesh: usize,
        /// Zero-based primitive index in the mesh.
        primitive: usize,
        /// Source topology.
        mode: gltf::mesh::Mode,
    },
    /// A preview omitted every source primitive because none used triangles.
    NoRenderableTrianglePrimitives {
        /// Number of explicitly skipped non-triangle source primitives.
        skipped: usize,
    },
    /// A primitive did not include `POSITION`.
    MissingPositions {
        /// Zero-based mesh index.
        mesh: usize,
        /// Zero-based primitive index in the mesh.
        primitive: usize,
    },
    /// A primitive did not include an index accessor.
    MissingIndices {
        /// Zero-based mesh index.
        mesh: usize,
        /// Zero-based primitive index in the mesh.
        primitive: usize,
    },
    /// A texture uses a UV set other than zero.
    UnsupportedTextureCoordinateSet(u32),
    /// A decomposed node transform did not meet lossless import invariants.
    InvalidNodeTransform {
        /// Zero-based source node index.
        node: usize,
        /// Rejected transform property.
        issue: NodeTransformIssue,
    },
    /// A camera projection contained a non-finite property.
    InvalidCamera {
        /// Zero-based source camera index.
        camera: usize,
    },
    /// A directional light contained invalid color or illuminance.
    InvalidDirectionalLight {
        /// Zero-based source punctual-light index.
        light: usize,
    },
    /// Mesh attributes or indices did not meet Yuyib model invariants.
    InvalidPrimitive {
        /// Zero-based mesh index.
        mesh: usize,
        /// Zero-based primitive index in the mesh.
        primitive: usize,
        /// Violated primitive invariant.
        source: yuyib_model::MeshValidationError,
    },
    /// The imported mesh was empty.
    InvalidMesh {
        /// Zero-based mesh index.
        mesh: usize,
        /// Violated mesh invariant.
        source: yuyib_model::MeshError,
    },
    /// Cross-resource references did not meet Yuyib model invariants.
    InvalidModel(yuyib_model::ModelValidationError),
}

impl fmt::Display for ImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReadDocument { path, source } => write!(
                f,
                "could not read glTF document {}: {source}",
                path.display()
            ),
            Self::ReadBasePath { path, source } => write!(
                f,
                "could not resolve glTF base path {}: {source}",
                path.display()
            ),
            Self::ReadBuffer { path, source } => {
                write!(f, "could not read glTF buffer {}: {source}", path.display())
            }
            Self::Parse(source) => write!(f, "invalid glTF document: {source}"),
            Self::MissingGlbBinary => f.write_str("GLB document is missing its binary chunk"),
            Self::UnsafeExternalPath(uri) => {
                write!(f, "external buffer URI escapes document directory: {uri}")
            }
            Self::ExternalBufferRequiresResolver(uri) => write!(
                f,
                "external buffer `{uri}` requires an explicit host resolver"
            ),
            Self::MalformedDataUri => f.write_str("malformed data URI"),
            Self::UnsupportedDataUriEncoding => {
                f.write_str("only base64 data URI buffers are supported")
            }
            Self::DecodeDataUri(source) => write!(f, "could not decode base64 data URI: {source}"),
            Self::BufferTooShort {
                buffer,
                expected,
                actual,
            } => write!(
                f,
                "buffer {buffer} declares {expected} bytes but has {actual}"
            ),
            Self::EmbeddedImageRangeOverflow {
                image,
                offset,
                length,
            } => write!(
                f,
                "embedded image {image} range {offset}+{length} overflows host address space"
            ),
            Self::EmbeddedImageOutOfBounds {
                image,
                buffer,
                offset,
                length,
            } => write!(
                f,
                "embedded image {image} range {offset}+{length} is outside buffer {buffer}"
            ),
            Self::LimitExceeded { resource, limit } => {
                write!(f, "glTF {resource} exceeds configured limit {limit}")
            }
            Self::UnsupportedFeature(feature) => write!(f, "unsupported glTF feature: {feature}"),
            Self::UnsupportedPrimitiveMode {
                mesh,
                primitive,
                mode,
            } => write!(
                f,
                "mesh {mesh} primitive {primitive} uses unsupported mode {mode:?}"
            ),
            Self::NoRenderableTrianglePrimitives { skipped } => write!(
                f,
                "preview contains no triangle primitives; {skipped} non-triangle primitive(s) were skipped"
            ),
            Self::MissingPositions { mesh, primitive } => write!(
                f,
                "mesh {mesh} primitive {primitive} has no POSITION attribute"
            ),
            Self::MissingIndices { mesh, primitive } => {
                write!(f, "mesh {mesh} primitive {primitive} has no index accessor")
            }
            Self::UnsupportedTextureCoordinateSet(set) => write!(
                f,
                "texture coordinate set {set} is not supported; use TEXCOORD_0"
            ),
            Self::InvalidNodeTransform { node, issue } => {
                write!(f, "node {node} has invalid local transform: {issue:?}")
            }
            Self::InvalidCamera { camera } => {
                write!(f, "camera {camera} has non-finite projection data")
            }
            Self::InvalidDirectionalLight { light } => write!(
                f,
                "directional light {light} has invalid color or illuminance"
            ),
            Self::InvalidPrimitive {
                mesh,
                primitive,
                source,
            } => write!(f, "mesh {mesh} primitive {primitive} is invalid: {source}"),
            Self::InvalidMesh { mesh, source } => write!(f, "mesh {mesh} is invalid: {source}"),
            Self::InvalidModel(source) => write!(f, "imported model is invalid: {source}"),
        }
    }
}

impl Error for ImportError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::ReadDocument { source, .. }
            | Self::ReadBasePath { source, .. }
            | Self::ReadBuffer { source, .. } => Some(source),
            Self::Parse(source) => Some(source),
            Self::DecodeDataUri(source) => Some(source),
            Self::InvalidPrimitive { source, .. } => Some(source),
            Self::InvalidMesh { source, .. } => Some(source),
            Self::InvalidModel(source) => Some(source),
            _ => None,
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "Fixture values are copied exactly and validate lossless metadata."
)]
mod tests {
    use base64::Engine as _;
    use yuyib_assets::{ImportSource, ImporterRegistry};

    use super::*;

    #[test]
    fn registry_adapter_imports_embedded_glb_without_filesystem_access() {
        let mut registry = ImporterRegistry::default();
        registry
            .register(GltfAssetImporter::default())
            .expect("register glTF importer");
        let glb = valid_triangle_glb();
        let result = registry
            .import(ImportSource::new("triangle.glb", &glb))
            .expect("registry imports self-contained GLB");
        assert_eq!(result.importer.id, "yuyib.gltf");
        assert_eq!(result.asset.model.meshes().len(), 1);
    }

    #[test]
    fn importer_publishes_factor_only_and_unbound_material_diagnostics() {
        let mut registry = ImporterRegistry::default();
        registry
            .register(GltfAssetImporter::default())
            .expect("register glTF importer");
        let unbound = registry
            .import(ImportSource::new("triangle.glb", &valid_triangle_glb()))
            .expect("import unbound triangle");
        assert!(
            unbound
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "gltf-unbound-material"),
            "unbound primitives must be observable: {:?}",
            unbound.diagnostics
        );

        let mut binary = Vec::new();
        binary.extend([0_u16, 1, 2].into_iter().flat_map(u16::to_le_bytes));
        binary.extend([0_u8; 2]);
        for position in [[0.0_f32, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]] {
            binary.extend(position.into_iter().flat_map(f32::to_le_bytes));
        }
        let factor_only = glb_from_json_and_binary(
            br#"{"asset":{"version":"2.0"},"buffers":[{"byteLength":44}],"bufferViews":[{"buffer":0,"byteOffset":0,"byteLength":6},{"buffer":0,"byteOffset":8,"byteLength":36}],"accessors":[{"bufferView":0,"componentType":5123,"count":3,"type":"SCALAR"},{"bufferView":1,"componentType":5126,"count":3,"type":"VEC3","min":[0,0,0],"max":[1,1,0]}],"materials":[{"name":"material_0"}],"meshes":[{"primitives":[{"attributes":{"POSITION":1},"indices":0,"material":0}]}]}"#,
            binary,
        );
        let result = registry
            .import(ImportSource::new("material0.glb", &factor_only))
            .expect("import factor-only material");
        assert!(
            result
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "gltf-factor-only-material"),
            "factor-only materials must be observable: {:?}",
            result.diagnostics
        );
    }

    #[test]
    fn importer_publishes_unused_texture_diagnostics() {
        let mut registry = ImporterRegistry::default();
        registry
            .register(GltfAssetImporter::default())
            .expect("register glTF importer");
        let result = registry
            .import(ImportSource::new(
                "textured.glb",
                &embedded_image_fixture_glb(false),
            ))
            .expect("import textured fixture");
        // Fixture material references the embedded image; renaming source alone
        // is not enough for unused — inject a second unused URI texture via a
        // dedicated tiny document instead.
        let mut binary = Vec::new();
        binary.extend([0_u16, 1, 2].into_iter().flat_map(u16::to_le_bytes));
        binary.extend([0_u8; 2]);
        for position in [[0.0_f32, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]] {
            binary.extend(position.into_iter().flat_map(f32::to_le_bytes));
        }
        let unused = glb_from_json_and_binary(
            br#"{"asset":{"version":"2.0"},"buffers":[{"byteLength":44}],"bufferViews":[{"buffer":0,"byteOffset":0,"byteLength":6},{"buffer":0,"byteOffset":8,"byteLength":36}],"accessors":[{"bufferView":0,"componentType":5123,"count":3,"type":"SCALAR"},{"bufferView":1,"componentType":5126,"count":3,"type":"VEC3","min":[0,0,0],"max":[1,1,0]}],"images":[{"uri":"orphan.png"}],"textures":[{"source":0}],"materials":[{"name":"plain"}],"meshes":[{"primitives":[{"attributes":{"POSITION":1},"indices":0,"material":0}]}]}"#,
            binary,
        );
        let unused_result = registry
            .import(ImportSource::new("unused.glb", &unused))
            .expect("import unused texture");
        assert!(
            unused_result
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "gltf-unused-texture"),
            "unused textures must be observable: {:?}",
            unused_result.diagnostics
        );
        assert!(
            unused_result
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "gltf-external-texture-uri"),
            "external URIs must be observable: {:?}",
            unused_result.diagnostics
        );
        let _ = result;
    }

    #[test]
    fn byte_only_adapter_never_opens_external_buffers() {
        let json = br#"{
            "asset":{"version":"2.0"},
            "buffers":[{"uri":"mesh.bin","byteLength":1}]
        }"#;
        let error = import_scene_bytes_embedded(json, ImportOptions::default())
            .expect_err("external dependency needs host resolver");
        assert!(matches!(
            error,
            ImportError::ExternalBufferRequiresResolver(uri) if uri == "mesh.bin"
        ));
    }

    #[test]
    fn discover_external_dependencies_classifies_buffers_and_images() {
        let json = br#"{
            "asset":{"version":"2.0"},
            "buffers":[
                {"uri":"mesh.bin","byteLength":1},
                {"uri":"data:application/octet-stream;base64,AA==","byteLength":1},
                {"uri":"mesh.bin","byteLength":1}
            ],
            "images":[
                {"uri":"albedo.png"},
                {"uri":"data:image/png;base64,AA=="},
                {"uri":"mesh.bin"}
            ]
        }"#;
        let deps = discover_external_dependencies(json).expect("discover");
        assert_eq!(
            deps,
            vec![
                ImportDependency {
                    uri: "mesh.bin".to_owned(),
                    kind: ImportDependencyKind::Required,
                },
                ImportDependency {
                    uri: "albedo.png".to_owned(),
                    kind: ImportDependencyKind::Optional,
                },
            ]
        );
    }

    const _TRIANGLE_GLB: &[u8] = &[
        0x67, 0x6C, 0x54, 0x46, 0x02, 0, 0, 0, 0xCC, 0, 0, 0, 0xA4, 0, 0, 0, 0x4A, 0x53, 0x4F,
        0x4E, b'{', b'"', b'a', b's', b's', b'e', b't', b'"', b':', b'{', b'"', b'v', b'e', b'r',
        b's', b'i', b'o', b'n', b'"', b':', b'"', b'2', b'.', b'0', b'"', b'}', b',', b'"', b'b',
        b'u', b'f', b'f', b'e', b'r', b's', b'"', b':', b'[', b'{', b'"', b'b', b'y', b't', b'e',
        b'L', b'e', b'n', b'g', b't', b'h', b'"', b':', b'4', b'2', b'}', b']', b',', b'"', b'b',
        b'u', b'f', b'f', b'e', b'r', b'V', b'i', b'e', b'w', b's', b'"', b':', b'[', b'{', b'"',
        b'b', b'u', b'f', b'f', b'e', b'r', b'"', b':', b'0', b',', b'"', b'b', b'y', b't', b'e',
        b'L', b'e', b'n', b'g', b't', b'h', b'"', b':', b'4', b'2', b'}', b']', b',', b'"', b'a',
        b'c', b'c', b'e', b's', b's', b'o', b'r', b's', b'"', b':', b'[', b'{', b'"', b'b', b'u',
        b'f', b'f', b'e', b'r', b'V', b'i', b'e', b'w', b'"', b':', b'0', b',', b'"', b'c', b'o',
        b'm', b'p', b'o', b'n', b'e', b'n', b't', b'T', b'y', b'p', b'e', b'"', b':', b'5', b'1',
        b'2', b'6', b',', b'"', b'c', b'o', b'u', b'n', b't', b'"', b':', b'3', b',', b'"', b't',
        b'y', b'p', b'e', b'"', b':', b'"', b'S', b'C', b'A', b'L', b'A', b'R', b'"', b'}', b',',
        b'{', b'"', b'b', b'u', b'f', b'f', b'e', b'r', b'V', b'i', b'e', b'w', b'"', b':', b'0',
        b',', b'"', b'b', b'y', b't', b'e', b'O', b'f', b'f', b's', b'e', b't', b'"', b':', b'6',
        b',', b'"', b'c', b'o', b'm', b'p', b'o', b'n', b'e', b'n', b't', b'T', b'y', b'p', b'e',
        b'"', b':', b'5', b'1', b'2', b'6', b',', b'"', b'c', b'o', b'u', b'n', b't', b'"', b':',
        b'3', b',', b'"', b't', b'y', b'p', b'e', b'"', b':', b'"', b'V', b'E', b'C', b'3', b'"',
        b'}', b']', b',', b'"', b'm', b'e', b's', b'h', b'e', b's', b'"', b':', b'[', b'{', b'"',
        b'p', b'r', b'i', b'm', b'i', b't', b'i', b'v', b'e', b's', b'"', b':', b'[', b'{', b'"',
        b'a', b't', b't', b'r', b'i', b'b', b'u', b't', b'e', b's', b'"', b':', b'{', b'"', b'P',
        b'O', b'S', b'I', b'T', b'I', b'O', b'N', b'"', b':', b'1', b'}', b',', b'"', b'i', b'n',
        b'd', b'i', b'c', b'e', b's', b'"', b':', b'0', b'}', b']', b'}', b']', b'}', b' ', b' ',
        b' ', b' ', 0x2A, 0, 0, 0, 0x42, 0x49, 0x4E, 0x00, 0, 0, 1, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x80, 0x3F, 0, 0, 0, 0,
    ];

    fn valid_triangle_glb() -> Vec<u8> {
        let mut binary = Vec::new();
        binary.extend([0_u16, 1, 2].into_iter().flat_map(u16::to_le_bytes));
        binary.extend([0_u8; 2]);
        for position in [[0.0_f32, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]] {
            binary.extend(position.into_iter().flat_map(f32::to_le_bytes));
        }
        glb_from_json_and_binary(
            br#"{"asset":{"version":"2.0"},"buffers":[{"byteLength":44}],"bufferViews":[{"buffer":0,"byteOffset":0,"byteLength":6},{"buffer":0,"byteOffset":8,"byteLength":36}],"accessors":[{"bufferView":0,"componentType":5123,"count":3,"type":"SCALAR"},{"bufferView":1,"componentType":5126,"count":3,"type":"VEC3","min":[0,0,0],"max":[1,1,0]}],"meshes":[{"primitives":[{"attributes":{"POSITION":1},"indices":0}]}]}"#,
            binary,
        )
    }

    /// Builds a minimal valid GLB and deliberately pads only its JSON chunk.
    ///
    /// glTF declares the actual buffer length separately, so the binary chunk
    /// remains byte-for-byte equal to the source fixture payload.
    fn glb_from_json_and_binary(json: &[u8], binary: Vec<u8>) -> Vec<u8> {
        let mut json = json.to_vec();
        while !json.len().is_multiple_of(4) {
            json.push(b' ');
        }
        let total = 12 + 8 + json.len() + 8 + binary.len();
        let mut glb = Vec::with_capacity(total);
        glb.extend(b"glTF");
        glb.extend(2_u32.to_le_bytes());
        glb.extend(u32::try_from(total).expect("small test GLB").to_le_bytes());
        glb.extend(
            u32::try_from(json.len())
                .expect("small JSON chunk")
                .to_le_bytes(),
        );
        glb.extend(*b"JSON");
        glb.extend(json);
        glb.extend(
            u32::try_from(binary.len())
                .expect("small BIN chunk")
                .to_le_bytes(),
        );
        glb.extend([b'B', b'I', b'N', 0]);
        glb.extend(binary);
        glb
    }

    #[test]
    fn imports_indexed_glb_triangle() {
        let glb = valid_triangle_glb();
        let model = import_bytes_with_base_path(&glb, ".", ImportOptions::default())
            .expect("valid GLB triangle");
        let primitive = &model.meshes()[0].primitives()[0];
        assert_eq!(primitive.positions().len(), 3);
        assert_eq!(primitive.indices(), [0, 1, 2]);
    }

    fn triangle_and_line_fixture_glb() -> Vec<u8> {
        let mut binary = Vec::new();
        binary.extend([0_u16, 1, 2].into_iter().flat_map(u16::to_le_bytes));
        binary.extend([0_u8; 2]);
        for position in [[0.0_f32, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]] {
            binary.extend(position.into_iter().flat_map(f32::to_le_bytes));
        }
        glb_from_json_and_binary(
            br#"{"asset":{"version":"2.0"},"buffers":[{"byteLength":44}],"bufferViews":[{"buffer":0,"byteOffset":0,"byteLength":6},{"buffer":0,"byteOffset":8,"byteLength":36}],"accessors":[{"bufferView":0,"componentType":5123,"count":3,"type":"SCALAR"},{"bufferView":1,"componentType":5126,"count":3,"type":"VEC3","min":[0,0,0],"max":[1,1,0]}],"meshes":[{"primitives":[{"attributes":{"POSITION":1},"indices":0,"mode":1},{"attributes":{"POSITION":1},"indices":0}]}],"nodes":[{"mesh":0}],"scenes":[{"nodes":[0]}],"scene":0}"#,
            binary,
        )
    }

    #[test]
    fn skeletal_preview_skips_non_triangles_and_keeps_scene_mesh_mapping() {
        let glb = triangle_and_line_fixture_glb();
        let strict_error = import_scene_bytes_with_base_path(&glb, ".", ImportOptions::skeletal())
            .expect_err("normal skeletal import must still reject line geometry");
        assert!(matches!(
            strict_error,
            ImportError::UnsupportedPrimitiveMode {
                mesh: 0,
                primitive: 0,
                mode: gltf::mesh::Mode::Lines,
            }
        ));

        let asset = import_scene_bytes_with_base_path(&glb, ".", ImportOptions::skeletal_preview())
            .expect("preview explicitly skips the helper line and imports the triangle");
        assert_eq!(asset.model.meshes().len(), 1);
        assert_eq!(asset.model.meshes()[0].primitives().len(), 1);
        assert_eq!(asset.scene.nodes()[0].mesh(), Some(0));
        assert_eq!(asset.report().skipped_primitive_count(), 1);
        assert_eq!(
            asset.report().skipped_primitives(),
            [SkippedPrimitive {
                mesh: 0,
                primitive: 0,
                mode: gltf::mesh::Mode::Lines,
            }]
        );
    }

    fn static_preview_fixture_glb() -> Vec<u8> {
        let mut binary = Vec::new();
        binary.extend([0_u16, 1, 2].into_iter().flat_map(u16::to_le_bytes));
        binary.extend([0_u8; 2]);
        for position in [[0.0_f32, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]] {
            binary.extend(position.into_iter().flat_map(f32::to_le_bytes));
        }
        for uv in [[0.0_f32, 0.0], [1.0, 0.0], [0.0, 1.0]] {
            binary.extend(uv.into_iter().flat_map(f32::to_le_bytes));
        }
        for joints in [[0_u16, 0, 0, 0]; 3] {
            binary.extend(joints.into_iter().flat_map(u16::to_le_bytes));
        }
        for weights in [[1.0_f32, 0.0, 0.0, 0.0]; 3] {
            binary.extend(weights.into_iter().flat_map(f32::to_le_bytes));
        }
        binary.extend([0.0_f32, 1.0].into_iter().flat_map(f32::to_le_bytes));
        for translation in [[0.0_f32, 0.0, 0.0], [0.0, 1.0, 0.0]] {
            binary.extend(translation.into_iter().flat_map(f32::to_le_bytes));
        }
        assert_eq!(binary.len(), 172);
        glb_from_json_and_binary(
            br#"{"asset":{"version":"2.0"},"buffers":[{"byteLength":172}],"bufferViews":[{"buffer":0,"byteOffset":0,"byteLength":6},{"buffer":0,"byteOffset":8,"byteLength":36},{"buffer":0,"byteOffset":44,"byteLength":24},{"buffer":0,"byteOffset":68,"byteLength":24},{"buffer":0,"byteOffset":92,"byteLength":48},{"buffer":0,"byteOffset":140,"byteLength":8},{"buffer":0,"byteOffset":148,"byteLength":24}],"accessors":[{"bufferView":0,"componentType":5123,"count":3,"type":"SCALAR"},{"bufferView":1,"componentType":5126,"count":3,"type":"VEC3","min":[0,0,0],"max":[1,1,0]},{"bufferView":2,"componentType":5126,"count":3,"type":"VEC2"},{"bufferView":3,"componentType":5123,"count":3,"type":"VEC4"},{"bufferView":4,"componentType":5126,"count":3,"type":"VEC4"},{"bufferView":5,"componentType":5126,"count":2,"type":"SCALAR"},{"bufferView":6,"componentType":5126,"count":2,"type":"VEC3"}],"meshes":[{"primitives":[{"attributes":{"POSITION":1,"TEXCOORD_1":2,"JOINTS_0":3,"WEIGHTS_0":4},"indices":0}]}],"nodes":[{"mesh":0,"skin":0},{"name":"Joint"}],"skins":[{"joints":[1]}],"animations":[{"samplers":[{"input":5,"output":6,"interpolation":"LINEAR"}],"channels":[{"sampler":0,"target":{"node":0,"path":"translation"}}]}]}"#,
            binary,
        )
    }

    #[test]
    fn static_preview_explicitly_discards_rigging_data_without_weakening_strict_import() {
        let glb = static_preview_fixture_glb();
        let strict_error = import_bytes_with_base_path(&glb, ".", ImportOptions::default())
            .expect_err("strict import must still reject a rigged source asset");
        assert!(matches!(
            strict_error,
            ImportError::UnsupportedFeature("vertex attribute")
        ));

        let model = import_bytes_with_base_path(&glb, ".", ImportOptions::static_preview())
            .expect("static preview ignores only the explicitly documented rigging data");
        assert_eq!(model.meshes().len(), 1);
        assert_eq!(model.meshes()[0].primitives()[0].positions().len(), 3);
        assert_eq!(model.meshes()[0].primitives()[0].tex_coords_0(), None);
        assert_eq!(
            model.meshes()[0].primitives()[0].tex_coords(1),
            Some(&[[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]][..])
        );
    }

    #[test]
    fn material_texture_binding_preserves_non_primary_uv_set() {
        assert_eq!(
            convert_texture_parts(3, 2).expect("UV2 is inside the glTF core range"),
            TextureBinding::new(ModelTextureIndex::new(3), 2)
        );
        assert!(matches!(
            convert_texture_parts(3, 8),
            Err(ImportError::UnsupportedTextureCoordinateSet(8))
        ));
    }

    #[test]
    fn emissive_strength_extension_is_folded_into_linear_emissive_colour() {
        let gltf = gltf::Gltf::from_slice(
            br#"{
                "asset":{"version":"2.0"},
                "extensionsUsed":["KHR_materials_emissive_strength"],
                "materials":[{
                    "emissiveFactor":[1.0,0.5,0.25],
                    "extensions":{"KHR_materials_emissive_strength":{"emissiveStrength":2.0}}
                }]
            }"#,
        )
        .expect("valid emissive-strength glTF");
        let materials = convert_materials(&gltf.document).expect("extension converts");
        assert_eq!(materials[0].emissive_factor(), [2.0, 1.0, 0.5]);
    }

    #[test]
    fn static_preview_does_not_accept_unrelated_vertex_attributes() {
        let mut glb = static_preview_fixture_glb();
        let source = b"JOINTS_0";
        let replacement = b"COLOR_00";
        let location = glb
            .windows(source.len())
            .position(|window| window == source)
            .expect("fixture contains the extra UV semantic");
        glb[location..location + source.len()].copy_from_slice(replacement);
        let error = import_bytes_with_base_path(&glb, ".", ImportOptions::static_preview())
            .expect_err("preview must not silently discard vertex colours");
        assert!(matches!(
            error,
            ImportError::UnsupportedFeature("vertex attribute")
        ));
    }

    #[test]
    fn skeletal_import_samples_trs_tracks_and_builds_a_gpu_palette_snapshot() {
        let glb = static_preview_fixture_glb();
        let asset = import_scene_bytes_with_base_path(&glb, ".", ImportOptions::skeletal())
            .expect("skeletal policy imports JOINTS_0, WEIGHTS_0, skin and TRS animation");
        assert_eq!(asset.scene.skins().len(), 1);
        assert_eq!(asset.scene.skins()[0].joints(), [NodeIndex::new(1)]);
        assert_eq!(asset.scene.skinned_primitives().len(), 1);
        assert_eq!(asset.scene.skinned_primitives()[0].vertices().len(), 3);
        assert_eq!(asset.scene.animations().len(), 1);
        assert_eq!(asset.scene.animations()[0].duration_seconds(), 1.0);

        let mut player = AnimationPlayer::new(AnimationClipIndex::new(0)).with_looping(false);
        player
            .advance(&asset.scene, 0.5)
            .expect("finite frame delta advances the selected clip");
        let snapshot = player
            .snapshot(&asset.scene)
            .expect("sampled pose is valid");
        assert!(matches!(
            snapshot.local_transforms()[0],
            LocalTransform::Trs {
                translation: [0.0, 0.5, 0.0],
                ..
            }
        ));
        assert_eq!(snapshot.skin_palettes().len(), 1);
        assert_eq!(snapshot.skin_palettes()[0].matrices().len(), 1);
        assert!((snapshot.skin_palettes()[0].matrices()[0][13] + 0.5).abs() < 1.0e-6);

        player.pause();
        player
            .advance(&asset.scene, 1.0)
            .expect("paused player accepts a normal delta");
        assert_eq!(player.time_seconds(), 0.5);
        player.play();
        player
            .advance(&asset.scene, 1.0)
            .expect("non-looping player clamps at clip end");
        assert_eq!(player.time_seconds(), 1.0);
        assert_eq!(player.state(), AnimationPlayState::Paused);
    }

    #[test]
    fn skeletal_import_keeps_skin_limits_bounded() {
        let error = import_scene_bytes_with_base_path(
            &static_preview_fixture_glb(),
            ".",
            ImportOptions {
                limits: ImportLimits {
                    max_skin_joints: 0,
                    ..ImportLimits::default()
                },
                policy: ImportPolicy::Skeletal,
            },
        )
        .expect_err("a skin joint must respect the configured budget");
        assert!(matches!(
            error,
            ImportError::LimitExceeded {
                resource: "skin joints",
                limit: 0
            }
        ));
    }

    #[test]
    #[ignore = "uses the optional user-provided integration fixture"]
    fn velina_skeletal_import_preserves_transparent_material_metadata() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("for_tests")
            .join("velina_zzz.glb");
        let asset = import_scene_path_with_options(path, ImportOptions::skeletal())
            .expect("skeletal import must preserve the source alpha contract");
        assert!(
            asset
                .model
                .materials()
                .iter()
                .any(|material| material.alpha_mode() == AlphaMode::Blend)
        );
        assert!(!asset.scene.skins().is_empty());
    }

    #[test]
    #[ignore = "uses the optional user-provided integration fixture"]
    fn sci_fi_girl_preview_keeps_walk_skin_and_cloth_morphs() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("for_tests")
            .join("sci-fi_girl_v.02_walkcycle_test.glb");
        let asset = import_scene_path_with_options(path, ImportOptions::skeletal_preview())
            .expect("skeletal preview should retain the walk-cycle TRS tracks");
        assert_eq!(asset.scene.skins().len(), 1);
        assert_eq!(asset.scene.animations().len(), 1);
        assert_eq!(asset.scene.animations()[0].name(), Some("walk"));
        assert_eq!(asset.scene.morph_primitives().len(), 1);
        assert_eq!(asset.scene.morph_primitives()[0].targets().len(), 29);
        assert_eq!(asset.scene.animations()[0].morph_tracks().len(), 1);
        assert_eq!(
            asset.scene.animations()[0].morph_tracks()[0]
                .weights()
                .len(),
            188
        );
        let pose = sample_animation(&asset.scene, AnimationClipIndex::new(0), 2.0)
            .expect("walk morph track should sample");
        assert_eq!(
            pose.morph_weights(NodeIndex::new(162)).map(<[f32]>::len),
            Some(29)
        );
    }

    fn embedded_image_fixture_json(specular_glossiness: bool) -> String {
        let binary = embedded_image_fixture_binary();
        let encoded = base64::engine::general_purpose::STANDARD.encode(binary);
        let (extensions, material) = if specular_glossiness {
            (
                r#","extensionsUsed":["KHR_materials_pbrSpecularGlossiness"],"extensionsRequired":["KHR_materials_pbrSpecularGlossiness"]"#,
                r#"{"extensions":{"KHR_materials_pbrSpecularGlossiness":{"diffuseFactor":[0.2,0.3,0.4,1],"diffuseTexture":{"index":0},"specularFactor":[0.5,0.6,0.7],"glossinessFactor":0.8}}}"#,
            )
        } else {
            (
                "",
                r#"{"pbrMetallicRoughness":{"baseColorTexture":{"index":0}}}"#,
            )
        };
        [
            r#"{"asset":{"version":"2.0"},"buffers":[{"uri":"data:application/octet-stream;base64,"#,
            &encoded,
            r#"","byteLength":48}],"bufferViews":[{"buffer":0,"byteOffset":0,"byteLength":6},{"buffer":0,"byteOffset":8,"byteLength":36},{"buffer":0,"byteOffset":44,"byteLength":4}],"accessors":[{"bufferView":0,"componentType":5123,"count":3,"type":"SCALAR"},{"bufferView":1,"componentType":5126,"count":3,"type":"VEC3","min":[0,0,0],"max":[1,1,0]}],"images":[{"bufferView":2,"mimeType":"image/png"}],"textures":[{"source":0}],"materials":["#,
            material,
            r#"],"meshes":[{"primitives":[{"attributes":{"POSITION":1},"indices":0,"material":0}]}]"#,
            extensions,
            "}",
        ]
        .concat()
    }

    fn embedded_image_fixture_binary() -> Vec<u8> {
        let mut binary = Vec::new();
        binary.extend([0_u16, 1, 2].into_iter().flat_map(u16::to_le_bytes));
        binary.extend([0_u8; 2]);
        for position in [[0.0_f32, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]] {
            binary.extend(position.into_iter().flat_map(f32::to_le_bytes));
        }
        binary.extend([0x89, b'P', b'N', b'G']);
        binary
    }

    fn embedded_image_fixture_glb(specular_glossiness: bool) -> Vec<u8> {
        let (extensions, material) = if specular_glossiness {
            (
                r#","extensionsUsed":["KHR_materials_pbrSpecularGlossiness"],"extensionsRequired":["KHR_materials_pbrSpecularGlossiness"]"#,
                r#"{"extensions":{"KHR_materials_pbrSpecularGlossiness":{"diffuseFactor":[0.2,0.3,0.4,1],"diffuseTexture":{"index":0},"specularFactor":[0.5,0.6,0.7],"glossinessFactor":0.8}}}"#,
            )
        } else {
            (
                "",
                r#"{"pbrMetallicRoughness":{"baseColorTexture":{"index":0}}}"#,
            )
        };
        let json = [
            r#"{"asset":{"version":"2.0"},"buffers":[{"byteLength":48}],"bufferViews":[{"buffer":0,"byteOffset":0,"byteLength":6},{"buffer":0,"byteOffset":8,"byteLength":36},{"buffer":0,"byteOffset":44,"byteLength":4}],"accessors":[{"bufferView":0,"componentType":5123,"count":3,"type":"SCALAR"},{"bufferView":1,"componentType":5126,"count":3,"type":"VEC3","min":[0,0,0],"max":[1,1,0]}],"images":[{"bufferView":2,"mimeType":"image/png"}],"textures":[{"source":0}],"materials":["#,
            material,
            r#"],"meshes":[{"primitives":[{"attributes":{"POSITION":1},"indices":0,"material":0}]}]"#,
            extensions,
            "}",
        ]
        .concat();
        glb_from_json_and_binary(json.as_bytes(), embedded_image_fixture_binary())
    }

    #[test]
    fn imports_embedded_image_buffer_view_with_a_bounded_source_contract() {
        let glb = embedded_image_fixture_glb(false);
        let model = import_bytes_with_base_path(&glb, ".", ImportOptions::default())
            .expect("embedded PNG payload is preserved without decoding it");
        assert_eq!(model.textures()[0].uri(), None);
        assert_eq!(
            model.textures()[0].encoded(),
            Some(("image/png", &[0x89, b'P', b'N', b'G'][..]))
        );

        let error = import_bytes_with_base_path(
            &glb,
            ".",
            ImportOptions {
                limits: ImportLimits {
                    max_embedded_image_bytes: 3,
                    ..ImportLimits::default()
                },
                ..ImportOptions::default()
            },
        )
        .expect_err("four encoded image bytes exceed the configured limit");
        assert!(matches!(
            error,
            ImportError::LimitExceeded {
                resource: "embedded image bytes",
                limit: 3
            }
        ));
    }

    #[test]
    fn imports_texture_sampler_without_changing_wrap_or_filter_settings() {
        let json = embedded_image_fixture_json(false).replacen(
            r#""textures":[{"source":0}]"#,
            r#""samplers":[{"wrapS":33071,"wrapT":33648,"magFilter":9728,"minFilter":9987}],"textures":[{"sampler":0,"source":0}]"#,
            1,
        );
        let model = import_bytes_with_base_path(json.as_bytes(), ".", ImportOptions::default())
            .expect("sampler metadata does not change a valid texture source");
        assert_eq!(
            model.textures()[0].sampler(),
            Some(ModelTextureSampler {
                address_mode_u: ModelTextureAddressMode::ClampToEdge,
                address_mode_v: ModelTextureAddressMode::MirroredRepeat,
                mag_filter: ModelTextureMagFilter::Nearest,
                min_filter: ModelTextureMinFilter::LinearMipmapLinear,
            })
        );
    }

    #[test]
    fn imports_required_specular_glossiness_without_silent_workflow_conversion() {
        let glb = embedded_image_fixture_glb(true);
        let model = import_bytes_with_base_path(&glb, ".", ImportOptions::default())
            .expect("the explicitly supported required GLB extension imports");
        let workflow = model.materials()[0]
            .specular_glossiness()
            .expect("extension workflow is retained");
        assert_eq!(workflow.diffuse_factor(), [0.2, 0.3, 0.4, 1.0]);
        assert_eq!(workflow.specular_factor(), [0.5, 0.6, 0.7]);
        assert_eq!(workflow.glossiness_factor(), 0.8);
        assert_eq!(
            workflow.diffuse_texture(),
            Some(TextureBinding::new(ModelTextureIndex::new(0), 0))
        );
        assert_eq!(model.materials()[0].base_color_texture(), None);
    }

    #[test]
    fn imports_double_sided_material_as_typed_rasterization_metadata() {
        let json = embedded_image_fixture_json(false).replacen(
            r#"{"pbrMetallicRoughness":{"baseColorTexture":{"index":0}}}"#,
            r#"{"doubleSided":true,"pbrMetallicRoughness":{"baseColorTexture":{"index":0}}}"#,
            1,
        );
        let model = import_bytes_with_base_path(json.as_bytes(), ".", ImportOptions::default())
            .expect("opaque double-sided material is losslessly representable");
        assert!(model.materials()[0].double_sided());
    }

    #[test]
    fn imports_blend_and_mask_alpha_metadata_without_dropping_source_policy() {
        let blend = embedded_image_fixture_json(false).replacen(
            r#"{"pbrMetallicRoughness":{"baseColorTexture":{"index":0}}}"#,
            r#"{"alphaMode":"BLEND","pbrMetallicRoughness":{"baseColorTexture":{"index":0}}}"#,
            1,
        );
        let blend = import_bytes_with_base_path(blend.as_bytes(), ".", ImportOptions::default())
            .expect("blend is preserved for a compatible renderer phase");
        assert_eq!(blend.materials()[0].alpha_mode(), AlphaMode::Blend);

        let mask = embedded_image_fixture_json(false).replacen(
            r#"{"pbrMetallicRoughness":{"baseColorTexture":{"index":0}}}"#,
            r#"{"alphaMode":"MASK","alphaCutoff":0.25,"pbrMetallicRoughness":{"baseColorTexture":{"index":0}}}"#,
            1,
        );
        let mask = import_bytes_with_base_path(mask.as_bytes(), ".", ImportOptions::default())
            .expect("mask source data is preserved for a later masked pass");
        assert_eq!(
            mask.materials()[0].alpha_mode(),
            AlphaMode::Mask { cutoff: 0.25 }
        );
    }

    fn scene_fixture_json(matrix_transform: bool, directional_light: bool) -> String {
        let mut binary = Vec::new();
        binary.extend([0_u16, 1, 2].into_iter().flat_map(u16::to_le_bytes));
        binary.extend([0_u8; 2]);
        for position in [[0.0_f32, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]] {
            binary.extend(position.into_iter().flat_map(f32::to_le_bytes));
        }
        let encoded = base64::engine::general_purpose::STANDARD.encode(binary);
        let transform = if matrix_transform {
            r#","matrix":[1,0,0,0,0,1,0,0,0,0,1,0,1,2,3,1]"#
        } else {
            r#","translation":[1,2,3],"rotation":[0,0,0,1]"#
        };
        let light_extension = if directional_light {
            r#","extensionsUsed":["KHR_lights_punctual"],"extensions":{"KHR_lights_punctual":{"lights":[{"name":"Sun","type":"directional","color":[1,0.5,0.25],"intensity":4}]}}"#
        } else {
            ""
        };
        let light_node = if directional_light {
            r#","extensions":{"KHR_lights_punctual":{"light":0}}"#
        } else {
            ""
        };
        [
            r#"{"asset":{"version":"2.0"},"buffers":[{"uri":"data:application/octet-stream;base64,"#,
            &encoded,
            r#"","byteLength":44}],"bufferViews":[{"buffer":0,"byteOffset":0,"byteLength":6},{"buffer":0,"byteOffset":8,"byteLength":36}],"accessors":[{"bufferView":0,"componentType":5123,"count":3,"type":"SCALAR"},{"bufferView":1,"componentType":5126,"count":3,"type":"VEC3","min":[0,0,0],"max":[1,1,0]}],"meshes":[{"name":"Triangle","primitives":[{"attributes":{"POSITION":1},"indices":0}]}],"cameras":[{"name":"Lens","type":"perspective","perspective":{"yfov":1,"znear":0.1}}],"nodes":[{"name":"Parent","mesh":0,"children":[1]"#,
            transform,
            r#"},{"name":"Camera","camera":0,"scale":[2,2,2]}"#,
            if directional_light { r#",{"name":"SunNode""# } else { "" },
            light_node,
            if directional_light { "}" } else { "" },
            r#"],"scenes":[{"name":"Main","nodes":[0]}],"scene":0"#,
            light_extension,
            "}",
        ]
        .concat()
    }

    #[test]
    fn imports_scene_hierarchy_camera_and_directional_light_without_flattening() {
        let json = scene_fixture_json(false, true);
        let asset =
            import_scene_bytes_with_base_path(json.as_bytes(), ".", ImportOptions::default())
                .expect("valid scene fixture");
        assert_eq!(asset.model.meshes().len(), 1);
        assert_eq!(asset.scene.default_scene(), Some(SceneIndex::new(0)));
        assert_eq!(asset.scene.scenes()[0].roots(), [NodeIndex::new(0)]);
        assert_eq!(asset.scene.nodes()[0].children(), [NodeIndex::new(1)]);
        assert_eq!(asset.scene.nodes()[0].mesh(), Some(0));
        assert!(matches!(
            asset.scene.nodes()[0].local_transform(),
            LocalTransform::Trs {
                translation: [1.0, 2.0, 3.0],
                ..
            }
        ));
        assert_eq!(asset.scene.nodes()[1].camera(), Some(CameraIndex::new(0)));
        assert!(matches!(
            asset.scene.cameras()[0].projection(),
            CameraProjection::Perspective {
                aspect_ratio: None,
                zfar: None,
                ..
            }
        ));
        assert_eq!(
            asset.scene.nodes()[2].directional_light(),
            Some(DirectionalLightIndex::new(0))
        );
        assert_eq!(asset.scene.directional_lights()[0].illuminance_lux(), 4.0);
    }

    #[test]
    fn retains_node_matrix_instead_of_heuristic_decomposition() {
        let json = scene_fixture_json(true, false);
        let asset =
            import_scene_bytes_with_base_path(json.as_bytes(), ".", ImportOptions::default())
                .expect("finite affine matrix is retained exactly");
        assert!(matches!(
            asset.scene.nodes()[0].local_transform(),
            LocalTransform::Matrix {
                column_major: [
                    1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 1.0, 2.0, 3.0, 1.0
                ]
            }
        ));
    }

    #[test]
    fn rejects_triangle_without_indices() {
        let json = br#"{"asset":{"version":"2.0"},"buffers":[{"uri":"data:application/octet-stream;base64,AAAAAAAAAAAAAAAA","byteLength":12}],"bufferViews":[{"buffer":0,"byteLength":12}],"accessors":[{"bufferView":0,"componentType":5126,"count":1,"type":"VEC3","min":[0,0,0],"max":[0,0,0]}],"meshes":[{"primitives":[{"attributes":{"POSITION":0}}]}]}"#;
        let error = import_bytes_with_base_path(json, ".", ImportOptions::default())
            .expect_err("indices are intentionally required");
        assert!(matches!(error, ImportError::MissingIndices { .. }));
    }

    #[test]
    fn rejects_buffer_budget_overrun() {
        let json = br#"{"asset":{"version":"2.0"},"buffers":[{"uri":"data:application/octet-stream;base64,AAAAAAAA","byteLength":6}]}"#;
        let options = ImportOptions {
            limits: ImportLimits {
                max_buffer_bytes: 1,
                ..ImportLimits::default()
            },
            ..ImportOptions::default()
        };
        let error =
            import_bytes_with_base_path(json, ".", options).expect_err("six bytes exceed budget");
        assert!(matches!(
            error,
            ImportError::LimitExceeded {
                resource: "buffer bytes",
                ..
            }
        ));
    }
}
