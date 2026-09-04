//! Standalone Source 1 `StudioModel` loading into renderer-neutral Yuyib models.

use std::{
    collections::{HashMap, HashSet},
    error::Error,
    fmt, fs,
    path::{Component, Path, PathBuf},
};

use yuyib_model::{
    AlphaMode, Material, MaterialIndex, Mesh, MeshPrimitive, Model, ModelTexture,
    ModelTextureAddressMode, ModelTextureIndex, ModelTextureMagFilter, ModelTextureMinFilter,
    ModelTextureRgba8Error, ModelTextureSampler, TextureBinding,
};
use yuyib_source1_assets::{
    Source1AssetError, Source1MaterialAlphaMode, Source1MaterialResolver, Source1ResolvedMaterial,
};

use crate::{
    Source1AnimationSet, Source1StaticPropTransform, Source1StudioError, Source1StudioLimits,
    Source1StudioMaterial, Source1StudioModel, Source1StudioModelFiles,
};

/// Policy for a `StudioModel` material that cannot resolve its VMT/VTF chain.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Source1ModelMaterialPolicy {
    /// Reject the import. This is the production-safe default.
    #[default]
    RequireTextures,
    /// Retain geometry with a deterministic coloured factor material and report it.
    FactorFallback,
}

/// Standalone `StudioModel` import policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Source1ModelImportOptions {
    /// Bounded-work policy for MDL/VVD/VTX decoding.
    pub studio_limits: Source1StudioLimits,
    /// Source skin-family index. Invalid values select skin zero like Source.
    pub skin: i32,
    /// Source body-group integer. Each body part selects `(body / base) % model_count`.
    pub body: i32,
    /// Missing/invalid material behaviour.
    pub material_policy: Source1ModelMaterialPolicy,
}

impl Default for Source1ModelImportOptions {
    fn default() -> Self {
        Self {
            studio_limits: Source1StudioLimits::default(),
            skin: 0,
            body: 0,
            material_policy: Source1ModelMaterialPolicy::RequireTextures,
        }
    }
}

/// Deterministic diagnostics and workload counts from one MDL import.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Source1ModelImportReport {
    /// Canonical Source-relative MDL path.
    pub model_path: String,
    /// MDL format version.
    pub mdl_version: i32,
    /// Cross-sidecar checksum.
    pub checksum: i32,
    /// Requested skin-family index.
    pub skin: i32,
    /// Requested body-group integer.
    pub body: i32,
    /// Decoded LOD-0 meshes.
    pub meshes: usize,
    /// Unique runtime material slots.
    pub materials: usize,
    /// Runtime materials with decoded VTF base textures.
    pub textured_materials: usize,
    /// Total decoded vertices.
    pub vertices: usize,
    /// Total decoded triangles.
    pub triangles: usize,
    /// Material names retained on the explicit factor fallback path.
    pub fallback_materials: Vec<String>,
    /// Bones decoded from the owning MDL.
    pub bones: usize,
    /// Playable local and resolved include-model sequences.
    pub animation_clips: usize,
    /// `$includemodel` paths successfully merged.
    pub included_animation_models: Vec<String>,
    /// Optional `$includemodel` paths absent below the content root.
    pub missing_animation_models: Vec<String>,
}

/// CPU-side result of a standalone Source 1 MDL import.
#[derive(Clone, Debug, PartialEq)]
pub struct LoadedSource1Model {
    model: Model,
    skin_vertices: Vec<Vec<crate::Source1SkinVertex>>,
    animations: Source1AnimationSet,
    report: Source1ModelImportReport,
}

impl LoadedSource1Model {
    /// Returns the renderer-neutral model.
    #[must_use]
    pub const fn model(&self) -> &Model {
        &self.model
    }

    /// Returns deterministic import diagnostics.
    #[must_use]
    pub const fn report(&self) -> &Source1ModelImportReport {
        &self.report
    }

    /// Returns the `StudioModel` skeleton and animation sequences.
    #[must_use]
    pub const fn animations(&self) -> &Source1AnimationSet {
        &self.animations
    }

    /// Returns one skin stream per renderer-neutral model mesh.
    #[must_use]
    pub fn skin_vertices(&self) -> &[Vec<crate::Source1SkinVertex>] {
        &self.skin_vertices
    }

    /// Consumes the import and returns its renderer-neutral model.
    #[must_use]
    pub fn into_model(self) -> Model {
        self.model
    }

