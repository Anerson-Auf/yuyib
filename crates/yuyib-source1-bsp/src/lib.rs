//! High-level Source 1 BSP import and static-world cooking.
//!
//! [`Source1BspLoader`] composes the low-level bounded BSP reader with Source
//! VMT/VTF decoding, PAKFILE and loose-file material providers, Yuyib model
//! geometry, textured static-world batching and triangle-mesh collision.
//!
//! Brush-entity submodels are instantiated at their authored `origin` and
//! `angles` in the static render world and collider. Entity I/O and moving-door
//! simulation remain runtime concerns outside this importer.

#![forbid(unsafe_code)]
#![allow(
    missing_docs,
    reason = "the crate-level pipeline documentation and typed field names define this initial integration surface"
)]

mod static_props;

pub use static_props::{
    Source1StaticPropAssetError, Source1StaticPropAssetOptions, Source1StaticPropAssets,
    Source1StaticPropModelFiles, load_static_prop_assets,
};
pub use yuyib_source1::{
    Source1StaticPropTransform, Source1StudioError, Source1StudioLimits, Source1StudioMaterial,
    Source1StudioMesh, Source1StudioModel, decode_studio_model,
};

use std::{
    collections::{BTreeSet, HashMap},
    error::Error,
    fmt,
    path::{Path, PathBuf},
    sync::Arc,
};

use yuyib_bsp::{
    Bsp, BspDisplacementInfo, BspDisplacementVertex, BspEdge, BspEntity, BspError, BspFace,
    BspLimits, BspModel, BspReadError, BspTexData, BspTexInfo, BspVertex,
};
use yuyib_model::{Material, MaterialIndex, Mesh, MeshPrimitive, Model};
use yuyib_physics::{TriangleMesh3d, TriangleMeshError, Vec3};
use yuyib_render_3d::{
    Source1WaterBatch3d, Source1WaterBatchError3d, Source1WaterLimits3d, Source1WaterMaterial3d,
    Source1WaterTexture3d, Source1WaterWorld3d, Source1WaterWorldBuildError3d,
    StaticWorldTexture3d, StaticWorldTextureError3d, TexturedStaticWorld3d,
    TexturedStaticWorldBuildError3d, TexturedStaticWorldMaterial3d,
};
use yuyib_source1_assets::{
    Source1AssetError, Source1MaterialResolver, Source1MaterialTextureReferences,
};
use yuyib_vmt::{VmtBlock, VmtMaterial, VmtProxy, parse as parse_vmt};
use yuyib_vtf::{decode as decode_vtf, decode_frames as decode_vtf_frames};

const SURF_SKY_2D: i32 = 0x0002;
const SURF_SKY: i32 = 0x0004;
const SURF_NODRAW: i32 = 0x0080;
const SURF_HINT: i32 = 0x0100;
const SURF_SKIP: i32 = 0x0200;

/// High-level BSP import policy.
#[derive(Clone, Debug)]
pub struct Source1BspImportOptions {
    pub bsp_limits: BspLimits,
    /// Loose `materials` root used after the embedded BSP PAKFILE.
    pub external_material_root: Option<PathBuf>,
    /// Loose Source content root used for `models/*.mdl/.vvd/.vtx` after the
    /// embedded BSP PAKFILE.
    pub external_content_root: Option<PathBuf>,
    /// Material prefixes omitted from render geometry but retained in collision.
    pub hidden_material_prefixes: Vec<String>,
    /// Skip faces carrying Source editor/non-render surface flags.
    pub skip_non_render_surface_flags: bool,
}

impl Default for Source1BspImportOptions {
    fn default() -> Self {
        Self {
            bsp_limits: BspLimits::default(),
            external_material_root: None,
            external_content_root: None,
            hidden_material_prefixes: vec![
                "tools/".to_owned(),
                "decals/".to_owned(),
                "sprites/".to_owned(),
                "__bsp_hidden/".to_owned(),
            ],
            skip_non_render_surface_flags: true,
        }
    }
}

/// One material that remained on the explicit factor fallback path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Source1BspMaterialDiagnostic {
    pub material: String,
    pub reason: String,
}

/// Deterministic import metrics suitable for loading UI and diagnostics.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Source1BspImportReport {
    pub bsp_version: i32,
    pub map_revision: i32,
    pub source_faces: usize,
    pub brush_model_instances: usize,
    pub static_prop_instances: usize,
    pub static_prop_models: usize,
    pub static_prop_meshes: usize,
    pub water_materials: usize,
    pub water_batches: usize,
    pub water_normal_frames: usize,
    pub cooked_primitives: usize,
    pub skipped_faces: usize,
    pub entities: usize,
    pub material_slots: usize,
    pub textured_materials: usize,
    pub unique_vtf_textures: usize,
    pub embedded_material_files: usize,
    pub external_materials: usize,
    pub hidden_materials: usize,
    pub embedded_source_models: usize,
    pub skipped_collision_triangles: usize,
    pub unresolved_materials: Vec<Source1BspMaterialDiagnostic>,
}

/// CPU-side runtime result of one BSP import.
pub struct LoadedSource1Bsp {
    model: Model,
    render_world: TexturedStaticWorld3d,
    water_world: Source1WaterWorld3d,
    collider: TriangleMesh3d,
    entities: Vec<BspEntity>,
    report: Source1BspImportReport,
}

impl LoadedSource1Bsp {
    #[must_use]
    pub const fn model(&self) -> &Model {
        &self.model
    }

    #[must_use]
    pub const fn render_world(&self) -> &TexturedStaticWorld3d {
        &self.render_world
    }

    #[must_use]
    pub const fn water_world(&self) -> &Source1WaterWorld3d {
        &self.water_world
    }

    #[must_use]
    pub const fn collider(&self) -> &TriangleMesh3d {
        &self.collider
    }

    #[must_use]
    pub fn entities(&self) -> &[BspEntity] {
        &self.entities
    }

    #[must_use]
    pub const fn report(&self) -> &Source1BspImportReport {
        &self.report
    }

    #[must_use]
    pub fn into_parts(
        self,
    ) -> (
        Model,
        TexturedStaticWorld3d,
        TriangleMesh3d,
        Vec<BspEntity>,
        Source1BspImportReport,
    ) {
        (
            self.model,
            self.render_world,
            self.collider,
            self.entities,
            self.report,
        )
    }

    /// Consumes the import while retaining its separate transparent water phase.
    #[must_use]
    pub fn into_parts_with_water(
        self,
    ) -> (
        Model,
        TexturedStaticWorld3d,
        Source1WaterWorld3d,
        TriangleMesh3d,
        Vec<BspEntity>,
        Source1BspImportReport,
    ) {
        (
            self.model,
            self.render_world,
            self.water_world,
            self.collider,
            self.entities,
            self.report,
        )
    }
}

/// Stateless high-level Source BSP loader.
#[derive(Clone, Debug, Default)]
pub struct Source1BspLoader {
    options: Source1BspImportOptions,
}

impl Source1BspLoader {
    #[must_use]
    pub fn new(options: Source1BspImportOptions) -> Self {
        Self { options }
    }

    #[must_use]
    pub const fn options(&self) -> &Source1BspImportOptions {
        &self.options
    }

    /// Reads, validates, resolves and cooks one BSP into runtime CPU assets.
    ///
    /// Embedded PAKFILE material files take precedence over the optional loose
    /// root. No archive entry is extracted to disk.
    ///
    /// # Errors
    ///
    /// Returns typed BSP, geometry, model, texture, static-world or collision failures.
    pub fn load(&self, path: impl AsRef<Path>) -> Result<LoadedSource1Bsp, Source1BspImportError> {
        let bsp = Bsp::read(path, self.options.bsp_limits).map_err(Source1BspImportError::Read)?;
        self.cook(&bsp)
    }

