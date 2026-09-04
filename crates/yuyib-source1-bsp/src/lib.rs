//! High-level Source 1 BSP import and static-world cooking.
//!
//! [`Source1BspLoader`] composes the low-level bounded BSP reader with Source
//! VMT/VTF decoding, PAKFILE and loose-file material providers, Yuyib model
//! geometry, textured static-world batching and triangle-mesh collision.

#![forbid(unsafe_code)]
#![allow(
    missing_docs,
    reason = "the crate-level pipeline documentation and typed field names define this initial integration surface"
)]

use std::{
    collections::{BTreeSet, HashMap},
    error::Error,
    fmt,
    path::{Path, PathBuf},
    sync::Arc,
};

use yuyib_bsp::{
    Bsp, BspDisplacementInfo, BspDisplacementVertex, BspEdge, BspEntity, BspError, BspFace,
    BspLimits, BspReadError, BspTexData, BspTexInfo, BspVertex,
};
use yuyib_model::{Material, MaterialIndex, Mesh, MeshPrimitive, Model};
use yuyib_physics::{TriangleMesh3d, TriangleMeshError, Vec3};
use yuyib_render_3d::{
    StaticWorldTexture3d, StaticWorldTextureError3d, TexturedStaticWorld3d,
    TexturedStaticWorldBuildError3d, TexturedStaticWorldMaterial3d,
};
use yuyib_source1_assets::{Source1AssetError, Source1MaterialResolver};
use yuyib_vmt::{VmtBlock, parse as parse_vmt};
use yuyib_vtf::decode as decode_vtf;

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

        let geometry = cook_geometry(bsp, self.options.skip_non_render_surface_flags)?;
        let mut textures_by_material = HashMap::new();
        let mut textures_by_path = HashMap::<String, Arc<StaticWorldTexture3d>>::new();
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
            match resolve_material(material_name, &pak, external.as_ref()) {
                Ok(resolved) => {
                    external_materials += usize::from(resolved.external);
                    let cache_key = resolved.cache_key;
                    let texture = if let Some(texture) = textures_by_path.get(&cache_key) {
                        Arc::clone(texture)
                    } else {
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
                        texture
                    };
                    textures_by_material.insert(normalize_material(material_name), texture);
                }
                Err(reason) => diagnostics.push(Source1BspMaterialDiagnostic {
                    material: material_name.clone(),
                    reason,
                }),
            }
        }

        let render_world = TexturedStaticWorld3d::from_model_with_materials(
            &geometry.model,
            |_index, material| {
                let Some(name) = material.name() else {
                    return TexturedStaticWorldMaterial3d::factor(material);
                };
                if self.hidden_material(name) {
                    TexturedStaticWorldMaterial3d::Skip
                } else if let Some(texture) = textures_by_material.get(&normalize_material(name)) {
                    TexturedStaticWorldMaterial3d::texture(material, Arc::clone(texture))
                } else {
                    TexturedStaticWorldMaterial3d::factor(material)
                }
            },
        )
        .map_err(Source1BspImportError::StaticWorld)?;
        let (collider, skipped_collision_triangles) = collider_from_model(&geometry.model)?;
        let hidden_materials = unique_material_names
            .iter()
            .filter(|material| self.hidden_material(material))
            .count();
        let report = Source1BspImportReport {
            bsp_version: bsp.version(),
            map_revision: bsp.map_revision(),
            source_faces: geometry.source_faces,
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
    cooked_primitives: usize,
    skipped_faces: usize,
}

#[allow(
    clippy::too_many_lines,
    reason = "the BSP face cook keeps validated lump relationships and deterministic diagnostics adjacent"
)]
fn cook_geometry(
    bsp: &Bsp,
    skip_non_render_flags: bool,
) -> Result<CookedGeometry, Source1BspImportError> {
    let vertices = bsp.vertices().map_err(Source1BspImportError::Bsp)?;
    let edges = bsp.edges().map_err(Source1BspImportError::Bsp)?;
    let surf_edges = bsp.surf_edges().map_err(Source1BspImportError::Bsp)?;
    let planes = bsp.planes().map_err(Source1BspImportError::Bsp)?;
    let faces = bsp.faces().map_err(Source1BspImportError::Bsp)?;
    let tex_info = bsp.tex_info().map_err(Source1BspImportError::Bsp)?;
    let tex_data = bsp.tex_data().map_err(Source1BspImportError::Bsp)?;
    let texture_names = bsp.texture_names().map_err(Source1BspImportError::Bsp)?;
    let displacement_info = bsp
        .displacement_info()
        .map_err(Source1BspImportError::Bsp)?;
    let displacement_vertices = bsp
        .displacement_vertices()
        .map_err(Source1BspImportError::Bsp)?;

    let mut materials = Vec::new();
    let mut material_indices = HashMap::<String, MaterialIndex>::new();
    let mut primitives = Vec::with_capacity(faces.len());
    let mut skipped_faces = 0;
    for (face_index, face) in faces.iter().copied().enumerate() {
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
        let source_positions = face_positions(face_index, face, &vertices, &edges, &surf_edges)?;
        let outward_source = if face.side {
            scale3(plane.normal, -1.0)
        } else {
            plane.normal
        };
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
            source_positions
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
                &source_positions,
                uv.as_deref(),
                outward,
                &displacement_info,
                &displacement_vertices,
                material,
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
        source_faces: faces.len(),
        cooked_primitives,
        skipped_faces,
    })
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
            positions.push(source_position_to_yuyib(add3(
                base,
                scale3(displacement.vector, displacement.distance),
            )));
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