    /// Consumes the import into both model and report.
    #[must_use]
    pub fn into_parts(
        self,
    ) -> (
        Model,
        Vec<Vec<crate::Source1SkinVertex>>,
        Source1AnimationSet,
        Source1ModelImportReport,
    ) {
        (self.model, self.skin_vertices, self.animations, self.report)
    }
}

/// Root-confined standalone Source 1 MDL loader.
///
/// The content root must contain `models/` and `materials/`. Call [`Self::load`]
/// with a Source-relative path such as `models/props/tree.mdl`; the loader
/// resolves matching `.vvd` and optimized `.vtx` sidecars and embeds decoded
/// VTF base textures in the returned [`Model`].
#[derive(Clone, Debug)]
pub struct Source1ModelLoader {
    content_root: PathBuf,
    materials: Source1MaterialResolver,
    options: Source1ModelImportOptions,
}

impl Source1ModelLoader {
    /// Creates a strict loader rooted at one Source content directory.
    ///
    /// # Errors
    ///
    /// Returns a typed root/material-resolver error when the content or
    /// `materials` directory is absent or not a directory.
    pub fn new(content_root: impl AsRef<Path>) -> Result<Self, Source1ModelImportError> {
        Self::with_options(content_root, Source1ModelImportOptions::default())
    }

    /// Creates a loader with explicit bounded-work, skin and fallback policy.
    ///
    /// # Errors
    ///
    /// Returns a typed root/material-resolver error when the content or
    /// `materials` directory is absent or not a directory.
    pub fn with_options(
        content_root: impl AsRef<Path>,
        options: Source1ModelImportOptions,
    ) -> Result<Self, Source1ModelImportError> {
        let content_root = fs::canonicalize(content_root.as_ref()).map_err(|source| {
            Source1ModelImportError::Root {
                path: content_root.as_ref().to_path_buf(),
                source,
            }
        })?;
        if !content_root.is_dir() {
            return Err(Source1ModelImportError::RootNotDirectory { path: content_root });
        }
        let materials_root = content_root.join("materials");
        let materials = Source1MaterialResolver::new(&materials_root, &materials_root)
            .map_err(Source1ModelImportError::MaterialResolver)?;
        Ok(Self {
            content_root,
            materials,
            options,
        })
    }

    /// Returns the canonical content root.
    #[must_use]
    pub fn content_root(&self) -> &Path {
        &self.content_root
    }

    /// Returns the active import policy.
    #[must_use]
    pub const fn options(&self) -> Source1ModelImportOptions {
        self.options
    }

    /// Loads one Source-relative `.mdl` plus its VVD/VTX and VMT/VTF assets.
    ///
    /// # Errors
    ///
    /// Returns typed path, I/O, sidecar, `StudioModel`, material, texture pixel,
    /// mesh or model validation failures. No partial model is returned.
    pub fn load(
        &self,
        model_path: impl AsRef<Path>,
    ) -> Result<LoadedSource1Model, Source1ModelImportError> {
        let (relative, mdl_path) = self.resolve_model_path(model_path.as_ref())?;
        let files = self.read_sidecars(relative.clone(), &mdl_path)?;
        let studio = crate::decode_studio_model_with_body(
            &files,
            self.options.studio_limits,
            self.options.body,
        )
        .map_err(Source1ModelImportError::Studio)?;
        let mut loaded = self.cook(&studio)?;
        let ani = self.read_animation_sidecar(&mdl_path, &files.mdl)?;
        let mut animations =
            crate::decode_studio_animations(&files.mdl, ani.as_deref(), self.options.studio_limits)
                .map_err(Source1ModelImportError::Studio)?;
        let mut visited = HashSet::from([relative.clone()]);
        let mut resolved = Vec::new();
        let mut missing = Vec::new();
        self.merge_included_animations(&mut animations, &mut visited, &mut resolved, &mut missing)?;
        loaded.report.bones = animations.skeleton().bones().len();
        loaded.report.animation_clips = animations.clips().len();
        loaded.report.included_animation_models = resolved;
        loaded.report.missing_animation_models = missing;
        loaded.animations = animations;
        Ok(loaded)
    }