    /// Cooks an already validated low-level BSP.
    ///
    /// # Errors
    ///
    /// Returns typed geometry, asset or runtime-cook failures.
    #[allow(
        clippy::too_many_lines,
        reason = "the orchestration keeps provider precedence, caches and the final report visible together"
    )]
    pub fn cook(&self, bsp: &Bsp) -> Result<LoadedSource1Bsp, Source1BspImportError> {
        let entities = bsp.entities().map_err(Source1BspImportError::Bsp)?;
        let pak_entries = bsp.pak_entries().map_err(Source1BspImportError::Bsp)?;
        let embedded_source_models = pak_entries
            .iter()
            .filter(|entry| {
                Path::new(&entry.path)
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("mdl"))
            })
            .count();
        let pak_files = bsp
            .pak_files_by_extension(&["vmt", "vtf"])
            .map_err(Source1BspImportError::Bsp)?;
        let embedded_material_files = pak_files.len();
        let pak: HashMap<_, _> = pak_files
            .into_iter()
            .map(|file| (normalize_path(&file.path), file.bytes))
            .collect();
        let external = match self.options.external_material_root.as_ref() {
            Some(root) => Some(
                Source1MaterialResolver::new(root, root)
                    .map_err(Source1BspImportError::ExternalMaterialRoot)?,
            ),
            None => None,
        };

        let mut geometry =
            cook_geometry(bsp, &entities, self.options.skip_non_render_surface_flags)?;
        // Static-prop PHY collision is a distinct Source asset. Keep the BSP
        // world/brush collider authoritative instead of treating every render
        // triangle (foliage included) as solid.
        let (collider, skipped_collision_triangles) = collider_from_model(&geometry.model)?;
        let static_prop_assets = load_static_prop_assets(
            bsp,
            &Source1StaticPropAssetOptions {
                external_content_root: self.options.external_content_root.clone(),
            },
        )
        .map_err(Source1BspImportError::StaticPropAssets)?;
        let static_prop_stats = match static_prop_assets.as_ref() {
            Some(assets) => append_static_props(&mut geometry, assets, &pak)?,
            None => StaticPropCookStats::default(),
        };
        let mut textures_by_material = HashMap::new();
        let mut textures_by_path = HashMap::<String, Arc<StaticWorldTexture3d>>::new();
        let mut water_material_names = BTreeSet::new();
        let mut water_materials = HashMap::new();
        let mut water_normal_frames = HashMap::new();
        let mut diagnostics = Vec::new();
        let mut external_materials = 0;
        let unique_material_names: BTreeSet<_> = geometry
            .model
            .materials()
            .iter()
            .filter_map(|material| material.name().map(str::to_owned))
            .collect();

        for material_name in &unique_material_names {
            if self.hidden_material(material_name) {
                continue;
            }
            match resolve_pak_water_vmt(material_name, &pak, 0) {
                Ok(Some(water_vmt)) => {
                    let key = normalize_material(material_name);
                    water_material_names.insert(key.clone());
                    match prepare_water_material(
                        &water_vmt,
                        &pak,
                        external.as_ref(),
                        &mut water_normal_frames,
                    ) {
                        Ok(material) => {
                            water_materials.insert(key, material);
                        }
                        Err(reason) => diagnostics.push(Source1BspMaterialDiagnostic {
                            material: material_name.clone(),
                            reason,
                        }),
                    }
                    continue;
                }
                Ok(None) => {}
                Err(reason) => {
                    diagnostics.push(Source1BspMaterialDiagnostic {
                        material: material_name.clone(),
                        reason,
                    });
                    continue;
                }
            }
            match resolve_material(material_name, &pak, external.as_ref()) {
                Ok(resolved) => {
                    external_materials += usize::from(resolved.external);
                    let first = cache_resolved_texture(resolved.first, &mut textures_by_path)?;
                    let binding = if let Some(second) = resolved.second {
                        MaterialTextureBinding::Blend {
                            first,
                            second: cache_resolved_texture(second, &mut textures_by_path)?,
                        }
                    } else {
                        MaterialTextureBinding::Single(first)
                    };
                    textures_by_material.insert(normalize_material(material_name), binding);
                }
                Err(reason) => diagnostics.push(Source1BspMaterialDiagnostic {
                    material: material_name.clone(),
                    reason,
                }),
            }
        }

        let water_world = build_water_world(&geometry.model, &water_materials)?;

        let render_world = TexturedStaticWorld3d::from_model_with_materials(
            &geometry.model,
            |_index, material| {
                let Some(name) = material.name() else {
                    return TexturedStaticWorldMaterial3d::factor(material);
                };
                if self.hidden_material(name)
                    || water_material_names.contains(&normalize_material(name))
                {
                    TexturedStaticWorldMaterial3d::Skip
                } else if let Some(binding) = textures_by_material.get(&normalize_material(name)) {
                    match binding {
                        MaterialTextureBinding::Single(texture) => {
                            TexturedStaticWorldMaterial3d::texture(material, Arc::clone(texture))
                        }
                        MaterialTextureBinding::Blend { first, second } => {
                            TexturedStaticWorldMaterial3d::blend_textures(
                                material,
                                Arc::clone(first),
                                Arc::clone(second),
                            )
                        }
                    }
                } else {
                    TexturedStaticWorldMaterial3d::factor(material)
                }
            },
        )
        .map_err(Source1BspImportError::StaticWorld)?;
        let hidden_materials = unique_material_names
            .iter()
            .filter(|material| self.hidden_material(material))
            .count();
        let report = Source1BspImportReport {
            bsp_version: bsp.version(),
            map_revision: bsp.map_revision(),
            source_faces: geometry.source_faces,
            brush_model_instances: geometry.brush_model_instances,
            static_prop_instances: static_prop_stats.instances,
            static_prop_models: static_prop_stats.models,
            static_prop_meshes: static_prop_stats.meshes,
            water_materials: water_materials.len(),
            water_batches: water_world.stats().batches,
            water_normal_frames: water_normal_frames.values().map(Vec::len).sum(),
            cooked_primitives: geometry.cooked_primitives,
            skipped_faces: geometry.skipped_faces,
            entities: entities.len(),
            material_slots: geometry.model.materials().len(),
            textured_materials: textures_by_material.len(),
            unique_vtf_textures: textures_by_path.len(),
            embedded_material_files,
            external_materials,
            hidden_materials,
            embedded_source_models,
            skipped_collision_triangles,
            unresolved_materials: diagnostics,
        };
        Ok(LoadedSource1Bsp {
            model: geometry.model,
            render_world,
            water_world,
            collider,
            entities,
            report,
        })
    }

    fn hidden_material(&self, material: &str) -> bool {
        let material = normalize_material(material);
        self.options
            .hidden_material_prefixes
            .iter()
            .any(|prefix| material.starts_with(&normalize_material(prefix)))
    }
}

