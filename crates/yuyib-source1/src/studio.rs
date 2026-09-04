//! Bounded Source 1 `StudioModel` render-geometry decoding.
//!
//! This decoder intentionally covers the static render subset used by Source
//! MDL v48/v49, VVD v4 and optimized VTX v7 files: LOD 0 body-part models,
//! triangle lists/strips, positions, normals, UV0, skin tables and material
//! search paths and one explicit body-group combination. Animation, flexes, bone skinning,
//! collision PHY data and lower LOD selection are outside this module.

use std::{error::Error, fmt, sync::Arc};

/// Matching files that make one Source 1 StudioModel render asset.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Source1StudioModelFiles {
    /// Canonical model path relative to the Source content root.
    pub model_path: String,
    /// Studio header and material/body-part declarations.
    pub mdl: Arc<[u8]>,
    /// Studio vertex data.
    pub vvd: Arc<[u8]>,
    /// Selected optimized index/strip data.
    pub vtx: Arc<[u8]>,
    /// Actual VTX variant selected, normally `.dx90.vtx`.
    pub vtx_path: String,
}

const MDL_HEADER_MIN_BYTES: usize = 240;
const MDL_TEXTURE_BYTES: usize = 64;
const MDL_BODY_PART_BYTES: usize = 16;
const MDL_MODEL_BYTES: usize = 148;
const MDL_MESH_BYTES: usize = 116;
const VVD_HEADER_BYTES: usize = 64;
const VVD_FIXUP_BYTES: usize = 12;
const VVD_VERTEX_BYTES: usize = 48;
const VTX_HEADER_BYTES: usize = 36;
const VTX_BODY_PART_BYTES: usize = 8;
const VTX_MODEL_BYTES: usize = 8;
const VTX_LOD_BYTES: usize = 12;
const VTX_MESH_BYTES: usize = 9;
const VTX_STRIP_GROUP_BYTES: usize = 25;
const VTX_VERTEX_BYTES: usize = 9;
const VTX_STRIP_BYTES: usize = 27;
const STRIP_IS_TRI_LIST: u8 = 0x01;
const STRIP_IS_TRI_STRIP: u8 = 0x02;

/// Bounded-work policy for untrusted `StudioModel` sidecars.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Source1StudioLimits {
    /// Maximum bytes accepted for any one MDL/VVD/VTX sidecar.
    pub max_file_bytes: usize,
    /// Maximum texture declarations and skin references.
    pub max_materials: usize,
    /// Maximum body parts.
    pub max_body_parts: usize,
    /// Maximum models across all body parts.
    pub max_models: usize,
    /// Maximum meshes across all selected models.
    pub max_meshes: usize,
    /// Maximum decoded vertices across all meshes.
    pub max_vertices: usize,
    /// Maximum decoded triangle indices across all meshes.
    pub max_indices: usize,
    /// Maximum bytes scanned for one MDL string.
    pub max_string_bytes: usize,
}

impl Default for Source1StudioLimits {
    fn default() -> Self {
        Self {
            max_file_bytes: 256 * 1024 * 1024,
            max_materials: 65_536,
            max_body_parts: 4_096,
            max_models: 65_536,
            max_meshes: 1_000_000,
            max_vertices: 16_000_000,
            max_indices: 48_000_000,
            max_string_bytes: 16 * 1024,
        }
    }
}

/// One material name and the Source search paths declared by its MDL.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Source1StudioMaterial {
    /// Texture/material basename declared by `mstudiotexture_t`.
    pub name: String,
    /// Ordered relative VMT candidates (without `materials/` and extension).
    pub candidates: Vec<String>,
}

/// One indexed render mesh decoded from matching MDL/VVD/VTX records.
#[derive(Clone, Debug, PartialEq)]
pub struct Source1StudioMesh {
    /// MDL body-part index.
    pub body_part: usize,
    /// Model index within the body part. Static props normally select zero.
    pub body_model: usize,
    /// Name stored by `mstudiomodel_t`.
    pub model_name: String,
    /// Skin-reference slot used to choose a material per skin family.
    pub material_slot: usize,
    /// Source-space vertex positions.
    pub positions: Vec<[f32; 3]>,
    /// Source-space vertex normals.
    pub normals: Vec<[f32; 3]>,
    /// Primary texture coordinates.
    pub tex_coords_0: Vec<[f32; 2]>,
    /// Triangle-list indices into the arrays above.
    pub indices: Vec<u32>,
}

/// Real static render geometry decoded from a Source `StudioModel` family.
#[derive(Clone, Debug, PartialEq)]
pub struct Source1StudioModel {
    /// Source path of the owning MDL.
    pub path: String,
    /// MDL format version (48 or 49).
    pub mdl_version: i32,
    /// Sidecar checksum shared by MDL, VVD and VTX.
    pub checksum: i32,
    /// Texture declarations.
    pub materials: Vec<Source1StudioMaterial>,
    /// Skin families mapping each mesh material slot to `materials`.
    pub skin_families: Vec<Vec<u16>>,
    /// Decoded LOD-0 render meshes.
    pub meshes: Vec<Source1StudioMesh>,
}

impl Source1StudioModel {
    /// Resolves one mesh's material for an authored static-prop skin.
    #[must_use]
    pub fn material_for_skin(
        &self,
        mesh: &Source1StudioMesh,
        skin: i32,
    ) -> Option<&Source1StudioMaterial> {
        let family = usize::try_from(skin)
            .ok()
            .filter(|index| *index < self.skin_families.len())
            .unwrap_or(0);
        let material = self
            .skin_families
            .get(family)?
            .get(mesh.material_slot)
            .copied()?;
        self.materials.get(usize::from(material))
    }
}

/// Source static-prop placement transform.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Source1StaticPropTransform {
    /// Source-space origin.
    pub origin: [f32; 3],
    /// Source `QAngle` pitch/yaw/roll in degrees.
    pub angles: [f32; 3],
    /// Uniform model scale. `sprp` v10 instances use `1.0`.
    pub uniform_scale: f32,
}