    /// Loads one render model and merges sequences from supplemental animation MDLs.
    ///
    /// Supplemental models only need a readable `.mdl` and optional `.ani`; their
    /// VVD/VTX geometry and materials are not loaded. Sequence bones are remapped
    /// by case-insensitive Source bone name, while bones absent from a supplemental
    /// skeleton retain the owning model's bind transform.
    ///
    /// Missing supplemental models are non-fatal and are listed in
    /// [`Source1ModelImportReport::missing_animation_models`], matching authored
    /// `$includemodel` behaviour.
    ///
    /// # Errors
    ///
    /// Returns typed path, I/O, animation decode or bounded-work failures.
    pub fn load_with_animation_models<I, P>(
        &self,
        model_path: impl AsRef<Path>,
        animation_models: I,
    ) -> Result<LoadedSource1Model, Source1ModelImportError>
    where
        I: IntoIterator<Item = P>,
        P: AsRef<Path>,
    {
        let mut loaded = self.load(model_path)?;
        let mut visited = HashSet::from([loaded.report.model_path.clone()]);
        visited.extend(loaded.report.included_animation_models.iter().cloned());
        let mut resolved = Vec::new();
        let mut missing = Vec::new();
        for authored in animation_models {
            self.merge_animation_model(
                &mut loaded.animations,
                authored.as_ref(),
                &mut visited,
                &mut resolved,
                &mut missing,
            )?;
        }
        loaded.report.animation_clips = loaded.animations.clips().len();
        loaded.report.included_animation_models.extend(resolved);
        loaded.report.missing_animation_models.extend(missing);
        Ok(loaded)
    }

    /// Converts an already decoded `StudioModel` using this loader's material root.
    ///
    /// # Errors
    ///
    /// Returns material, texture pixel, mesh or model validation failures.
    pub fn cook(
        &self,
        studio: &Source1StudioModel,
    ) -> Result<LoadedSource1Model, Source1ModelImportError> {
        let transform = Source1StaticPropTransform {
            origin: [0.0; 3],
            angles: [0.0; 3],
            uniform_scale: 1.0,
        };
        let mut materials = Vec::new();
        let mut textures = Vec::new();
        let mut runtime_materials = HashMap::<usize, MaterialIndex>::new();
        let mut meshes = Vec::with_capacity(studio.meshes.len());
        let mut skin_vertices = Vec::with_capacity(studio.meshes.len());
        let mut fallback_materials = Vec::new();
        let mut vertices = 0_usize;
        let mut triangles = 0_usize;

        for studio_mesh in &studio.meshes {
            let studio_material = studio
                .material_for_skin(studio_mesh, self.options.skin)
                .ok_or(Source1ModelImportError::MaterialSlot {
                    slot: studio_mesh.material_slot,
                    skin: self.options.skin,
                })?;
            let declaration_index = studio
                .materials
                .iter()
                .position(|candidate| std::ptr::eq(candidate, studio_material))
                .ok_or(Source1ModelImportError::MaterialSlot {
                    slot: studio_mesh.material_slot,
                    skin: self.options.skin,
                })?;
            let material_index = if let Some(index) = runtime_materials.get(&declaration_index) {
                *index
            } else {
                let texture_index = ModelTextureIndex::new(textures.len());
                let (material, texture) = self.cook_material(studio_material, texture_index)?;
                let runtime_index = MaterialIndex::new(materials.len());
                if let Some(texture) = texture {
                    textures.push(texture);
                } else {
                    fallback_materials.push(studio_material.name.clone());
                }
                materials.push(material);
                runtime_materials.insert(declaration_index, runtime_index);
                runtime_index
            };

            let positions = studio_mesh
                .positions
                .iter()
                .copied()
                .map(|position| transform.transform_position(position))
                .collect();
            let normals = studio_mesh
                .normals
                .iter()
                .copied()
                .map(|normal| transform.transform_normal(normal))
                .collect();
            let primitive = MeshPrimitive::new(positions, studio_mesh.indices.clone())
                .map_err(Source1ModelImportError::Primitive)?
                .with_normals(normals)
                .map_err(Source1ModelImportError::Primitive)?
                .with_tex_coords_0(studio_mesh.tex_coords_0.clone())
                .map_err(Source1ModelImportError::Primitive)?
                .with_material(material_index);
            vertices = vertices.saturating_add(primitive.positions().len());
            triangles = triangles.saturating_add(primitive.indices().len() / 3);
            meshes.push(
                Mesh::new(Some(studio_mesh.model_name.clone()), vec![primitive])
                    .map_err(Source1ModelImportError::Mesh)?,
            );
            skin_vertices.push(studio_mesh.skin_vertices.clone());
        }

        let model =
            Model::new(meshes, materials, textures).map_err(Source1ModelImportError::Model)?;
        let report = Source1ModelImportReport {
            model_path: studio.path.clone(),
            mdl_version: studio.mdl_version,
            checksum: studio.checksum,
            skin: self.options.skin,
            body: self.options.body,
            meshes: model.meshes().len(),
            materials: model.materials().len(),
            textured_materials: model.textures().len(),
            vertices,
            triangles,
            fallback_materials,
            bones: 0,
            animation_clips: 0,
            included_animation_models: Vec::new(),
            missing_animation_models: Vec::new(),
        };
        Ok(LoadedSource1Model {
            model,
            skin_vertices,
            animations: Source1AnimationSet::default(),
            report,
        })
    }