struct CookedGeometry {
    model: Model,
    source_faces: usize,
    brush_model_instances: usize,
    cooked_primitives: usize,
    skipped_faces: usize,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct StaticPropCookStats {
    instances: usize,
    models: usize,
    meshes: usize,
}

#[allow(
    clippy::too_many_lines,
    reason = "StudioModel instances, skin materials and validated mesh streams are assembled transactionally"
)]
fn append_static_props(
    geometry: &mut CookedGeometry,
    assets: &Source1StaticPropAssets,
    pak: &HashMap<String, Vec<u8>>,
) -> Result<StaticPropCookStats, Source1BspImportError> {
    let decoded = assets
        .models
        .iter()
        .map(|files| {
            decode_studio_model(files, Source1StudioLimits::default())
                .map_err(Source1BspImportError::StudioModel)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut meshes = geometry.model.meshes().to_vec();
    let mut materials = geometry.model.materials().to_vec();
    let mut material_indices = materials
        .iter()
        .enumerate()
        .filter_map(|(index, material)| {
            material
                .name()
                .map(|name| (normalize_material(name), MaterialIndex::new(index)))
        })
        .collect::<HashMap<_, _>>();
    let mut instance_meshes = 0;

    for (instance_index, prop) in assets.lump.props.iter().enumerate() {
        let model = decoded.get(usize::from(prop.model_index)).ok_or(
            Source1BspImportError::InvalidStaticPropModel {
                instance: instance_index,
                model: prop.model_index,
            },
        )?;
        let transform = Source1StaticPropTransform {
            origin: prop.origin,
            angles: prop.angles,
            uniform_scale: 1.0,
        };
        for studio_mesh in &model.meshes {
            let studio_material = model.material_for_skin(studio_mesh, prop.skin).ok_or(
                Source1BspImportError::InvalidStaticPropMaterial {
                    instance: instance_index,
                    slot: studio_mesh.material_slot,
                },
            )?;
            let material_name = studio_material
                .candidates
                .iter()
                .find(|candidate| pak.contains_key(&material_asset_path(candidate, "vmt")))
                .or_else(|| studio_material.candidates.first())
                .map_or(studio_material.name.as_str(), String::as_str);
            let material_key = normalize_material(material_name);
            let material = if let Some(index) = material_indices.get(&material_key) {
                *index
            } else {
                let index = MaterialIndex::new(materials.len());
                materials.push(Material::new().with_name(material_key.clone()));
                material_indices.insert(material_key, index);
                index
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
                .map_err(|source| Source1BspImportError::StaticPropMesh {
                    instance: instance_index,
                    source,
                })?
                .with_normals(normals)
                .map_err(|source| Source1BspImportError::StaticPropMesh {
                    instance: instance_index,
                    source,
                })?
                .with_tex_coords_0(studio_mesh.tex_coords_0.clone())
                .map_err(|source| Source1BspImportError::StaticPropMesh {
                    instance: instance_index,
                    source,
                })?
                .with_material(material);
            meshes.push(
                Mesh::new(
                    Some(format!(
                        "static_prop[{instance_index}]:{}/{}",
                        model.path, studio_mesh.model_name
                    )),
                    vec![primitive],
                )
                .map_err(Source1BspImportError::ModelMesh)?,
            );
            instance_meshes += 1;
        }
    }

    geometry.model = Model::new(meshes, materials, geometry.model.textures().to_vec())
        .map_err(Source1BspImportError::Model)?;
    geometry.cooked_primitives += instance_meshes;
    Ok(StaticPropCookStats {
        instances: assets.lump.props.len(),
        models: decoded.len(),
        meshes: instance_meshes,
    })
}

#[allow(
    clippy::too_many_lines,
    reason = "the BSP face cook keeps validated lump relationships and deterministic diagnostics adjacent"
)]
fn cook_geometry(
    bsp: &Bsp,
    entities: &[BspEntity],
    skip_non_render_flags: bool,
) -> Result<CookedGeometry, Source1BspImportError> {
    let vertices = bsp.vertices().map_err(Source1BspImportError::Bsp)?;
    let edges = bsp.edges().map_err(Source1BspImportError::Bsp)?;
    let surf_edges = bsp.surf_edges().map_err(Source1BspImportError::Bsp)?;
    let planes = bsp.planes().map_err(Source1BspImportError::Bsp)?;
    let faces = bsp.faces().map_err(Source1BspImportError::Bsp)?;
    let models = bsp.models().map_err(Source1BspImportError::Bsp)?;
    let tex_info = bsp.tex_info().map_err(Source1BspImportError::Bsp)?;
    let tex_data = bsp.tex_data().map_err(Source1BspImportError::Bsp)?;
    let texture_names = bsp.texture_names().map_err(Source1BspImportError::Bsp)?;
    let displacement_info = bsp
        .displacement_info()
        .map_err(Source1BspImportError::Bsp)?;
    let displacement_vertices = bsp
        .displacement_vertices()
        .map_err(Source1BspImportError::Bsp)?;

    let (face_instances, brush_model_instances) = face_instances(&models, entities)?;
    let source_faces = face_instances.len();
    let mut materials = Vec::new();
    let mut material_indices = HashMap::<String, MaterialIndex>::new();
    let mut primitives = Vec::with_capacity(face_instances.len());
    let mut skipped_faces = 0;
    for instance in face_instances {
        let face_index = instance.face_index;
        let face = faces[face_index];
        if face.edge_count < 3 || face.first_edge < 0 {
            skipped_faces += 1;
            continue;
        }
        let plane =
            planes
                .get(usize::from(face.plane))
                .ok_or(Source1BspImportError::InvalidReference {
                    face: face_index,
                    field: "plane",
                    index: i64::from(face.plane),
                })?;
        let local_source_positions =
            face_positions(face_index, face, &vertices, &edges, &surf_edges)?;
        let source_positions = local_source_positions
            .iter()
            .copied()
            .map(|position| instance.transform.position(position))
            .collect::<Vec<_>>();
        let local_outward_source = if face.side {
            scale3(plane.normal, -1.0)
        } else {
            plane.normal
        };
        let outward_source = instance.transform.vector(local_outward_source);
        let outward = source_vector_to_yuyib(outward_source);
        let material_binding = face_material(
            face_index,
            face,
            &tex_info,
            &tex_data,
            &texture_names,
            &mut materials,
            &mut material_indices,
        )?;
        let uv = material_binding.as_ref().map(|binding| {
            local_source_positions
                .iter()
                .map(|position| texture_uv(*position, binding.tex_info, binding.tex_data))
                .collect::<Vec<_>>()
        });
        let hidden_surface = skip_non_render_flags
            && material_binding.as_ref().is_some_and(|binding| {
                binding.tex_info.flags
                    & (SURF_SKY_2D | SURF_SKY | SURF_NODRAW | SURF_HINT | SURF_SKIP)
                    != 0
            });
        if hidden_surface {
            skipped_faces += 1;
        }
        let material = if hidden_surface {
            Some(intern_material(
                "__bsp_hidden/surface",
                &mut materials,
                &mut material_indices,
            ))
        } else {
            material_binding.as_ref().map(|binding| binding.index)
        };
        let primitive = if face.displacement >= 0 {
            cook_displacement(
                face_index,
                face,
                &local_source_positions,
                uv.as_deref(),
                outward,
                &displacement_info,
                &displacement_vertices,
                material,
                instance.transform,
            )?
        } else {
            cook_polygon(&source_positions, uv.as_deref(), outward, material).map_err(|source| {
                Source1BspImportError::Mesh {
                    face: face_index,
                    source,
                }
            })?
        };
        primitives.push(primitive);
    }
    let cooked_primitives = primitives.len();
    let mesh = Mesh::new(Some("source1-bsp-world".to_owned()), primitives)
        .map_err(Source1BspImportError::ModelMesh)?;
    let model =
        Model::new(vec![mesh], materials, Vec::new()).map_err(Source1BspImportError::Model)?;
    Ok(CookedGeometry {
        model,
        source_faces,
        brush_model_instances,
        cooked_primitives,
        skipped_faces,
    })
}

#[derive(Clone, Copy)]
struct FaceInstance {
    face_index: usize,
    transform: SourceTransform,
}

#[derive(Clone, Copy)]
struct SourceTransform {
    translation: [f32; 3],
    rotation: [[f32; 3]; 3],
}

impl SourceTransform {
    const IDENTITY: Self = Self {
        translation: [0.0; 3],
        rotation: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
    };

    fn from_source_angles(translation: [f32; 3], angles: [f32; 3]) -> Self {
        let [pitch, yaw, roll] = angles.map(f32::to_radians);
        let (sin_pitch, cos_pitch) = pitch.sin_cos();
        let (sin_yaw, cos_yaw) = yaw.sin_cos();
        let (sin_roll, cos_roll) = roll.sin_cos();
        // Source QAngle applies roll, then pitch, then yaw (Rz * Ry * Rx).
        let rotation = [
            [
                cos_yaw * cos_pitch,
                cos_yaw * sin_pitch * sin_roll - sin_yaw * cos_roll,
                cos_yaw * sin_pitch * cos_roll + sin_yaw * sin_roll,
            ],
            [
                sin_yaw * cos_pitch,
                sin_yaw * sin_pitch * sin_roll + cos_yaw * cos_roll,
                sin_yaw * sin_pitch * cos_roll - cos_yaw * sin_roll,
            ],
            [-sin_pitch, cos_pitch * sin_roll, cos_pitch * cos_roll],
        ];
        Self {
            translation,
            rotation,
        }
    }

    fn position(self, position: [f32; 3]) -> [f32; 3] {
        add3(self.vector(position), self.translation)
    }

    fn vector(self, vector: [f32; 3]) -> [f32; 3] {
        [
            dot3(self.rotation[0], vector),
            dot3(self.rotation[1], vector),
            dot3(self.rotation[2], vector),
        ]
    }
}

fn face_instances(
    models: &[BspModel],
    entities: &[BspEntity],
) -> Result<(Vec<FaceInstance>, usize), Source1BspImportError> {
    let world = models
        .first()
        .ok_or(Source1BspImportError::MissingWorldModel)?;
    let mut instances = Vec::new();
    extend_model_faces(&mut instances, world, SourceTransform::IDENTITY);
    let mut brush_model_instances = 0;
    for (entity_index, entity) in entities.iter().enumerate() {
        let Some(reference) = entity.property("model") else {
            continue;
        };
        let Some(model_number) = reference.strip_prefix('*') else {
            continue;
        };
        let model_index = model_number.parse::<usize>().map_err(|_| {
            Source1BspImportError::InvalidBrushModelReference {
                entity: entity_index,
                reference: reference.to_owned(),
            }
        })?;
        if model_index == 0 {
            continue;
        }
        let model = models.get(model_index).ok_or_else(|| {
            Source1BspImportError::InvalidBrushModelReference {
                entity: entity_index,
                reference: reference.to_owned(),
            }
        })?;
        let translation = match entity.property("origin") {
            Some(value) => parse_source_vec3(value).ok_or_else(|| {
                Source1BspImportError::InvalidEntityTransform {
                    entity: entity_index,
                    field: "origin",
                    value: value.to_owned(),
                }
            })?,
            None => model.origin,
        };
        let angles = match entity.property("angles") {
            Some(value) => parse_source_vec3(value).ok_or_else(|| {
                Source1BspImportError::InvalidEntityTransform {
                    entity: entity_index,
                    field: "angles",
                    value: value.to_owned(),
                }
            })?,
            None => [0.0; 3],
        };
        extend_model_faces(
            &mut instances,
            model,
            SourceTransform::from_source_angles(translation, angles),
        );
        brush_model_instances += 1;
    }
    Ok((instances, brush_model_instances))
}

fn extend_model_faces(
    instances: &mut Vec<FaceInstance>,
    model: &BspModel,
    transform: SourceTransform,
) {
    // `Bsp::models` has already validated signed conversion, addition and the
    // upper bound against LUMP_FACES.
    let first = usize::try_from(model.first_face).expect("validated model face start");
    let count = usize::try_from(model.face_count).expect("validated model face count");
    instances.extend((first..first + count).map(|face_index| FaceInstance {
        face_index,
        transform,
    }));
}

fn parse_source_vec3(value: &str) -> Option<[f32; 3]> {
    let mut parts = value.split_ascii_whitespace();
    let parsed = [
        parts.next()?.parse::<f32>().ok()?,
        parts.next()?.parse::<f32>().ok()?,
        parts.next()?.parse::<f32>().ok()?,
    ];
    (parts.next().is_none() && parsed.iter().all(|component| component.is_finite()))
        .then_some(parsed)
}

struct FaceMaterial<'a> {
    index: MaterialIndex,
    tex_info: &'a BspTexInfo,
    tex_data: &'a BspTexData,
}