impl Source1StaticPropTransform {
    /// Transforms a Source-space point into Yuyib's right-handed Y-up space.
    #[must_use]
    pub fn transform_position(self, position: [f32; 3]) -> [f32; 3] {
        let scaled = scale3(position, self.uniform_scale);
        let source = add3(rotate_qangle(scaled, self.angles), self.origin);
        source_to_yuyib(source)
    }

    /// Rotates a Source-space normal into Yuyib space.
    #[must_use]
    pub fn transform_normal(self, normal: [f32; 3]) -> [f32; 3] {
        normalize3(source_to_yuyib(rotate_qangle(normal, self.angles)))
    }
}

/// Failure while decoding a `StudioModel` render subset.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Source1StudioError {
    /// A sidecar exceeds configured memory/work policy.
    FileLimit {
        /// Sidecar kind.
        file: &'static str,
        /// Observed bytes.
        actual: usize,
        /// Configured maximum.
        limit: usize,
    },
    /// Magic bytes do not identify the expected format.
    InvalidMagic {
        /// Sidecar kind.
        file: &'static str,
        /// Observed bytes.
        actual: [u8; 4],
    },
    /// Sidecar version is outside the deliberately supported subset.
    UnsupportedVersion {
        /// Sidecar kind.
        file: &'static str,
        /// Observed version.
        version: i32,
    },
    /// VVD/VTX does not belong to the supplied MDL.
    ChecksumMismatch {
        /// Sidecar kind.
        file: &'static str,
        /// MDL checksum.
        expected: i32,
        /// Sidecar checksum.
        actual: i32,
    },
    /// A byte range points outside its sidecar.
    InvalidRange {
        /// Sidecar kind.
        file: &'static str,
        /// Structure/array being read.
        section: &'static str,
        /// Absolute offset.
        offset: usize,
        /// Requested bytes.
        length: usize,
        /// Available bytes.
        available: usize,
    },
    /// A signed count/offset was negative.
    NegativeField {
        /// Sidecar kind.
        file: &'static str,
        /// Field name.
        field: &'static str,
        /// Invalid value.
        value: i32,
    },
    /// A record count exceeds configured work policy.
    RecordLimit {
        /// Logical section.
        section: &'static str,
        /// Observed/declared count.
        actual: usize,
        /// Configured maximum.
        limit: usize,
    },
    /// A fixed/relative MDL string is invalid or unterminated.
    InvalidString {
        /// Logical string owner.
        section: &'static str,
        /// Absolute byte offset.
        offset: usize,
    },
    /// MDL and VTX hierarchy counts disagree.
    HierarchyMismatch {
        /// Hierarchy level.
        section: &'static str,
        /// MDL count.
        mdl: usize,
        /// VTX count.
        vtx: usize,
    },
    /// A VTX strip has no supported, unambiguous primitive mode.
    UnsupportedStripFlags {
        /// Raw flags.
        flags: u8,
    },
    /// An index references outside its strip-group or decoded VVD vertices.
    InvalidReference {
        /// Referencing section.
        section: &'static str,
        /// Referenced index.
        index: usize,
        /// Available records.
        available: usize,
    },
    /// VVD fixups do not reconstruct the advertised LOD-0 vertex count.
    InvalidFixupVertexCount {
        /// Reconstructed vertices.
        actual: usize,
        /// VVD header count.
        expected: usize,
    },
}

impl fmt::Display for Source1StudioError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FileLimit {
                file,
                actual,
                limit,
            } => write!(
                formatter,
                "Source {file} is {actual} bytes; limit is {limit}"
            ),
            Self::InvalidMagic { file, actual } => {
                write!(formatter, "invalid Source {file} magic {actual:?}")
            }
            Self::UnsupportedVersion { file, version } => {
                write!(formatter, "unsupported Source {file} version {version}")
            }
            Self::ChecksumMismatch {
                file,
                expected,
                actual,
            } => write!(
                formatter,
                "Source {file} checksum {actual} does not match MDL checksum {expected}"
            ),
            Self::InvalidRange {
                file,
                section,
                offset,
                length,
                available,
            } => write!(
                formatter,
                "Source {file} {section} range {offset}+{length} exceeds {available} bytes"
            ),
            Self::NegativeField { file, field, value } => {
                write!(formatter, "Source {file} {field} is negative ({value})")
            }
            Self::RecordLimit {
                section,
                actual,
                limit,
            } => write!(
                formatter,
                "Source StudioModel {section} has {actual} records; limit is {limit}"
            ),
            Self::InvalidString { section, offset } => write!(
                formatter,
                "Source MDL {section} string near byte {offset} is invalid or unterminated"
            ),
            Self::HierarchyMismatch { section, mdl, vtx } => write!(
                formatter,
                "Source MDL/VTX {section} count mismatch: MDL {mdl}, VTX {vtx}"
            ),
            Self::UnsupportedStripFlags { flags } => {
                write!(formatter, "unsupported Source VTX strip flags {flags:#04x}")
            }
            Self::InvalidReference {
                section,
                index,
                available,
            } => write!(
                formatter,
                "Source StudioModel {section} index {index} exceeds {available} records"
            ),
            Self::InvalidFixupVertexCount { actual, expected } => write!(
                formatter,
                "Source VVD fixups reconstruct {actual} LOD-0 vertices; header declares {expected}"
            ),
        }
    }
}

impl Error for Source1StudioError {}

/// Decodes real LOD-0 render geometry from a matching MDL/VVD/VTX family.
///
/// # Errors
///
/// Returns typed format, range, checksum, hierarchy and work-limit failures.
pub fn decode_studio_model(
    files: &Source1StudioModelFiles,
    limits: Source1StudioLimits,
) -> Result<Source1StudioModel, Source1StudioError> {
    decode_studio_model_with_body(files, limits, 0)
}