    fn cook_material(
        &self,
        material: &Source1StudioMaterial,
        texture_index: ModelTextureIndex,
    ) -> Result<(Material, Option<ModelTexture>), Source1ModelImportError> {
        match self.resolve_material(material) {
            Ok((candidate, resolved)) => {
                let texture = resolved.base_texture;
                let descriptor = ModelTexture::decoded_rgba8(
                    u32::from(texture.width),
                    u32::from(texture.height),
                    texture.rgba8,
                )
                .map_err(Source1ModelImportError::TexturePixels)?
                .with_label(candidate)
                .with_sampler(source_sampler());
                let mut output = Material::new()
                    .with_name(material.name.clone())
                    .with_metallic_roughness(0.0, 0.8)
                    .with_double_sided(resolved.double_sided)
                    .with_base_color_texture(TextureBinding::new(texture_index, 0));
                output = output.with_alpha_mode(match resolved.alpha_mode {
                    Source1MaterialAlphaMode::Opaque => AlphaMode::Opaque,
                    Source1MaterialAlphaMode::AlphaTest => AlphaMode::Mask { cutoff: 0.5 },
                    Source1MaterialAlphaMode::Translucent => AlphaMode::Blend,
                });
                Ok((output, Some(descriptor)))
            }
            Err(_)
                if self.options.material_policy == Source1ModelMaterialPolicy::FactorFallback =>
            {
                let color = fallback_color(&material.name);
                Ok((
                    Material::new()
                        .with_name(material.name.clone())
                        .with_base_color_factor(color)
                        .with_metallic_roughness(0.0, 0.9),
                    None,
                ))
            }
            Err(source) => Err(source),
        }
    }

    fn resolve_material(
        &self,
        material: &Source1StudioMaterial,
    ) -> Result<(String, Source1ResolvedMaterial), Source1ModelImportError> {
        let candidates = if material.candidates.is_empty() {
            vec![material.name.clone()]
        } else {
            material.candidates.clone()
        };
        let mut last = None;
        for candidate in &candidates {
            match self.materials.resolve_vmt_material_path(candidate) {
                Ok(texture) => return Ok((candidate.clone(), texture)),
                Err(source) => last = Some((candidate.clone(), source)),
            }
        }
        let (candidate, source) = last.expect("at least one material candidate");
        Err(Source1ModelImportError::Material {
            material: material.name.clone(),
            candidate,
            source,
        })
    }

    fn resolve_model_path(
        &self,
        authored: &Path,
    ) -> Result<(String, PathBuf), Source1ModelImportError> {
        if authored.is_absolute()
            || authored
                .components()
                .any(|component| !matches!(component, Component::Normal(_) | Component::CurDir))
        {
            return Err(Source1ModelImportError::UnsafePath {
                path: authored.to_path_buf(),
            });
        }
        if authored
            .extension()
            .and_then(|extension| extension.to_str())
            .is_none_or(|extension| !extension.eq_ignore_ascii_case("mdl"))
        {
            return Err(Source1ModelImportError::WrongExtension {
                path: authored.to_path_buf(),
            });
        }
        let candidate = self.content_root.join(authored);
        let canonical =
            fs::canonicalize(&candidate).map_err(|source| Source1ModelImportError::Read {
                kind: "MDL",
                path: candidate,
                source,
            })?;
        if !canonical.starts_with(&self.content_root) {
            return Err(Source1ModelImportError::UnsafePath {
                path: authored.to_path_buf(),
            });
        }
        Ok((normalize_relative(authored), canonical))
    }