fn face_material<'a>(
    face_index: usize,
    face: BspFace,
    tex_infos: &'a [BspTexInfo],
    tex_data: &'a [BspTexData],
    texture_names: &[String],
    materials: &mut Vec<Material>,
    indices: &mut HashMap<String, MaterialIndex>,
) -> Result<Option<FaceMaterial<'a>>, Source1BspImportError> {
    if face.tex_info < 0 {
        return Ok(None);
    }
    let tex_info_index =
        usize::try_from(face.tex_info).map_err(|_| Source1BspImportError::InvalidReference {
            face: face_index,
            field: "texinfo",
            index: i64::from(face.tex_info),
        })?;
    let info = tex_infos
        .get(tex_info_index)
        .ok_or(Source1BspImportError::InvalidReference {
            face: face_index,
            field: "texinfo",
            index: i64::from(face.tex_info),
        })?;
    if info.tex_data < 0 {
        return Ok(None);
    }
    let tex_data_index =
        usize::try_from(info.tex_data).map_err(|_| Source1BspImportError::InvalidReference {
            face: face_index,
            field: "texdata",
            index: i64::from(info.tex_data),
        })?;
    let data = tex_data
        .get(tex_data_index)
        .ok_or(Source1BspImportError::InvalidReference {
            face: face_index,
            field: "texdata",
            index: i64::from(info.tex_data),
        })?;
    if data.name_string_table_id < 0 {
        return Ok(None);
    }
    let texture_name_index = usize::try_from(data.name_string_table_id).map_err(|_| {
        Source1BspImportError::InvalidReference {
            face: face_index,
            field: "texture name",
            index: i64::from(data.name_string_table_id),
        }
    })?;
    let name =
        texture_names
            .get(texture_name_index)
            .ok_or(Source1BspImportError::InvalidReference {
                face: face_index,
                field: "texture name",
                index: i64::from(data.name_string_table_id),
            })?;
    let normalized = normalize_material(name);
    let index = if let Some(index) = indices.get(&normalized) {
        *index
    } else {
        intern_material(&normalized, materials, indices)
    };
    Ok(Some(FaceMaterial {
        index,
        tex_info: info,
        tex_data: data,
    }))
}

fn intern_material(
    name: &str,
    materials: &mut Vec<Material>,
    indices: &mut HashMap<String, MaterialIndex>,
) -> MaterialIndex {
    if let Some(index) = indices.get(name) {
        return *index;
    }
    let index = MaterialIndex::new(materials.len());
    materials.push(Material::new().with_name(name.to_owned()));
    indices.insert(name.to_owned(), index);
    index
}

fn face_positions(
    face_index: usize,
    face: BspFace,
    vertices: &[BspVertex],
    edges: &[BspEdge],
    surf_edges: &[i32],
) -> Result<Vec<[f32; 3]>, Source1BspImportError> {
    let first =
        usize::try_from(face.first_edge).map_err(|_| Source1BspImportError::InvalidReference {
            face: face_index,
            field: "first edge",
            index: i64::from(face.first_edge),
        })?;
    let count =
        usize::try_from(face.edge_count).map_err(|_| Source1BspImportError::InvalidReference {
            face: face_index,
            field: "edge count",
            index: i64::from(face.edge_count),
        })?;
    let end = first
        .checked_add(count)
        .ok_or(Source1BspImportError::InvalidReference {
            face: face_index,
            field: "surface edge range",
            index: i64::MAX,
        })?;
    let face_edges = surf_edges
        .get(first..end)
        .ok_or(Source1BspImportError::InvalidReference {
            face: face_index,
            field: "surface edge range",
            index: i64::try_from(end).unwrap_or(i64::MAX),
        })?;
    let mut positions = Vec::with_capacity(count);
    for &signed_edge in face_edges {
        let edge_index = usize::try_from(signed_edge.unsigned_abs()).map_err(|_| {
            Source1BspImportError::InvalidReference {
                face: face_index,
                field: "edge",
                index: i64::from(signed_edge),
            }
        })?;
        let edge = edges
            .get(edge_index)
            .ok_or(Source1BspImportError::InvalidReference {
                face: face_index,
                field: "edge",
                index: i64::try_from(edge_index).unwrap_or(i64::MAX),
            })?;
        let vertex_index = usize::from(if signed_edge >= 0 {
            edge.vertices[0]
        } else {
            edge.vertices[1]
        });
        let vertex = vertices
            .get(vertex_index)
            .ok_or(Source1BspImportError::InvalidReference {
                face: face_index,
                field: "vertex",
                index: i64::try_from(vertex_index).unwrap_or(i64::MAX),
            })?;
        positions.push(vertex.position);
    }
    Ok(positions)
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "a BSP face edge count is an i16 and therefore always fits a u32 mesh index"
)]
fn cook_polygon(
    source_positions: &[[f32; 3]],
    uv: Option<&[[f32; 2]]>,
    outward: [f32; 3],
    material: Option<MaterialIndex>,
) -> Result<MeshPrimitive, yuyib_model::MeshValidationError> {
    let positions: Vec<_> = source_positions
        .iter()
        .copied()
        .map(source_position_to_yuyib)
        .collect();
    let mut indices = Vec::with_capacity((positions.len() - 2) * 3);
    for index in 1..positions.len() - 1 {
        indices.extend([0, index as u32, (index + 1) as u32]);
    }
    orient_triangles(&positions, &mut indices, outward);
    let position_count = positions.len();
    let mut primitive =
        MeshPrimitive::new(positions, indices)?.with_normals(vec![outward; position_count])?;
    if let Some(uv) = uv {
        primitive = primitive.with_tex_coords_0(uv.to_vec())?;
    }
    if let Some(material) = material {
        primitive = primitive.with_material(material);
    }
    Ok(primitive)
}