/// Decodes real LOD-0 render geometry for one Source body-group integer.
///
/// For each body part, Source selects `(body / base) % model_count`. Negative
/// values select the default body (`0`).
///
/// # Errors
///
/// Returns typed format, range, checksum, hierarchy and work-limit failures.
pub fn decode_studio_model_with_body(
    files: &Source1StudioModelFiles,
    limits: Source1StudioLimits,
    body: i32,
) -> Result<Source1StudioModel, Source1StudioError> {
    check_file_limit("MDL", files.mdl.len(), limits.max_file_bytes)?;
    check_file_limit("VVD", files.vvd.len(), limits.max_file_bytes)?;
    check_file_limit("VTX", files.vtx.len(), limits.max_file_bytes)?;
    let mdl = Blob::new("MDL", &files.mdl);
    let vvd = Blob::new("VVD", &files.vvd);
    let vtx = Blob::new("VTX", &files.vtx);

    mdl.require(0, MDL_HEADER_MIN_BYTES, "header")?;
    mdl.magic(b"IDST")?;
    let mdl_version = mdl.i32(4, "version")?;
    if !matches!(mdl_version, 44..=49) {
        return Err(Source1StudioError::UnsupportedVersion {
            file: "MDL",
            version: mdl_version,
        });
    }
    let checksum = mdl.i32(8, "checksum")?;
    validate_vvd(&vvd, checksum)?;
    validate_vtx(&vtx, checksum)?;

    let (materials, skin_families) = decode_materials(&mdl, limits)?;
    let vvd_vertices = decode_vvd_lod0(&vvd, limits)?;
    let meshes = decode_meshes(&mdl, &vtx, &vvd_vertices, limits, body)?;
    Ok(Source1StudioModel {
        path: files.model_path.clone(),
        mdl_version,
        checksum,
        materials,
        skin_families,
        meshes,
    })
}

#[derive(Clone, Copy)]
struct StudioVertex {
    position: [f32; 3],
    normal: [f32; 3],
    uv: [f32; 2],
}

#[derive(Clone, Copy)]
struct Blob<'a> {
    kind: &'static str,
    bytes: &'a [u8],
}

impl<'a> Blob<'a> {
    const fn new(kind: &'static str, bytes: &'a [u8]) -> Self {
        Self { kind, bytes }
    }

    #[allow(
        clippy::trivially_copy_pass_by_ref,
        reason = "byte-string magic constants are naturally passed as references"
    )]
    fn magic(self, expected: &[u8; 4]) -> Result<(), Source1StudioError> {
        self.require(0, 4, "magic")?;
        let actual: [u8; 4] = self.bytes[..4].try_into().expect("validated magic");
        if &actual == expected {
            Ok(())
        } else {
            Err(Source1StudioError::InvalidMagic {
                file: self.kind,
                actual,
            })
        }
    }

    fn require(
        self,
        offset: usize,
        length: usize,
        section: &'static str,
    ) -> Result<&'a [u8], Source1StudioError> {
        let end = offset.saturating_add(length);
        self.bytes
            .get(offset..end)
            .ok_or(Source1StudioError::InvalidRange {
                file: self.kind,
                section,
                offset,
                length,
                available: self.bytes.len(),
            })
    }

    fn array(
        self,
        offset: usize,
        count: usize,
        stride: usize,
        section: &'static str,
        limit: usize,
    ) -> Result<&'a [u8], Source1StudioError> {
        bounded(section, count, limit)?;
        let length = count
            .checked_mul(stride)
            .ok_or(Source1StudioError::RecordLimit {
                section,
                actual: count,
                limit,
            })?;
        self.require(offset, length, section)
    }

    fn u8(self, offset: usize, section: &'static str) -> Result<u8, Source1StudioError> {
        Ok(self.require(offset, 1, section)?[0])
    }

    fn u16(self, offset: usize, section: &'static str) -> Result<u16, Source1StudioError> {
        Ok(u16::from_le_bytes(
            self.require(offset, 2, section)?
                .try_into()
                .expect("validated u16"),
        ))
    }

    fn i32(self, offset: usize, section: &'static str) -> Result<i32, Source1StudioError> {
        Ok(i32::from_le_bytes(
            self.require(offset, 4, section)?
                .try_into()
                .expect("validated i32"),
        ))
    }

    fn f32(self, offset: usize, section: &'static str) -> Result<f32, Source1StudioError> {
        Ok(f32::from_le_bytes(
            self.require(offset, 4, section)?
                .try_into()
                .expect("validated f32"),
        ))
    }

    fn non_negative(self, offset: usize, field: &'static str) -> Result<usize, Source1StudioError> {
        let value = self.i32(offset, field)?;
        usize::try_from(value).map_err(|_| Source1StudioError::NegativeField {
            file: self.kind,
            field,
            value,
        })
    }

    fn relative(
        self,
        base: usize,
        field_offset: usize,
        field: &'static str,
    ) -> Result<usize, Source1StudioError> {
        let relative = self.non_negative(base + field_offset, field)?;
        base.checked_add(relative)
            .ok_or(Source1StudioError::InvalidRange {
                file: self.kind,
                section: field,
                offset: base,
                length: relative,
                available: self.bytes.len(),
            })
    }

    fn c_string(
        self,
        offset: usize,
        max_bytes: usize,
        section: &'static str,
    ) -> Result<String, Source1StudioError> {
        let available = self
            .bytes
            .get(offset..)
            .ok_or(Source1StudioError::InvalidString { section, offset })?;
        let bounded = &available[..available.len().min(max_bytes)];
        let end = bounded
            .iter()
            .position(|byte| *byte == 0)
            .ok_or(Source1StudioError::InvalidString { section, offset })?;
        std::str::from_utf8(&bounded[..end])
            .map(str::to_owned)
            .map_err(|_| Source1StudioError::InvalidString { section, offset })
    }
}