    fn read_sidecars(
        &self,
        model_path: String,
        mdl_path: &Path,
    ) -> Result<Source1StudioModelFiles, Source1ModelImportError> {
        let base = mdl_path.with_extension("");
        let vvd_path = base.with_extension("vvd");
        let vtx_candidates = [
            path_with_suffix(&base, "dx90.vtx"),
            base.with_extension("vtx"),
            path_with_suffix(&base, "dx80.vtx"),
            path_with_suffix(&base, "sw.vtx"),
        ];
        let mdl = read_file("MDL", mdl_path)?;
        let vvd = read_file("VVD", &vvd_path)?;
        let (vtx_path, vtx) = read_first("VTX", &vtx_candidates)?;
        let vtx_relative = vtx_path
            .strip_prefix(&self.content_root)
            .map(normalize_relative)
            .unwrap_or_else(|_| vtx_path.display().to_string());
        Ok(Source1StudioModelFiles {
            model_path,
            mdl: mdl.into(),
            vvd: vvd.into(),
            vtx: vtx.into(),
            vtx_path: vtx_relative,
        })
    }

    fn merge_included_animations(
        &self,
        target: &mut Source1AnimationSet,
        visited: &mut HashSet<String>,
        resolved: &mut Vec<String>,
        missing: &mut Vec<String>,
    ) -> Result<(), Source1ModelImportError> {
        let includes = target.included_models().to_vec();
        for authored in includes {
            self.merge_animation_model(target, Path::new(&authored), visited, resolved, missing)?;
        }
        Ok(())
    }

    fn merge_animation_model(
        &self,
        target: &mut Source1AnimationSet,
        authored: &Path,
        visited: &mut HashSet<String>,
        resolved: &mut Vec<String>,
        missing: &mut Vec<String>,
    ) -> Result<(), Source1ModelImportError> {
        let authored = authored
            .to_str()
            .ok_or_else(|| Source1ModelImportError::UnsafePath {
                path: authored.to_path_buf(),
            })?;
        let relative = normalize_source_model_name(authored);
        if !visited.insert(relative.clone()) {
            return Ok(());
        }
        if visited.len() > self.options.studio_limits.max_included_models {
            return Err(Source1ModelImportError::Studio(
                Source1StudioError::RecordLimit {
                    section: "recursive include models",
                    actual: visited.len(),
                    limit: self.options.studio_limits.max_included_models,
                },
            ));
        }
        let path = Path::new(&relative);
        if path.is_absolute()
            || path
                .components()
                .any(|component| !matches!(component, Component::Normal(_) | Component::CurDir))
        {
            return Err(Source1ModelImportError::UnsafePath {
                path: path.to_path_buf(),
            });
        }
        let mdl_path = self.content_root.join(path);
        let mdl = match fs::read(&mdl_path) {
            Ok(bytes) => bytes,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                missing.push(relative);
                return Ok(());
            }
            Err(source) => {
                return Err(Source1ModelImportError::Read {
                    kind: "animation MDL",
                    path: mdl_path,
                    source,
                });
            }
        };
        let canonical =
            fs::canonicalize(&mdl_path).map_err(|source| Source1ModelImportError::Read {
                kind: "animation MDL",
                path: mdl_path.clone(),
                source,
            })?;
        if !canonical.starts_with(&self.content_root) {
            return Err(Source1ModelImportError::UnsafePath {
                path: path.to_path_buf(),
            });
        }
        let ani = self.read_animation_sidecar(&mdl_path, &mdl)?;
        let mut included_set =
            crate::decode_studio_animations(&mdl, ani.as_deref(), self.options.studio_limits)
                .map_err(Source1ModelImportError::Studio)?;
        self.merge_included_animations(&mut included_set, visited, resolved, missing)?;
        target.merge_included(&included_set);
        resolved.push(relative);
        Ok(())
    }

    fn read_animation_sidecar(
        &self,
        mdl_path: &Path,
        mdl: &[u8],
    ) -> Result<Option<Vec<u8>>, Source1ModelImportError> {
        let mut candidates = Vec::new();
        if let Some(offset_bytes) = mdl.get(348..352) {
            let offset = i32::from_le_bytes(offset_bytes.try_into().expect("four-byte slice"));
            if let Ok(offset) = usize::try_from(offset)
                && offset != 0
                && let Some(available) = mdl.get(offset..)
                && let Some(end) = available.iter().position(|byte| *byte == 0)
                && let Ok(authored) = std::str::from_utf8(&available[..end])
                && !authored.is_empty()
            {
                let relative = authored.replace('\\', "/");
                let path = Path::new(relative.trim_start_matches('/'));
                if path.is_absolute()
                    || path.components().any(|component| {
                        !matches!(component, Component::Normal(_) | Component::CurDir)
                    })
                {
                    return Err(Source1ModelImportError::UnsafePath {
                        path: path.to_path_buf(),
                    });
                }
                candidates.push(self.content_root.join(path));
            }
        }
        let conventional = mdl_path.with_extension("ani");
        if !candidates.contains(&conventional) {
            candidates.push(conventional);
        }
        for candidate in candidates {
            match fs::read(&candidate) {
                Ok(bytes) => {
                    let canonical = fs::canonicalize(&candidate).map_err(|source| {
                        Source1ModelImportError::Read {
                            kind: "ANI",
                            path: candidate.clone(),
                            source,
                        }
                    })?;
                    if !canonical.starts_with(&self.content_root) {
                        return Err(Source1ModelImportError::UnsafePath { path: candidate });
                    }
                    return Ok(Some(bytes));
                }
                Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
                Err(source) => {
                    return Err(Source1ModelImportError::Read {
                        kind: "ANI",
                        path: candidate,
                        source,
                    });
                }
            }
        }
        Ok(None)
    }
}