#[allow(
    clippy::too_many_arguments,
    reason = "displacement cooking validates the face, source quad, displacement records and material together"
)]
#[allow(
    clippy::too_many_lines,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    reason = "Source displacement power is validated to 2..=4, bounding grid indices and coordinates to 17x17"
)]
fn cook_displacement(
    face_index: usize,
    face: BspFace,
    source_positions: &[[f32; 3]],
    source_uv: Option<&[[f32; 2]]>,
    outward: [f32; 3],
    infos: &[BspDisplacementInfo],
    displacement_vertices: &[BspDisplacementVertex],
    material: Option<MaterialIndex>,
    transform: SourceTransform,
) -> Result<MeshPrimitive, Source1BspImportError> {
    if source_positions.len() != 4 {
        return Err(Source1BspImportError::InvalidDisplacement {
            face: face_index,
            reason: "base face is not a quad",
        });
    }
    let displacement_index = usize::try_from(face.displacement).map_err(|_| {
        Source1BspImportError::InvalidDisplacement {
            face: face_index,
            reason: "negative dispinfo index",
        }
    })?;
    let info = infos
        .get(displacement_index)
        .ok_or(Source1BspImportError::InvalidDisplacement {
            face: face_index,
            reason: "dispinfo index is outside the lump",
        })?;
    if !(2..=4).contains(&info.power) || info.displacement_vertex_start < 0 {
        return Err(Source1BspImportError::InvalidDisplacement {
            face: face_index,
            reason: "unsupported displacement power or vertex start",
        });
    }
    let mut corners = source_positions.to_vec();
    let start_corner = corners
        .iter()
        .enumerate()
        .min_by(|(_, left), (_, right)| {
            squared_distance(**left, info.start_position)
                .total_cmp(&squared_distance(**right, info.start_position))
        })
        .map_or(0, |(index, _)| index);
    corners.rotate_left(start_corner);
    let mut uvs = source_uv.map(<[[f32; 2]]>::to_vec);
    if let Some(uvs) = uvs.as_mut() {
        uvs.rotate_left(start_corner);
    }
    let subdivisions = 1_usize << info.power;
    let side = subdivisions + 1;
    let vertex_count = side * side;
    let first = usize::try_from(info.displacement_vertex_start).map_err(|_| {
        Source1BspImportError::InvalidDisplacement {
            face: face_index,
            reason: "negative displacement vertex start",
        }
    })?;
    let disp = displacement_vertices
        .get(first..first + vertex_count)
        .ok_or(Source1BspImportError::InvalidDisplacement {
            face: face_index,
            reason: "displacement vertex range is outside the lump",
        })?;
    let mut positions = Vec::with_capacity(vertex_count);
    let mut texture_blend_weights = Vec::with_capacity(vertex_count);
    let mut tex_coords = uvs.as_ref().map(|_| Vec::with_capacity(vertex_count));
    for row in 0..side {
        let along_edge = row as f32 / subdivisions as f32;
        for column in 0..side {
            let across_edges = column as f32 / subdivisions as f32;
            // Source stores each outer row along corner 0 -> 1, then each
            // inner column across that row toward corner 3 -> 2.
            let base = bilinear3(
                corners[0],
                corners[1],
                corners[2],
                corners[3],
                along_edge,
                across_edges,
            );
            let displacement = disp[row * side + column];
            texture_blend_weights.push(if displacement.alpha.is_finite() {
                (displacement.alpha / 255.0).clamp(0.0, 1.0)
            } else {
                0.0
            });
            positions.push(source_position_to_yuyib(transform.position(add3(
                base,
                scale3(displacement.vector, displacement.distance),
            ))));
            if let (Some(source), Some(target)) = (uvs.as_ref(), tex_coords.as_mut()) {
                target.push(bilinear2(
                    source[0],
                    source[1],
                    source[2],
                    source[3],
                    along_edge,
                    across_edges,
                ));
            }
        }
    }
    let mut indices = Vec::with_capacity(subdivisions * subdivisions * 6);
    for row in 0..subdivisions {
        for column in 0..subdivisions {
            let top_left = (row * side + column) as u32;
            let top_right = top_left + 1;
            let bottom_left = ((row + 1) * side + column) as u32;
            let bottom_right = bottom_left + 1;
            if top_left.is_multiple_of(2) {
                // Source's BL-to-TR diagonal.
                indices.extend([
                    top_left,
                    bottom_left,
                    bottom_right,
                    top_left,
                    bottom_right,
                    top_right,
                ]);
            } else {
                // Source's TL-to-BR diagonal.
                indices.extend([
                    top_left,
                    bottom_left,
                    top_right,
                    top_right,
                    bottom_left,
                    bottom_right,
                ]);
            }
        }
    }
    orient_triangles(&positions, &mut indices, outward);
    let normals = smooth_normals(&positions, &indices, outward);
    let mut primitive = MeshPrimitive::new(positions, indices)
        .map_err(|source| Source1BspImportError::Mesh {
            face: face_index,
            source,
        })?
        .with_normals(normals)
        .map_err(|source| Source1BspImportError::Mesh {
            face: face_index,
            source,
        })?;
    primitive = primitive
        .with_texture_blend_weights(texture_blend_weights)
        .map_err(|source| Source1BspImportError::Mesh {
            face: face_index,
            source,
        })?;
    if let Some(tex_coords) = tex_coords {
        primitive = primitive.with_tex_coords_0(tex_coords).map_err(|source| {
            Source1BspImportError::Mesh {
                face: face_index,
                source,
            }
        })?;
    }
    if let Some(material) = material {
        primitive = primitive.with_material(material);
    }
    Ok(primitive)
}

fn collider_from_model(model: &Model) -> Result<(TriangleMesh3d, usize), Source1BspImportError> {
    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    let mut skipped = 0;
    for mesh in model.meshes() {
        for primitive in mesh.primitives() {
            let base = u32::try_from(vertices.len())
                .map_err(|_| Source1BspImportError::ColliderVertexOverflow)?;
            vertices.extend(
                primitive
                    .positions()
                    .iter()
                    .map(|position| Vec3::new(position[0], position[1], position[2])),
            );
            for &[a, b, c] in primitive.indices().as_chunks::<3>().0 {
                let positions = primitive.positions();
                let point = |index: u32| {
                    let position = positions[index as usize];
                    Vec3::new(position[0], position[1], position[2])
                };
                let first_edge = point(b) - point(a);
                let second_edge = point(c) - point(a);
                let area_vector = Vec3::new(
                    first_edge.y * second_edge.z - first_edge.z * second_edge.y,
                    first_edge.z * second_edge.x - first_edge.x * second_edge.z,
                    first_edge.x * second_edge.y - first_edge.y * second_edge.x,
                );
                if area_vector.length_squared() <= f32::EPSILON {
                    skipped += 1;
                    continue;
                }
                for index in [a, b, c] {
                    indices.push(
                        base.checked_add(index)
                            .ok_or(Source1BspImportError::ColliderVertexOverflow)?,
                    );
                }
            }
        }
    }
    let collider = TriangleMesh3d::from_indexed(&vertices, &indices)
        .map_err(Source1BspImportError::Collider)?;
    Ok((collider, skipped))
}

enum MaterialTextureBinding {
    Single(Arc<StaticWorldTexture3d>),
    Blend {
        first: Arc<StaticWorldTexture3d>,
        second: Arc<StaticWorldTexture3d>,
    },
}

fn resolve_pak_water_vmt(
    material: &str,
    pak: &HashMap<String, Vec<u8>>,
    depth: usize,
) -> Result<Option<VmtMaterial>, String> {
    if depth >= 16 {
        return Err("VMT patch include depth exceeds 16".to_owned());
    }
    let path = material_asset_path(material, "vmt");
    let Some(bytes) = pak.get(&path) else {
        return Ok(None);
    };
    let text =
        std::str::from_utf8(bytes).map_err(|_| format!("embedded VMT {path} is not UTF-8"))?;
    let vmt =
        parse_vmt(text).map_err(|error| format!("cannot parse embedded VMT {path}: {error}"))?;
    if vmt.shader().eq_ignore_ascii_case("water") {
        return Ok(Some(vmt));
    }
    if !vmt.shader().eq_ignore_ascii_case("patch") {
        return Ok(None);
    }
    let include = vmt
        .block()
        .property("include")
        .ok_or_else(|| format!("embedded patch VMT {path} has no include"))?;
    resolve_pak_water_vmt(include, pak, depth + 1)
}

fn prepare_water_material(
    vmt: &VmtMaterial,
    pak: &HashMap<String, Vec<u8>>,
    external: Option<&Source1MaterialResolver>,
    cache: &mut HashMap<String, Vec<Arc<Source1WaterTexture3d>>>,
) -> Result<Source1WaterMaterial3d, String> {
    let normal_reference = vmt
        .normal_map()
        .ok_or_else(|| "Water VMT has no $normalmap".to_owned())?;
    let normal_path = material_asset_path(normal_reference, "vtf");
    let frames = if let Some(frames) = cache.get(&normal_path) {
        frames.clone()
    } else {
        let frames = if let Some(bytes) = pak.get(&normal_path) {
            let decoded = decode_vtf_frames(bytes).map_err(|error| error.to_string())?;
            decoded
                .frames_rgba8()
                .iter()
                .enumerate()
                .map(|(frame, rgba)| {
                    Source1WaterTexture3d::rgba8_repeating(
                        format!("bsp-pak:{normal_path}#frame={frame}"),
                        u32::from(decoded.width()),
                        u32::from(decoded.height()),
                        rgba.clone(),
                    )
                    .map(Arc::new)
                    .map_err(|error| error.to_string())
                })
                .collect::<Result<Vec<_>, _>>()?
        } else {
            let external = external.ok_or_else(|| {
                format!(
                    "Water normal VTF {normal_path} is absent from PAKFILE and no loose root is configured"
                )
            })?;
            let image = external
                .resolve_texture_reference(normal_reference)
                .map_err(|error| error.to_string())?;
            vec![Arc::new(
                Source1WaterTexture3d::rgba8_repeating(
                    image.path.to_string_lossy().into_owned(),
                    u32::from(image.width),
                    u32::from(image.height),
                    image.rgba8,
                )
                .map_err(|error| error.to_string())?,
            )]
        };
        cache.insert(normal_path.clone(), frames.clone());
        frames
    };

    let mut frame_rate = 0.0;
    let mut uv_scale = 1.0;
    let mut scroll_velocity = [0.0; 2];
    for proxy in vmt.proxies() {
        match proxy {
            VmtProxy::AnimatedTexture(proxy)
                if proxy
                    .texture_variable()
                    .is_some_and(|value| value.eq_ignore_ascii_case("$normalmap")) =>
            {
                frame_rate = proxy.frame_rate().unwrap_or(0.0);
            }
            VmtProxy::TextureScroll(proxy) => {
                uv_scale = proxy.texture_scale().unwrap_or(uv_scale);
                let rate = proxy.rate().unwrap_or(0.0);
                let angle = proxy.angle_degrees().unwrap_or(0.0).to_radians();
                scroll_velocity = [rate * angle.cos(), rate * angle.sin()];
            }
            _ => {}
        }
    }
    if frames.len() > 1 && frame_rate <= 0.0 {
        return Err("animated Water normal map has no positive AnimatedTexture frame rate".into());
    }
    let fog_rgb = vmt.fog_color().unwrap_or([27, 38, 41]);
    let fog_color = fog_rgb.map(srgb8_to_linear);
    let fog_start = vmt.fog_start().unwrap_or(150.0);
    let fog_end = vmt.fog_end().unwrap_or(304.0);
    let refract_amount = vmt.refract_amount().unwrap_or(0.08).clamp(0.0, 1.0);
    let reflect_amount = vmt.reflect_amount().unwrap_or(0.0).clamp(0.0, 1.0);
    let opacity = if vmt.above_water().unwrap_or(true) {
        (0.55 + refract_amount).clamp(0.0, 0.78)
    } else {
        0.72
    };
    let tint = fog_color.map(|component| (component * 1.35).min(1.0));
    let first = Arc::clone(
        frames
            .first()
            .ok_or_else(|| "Water normal VTF decoded no frames".to_owned())?,
    );
    let mut material = Source1WaterMaterial3d::new(first)
        .with_tint_and_opacity(tint, opacity)
        .with_fog(fog_color, fog_start, fog_end)
        .with_normal_strength((refract_amount * 4.0).clamp(0.12, 0.6))
        .with_normal_uv_scale(uv_scale)
        .with_scroll_velocity(scroll_velocity)
        .with_fresnel(5.0, reflect_amount)
        .with_scene_distortion(refract_amount, reflect_amount);
    if frames.len() > 1 {
        material = material.with_normal_animation(frames, frame_rate);
    }
    material.validate().map_err(|error| error.to_string())?;
    Ok(material)
}