fn validate_vvd(vvd: &Blob<'_>, checksum: i32) -> Result<(), Source1StudioError> {
    vvd.require(0, VVD_HEADER_BYTES, "header")?;
    vvd.magic(b"IDSV")?;
    let version = vvd.i32(4, "version")?;
    if version != 4 {
        return Err(Source1StudioError::UnsupportedVersion {
            file: "VVD",
            version,
        });
    }
    matching_checksum(*vvd, 8, checksum)
}

fn validate_vtx(vtx: &Blob<'_>, checksum: i32) -> Result<(), Source1StudioError> {
    vtx.require(0, VTX_HEADER_BYTES, "header")?;
    let version = vtx.i32(0, "version")?;
    if version != 7 {
        return Err(Source1StudioError::UnsupportedVersion {
            file: "VTX",
            version,
        });
    }
    matching_checksum(*vtx, 16, checksum)
}

fn matching_checksum(
    blob: Blob<'_>,
    offset: usize,
    expected: i32,
) -> Result<(), Source1StudioError> {
    let actual = blob.i32(offset, "checksum")?;
    if actual == expected {
        Ok(())
    } else {
        Err(Source1StudioError::ChecksumMismatch {
            file: blob.kind,
            expected,
            actual,
        })
    }
}

fn decode_materials(
    mdl: &Blob<'_>,
    limits: Source1StudioLimits,
) -> Result<(Vec<Source1StudioMaterial>, Vec<Vec<u16>>), Source1StudioError> {
    let texture_count = mdl.non_negative(204, "texture count")?;
    let texture_offset = mdl.non_negative(208, "texture offset")?;
    let texture_records = mdl.array(
        texture_offset,
        texture_count,
        MDL_TEXTURE_BYTES,
        "textures",
        limits.max_materials,
    )?;
    let directory_count = mdl.non_negative(212, "texture directory count")?;
    bounded("texture directories", directory_count, limits.max_materials)?;
    let directory_table = mdl.non_negative(216, "texture directory table")?;
    mdl.array(
        directory_table,
        directory_count,
        4,
        "texture directories",
        limits.max_materials,
    )?;
    let mut directories = Vec::with_capacity(directory_count);
    for index in 0..directory_count {
        let offset = mdl.non_negative(directory_table + index * 4, "texture directory")?;
        directories.push(normalize_material_path(&mdl.c_string(
            offset,
            limits.max_string_bytes,
            "texture directory",
        )?));
    }

    let mut materials = Vec::with_capacity(texture_count);
    for (index, _) in texture_records.chunks_exact(MDL_TEXTURE_BYTES).enumerate() {
        let record = texture_offset + index * MDL_TEXTURE_BYTES;
        let name_offset = mdl.relative(record, 0, "texture name")?;
        let name = normalize_material_path(&mdl.c_string(
            name_offset,
            limits.max_string_bytes,
            "texture name",
        )?);
        let mut candidates = Vec::new();
        if name.contains('/') {
            candidates.push(name.clone());
        } else {
            for directory in &directories {
                let candidate = if directory.is_empty() {
                    name.clone()
                } else {
                    format!("{}/{name}", directory.trim_end_matches('/'))
                };
                if !candidates.contains(&candidate) {
                    candidates.push(candidate);
                }
            }
            if candidates.is_empty() {
                candidates.push(name.clone());
            }
        }
        materials.push(Source1StudioMaterial { name, candidates });
    }

    let references = mdl.non_negative(220, "skin reference count")?;
    let families = mdl.non_negative(224, "skin family count")?;
    bounded("skin references", references, limits.max_materials)?;
    bounded("skin families", families, limits.max_materials)?;
    let total = references
        .checked_mul(families)
        .ok_or(Source1StudioError::RecordLimit {
            section: "skin table",
            actual: usize::MAX,
            limit: limits.max_materials,
        })?;
    bounded("skin table", total, limits.max_materials)?;
    let skin_offset = mdl.non_negative(228, "skin table offset")?;
    let skin_bytes = mdl.array(skin_offset, total, 2, "skin table", limits.max_materials)?;
    let skin_families = skin_bytes
        .chunks_exact(references.saturating_mul(2).max(1))
        .take(families)
        .map(|family| {
            family
                .chunks_exact(2)
                .map(|entry| u16::from_le_bytes(entry.try_into().expect("skin ref")))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    for material in skin_families.iter().flatten().copied() {
        if usize::from(material) >= materials.len() {
            return Err(Source1StudioError::InvalidReference {
                section: "skin material",
                index: usize::from(material),
                available: materials.len(),
            });
        }
    }
    Ok((materials, skin_families))
}

fn decode_vvd_lod0(
    vvd: &Blob<'_>,
    limits: Source1StudioLimits,
) -> Result<Vec<StudioVertex>, Source1StudioError> {
    let expected = vvd.non_negative(16, "LOD-0 vertex count")?;
    bounded("VVD vertices", expected, limits.max_vertices)?;
    let fixup_count = vvd.non_negative(48, "fixup count")?;
    bounded("VVD fixups", fixup_count, limits.max_vertices)?;
    let fixup_offset = vvd.non_negative(52, "fixup offset")?;
    let vertex_offset = vvd.non_negative(56, "vertex offset")?;
    let mut source_indices = Vec::with_capacity(expected);
    if fixup_count == 0 {
        source_indices.extend(0..expected);
    } else {
        let records = vvd.array(
            fixup_offset,
            fixup_count,
            VVD_FIXUP_BYTES,
            "VVD fixups",
            limits.max_vertices,
        )?;
        for record in records.chunks_exact(VVD_FIXUP_BYTES) {
            let record = Blob::new("VVD", record);
            let lod = record.i32(0, "fixup LOD")?;
            if lod < 0 {
                return Err(Source1StudioError::NegativeField {
                    file: "VVD",
                    field: "fixup LOD",
                    value: lod,
                });
            }
            let source = record.non_negative(4, "fixup source vertex")?;
            let count = record.non_negative(8, "fixup vertex count")?;
            let end = source
                .checked_add(count)
                .ok_or(Source1StudioError::InvalidReference {
                    section: "VVD fixup vertex",
                    index: usize::MAX,
                    available: expected,
                })?;
            source_indices.extend(source..end);
            bounded("VVD vertices", source_indices.len(), limits.max_vertices)?;
        }
    }
    if source_indices.len() != expected {
        return Err(Source1StudioError::InvalidFixupVertexCount {
            actual: source_indices.len(),
            expected,
        });
    }
    let max_source = source_indices.iter().copied().max().unwrap_or(0);
    if !source_indices.is_empty() {
        vvd.array(
            vertex_offset,
            max_source + 1,
            VVD_VERTEX_BYTES,
            "VVD vertices",
            limits.max_vertices,
        )?;
    }
    let mut vertices = Vec::with_capacity(expected);
    for source in source_indices {
        let record = vertex_offset + source * VVD_VERTEX_BYTES;
        vertices.push(StudioVertex {
            position: [
                vvd.f32(record + 16, "vertex position")?,
                vvd.f32(record + 20, "vertex position")?,
                vvd.f32(record + 24, "vertex position")?,
            ],
            normal: [
                vvd.f32(record + 28, "vertex normal")?,
                vvd.f32(record + 32, "vertex normal")?,
                vvd.f32(record + 36, "vertex normal")?,
            ],
            uv: [
                vvd.f32(record + 40, "vertex UV")?,
                vvd.f32(record + 44, "vertex UV")?,
            ],
        });
    }
    Ok(vertices)
}

#[allow(
    clippy::too_many_lines,
    reason = "parallel MDL/VTX hierarchy traversal keeps all cross-file range validation adjacent"
)]
fn decode_meshes(
    mdl: &Blob<'_>,
    vtx: &Blob<'_>,
    vertices: &[StudioVertex],
    limits: Source1StudioLimits,
    body: i32,
) -> Result<Vec<Source1StudioMesh>, Source1StudioError> {
    let mdl_body_count = mdl.non_negative(232, "body part count")?;
    bounded("body parts", mdl_body_count, limits.max_body_parts)?;
    let mdl_body_offset = mdl.non_negative(236, "body part offset")?;
    mdl.array(
        mdl_body_offset,
        mdl_body_count,
        MDL_BODY_PART_BYTES,
        "body parts",
        limits.max_body_parts,
    )?;
    let vtx_body_count = vtx.non_negative(28, "body part count")?;
    let vtx_body_offset = vtx.non_negative(32, "body part offset")?;
    vtx.array(
        vtx_body_offset,
        vtx_body_count,
        VTX_BODY_PART_BYTES,
        "body parts",
        limits.max_body_parts,
    )?;
    same_count("body parts", mdl_body_count, vtx_body_count)?;

    let mut output = Vec::new();
    let mut model_total = 0;
    let mut vertex_total = 0;
    let mut index_total = 0;
    for body_index in 0..mdl_body_count {
        let mdl_body = mdl_body_offset + body_index * MDL_BODY_PART_BYTES;
        let vtx_body = vtx_body_offset + body_index * VTX_BODY_PART_BYTES;
        let mdl_model_count = mdl.non_negative(mdl_body + 4, "body model count")?;
        let mdl_models = mdl.relative(mdl_body, 12, "body model offset")?;
        mdl.array(
            mdl_models,
            mdl_model_count,
            MDL_MODEL_BYTES,
            "body models",
            limits.max_models,
        )?;
        let vtx_model_count = vtx.non_negative(vtx_body, "body model count")?;
        let vtx_models = vtx.relative(vtx_body, 4, "body model offset")?;
        vtx.array(
            vtx_models,
            vtx_model_count,
            VTX_MODEL_BYTES,
            "body models",
            limits.max_models,
        )?;
        same_count("body models", mdl_model_count, vtx_model_count)?;
        model_total = checked_total("models", model_total, mdl_model_count, limits.max_models)?;

        let body_base = mdl.non_negative(mdl_body + 8, "body part base")?;
        let selected_model = body_model_index(body, body_base, mdl_model_count);
        for model_index in selected_model..selected_model.saturating_add(1) {
            let mdl_model = mdl_models + model_index * MDL_MODEL_BYTES;
            let vtx_model = vtx_models + model_index * VTX_MODEL_BYTES;
            let model_name = fixed_string(*mdl, mdl_model, 64, "model name")?;
            let model_vertex_bytes = mdl.non_negative(mdl_model + 84, "model vertex offset")?;
            if !model_vertex_bytes.is_multiple_of(VVD_VERTEX_BYTES) {
                return Err(Source1StudioError::InvalidReference {
                    section: "model vertex byte offset",
                    index: model_vertex_bytes,
                    available: vertices.len() * VVD_VERTEX_BYTES,
                });
            }
            let model_vertex_start = model_vertex_bytes / VVD_VERTEX_BYTES;
            let mdl_mesh_count = mdl.non_negative(mdl_model + 72, "model mesh count")?;
            let mdl_meshes = mdl.relative(mdl_model, 76, "model mesh offset")?;
            mdl.array(
                mdl_meshes,
                mdl_mesh_count,
                MDL_MESH_BYTES,
                "meshes",
                limits.max_meshes,
            )?;
            let lod_count = vtx.non_negative(vtx_model, "LOD count")?;
            if lod_count == 0 {
                continue;
            }
            let lod_array = vtx.relative(vtx_model, 4, "LOD offset")?;
            vtx.array(lod_array, lod_count, VTX_LOD_BYTES, "LODs", 8)?;
            let lod0 = lod_array;
            let vtx_mesh_count = vtx.non_negative(lod0, "LOD mesh count")?;
            let vtx_meshes = vtx.relative(lod0, 4, "LOD mesh offset")?;
            vtx.array(
                vtx_meshes,
                vtx_mesh_count,
                VTX_MESH_BYTES,
                "meshes",
                limits.max_meshes,
            )?;
            same_count("meshes", mdl_mesh_count, vtx_mesh_count)?;
            checked_total("meshes", output.len(), mdl_mesh_count, limits.max_meshes)?;

            for mesh_index in 0..mdl_mesh_count {
                let mdl_mesh = mdl_meshes + mesh_index * MDL_MESH_BYTES;
                let vtx_mesh = vtx_meshes + mesh_index * VTX_MESH_BYTES;
                let material_slot = mdl.non_negative(mdl_mesh, "mesh material slot")?;
                let mesh_vertex_offset = mdl.non_negative(mdl_mesh + 12, "mesh vertex offset")?;
                let global_vertex_start = model_vertex_start
                    .checked_add(mesh_vertex_offset)
                    .ok_or(Source1StudioError::InvalidReference {
                        section: "mesh vertex start",
                        index: usize::MAX,
                        available: vertices.len(),
                    })?;
                let strip_group_count = vtx.non_negative(vtx_mesh, "strip group count")?;
                let strip_groups = vtx.relative(vtx_mesh, 4, "strip group offset")?;
                vtx.array(
                    strip_groups,
                    strip_group_count,
                    VTX_STRIP_GROUP_BYTES,
                    "strip groups",
                    limits.max_meshes,
                )?;
                let mut mesh = Source1StudioMesh {
                    body_part: body_index,
                    body_model: model_index,
                    model_name: model_name.clone(),
                    material_slot,
                    positions: Vec::new(),
                    normals: Vec::new(),
                    tex_coords_0: Vec::new(),
                    indices: Vec::new(),
                };
                for group_index in 0..strip_group_count {
                    decode_strip_group(
                        *vtx,
                        strip_groups + group_index * VTX_STRIP_GROUP_BYTES,
                        vertices,
                        global_vertex_start,
                        &mut mesh,
                        limits,
                    )?;
                }
                vertex_total = checked_total(
                    "decoded vertices",
                    vertex_total,
                    mesh.positions.len(),
                    limits.max_vertices,
                )?;
                index_total = checked_total(
                    "decoded indices",
                    index_total,
                    mesh.indices.len(),
                    limits.max_indices,
                )?;
                output.push(mesh);
            }
        }
    }
    let _ = model_total;
    Ok(output)
}