fn source_sampler() -> ModelTextureSampler {
    ModelTextureSampler {
        address_mode_u: ModelTextureAddressMode::Repeat,
        address_mode_v: ModelTextureAddressMode::Repeat,
        mag_filter: ModelTextureMagFilter::Linear,
        min_filter: ModelTextureMinFilter::LinearMipmapLinear,
    }
}

fn fallback_color(name: &str) -> [f32; 4] {
    let hash = name.bytes().fold(2_166_136_261_u32, |hash, byte| {
        (hash ^ u32::from(byte)).wrapping_mul(16_777_619)
    });
    [
        0.25 + ((hash & 0xff) as f32 / 255.0) * 0.55,
        0.25 + (((hash >> 8) & 0xff) as f32 / 255.0) * 0.55,
        0.25 + (((hash >> 16) & 0xff) as f32 / 255.0) * 0.55,
        1.0,
    ]
}

fn path_with_suffix(base: &Path, suffix: &str) -> PathBuf {
    let mut value = base.as_os_str().to_os_string();
    value.push(".");
    value.push(suffix);
    PathBuf::from(value)
}

fn read_file(kind: &'static str, path: &Path) -> Result<Vec<u8>, Source1ModelImportError> {
    fs::read(path).map_err(|source| Source1ModelImportError::Read {
        kind,
        path: path.to_path_buf(),
        source,
    })
}

fn normalize_source_model_name(value: &str) -> String {
    let normalized = value.replace('\\', "/");
    let normalized = normalized.trim_start_matches('/');
    if normalized.to_ascii_lowercase().starts_with("models/") {
        normalized.to_owned()
    } else {
        format!("models/{normalized}")
    }
}

fn read_first(
    kind: &'static str,
    candidates: &[PathBuf],
) -> Result<(PathBuf, Vec<u8>), Source1ModelImportError> {
    for path in candidates {
        match fs::read(path) {
            Ok(bytes) => return Ok((path.clone(), bytes)),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(Source1ModelImportError::Read {
                    kind,
                    path: path.clone(),
                    source,
                });
            }
        }
    }
    Err(Source1ModelImportError::MissingVtx {
        tried: candidates.to_vec(),
    })
}