fn build_water_world(
    model: &Model,
    water_materials: &HashMap<String, Source1WaterMaterial3d>,
) -> Result<Source1WaterWorld3d, Source1BspImportError> {
    let mut batches = Vec::new();
    for mesh in model.meshes() {
        for primitive in mesh.primitives() {
            let Some(material_index) = primitive.material() else {
                continue;
            };
            let Some(material_name) = model
                .materials()
                .get(material_index.get())
                .and_then(Material::name)
            else {
                continue;
            };
            let Some(material) = water_materials.get(&normalize_material(material_name)) else {
                continue;
            };
            batches.push(
                Source1WaterBatch3d::new(primitive.clone(), material.clone())
                    .map_err(Source1BspImportError::WaterBatch)?,
            );
        }
    }
    Source1WaterWorld3d::new(batches, Source1WaterLimits3d::default())
        .map_err(Source1BspImportError::WaterWorld)
}

fn srgb8_to_linear(component: u8) -> f32 {
    let value = f32::from(component) / 255.0;
    if value <= 0.040_45 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

struct ResolvedTexture {
    cache_key: String,
    width: u32,
    height: u32,
    rgba8: Vec<u8>,
    external: bool,
}

struct ResolvedMaterial {
    first: ResolvedTexture,
    second: Option<ResolvedTexture>,
    external: bool,
}

fn cache_resolved_texture(
    resolved: ResolvedTexture,
    textures_by_path: &mut HashMap<String, Arc<StaticWorldTexture3d>>,
) -> Result<Arc<StaticWorldTexture3d>, Source1BspImportError> {
    if let Some(texture) = textures_by_path.get(&resolved.cache_key) {
        return Ok(Arc::clone(texture));
    }
    let cache_key = resolved.cache_key;
    let texture = Arc::new(
        StaticWorldTexture3d::rgba8_repeating(
            cache_key.clone(),
            resolved.width,
            resolved.height,
            resolved.rgba8,
        )
        .map_err(Source1BspImportError::TexturePrepare)?,
    );
    textures_by_path.insert(cache_key, Arc::clone(&texture));
    Ok(texture)
}

fn resolve_material(
    material: &str,
    pak: &HashMap<String, Vec<u8>>,
    external: Option<&Source1MaterialResolver>,
) -> Result<ResolvedMaterial, String> {
    match resolve_pak_texture_references(material, pak, 0) {
        Ok(Some(references)) => {
            let first = resolve_texture(&references.first, pak, external)?;
            let second = references
                .second
                .as_deref()
                .map(|reference| resolve_texture(reference, pak, external))
                .transpose()?;
            let is_external =
                first.external || second.as_ref().is_some_and(|texture| texture.external);
            Ok(ResolvedMaterial {
                first,
                second,
                external: is_external,
            })
        }
        Ok(None) => {
            let external = external.ok_or_else(|| {
                "VMT is absent from PAKFILE and no loose material root is configured".to_owned()
            })?;
            let references = external
                .resolve_vmt_texture_references(material)
                .map_err(|error| error.to_string())?;
            let first = resolve_external_texture(&references.first, external)?;
            let second = references
                .second
                .as_deref()
                .map(|reference| resolve_external_texture(reference, external))
                .transpose()?;
            Ok(ResolvedMaterial {
                first,
                second,
                external: true,
            })
        }
        Err(error) => Err(error),
    }
}

fn resolve_texture(
    reference: &str,
    pak: &HashMap<String, Vec<u8>>,
    external: Option<&Source1MaterialResolver>,
) -> Result<ResolvedTexture, String> {
    let vtf_path = material_asset_path(reference, "vtf");
    if let Some(bytes) = pak.get(&vtf_path) {
        let image = decode_vtf(bytes).map_err(|error| error.to_string())?;
        return Ok(ResolvedTexture {
            cache_key: format!("bsp-pak:{vtf_path}"),
            width: u32::from(image.width()),
            height: u32::from(image.height()),
            rgba8: image.pixels_rgba8().to_vec(),
            external: false,
        });
    }
    let external = external.ok_or_else(|| {
        format!("VTF {vtf_path} is absent from PAKFILE and no loose root is configured")
    })?;
    resolve_external_texture(reference, external)
}

fn resolve_external_texture(
    reference: &str,
    external: &Source1MaterialResolver,
) -> Result<ResolvedTexture, String> {
    let image = external
        .resolve_texture_reference(reference)
        .map_err(|error| error.to_string())?;
    Ok(ResolvedTexture {
        cache_key: image.path.to_string_lossy().into_owned(),
        width: u32::from(image.width),
        height: u32::from(image.height),
        rgba8: image.rgba8,
        external: true,
    })
}

fn resolve_pak_texture_references(
    material: &str,
    pak: &HashMap<String, Vec<u8>>,
    depth: usize,
) -> Result<Option<Source1MaterialTextureReferences>, String> {
    if depth >= 16 {
        return Err("VMT patch include depth exceeds 16".to_owned());
    }
    let path = material_asset_path(material, "vmt");
    let Some(bytes) = pak.get(&path) else {
        return Ok(None);
    };
    let text =
        std::str::from_utf8(bytes).map_err(|_| format!("embedded VMT {path} is not UTF-8"))?;
    let vmt =
        parse_vmt(text).map_err(|error| format!("cannot parse embedded VMT {path}: {error}"))?;
    if !vmt.shader().eq_ignore_ascii_case("patch") {
        return Ok(Some(Source1MaterialTextureReferences {
            first: vmt
                .base_texture()
                .ok_or_else(|| format!("embedded VMT {path} has no $basetexture"))?
                .to_owned(),
            second: vmt.base_texture2().map(str::to_owned),
        }));
    }
    let include = vmt
        .block()
        .property("include")
        .ok_or_else(|| format!("embedded patch VMT {path} has no include"))?;
    let mut references = resolve_pak_texture_references(include, pak, depth + 1)?
        .ok_or_else(|| format!("embedded patch VMT {path} includes missing material {include}"))?;
    if let Some(first) = patch_texture_property(vmt.block(), "$basetexture") {
        first.clone_into(&mut references.first);
    }
    if let Some(second) = patch_texture_property(vmt.block(), "$basetexture2") {
        references.second = Some(second.to_owned());
    }
    Ok(Some(references))
}

fn patch_texture_property<'a>(block: &'a VmtBlock, property: &str) -> Option<&'a str> {
    block
        .blocks()
        .iter()
        .filter(|block| {
            block.name().eq_ignore_ascii_case("replace")
                || block.name().eq_ignore_ascii_case("insert")
        })
        .find_map(|block| block.property(property))
}

fn material_asset_path(value: &str, extension: &str) -> String {
    let value = normalize_path(value)
        .trim_start_matches("materials/")
        .to_owned();
    let value = value
        .strip_suffix(&format!(".{extension}"))
        .unwrap_or(&value);
    format!("materials/{value}.{extension}")
}