fn body_model_index(body: i32, base: usize, model_count: usize) -> usize {
    if body < 0 || base == 0 || model_count == 0 {
        return 0;
    }
    usize::try_from(body).unwrap_or(0) / base % model_count
}

#[allow(
    clippy::too_many_lines,
    reason = "one bounded strip-group decoder keeps vertex remap and both Source primitive modes together"
)]
fn decode_strip_group(
    vtx: Blob<'_>,
    group: usize,
    source_vertices: &[StudioVertex],
    mesh_vertex_start: usize,
    mesh: &mut Source1StudioMesh,
    limits: Source1StudioLimits,
) -> Result<(), Source1StudioError> {
    let vertex_count = vtx.non_negative(group, "strip-group vertex count")?;
    let vertex_offset = vtx.relative(group, 4, "strip-group vertex offset")?;
    vtx.array(
        vertex_offset,
        vertex_count,
        VTX_VERTEX_BYTES,
        "strip-group vertices",
        limits.max_vertices,
    )?;
    let base =
        u32::try_from(mesh.positions.len()).map_err(|_| Source1StudioError::RecordLimit {
            section: "mesh vertices",
            actual: mesh.positions.len(),
            limit: u32::MAX as usize,
        })?;
    for vertex_index in 0..vertex_count {
        let record = vertex_offset + vertex_index * VTX_VERTEX_BYTES;
        let original = usize::from(vtx.u16(record + 4, "original mesh vertex")?);
        let global = mesh_vertex_start.checked_add(original).ok_or(
            Source1StudioError::InvalidReference {
                section: "VVD vertex",
                index: usize::MAX,
                available: source_vertices.len(),
            },
        )?;
        let vertex = source_vertices
            .get(global)
            .ok_or(Source1StudioError::InvalidReference {
                section: "VVD vertex",
                index: global,
                available: source_vertices.len(),
            })?;
        mesh.positions.push(vertex.position);
        mesh.normals.push(vertex.normal);
        mesh.tex_coords_0.push(vertex.uv);
    }

    let index_count = vtx.non_negative(group + 8, "strip-group index count")?;
    let index_offset = vtx.relative(group, 12, "strip-group index offset")?;
    vtx.array(
        index_offset,
        index_count,
        2,
        "strip-group indices",
        limits.max_indices,
    )?;
    let strip_count = vtx.non_negative(group + 16, "strip count")?;
    let strip_offset = vtx.relative(group, 20, "strip offset")?;
    vtx.array(
        strip_offset,
        strip_count,
        VTX_STRIP_BYTES,
        "strips",
        limits.max_indices,
    )?;
    for strip_index in 0..strip_count {
        let strip = strip_offset + strip_index * VTX_STRIP_BYTES;
        let count = vtx.non_negative(strip, "strip index count")?;
        let first = vtx.non_negative(strip + 4, "strip index offset")?;
        let end = first
            .checked_add(count)
            .ok_or(Source1StudioError::InvalidReference {
                section: "strip index",
                index: usize::MAX,
                available: index_count,
            })?;
        if end > index_count {
            return Err(Source1StudioError::InvalidReference {
                section: "strip index",
                index: end,
                available: index_count,
            });
        }
        let flags = vtx.u8(strip + 18, "strip flags")?;
        let mut local = Vec::with_capacity(count);
        for index in first..end {
            let value = usize::from(vtx.u16(index_offset + index * 2, "mesh index")?);
            if value >= vertex_count {
                return Err(Source1StudioError::InvalidReference {
                    section: "strip-group vertex",
                    index: value,
                    available: vertex_count,
                });
            }
            local.push(base + u32::try_from(value).expect("u16 index"));
        }
        if flags & STRIP_IS_TRI_LIST != 0 {
            for triangle in local.chunks_exact(3) {
                push_triangle(&mut mesh.indices, triangle[0], triangle[1], triangle[2]);
            }
        } else if flags & STRIP_IS_TRI_STRIP != 0 {
            for index in 2..local.len() {
                let (a, b) = if index.is_multiple_of(2) {
                    (local[index - 2], local[index - 1])
                } else {
                    (local[index - 1], local[index - 2])
                };
                push_triangle(&mut mesh.indices, a, b, local[index]);
            }
        } else {
            return Err(Source1StudioError::UnsupportedStripFlags { flags });
        }
    }
    Ok(())
}