fn normalize_relative(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// Failure while resolving, decoding or cooking a standalone Source 1 MDL.
#[derive(Debug)]
pub enum Source1ModelImportError {
    /// Content root could not be canonicalized.
    Root {
        /// Requested root.
        path: PathBuf,
        /// Filesystem failure.
        source: std::io::Error,
    },
    /// Canonical content root is not a directory.
    RootNotDirectory {
        /// Canonical root.
        path: PathBuf,
    },
    /// Material resolver construction failed.
    MaterialResolver(Source1AssetError),
    /// Authored model path was absolute or contained traversal.
    UnsafePath {
        /// Rejected path.
        path: PathBuf,
    },
    /// Authored path did not end in `.mdl`.
    WrongExtension {
        /// Rejected path.
        path: PathBuf,
    },
    /// A required sidecar could not be read.
    Read {
        /// Sidecar kind.
        kind: &'static str,
        /// Attempted path.
        path: PathBuf,
        /// Filesystem failure.
        source: std::io::Error,
    },
    /// No supported optimized VTX sidecar exists.
    MissingVtx {
        /// Candidates checked in priority order.
        tried: Vec<PathBuf>,
    },
    /// MDL/VVD/VTX decode failed.
    Studio(Source1StudioError),
    /// Requested skin/mesh material slot was invalid.
    MaterialSlot {
        /// Mesh skin-reference slot.
        slot: usize,
        /// Requested skin family.
        skin: i32,
    },
    /// VMT/VTF material resolution failed.
    Material {
        /// MDL material declaration.
        material: String,
        /// Last candidate attempted.
        candidate: String,
        /// Resolver failure.
        source: Source1AssetError,
    },
    /// Decoded VTF pixels did not form a valid renderer-neutral RGBA8 texture.
    TexturePixels(ModelTextureRgba8Error),
    /// Runtime mesh validation failed.
    Primitive(yuyib_model::MeshValidationError),
    /// Runtime mesh container validation failed.
    Mesh(yuyib_model::MeshError),
    /// Final model validation failed.
    Model(yuyib_model::ModelValidationError),
}

impl fmt::Display for Source1ModelImportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Root { path, source } => {
                write!(
                    formatter,
                    "cannot resolve Source content root {}: {source}",
                    path.display()
                )
            }
            Self::RootNotDirectory { path } => {
                write!(
                    formatter,
                    "Source content root is not a directory: {}",
                    path.display()
                )
            }
            Self::MaterialResolver(source) => {
                write!(formatter, "cannot initialize Source materials: {source}")
            }
            Self::UnsafePath { path } => {
                write!(formatter, "unsafe Source MDL path {}", path.display())
            }
            Self::WrongExtension { path } => write!(
                formatter,
                "Source model path must use .mdl: {}",
                path.display()
            ),
            Self::Read { kind, path, source } => {
                write!(
                    formatter,
                    "cannot read Source {kind} {}: {source}",
                    path.display()
                )
            }
            Self::MissingVtx { tried } => write!(
                formatter,
                "Source model has no supported VTX sidecar; tried {}",
                tried
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::Studio(source) => write!(formatter, "cannot decode Source StudioModel: {source}"),
            Self::MaterialSlot { slot, skin } => {
                write!(
                    formatter,
                    "Source skin {skin} has no material for slot {slot}"
                )
            }
            Self::Material {
                material,
                candidate,
                source,
            } => write!(
                formatter,
                "cannot resolve Source material {material:?} via {candidate:?}: {source}"
            ),
            Self::TexturePixels(source) => {
                write!(formatter, "invalid decoded Source texture: {source}")
            }
            Self::Primitive(source) => {
                write!(formatter, "invalid Source model primitive: {source}")
            }
            Self::Mesh(source) => write!(formatter, "invalid Source model mesh: {source}"),
            Self::Model(source) => write!(formatter, "invalid Source model: {source}"),
        }
    }
}