fn normalize_path(value: &str) -> String {
    value
        .replace('\\', "/")
        .trim_start_matches('/')
        .to_ascii_lowercase()
}

fn normalize_material(value: &str) -> String {
    normalize_path(value)
        .trim_start_matches("materials/")
        .trim_end_matches(".vmt")
        .to_owned()
}

#[allow(
    clippy::cast_precision_loss,
    reason = "Source texture dimensions are authored integer pixel counts used in f32 UV equations"
)]
fn texture_uv(position: [f32; 3], info: &BspTexInfo, data: &BspTexData) -> [f32; 2] {
    let width = data.width.max(1) as f32;
    let height = data.height.max(1) as f32;
    [
        (dot3(
            position,
            [
                info.texture_vectors[0][0],
                info.texture_vectors[0][1],
                info.texture_vectors[0][2],
            ],
        ) + info.texture_vectors[0][3])
            / width,
        (dot3(
            position,
            [
                info.texture_vectors[1][0],
                info.texture_vectors[1][1],
                info.texture_vectors[1][2],
            ],
        ) + info.texture_vectors[1][3])
            / height,
    ]
}

/// Converts Source's Z-up right-handed position into Yuyib's Y-up convention.
#[must_use]
pub const fn source_position_to_yuyib(source: [f32; 3]) -> [f32; 3] {
    [source[0], source[2], -source[1]]
}

/// Converts a Source direction without applying translation.
#[must_use]
pub const fn source_vector_to_yuyib(source: [f32; 3]) -> [f32; 3] {
    [source[0], source[2], -source[1]]
}

fn orient_triangles(positions: &[[f32; 3]], indices: &mut [u32], outward: [f32; 3]) {
    let Some(first) = indices.first_chunk::<3>() else {
        return;
    };
    let a = positions[first[0] as usize];
    let b = positions[first[1] as usize];
    let c = positions[first[2] as usize];
    if dot3(cross3(sub3(b, a), sub3(c, a)), outward) < 0.0 {
        for triangle in indices.as_chunks_mut::<3>().0 {
            triangle.swap(1, 2);
        }
    }
}

fn smooth_normals(positions: &[[f32; 3]], indices: &[u32], fallback: [f32; 3]) -> Vec<[f32; 3]> {
    let mut normals = vec![[0.0; 3]; positions.len()];
    for &[a, b, c] in indices.as_chunks::<3>().0 {
        let normal = cross3(
            sub3(positions[b as usize], positions[a as usize]),
            sub3(positions[c as usize], positions[a as usize]),
        );
        for index in [a, b, c] {
            normals[index as usize] = add3(normals[index as usize], normal);
        }
    }
    normals
        .into_iter()
        .map(|normal| normalize3(normal).unwrap_or(fallback))
        .collect()
}

fn bilinear3(
    top_left: [f32; 3],
    top_right: [f32; 3],
    bottom_right: [f32; 3],
    bottom_left: [f32; 3],
    horizontal: f32,
    vertical: f32,
) -> [f32; 3] {
    lerp3(
        lerp3(top_left, top_right, horizontal),
        lerp3(bottom_left, bottom_right, horizontal),
        vertical,
    )
}

fn bilinear2(
    top_left: [f32; 2],
    top_right: [f32; 2],
    bottom_right: [f32; 2],
    bottom_left: [f32; 2],
    horizontal: f32,
    vertical: f32,
) -> [f32; 2] {
    lerp2(
        lerp2(top_left, top_right, horizontal),
        lerp2(bottom_left, bottom_right, horizontal),
        vertical,
    )
}

fn lerp3(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [
        (b[0] - a[0]).mul_add(t, a[0]),
        (b[1] - a[1]).mul_add(t, a[1]),
        (b[2] - a[2]).mul_add(t, a[2]),
    ]
}

fn lerp2(a: [f32; 2], b: [f32; 2], t: f32) -> [f32; 2] {
    [
        (b[0] - a[0]).mul_add(t, a[0]),
        (b[1] - a[1]).mul_add(t, a[1]),
    ]
}

fn squared_distance(a: [f32; 3], b: [f32; 3]) -> f32 {
    let delta = sub3(a, b);
    dot3(delta, delta)
}

fn normalize3(value: [f32; 3]) -> Option<[f32; 3]> {
    let length = dot3(value, value).sqrt();
    (length.is_finite() && length > f32::EPSILON).then(|| scale3(value, length.recip()))
}

fn add3(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn sub3(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn scale3(value: [f32; 3], scale: f32) -> [f32; 3] {
    [value[0] * scale, value[1] * scale, value[2] * scale]
}

fn dot3(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0].mul_add(b[0], a[1].mul_add(b[1], a[2] * b[2]))
}

fn cross3(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1].mul_add(b[2], -a[2] * b[1]),
        a[2].mul_add(b[0], -a[0] * b[2]),
        a[0].mul_add(b[1], -a[1] * b[0]),
    ]
}

#[derive(Debug)]
pub enum Source1BspImportError {
    Read(BspReadError),
    Bsp(BspError),
    ExternalMaterialRoot(Source1AssetError),
    StaticPropAssets(Source1StaticPropAssetError),
    StudioModel(Source1StudioError),
    InvalidStaticPropModel {
        instance: usize,
        model: u16,
    },
    InvalidStaticPropMaterial {
        instance: usize,
        slot: usize,
    },
    StaticPropMesh {
        instance: usize,
        source: yuyib_model::MeshValidationError,
    },
    WaterBatch(Source1WaterBatchError3d),
    WaterWorld(Source1WaterWorldBuildError3d),
    MissingWorldModel,
    InvalidBrushModelReference {
        entity: usize,
        reference: String,
    },
    InvalidEntityTransform {
        entity: usize,
        field: &'static str,
        value: String,
    },
    InvalidReference {
        face: usize,
        field: &'static str,
        index: i64,
    },
    InvalidDisplacement {
        face: usize,
        reason: &'static str,
    },
    Mesh {
        face: usize,
        source: yuyib_model::MeshValidationError,
    },
    ModelMesh(yuyib_model::MeshError),
    Model(yuyib_model::ModelValidationError),
    TexturePrepare(StaticWorldTextureError3d),
    StaticWorld(TexturedStaticWorldBuildError3d),
    ColliderVertexOverflow,
    Collider(TriangleMeshError),
}

impl fmt::Display for Source1BspImportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read(source) => source.fmt(formatter),
            Self::Bsp(source) => source.fmt(formatter),
            Self::ExternalMaterialRoot(source) => {
                write!(formatter, "invalid loose Source material root: {source}")
            }
            Self::StaticPropAssets(source) => {
                write!(formatter, "cannot resolve BSP static-prop assets: {source}")
            }
            Self::StudioModel(source) => {
                write!(formatter, "cannot decode Source StudioModel: {source}")
            }
            Self::InvalidStaticPropModel { instance, model } => write!(
                formatter,
                "BSP static prop {instance} references missing decoded model {model}"
            ),
            Self::InvalidStaticPropMaterial { instance, slot } => write!(
                formatter,
                "BSP static prop {instance} has no material for mesh slot {slot}"
            ),
            Self::StaticPropMesh { instance, source } => write!(
                formatter,
                "BSP static prop {instance} produced invalid mesh geometry: {source}"
            ),
            Self::WaterBatch(source) => write!(formatter, "invalid BSP water batch: {source}"),
            Self::WaterWorld(source) => write!(formatter, "invalid BSP water world: {source}"),
            Self::MissingWorldModel => formatter.write_str("BSP has no world model (model 0)"),
            Self::InvalidBrushModelReference { entity, reference } => write!(
                formatter,
                "BSP entity {entity} references invalid brush model {reference}"
            ),
            Self::InvalidEntityTransform {
                entity,
                field,
                value,
            } => write!(
                formatter,
                "BSP entity {entity} has invalid {field} transform value {value:?}"
            ),
            Self::InvalidReference { face, field, index } => write!(
                formatter,
                "BSP face {face} references invalid {field} index {index}"
            ),
            Self::InvalidDisplacement { face, reason } => write!(
                formatter,
                "BSP face {face} has invalid displacement: {reason}"
            ),
            Self::Mesh { face, source } => write!(
                formatter,
                "BSP face {face} produced invalid mesh geometry: {source}"
            ),
            Self::ModelMesh(source) => write!(formatter, "BSP world mesh is invalid: {source}"),
            Self::Model(source) => write!(formatter, "BSP world model is invalid: {source}"),
            Self::TexturePrepare(source) => write!(
                formatter,
                "BSP material texture preparation failed: {source}"
            ),
            Self::StaticWorld(source) => {
                write!(formatter, "BSP static-world cook failed: {source}")
            }
            Self::ColliderVertexOverflow => {
                formatter.write_str("BSP collider exceeds u32 vertex addressing")
            }
            Self::Collider(source) => write!(formatter, "BSP collider cook failed: {source}"),
        }
    }
}