fn push_triangle(indices: &mut Vec<u32>, a: u32, b: u32, c: u32) {
    if a != b && b != c && a != c {
        indices.extend_from_slice(&[a, b, c]);
    }
}

fn fixed_string(
    blob: Blob<'_>,
    offset: usize,
    length: usize,
    section: &'static str,
) -> Result<String, Source1StudioError> {
    let bytes = blob.require(offset, length, section)?;
    let end = bytes
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(bytes.len());
    std::str::from_utf8(&bytes[..end])
        .map(str::to_owned)
        .map_err(|_| Source1StudioError::InvalidString { section, offset })
}

fn check_file_limit(
    file: &'static str,
    actual: usize,
    limit: usize,
) -> Result<(), Source1StudioError> {
    if actual > limit {
        Err(Source1StudioError::FileLimit {
            file,
            actual,
            limit,
        })
    } else {
        Ok(())
    }
}

fn bounded(section: &'static str, actual: usize, limit: usize) -> Result<(), Source1StudioError> {
    if actual > limit {
        Err(Source1StudioError::RecordLimit {
            section,
            actual,
            limit,
        })
    } else {
        Ok(())
    }
}

fn checked_total(
    section: &'static str,
    current: usize,
    added: usize,
    limit: usize,
) -> Result<usize, Source1StudioError> {
    let actual = current.saturating_add(added);
    bounded(section, actual, limit)?;
    Ok(actual)
}