impl Error for Source1ModelImportError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Root { source, .. } | Self::Read { source, .. } => Some(source),
            Self::MaterialResolver(source) | Self::Material { source, .. } => Some(source),
            Self::Studio(source) => Some(source),
            Self::TexturePixels(source) => Some(source),
            Self::Primitive(source) => Some(source),
            Self::Mesh(source) => Some(source),
            Self::Model(source) => Some(source),
            Self::RootNotDirectory { .. }
            | Self::UnsafePath { .. }
            | Self::WrongExtension { .. }
            | Self::MissingVtx { .. }
            | Self::MaterialSlot { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_root() -> PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "yuyib-source1-model-{}-{unique}",
            std::process::id()
        ))
    }

    fn rgba_vtf(pixel: [u8; 4]) -> Vec<u8> {
        let mut bytes = vec![0; 80];
        bytes[..4].copy_from_slice(b"VTF\0");
        bytes[4..8].copy_from_slice(&7_u32.to_le_bytes());
        bytes[8..12].copy_from_slice(&2_u32.to_le_bytes());
        bytes[12..16].copy_from_slice(&80_u32.to_le_bytes());
        bytes[16..18].copy_from_slice(&1_u16.to_le_bytes());
        bytes[18..20].copy_from_slice(&1_u16.to_le_bytes());
        bytes[24..26].copy_from_slice(&1_u16.to_le_bytes());
        bytes[52..56].copy_from_slice(&0_i32.to_le_bytes());
        bytes[56] = 1;
        bytes[57..61].copy_from_slice(&(-1_i32).to_le_bytes());
        bytes[63..65].copy_from_slice(&1_u16.to_le_bytes());
        bytes.extend(pixel);
        bytes
    }

    fn studio_mesh(material_slot: usize, x: f32) -> crate::Source1StudioMesh {
        crate::Source1StudioMesh {
            body_part: 0,
            body_model: 0,
            model_name: format!("mesh-{material_slot}"),
            material_slot,
            positions: vec![[x, 2.0, 3.0], [x + 1.0, 2.0, 3.0], [x, 3.0, 3.0]],
            normals: vec![[0.0, 0.0, 1.0]; 3],
            tex_coords_0: vec![[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]],
            skin_vertices: vec![crate::Source1SkinVertex::new([0; 4], [1.0, 0.0, 0.0, 0.0]); 3],
            indices: vec![0, 1, 2],
        }
    }

    #[test]
    fn vtx_candidate_order_prefers_dx90() {
        let base = Path::new("models/props/tree");
        assert_eq!(
            path_with_suffix(base, "dx90.vtx"),
            Path::new("models/props/tree.dx90.vtx")
        );
    }

    #[test]
    fn fallback_color_is_stable_and_opaque() {
        assert_eq!(fallback_color("models/tree"), fallback_color("models/tree"));
        assert_eq!(fallback_color("models/tree")[3], 1.0);
        assert_ne!(fallback_color("models/tree"), fallback_color("models/rock"));
    }

    #[test]
    fn cooks_textured_studio_model_with_distinct_material_slots() {
        let root = test_root();
        fs::create_dir_all(root.join("materials/test")).expect("material fixture directory");
        fs::create_dir_all(root.join("models")).expect("model fixture directory");
        for (name, pixel) in [("first", [10, 20, 30, 255]), ("second", [80, 90, 100, 120])] {
            fs::write(
                root.join(format!("materials/test/{name}.vmt")),
                format!("VertexLitGeneric {{ \"$basetexture\" \"test/{name}\" }}"),
            )
            .expect("VMT fixture");
            fs::write(
                root.join(format!("materials/test/{name}.vtf")),
                rgba_vtf(pixel),
            )
            .expect("VTF fixture");
        }
        let loader = Source1ModelLoader::new(&root).expect("loader");
        let studio = Source1StudioModel {
            path: "models/test.mdl".to_owned(),
            mdl_version: 49,
            checksum: 42,
            materials: vec![
                Source1StudioMaterial {
                    name: "first".to_owned(),
                    candidates: vec!["test/first".to_owned()],
                },
                Source1StudioMaterial {
                    name: "second".to_owned(),
                    candidates: vec!["test/second".to_owned()],
                },
            ],
            skin_families: vec![vec![0, 1]],
            meshes: vec![studio_mesh(0, 1.0), studio_mesh(1, 4.0)],
        };

        let loaded = loader.cook(&studio).expect("textured model");
        assert_eq!(loaded.report().meshes, 2);
        assert_eq!(loaded.report().textured_materials, 2);
        assert!(loaded.report().fallback_materials.is_empty());
        assert_eq!(
            loaded.model().meshes()[0].primitives()[0].positions()[0],
            [1.0, 3.0, -2.0]
        );
        assert_eq!(
            loaded.model().materials()[0]
                .base_color_texture()
                .expect("first binding")
                .texture(),
            ModelTextureIndex::new(0)
        );
        assert_eq!(
            loaded.model().materials()[1]
                .base_color_texture()
                .expect("second binding")
                .texture(),
            ModelTextureIndex::new(1)
        );
        assert_eq!(
            loaded.model().materials()[0].alpha_mode(),
            AlphaMode::Opaque
        );
        assert_eq!(
            loaded.model().materials()[1].alpha_mode(),
            AlphaMode::Opaque
        );
        for texture in loaded.model().textures() {
            let (width, height, pixels) = texture
                .decoded_rgba8_pixels()
                .expect("importer-decoded RGBA8");
            assert_eq!((width, height), (1, 1));
            assert_eq!(pixels.len(), 4);
        }
        let _ = fs::remove_dir_all(root);
    }
}