impl Error for Source1BspImportError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Read(source) => Some(source),
            Self::Bsp(source) => Some(source),
            Self::ExternalMaterialRoot(source) => Some(source),
            Self::StaticPropAssets(source) => Some(source),
            Self::StudioModel(source) => Some(source),
            Self::StaticPropMesh { source, .. } | Self::Mesh { source, .. } => Some(source),
            Self::WaterBatch(source) => Some(source),
            Self::WaterWorld(source) => Some(source),
            Self::ModelMesh(source) => Some(source),
            Self::Model(source) => Some(source),
            Self::TexturePrepare(source) => Some(source),
            Self::StaticWorld(source) => Some(source),
            Self::Collider(source) => Some(source),
            Self::MissingWorldModel
            | Self::InvalidStaticPropModel { .. }
            | Self::InvalidStaticPropMaterial { .. }
            | Self::InvalidBrushModelReference { .. }
            | Self::InvalidEntityTransform { .. }
            | Self::InvalidReference { .. }
            | Self::InvalidDisplacement { .. }
            | Self::ColliderVertexOverflow => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn single_pixel_bgr_vtf(bgr: [u8; 3]) -> Vec<u8> {
        let mut bytes = vec![0; 80];
        bytes[..4].copy_from_slice(b"VTF\0");
        bytes[4..8].copy_from_slice(&7_u32.to_le_bytes());
        bytes[8..12].copy_from_slice(&2_u32.to_le_bytes());
        bytes[12..16].copy_from_slice(&80_u32.to_le_bytes());
        bytes[16..18].copy_from_slice(&1_u16.to_le_bytes());
        bytes[18..20].copy_from_slice(&1_u16.to_le_bytes());
        bytes[24..26].copy_from_slice(&1_u16.to_le_bytes());
        bytes[52..56].copy_from_slice(&3_i32.to_le_bytes());
        bytes[56] = 1;
        bytes[57..61].copy_from_slice(&(-1_i32).to_le_bytes());
        bytes[63..65].copy_from_slice(&1_u16.to_le_bytes());
        bytes.extend_from_slice(&bgr);
        bytes
    }

    fn entities_from_text(text: &str) -> Vec<BspEntity> {
        const HEADER_BYTES: usize = 8 + 64 * 16 + 4;
        let mut bytes = vec![0; HEADER_BYTES];
        bytes[..4].copy_from_slice(b"VBSP");
        bytes[4..8].copy_from_slice(&20_i32.to_le_bytes());
        let offset = bytes.len();
        bytes.extend_from_slice(text.as_bytes());
        bytes.push(0);
        bytes[8..12].copy_from_slice(&i32::try_from(offset).expect("offset").to_le_bytes());
        bytes[12..16].copy_from_slice(
            &i32::try_from(text.len() + 1)
                .expect("entity length")
                .to_le_bytes(),
        );
        Bsp::parse(bytes, BspLimits::default())
            .expect("synthetic BSP")
            .entities()
            .expect("entities")
    }

    fn test_model(first_face: i32, face_count: i32, origin: [f32; 3]) -> BspModel {
        BspModel {
            mins: [0.0; 3],
            maxs: [0.0; 3],
            origin,
            head_node: 0,
            first_face,
            face_count,
        }
    }

    #[test]
    fn brush_model_faces_are_instanced_at_entity_origin_and_angles() {
        let entities = entities_from_text(
            r#"{
                "classname" "func_door_rotating"
                "model" "*1"
                "origin" "100 200 300"
                "angles" "0 90 0"
                "angle" "180"
            }"#,
        );
        let models = [test_model(0, 2, [0.0; 3]), test_model(2, 1, [999.0; 3])];

        let (instances, brush_instances) =
            face_instances(&models, &entities).expect("face instances");

        assert_eq!(brush_instances, 1);
        assert_eq!(
            instances
                .iter()
                .map(|instance| instance.face_index)
                .collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        let transformed = instances[2].transform.position([10.0, 0.0, 0.0]);
        assert!((transformed[0] - 100.0).abs() < 0.001);
        assert!((transformed[1] - 210.0).abs() < 0.001);
        assert!((transformed[2] - 300.0).abs() < 0.001);
    }

    #[test]
    fn displacement_rows_follow_source_corner_order_and_checkerboard_diagonals() {
        let corners = [
            [0.0, 0.0, 0.0],
            [4.0, 0.0, 0.0],
            [6.0, 3.0, 0.0],
            [0.0, 2.0, 0.0],
        ];
        let source_uv = [[0.0, 0.0], [4.0, 0.0], [6.0, 3.0], [0.0, 2.0]];
        let face = BspFace {
            plane: 0,
            side: false,
            on_node: true,
            first_edge: 0,
            edge_count: 4,
            tex_info: 0,
            displacement: 0,
            light_offset: -1,
            area: 12.0,
        };
        let info = BspDisplacementInfo {
            start_position: corners[0],
            displacement_vertex_start: 0,
            power: 2,
            map_face: 0,
        };
        let mut vertices = vec![
            BspDisplacementVertex {
                vector: [0.0, 0.0, 1.0],
                distance: 0.0,
                alpha: 0.0,
            };
            25
        ];
        vertices[0].alpha = -10.0;
        vertices[1].alpha = 89.25;
        vertices[2].alpha = 255.0;
        vertices[3].alpha = 8_192.0;

        let primitive = cook_displacement(
            0,
            face,
            &corners,
            Some(&source_uv),
            [0.0, 1.0, 0.0],
            &[info],
            &vertices,
            None,
            SourceTransform::IDENTITY,
        )
        .expect("valid displacement");

        // Inner column 1 moves from corner 0 toward corner 3. Outer row 1
        // moves from corner 0 toward corner 1, matching Valve's builddisp.
        assert_eq!(primitive.positions()[1], [0.0, 0.0, -0.5]);
        assert_eq!(primitive.positions()[5], [1.0, 0.0, 0.0]);
        assert_eq!(primitive.tex_coords_0().expect("UV0")[1], [0.0, 0.5]);
        assert_eq!(primitive.tex_coords_0().expect("UV0")[5], [1.0, 0.0]);
        assert_eq!(
            &primitive.texture_blend_weights().expect("blend weights")[..4],
            &[0.0, 0.35, 1.0, 1.0]
        );

        assert_eq!(
            &primitive.indices()[..12],
            &[0, 5, 6, 0, 6, 1, 1, 6, 2, 2, 6, 7]
        );
    }

    #[test]
    fn embedded_world_vertex_transition_retains_both_texture_references() {
        let pak = HashMap::from([(
            "materials/terrain/blend.vmt".to_owned(),
            br#"WorldVertexTransition {
                "$basetexture" "terrain/grass"
                "$basetexture2" "terrain/dirt"
            }"#
            .to_vec(),
        )]);

        let references = resolve_pak_texture_references("terrain/blend", &pak, 0)
            .expect("valid VMT")
            .expect("embedded VMT");

        assert_eq!(references.first, "terrain/grass");
        assert_eq!(references.second.as_deref(), Some("terrain/dirt"));
    }

    #[test]
    fn water_vmt_maps_refraction_and_reflection_distortion_in_source_order() {
        let vmt = parse_vmt(
            r#"Water {
                "$normalmap" "water/test_normal"
                "$refractamount" "0.17"
                "$reflectamount" "0.03"
                "$fogstart" "150"
                "$fogend" "304"
            }"#,
        )
        .expect("valid Water VMT");
        let pak = HashMap::from([(
            "materials/water/test_normal.vtf".to_owned(),
            single_pixel_bgr_vtf([255, 128, 128]),
        )]);
        let mut cache = HashMap::new();

        let material = prepare_water_material(&vmt, &pak, None, &mut cache)
            .expect("Water material with embedded normal map");

        assert_eq!(material.scene_distortion(), [0.17, 0.03]);
    }

    #[test]
    fn texture_uv_preserves_authored_source_scale_and_tiling() {
        let info = BspTexInfo {
            texture_vectors: [[0.35, 0.0, 0.0, 16.0], [0.0, 0.35, 0.0, -8.0]],
            lightmap_vectors: [[0.0; 4]; 2],
            flags: 0,
            tex_data: 0,
        };
        let data = BspTexData {
            reflectivity: [0.0; 3],
            name_string_table_id: 0,
            width: 128,
            height: 128,
            view_width: 128,
            view_height: 128,
        };

        let uv = texture_uv([1_280.0, 640.0, 0.0], &info, &data);

        assert_eq!(uv, [3.625, 1.6875]);
        assert!(
            uv[0] > 1.0,
            "authored UVs must repeat instead of stretching"
        );
    }
}