fn same_count(section: &'static str, mdl: usize, vtx: usize) -> Result<(), Source1StudioError> {
    if mdl == vtx {
        Ok(())
    } else {
        Err(Source1StudioError::HierarchyMismatch { section, mdl, vtx })
    }
}

fn normalize_material_path(path: &str) -> String {
    let path = path.replace('\\', "/").to_ascii_lowercase();
    path.trim_start_matches("materials/")
        .trim_start_matches('/')
        .strip_suffix(".vmt")
        .unwrap_or(
            path.trim_start_matches("materials/")
                .trim_start_matches('/'),
        )
        .to_owned()
}

fn rotate_qangle(vector: [f32; 3], angles: [f32; 3]) -> [f32; 3] {
    let [pitch, yaw, roll] = angles.map(f32::to_radians);
    let (sp, cp) = pitch.sin_cos();
    let (sy, cy) = yaw.sin_cos();
    let (sr, cr) = roll.sin_cos();
    let forward = [cp * cy, cp * sy, -sp];
    let left = [sr * sp * cy - cr * sy, sr * sp * sy + cr * cy, sr * cp];
    let up = [cr * sp * cy + sr * sy, cr * sp * sy - sr * cy, cr * cp];
    [
        forward[0] * vector[0] + left[0] * vector[1] + up[0] * vector[2],
        forward[1] * vector[0] + left[1] * vector[1] + up[1] * vector[2],
        forward[2] * vector[0] + left[2] * vector[1] + up[2] * vector[2],
    ]
}

fn source_to_yuyib(vector: [f32; 3]) -> [f32; 3] {
    [vector[0], vector[2], -vector[1]]
}

fn scale3(vector: [f32; 3], scale: f32) -> [f32; 3] {
    [vector[0] * scale, vector[1] * scale, vector[2] * scale]
}

fn add3(left: [f32; 3], right: [f32; 3]) -> [f32; 3] {
    [left[0] + right[0], left[1] + right[1], left[2] + right[2]]
}