struct ResolvedMaterial {
    cache_key: String,
    width: u32,
    height: u32,
    rgba8: Vec<u8>,
    external: bool,
}

fn resolve_material(
    material: &str,
    pak: &HashMap<String, Vec<u8>>,
    external: Option<&Source1MaterialResolver>,
) -> Result<ResolvedMaterial, String> {
    match resolve_pak_base_texture(material, pak, 0) {
        Ok(Some(base_texture)) => {
            let vtf_path = material_asset_path(&base_texture, "vtf");
            if let Some(bytes) = pak.get(&vtf_path) {
                let image = decode_vtf(bytes).map_err(|error| error.to_string())?;
                return Ok(ResolvedMaterial {
                    cache_key: format!("bsp-pak:{vtf_path}"),
                    width: u32::from(image.width()),
                    height: u32::from(image.height()),
                    rgba8: image.pixels_rgba8().to_vec(),
                    external: false,
                });
            }
            if let Some(external) = external {
                let image = external
                    .resolve_texture_reference(&base_texture)
                    .map_err(|error| error.to_string())?;
                return Ok(ResolvedMaterial {
                    cache_key: image.path.to_string_lossy().into_owned(),
                    width: u32::from(image.width),
                    height: u32::from(image.height),
                    rgba8: image.rgba8,
                    external: true,
                });
            }
            Err(format!(
                "VTF {vtf_path} is absent from PAKFILE and no loose root is configured"
            ))
        }
        Ok(None) => {
            let external = external.ok_or_else(|| {
                "VMT is absent from PAKFILE and no loose material root is configured".to_owned()
            })?;
            let image = external
                .resolve_vmt_path(material)
                .map_err(|error| error.to_string())?;
            Ok(ResolvedMaterial {
                cache_key: image.path.to_string_lossy().into_owned(),
                width: u32::from(image.width),
                height: u32::from(image.height),
                rgba8: image.rgba8,
                external: true,
            })
        }
        Err(error) => Err(error),
    }
}

fn resolve_pak_base_texture(
    material: &str,
    pak: &HashMap<String, Vec<u8>>,
    depth: usize,
) -> Result<Option<String>, String> {
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
        return vmt
            .base_texture()
            .map(str::to_owned)
            .map(Some)
            .ok_or_else(|| format!("embedded VMT {path} has no $basetexture"));
    }
    if let Some(base_texture) = patch_base_texture(vmt.block()) {
        return Ok(Some(base_texture.to_owned()));
    }
    let include = vmt
        .block()
        .property("include")
        .ok_or_else(|| format!("embedded patch VMT {path} has no include"))?;
    resolve_pak_base_texture(include, pak, depth + 1)?
        .ok_or_else(|| format!("embedded patch VMT {path} includes missing material {include}"))
        .map(Some)
}

fn patch_base_texture(block: &VmtBlock) -> Option<&str> {
    block
        .blocks()
        .iter()
        .filter(|block| {
            block.name().eq_ignore_ascii_case("replace")
                || block.name().eq_ignore_ascii_case("insert")
        })
        .find_map(|block| block.property("$basetexture"))
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
            Self::Mesh { source, .. } => Some(source),
            Self::ModelMesh(source) => Some(source),
            Self::Model(source) => Some(source),
            Self::TexturePrepare(source) => Some(source),
            Self::StaticWorld(source) => Some(source),
            Self::Collider(source) => Some(source),
            Self::InvalidReference { .. }
            | Self::InvalidDisplacement { .. }
            | Self::ColliderVertexOverflow => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let vertices = vec![
            BspDisplacementVertex {
                vector: [0.0, 0.0, 1.0],
                distance: 0.0,
                alpha: 0.0,
            };
            25
        ];

        let primitive = cook_displacement(
            0,
            face,
            &corners,
            Some(&source_uv),
            [0.0, 1.0, 0.0],
            &[info],
            &vertices,
            None,
        )
        .expect("valid displacement");

        // Inner column 1 moves from corner 0 toward corner 3. Outer row 1
        // moves from corner 0 toward corner 1, matching Valve's builddisp.
        assert_eq!(primitive.positions()[1], [0.0, 0.0, -0.5]);
        assert_eq!(primitive.positions()[5], [1.0, 0.0, 0.0]);
        assert_eq!(primitive.tex_coords_0().expect("UV0")[1], [0.0, 0.5]);
        assert_eq!(primitive.tex_coords_0().expect("UV0")[5], [1.0, 0.0]);

        assert_eq!(
            &primitive.indices()[..12],
            &[0, 5, 6, 0, 6, 1, 1, 6, 2, 2, 6, 7]
        );
    }
}