fn normalize3(vector: [f32; 3]) -> [f32; 3] {
    let length = vector[0]
        .mul_add(
            vector[0],
            vector[1].mul_add(vector[1], vector[2] * vector[2]),
        )
        .sqrt();
    if length > f32::EPSILON && length.is_finite() {
        scale3(vector, length.recip())
    } else {
        vector
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    #[test]
    fn body_group_integer_selects_each_body_part_independently() {
        assert_eq!(body_model_index(0, 1, 2), 0);
        assert_eq!(body_model_index(1, 1, 2), 1);
        assert_eq!(body_model_index(3, 2, 2), 1);
        assert_eq!(body_model_index(15, 8, 2), 1);
        assert_eq!(body_model_index(-1, 1, 2), 0);
        assert_eq!(body_model_index(99, 0, 2), 0);
    }

    #[test]
    fn decodes_minimal_matching_static_studio_model() {
        let checksum = 0x1234_5678;
        let mut mdl = vec![0_u8; 610];
        mdl[..4].copy_from_slice(b"IDST");
        put_i32(&mut mdl, 4, 48);
        put_i32(&mut mdl, 8, checksum);
        put_i32(&mut mdl, 76, 610);
        put_i32(&mut mdl, 204, 1);
        put_i32(&mut mdl, 208, 240);
        put_i32(&mut mdl, 212, 1);
        put_i32(&mut mdl, 216, 304);
        put_i32(&mut mdl, 220, 1);
        put_i32(&mut mdl, 224, 1);
        put_i32(&mut mdl, 228, 308);
        put_i32(&mut mdl, 232, 1);
        put_i32(&mut mdl, 236, 312);
        put_i32(&mut mdl, 240, 592 - 240);
        put_i32(&mut mdl, 304, 597);
        put_u16(&mut mdl, 308, 0);
        put_i32(&mut mdl, 312 + 4, 1);
        put_i32(&mut mdl, 312 + 12, 16);
        mdl[328..333].copy_from_slice(b"body\0");
        put_i32(&mut mdl, 328 + 72, 1);
        put_i32(&mut mdl, 328 + 76, 148);
        put_i32(&mut mdl, 328 + 80, 3);
        put_i32(&mut mdl, 328 + 84, 0);
        put_i32(&mut mdl, 476, 0);
        put_i32(&mut mdl, 476 + 8, 3);
        put_i32(&mut mdl, 476 + 12, 0);
        mdl[592..597].copy_from_slice(b"bark\0");
        mdl[597..609].copy_from_slice(b"models/tree\0");

        let mut vvd = vec![0_u8; VVD_HEADER_BYTES + 3 * VVD_VERTEX_BYTES];
        vvd[..4].copy_from_slice(b"IDSV");
        put_i32(&mut vvd, 4, 4);
        put_i32(&mut vvd, 8, checksum);
        put_i32(&mut vvd, 12, 1);
        put_i32(&mut vvd, 16, 3);
        put_i32(&mut vvd, 48, 0);
        put_i32(&mut vvd, 52, 64);
        put_i32(&mut vvd, 56, 64);
        for (index, position) in [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]]
            .into_iter()
            .enumerate()
        {
            let base = VVD_HEADER_BYTES + index * VVD_VERTEX_BYTES;
            put_vector(&mut vvd, base + 16, position);
            put_vector(&mut vvd, base + 28, [0.0, 0.0, 1.0]);
            put_f32(&mut vvd, base + 40, position[0]);
            put_f32(&mut vvd, base + 44, position[1]);
        }

        let mut vtx = vec![0_u8; 158];
        put_i32(&mut vtx, 0, 7);
        put_i32(&mut vtx, 16, checksum);
        put_i32(&mut vtx, 20, 1);
        put_i32(&mut vtx, 28, 1);
        put_i32(&mut vtx, 32, 36);
        put_i32(&mut vtx, 36, 1);
        put_i32(&mut vtx, 40, 8);
        put_i32(&mut vtx, 44, 1);
        put_i32(&mut vtx, 48, 8);
        put_i32(&mut vtx, 52, 1);
        put_i32(&mut vtx, 56, 12);
        put_i32(&mut vtx, 64, 1);
        put_i32(&mut vtx, 68, 9);
        let group = 73;
        put_i32(&mut vtx, group, 3);
        put_i32(&mut vtx, group + 4, 25);
        put_i32(&mut vtx, group + 8, 3);
        put_i32(&mut vtx, group + 12, 52);
        put_i32(&mut vtx, group + 16, 1);
        put_i32(&mut vtx, group + 20, 58);
        for index in 0..3 {
            put_u16(&mut vtx, 98 + index * VTX_VERTEX_BYTES + 4, index as u16);
            put_u16(&mut vtx, 125 + index * 2, index as u16);
        }
        put_i32(&mut vtx, 131, 3);
        put_i32(&mut vtx, 135, 0);
        put_i32(&mut vtx, 139, 3);
        put_i32(&mut vtx, 143, 0);
        put_u16(&mut vtx, 147, 0);
        vtx[149] = STRIP_IS_TRI_LIST;
        put_i32(&mut vtx, 150, 0);
        put_i32(&mut vtx, 154, 0);

        let files = Source1StudioModelFiles {
            model_path: "models/tree.mdl".to_owned(),
            mdl: Arc::from(mdl),
            vvd: Arc::from(vvd),
            vtx: Arc::from(vtx),
            vtx_path: "models/tree.dx90.vtx".to_owned(),
        };
        let model = decode_studio_model(&files, Source1StudioLimits::default())
            .expect("matching static model");
        assert_eq!(model.materials[0].candidates, ["models/tree/bark"]);
        assert_eq!(model.meshes.len(), 1);
        assert_eq!(model.meshes[0].positions.len(), 3);
        assert_eq!(model.meshes[0].indices, [0, 1, 2]);
        assert_eq!(
            model.material_for_skin(&model.meshes[0], 0),
            Some(&model.materials[0])
        );
    }

    #[test]
    fn source_prop_transform_converts_identity_and_yaw() {
        let identity = Source1StaticPropTransform {
            origin: [10.0, 20.0, 30.0],
            angles: [0.0, 0.0, 0.0],
            uniform_scale: 2.0,
        };
        assert_eq!(
            identity.transform_position([1.0, 2.0, 3.0]),
            [12.0, 36.0, -24.0]
        );
        let yaw = Source1StaticPropTransform {
            origin: [0.0; 3],
            angles: [0.0, 90.0, 0.0],
            uniform_scale: 1.0,
        };
        let transformed = yaw.transform_position([1.0, 0.0, 0.0]);
        assert!(transformed[0].abs() < 1.0e-6);
        assert!(transformed[1].abs() < 1.0e-6);
        assert!((transformed[2] + 1.0).abs() < 1.0e-6);
    }

    #[test]
    fn triangle_strips_flip_winding_and_drop_degenerates() {
        let mut indices = Vec::new();
        let strip = [0, 1, 2, 3, 3, 4];
        for index in 2..strip.len() {
            let (a, b) = if index.is_multiple_of(2) {
                (strip[index - 2], strip[index - 1])
            } else {
                (strip[index - 1], strip[index - 2])
            };
            push_triangle(&mut indices, a, b, strip[index]);
        }
        assert_eq!(indices, [0, 1, 2, 2, 1, 3]);
    }

    #[test]
    fn material_paths_are_source_canonical() {
        assert_eq!(
            normalize_material_path(r"Materials\Models\Tree\Bark.VMT"),
            "models/tree/bark"
        );
        assert_eq!(
            normalize_material_path("models/tree/leaf"),
            "models/tree/leaf"
        );
    }

    fn put_i32(bytes: &mut [u8], offset: usize, value: i32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn put_u16(bytes: &mut [u8], offset: usize, value: u16) {
        bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn put_f32(bytes: &mut [u8], offset: usize, value: f32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn put_vector(bytes: &mut [u8], offset: usize, value: [f32; 3]) {
        for (index, component) in value.into_iter().enumerate() {
            put_f32(bytes, offset + index * 4, component);
        }
    }
}
