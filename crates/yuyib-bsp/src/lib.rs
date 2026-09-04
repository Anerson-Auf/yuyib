//! Bounded low-level reader for Valve Source 1 BSP files.
//!
//! The crate validates the `VBSP` header and every declared lump before
//! exposing typed geometry records. It deliberately does not create renderer,
//! physics or asset resources. Embedded `PAKFILE` entries are read in memory
//! and are never extracted to the filesystem.

#![forbid(unsafe_code)]
#![allow(
    missing_docs,
    reason = "public low-level records and errors mirror the documented Source BSP binary fields"
)]
#![allow(
    clippy::missing_errors_doc,
    reason = "all typed lump accessors share the validation failures documented by BspError"
)]

use std::{
    error::Error,
    fmt,
    io::{Cursor, Read},
    path::Path,
    sync::Arc,
};

use zip::ZipArchive;

mod static_props;

pub use static_props::{
    BspGameLumpEntry, BspStaticProp, BspStaticPropError, BspStaticPropLump, GAME_LUMP_STATIC_PROPS,
    parse_static_prop_game_lump,
};

const BSP_MAGIC: &[u8; 4] = b"VBSP";
const HEADER_BYTES: usize = 8 + 64 * 16 + 4;
const LUMP_COUNT: usize = 64;

pub const LUMP_ENTITIES: usize = 0;
pub const LUMP_PLANES: usize = 1;
pub const LUMP_TEXDATA: usize = 2;
pub const LUMP_VERTEXES: usize = 3;
pub const LUMP_TEXINFO: usize = 6;
pub const LUMP_FACES: usize = 7;
pub const LUMP_EDGES: usize = 12;
pub const LUMP_SURFEDGES: usize = 13;
pub const LUMP_MODELS: usize = 14;
pub const LUMP_BRUSHES: usize = 18;
pub const LUMP_BRUSHSIDES: usize = 19;
pub const LUMP_DISPINFO: usize = 26;
pub const LUMP_DISP_VERTS: usize = 33;
pub const LUMP_GAME_LUMP: usize = 35;
pub const LUMP_LEAFWATERDATA: usize = 36;
pub const LUMP_PAKFILE: usize = 40;
pub const LUMP_TEXDATA_STRING_DATA: usize = 43;
pub const LUMP_TEXDATA_STRING_TABLE: usize = 44;

/// Work and allocation limits for untrusted BSP input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BspLimits {
    pub max_input_bytes: usize,
    pub max_lump_bytes: usize,
    pub max_records_per_lump: usize,
    pub max_entities: usize,
    pub max_entity_properties: usize,
    pub max_pak_entries: usize,
    pub max_pak_entry_bytes: usize,
    pub max_pak_total_bytes: usize,
}

impl Default for BspLimits {
    fn default() -> Self {
        Self {
            max_input_bytes: 512 * 1024 * 1024,
            max_lump_bytes: 384 * 1024 * 1024,
            max_records_per_lump: 16_000_000,
            max_entities: 1_000_000,
            max_entity_properties: 8_000_000,
            max_pak_entries: 200_000,
            max_pak_entry_bytes: 256 * 1024 * 1024,
            max_pak_total_bytes: 512 * 1024 * 1024,
        }
    }
}

/// Validated descriptor for one of the 64 Source BSP lumps.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct BspLump {
    pub offset: usize,
    pub length: usize,
    pub version: i32,
    pub four_cc: i32,
}

/// Owned, validated BSP bytes with typed low-level accessors.
#[derive(Clone)]
pub struct Bsp {
    bytes: Arc<[u8]>,
    version: i32,
    map_revision: i32,
    lumps: [BspLump; LUMP_COUNT],
    limits: BspLimits,
}

impl Bsp {
    /// Reads and validates a BSP file.
    ///
    /// # Errors
    ///
    /// Returns I/O or [`BspError`] validation failures.
    pub fn read(path: impl AsRef<Path>, limits: BspLimits) -> Result<Self, BspReadError> {
        let bytes = std::fs::read(path).map_err(BspReadError::Io)?;
        Self::parse(bytes, limits).map_err(BspReadError::Bsp)
    }

    /// Validates owned Source BSP bytes without retaining a caller lifetime.
    ///
    /// # Errors
    ///
    /// Rejects unsupported versions, invalid lump ranges and configured limits.
    pub fn parse(bytes: Vec<u8>, limits: BspLimits) -> Result<Self, BspError> {
        if bytes.len() > limits.max_input_bytes {
            return Err(BspError::InputLimit {
                actual: bytes.len(),
                limit: limits.max_input_bytes,
            });
        }
        if bytes.len() < HEADER_BYTES {
            return Err(BspError::TruncatedHeader {
                actual: bytes.len(),
            });
        }
        if &bytes[..4] != BSP_MAGIC {
            return Err(BspError::InvalidMagic([
                bytes[0], bytes[1], bytes[2], bytes[3],
            ]));
        }
        let version = i32_at(&bytes, 4);
        if !matches!(version, 20 | 21) {
            return Err(BspError::UnsupportedVersion(version));
        }
        let mut lumps = [BspLump::default(); LUMP_COUNT];
        for (index, lump) in lumps.iter_mut().enumerate() {
            let cursor = 8 + index * 16;
            let raw_offset = i32_at(&bytes, cursor);
            let raw_length = i32_at(&bytes, cursor + 4);
            if raw_offset < 0 || raw_length < 0 {
                return Err(BspError::InvalidLumpRange {
                    lump: index,
                    offset: raw_offset,
                    length: raw_length,
                });
            }
            let offset = usize::try_from(raw_offset).map_err(|_| BspError::InvalidLumpRange {
                lump: index,
                offset: raw_offset,
                length: raw_length,
            })?;
            let length = usize::try_from(raw_length).map_err(|_| BspError::InvalidLumpRange {
                lump: index,
                offset: raw_offset,
                length: raw_length,
            })?;
            if length > limits.max_lump_bytes {
                return Err(BspError::LumpLimit {
                    lump: index,
                    actual: length,
                    limit: limits.max_lump_bytes,
                });
            }
            let end = offset
                .checked_add(length)
                .ok_or(BspError::LumpOutOfBounds { lump: index })?;
            if length != 0 && (offset < HEADER_BYTES || end > bytes.len()) {
                return Err(BspError::LumpOutOfBounds { lump: index });
            }
            *lump = BspLump {
                offset,
                length,
                version: i32_at(&bytes, cursor + 8),
                four_cc: i32_at(&bytes, cursor + 12),
            };
        }
        Ok(Self {
            map_revision: i32_at(&bytes, 8 + LUMP_COUNT * 16),
            bytes: bytes.into(),
            version,
            lumps,
            limits,
        })
    }

    #[must_use]
    pub const fn version(&self) -> i32 {
        self.version
    }

    #[must_use]
    pub const fn map_revision(&self) -> i32 {
        self.map_revision
    }

    #[must_use]
    pub const fn lumps(&self) -> &[BspLump; LUMP_COUNT] {
        &self.lumps
    }

    /// Returns validated raw bytes for an uncompressed lump.
    ///
    /// # Errors
    ///
    /// Rejects an invalid index or a Valve-LZMA-compressed lump. Compression is
    /// reported explicitly rather than interpreting compressed bytes as records.
    pub fn lump_bytes(&self, index: usize) -> Result<&[u8], BspError> {
        let lump = *self
            .lumps
            .get(index)
            .ok_or(BspError::InvalidLumpIndex(index))?;
        if lump.four_cc != 0 {
            return Err(BspError::CompressedLump {
                lump: index,
                uncompressed_bytes: lump.four_cc,
            });
        }
        Ok(&self.bytes[lump.offset..lump.offset + lump.length])
    }

    pub fn vertices(&self) -> Result<Vec<BspVertex>, BspError> {
        let bytes = self.records(LUMP_VERTEXES, 12)?;
        Ok(bytes
            .chunks_exact(12)
            .map(|record| BspVertex {
                position: [f32_at(record, 0), f32_at(record, 4), f32_at(record, 8)],
            })
            .collect())
    }

    pub fn edges(&self) -> Result<Vec<BspEdge>, BspError> {
        let bytes = self.records(LUMP_EDGES, 4)?;
        Ok(bytes
            .chunks_exact(4)
            .map(|record| BspEdge {
                vertices: [u16_at(record, 0), u16_at(record, 2)],
            })
            .collect())
    }

    pub fn surf_edges(&self) -> Result<Vec<i32>, BspError> {
        Ok(self
            .records(LUMP_SURFEDGES, 4)?
            .chunks_exact(4)
            .map(|record| i32_at(record, 0))
            .collect())
    }

    pub fn planes(&self) -> Result<Vec<BspPlane>, BspError> {
        let bytes = self.records(LUMP_PLANES, 20)?;
        Ok(bytes
            .chunks_exact(20)
            .map(|record| BspPlane {
                normal: [f32_at(record, 0), f32_at(record, 4), f32_at(record, 8)],
                distance: f32_at(record, 12),
                kind: i32_at(record, 16),
            })
            .collect())
    }

    pub fn faces(&self) -> Result<Vec<BspFace>, BspError> {
        let bytes = self.records(LUMP_FACES, 56)?;
        Ok(bytes
            .chunks_exact(56)
            .map(|record| BspFace {
                plane: u16_at(record, 0),
                side: record[2] != 0,
                on_node: record[3] != 0,
                first_edge: i32_at(record, 4),
                edge_count: i16_at(record, 8),
                tex_info: i16_at(record, 10),
                displacement: i16_at(record, 12),
                light_offset: i32_at(record, 20),
                area: f32_at(record, 24),
            })
            .collect())
    }

    /// Reads BSP render models and validates every model's face range.
    ///
    /// Model zero is the world. Brush entities refer to later records through
    /// their `model` key (`*1`, `*2`, ...).
    pub fn models(&self) -> Result<Vec<BspModel>, BspError> {
        let face_count = self.records(LUMP_FACES, 56)?.len() / 56;
        let bytes = self.records(LUMP_MODELS, 48)?;
        let mut models = Vec::with_capacity(bytes.len() / 48);
        for (index, record) in bytes.chunks_exact(48).enumerate() {
            let first_face = i32_at(record, 40);
            let model_face_count = i32_at(record, 44);
            let valid_range = usize::try_from(first_face)
                .ok()
                .zip(usize::try_from(model_face_count).ok())
                .and_then(|(first, count)| first.checked_add(count))
                .is_some_and(|end| end <= face_count);
            if !valid_range {
                return Err(BspError::InvalidModelFaceRange {
                    model: index,
                    first_face,
                    face_count: model_face_count,
                    available_faces: face_count,
                });
            }
            models.push(BspModel {
                mins: [f32_at(record, 0), f32_at(record, 4), f32_at(record, 8)],
                maxs: [f32_at(record, 12), f32_at(record, 16), f32_at(record, 20)],
                origin: [f32_at(record, 24), f32_at(record, 28), f32_at(record, 32)],
                head_node: i32_at(record, 36),
                first_face,
                face_count: model_face_count,
            });
        }
        Ok(models)
    }

    pub fn tex_info(&self) -> Result<Vec<BspTexInfo>, BspError> {
        let bytes = self.records(LUMP_TEXINFO, 72)?;
        Ok(bytes
            .chunks_exact(72)
            .map(|record| BspTexInfo {
                texture_vectors: [
                    [
                        f32_at(record, 0),
                        f32_at(record, 4),
                        f32_at(record, 8),
                        f32_at(record, 12),
                    ],
                    [
                        f32_at(record, 16),
                        f32_at(record, 20),
                        f32_at(record, 24),
                        f32_at(record, 28),
                    ],
                ],
                lightmap_vectors: [
                    [
                        f32_at(record, 32),
                        f32_at(record, 36),
                        f32_at(record, 40),
                        f32_at(record, 44),
                    ],
                    [
                        f32_at(record, 48),
                        f32_at(record, 52),
                        f32_at(record, 56),
                        f32_at(record, 60),
                    ],
                ],
                flags: i32_at(record, 64),
                tex_data: i32_at(record, 68),
            })
            .collect())
    }

    pub fn tex_data(&self) -> Result<Vec<BspTexData>, BspError> {
        let bytes = self.records(LUMP_TEXDATA, 32)?;
        Ok(bytes
            .chunks_exact(32)
            .map(|record| BspTexData {
                reflectivity: [f32_at(record, 0), f32_at(record, 4), f32_at(record, 8)],
                name_string_table_id: i32_at(record, 12),
                width: i32_at(record, 16),
                height: i32_at(record, 20),
                view_width: i32_at(record, 24),
                view_height: i32_at(record, 28),
            })
            .collect())
    }

    /// Resolves all texture-data names through the BSP string table.
    pub fn texture_names(&self) -> Result<Vec<String>, BspError> {
        let data = self.lump_bytes(LUMP_TEXDATA_STRING_DATA)?;
        let table = self.records(LUMP_TEXDATA_STRING_TABLE, 4)?;
        let mut names = Vec::with_capacity(table.len() / 4);
        for (index, entry) in table.chunks_exact(4).enumerate() {
            let offset = i32_at(entry, 0);
            if offset < 0 {
                return Err(BspError::InvalidTextureStringOffset { index, offset });
            }
            let offset = usize::try_from(offset)
                .map_err(|_| BspError::InvalidTextureStringOffset { index, offset })?;
            let tail = data
                .get(offset..)
                .ok_or(BspError::InvalidTextureStringOffset {
                    index,
                    offset: i32::try_from(offset).unwrap_or(i32::MAX),
                })?;
            let end = tail
                .iter()
                .position(|byte| *byte == 0)
                .ok_or(BspError::UnterminatedTextureString { index })?;
            let name = std::str::from_utf8(&tail[..end])
                .map_err(|_| BspError::InvalidTextureStringUtf8 { index })?;
            names.push(name.to_owned());
        }
        Ok(names)
    }

    pub fn brushes(&self) -> Result<Vec<BspBrush>, BspError> {
        let bytes = self.records(LUMP_BRUSHES, 12)?;
        Ok(bytes
            .chunks_exact(12)
            .map(|record| BspBrush {
                first_side: i32_at(record, 0),
                side_count: i32_at(record, 4),
                contents: i32_at(record, 8),
            })
            .collect())
    }

    pub fn brush_sides(&self) -> Result<Vec<BspBrushSide>, BspError> {
        let bytes = self.records(LUMP_BRUSHSIDES, 8)?;
        Ok(bytes
            .chunks_exact(8)
            .map(|record| BspBrushSide {
                plane: u16_at(record, 0),
                tex_info: i16_at(record, 2),
                displacement: i16_at(record, 4),
                bevel: i16_at(record, 6) != 0,
            })
            .collect())
    }

    pub fn displacement_info(&self) -> Result<Vec<BspDisplacementInfo>, BspError> {
        let bytes = self.records(LUMP_DISPINFO, 176)?;
        Ok(bytes
            .chunks_exact(176)
            .map(|record| BspDisplacementInfo {
                start_position: [f32_at(record, 0), f32_at(record, 4), f32_at(record, 8)],
                displacement_vertex_start: i32_at(record, 12),
                power: i32_at(record, 20),
                map_face: u16_at(record, 36),
            })
            .collect())
    }

    pub fn displacement_vertices(&self) -> Result<Vec<BspDisplacementVertex>, BspError> {
        let bytes = self.records(LUMP_DISP_VERTS, 20)?;
        Ok(bytes
            .chunks_exact(20)
            .map(|record| BspDisplacementVertex {
                vector: [f32_at(record, 0), f32_at(record, 4), f32_at(record, 8)],
                distance: f32_at(record, 12),
                alpha: f32_at(record, 16),
            })
            .collect())
    }

    /// Reads per-water-volume surface and minimum heights.
    ///
    /// Each record references the texture info used by its water surface. The
    /// reference is validated against [`LUMP_TEXINFO`] before any record is
    /// returned.
    pub fn leaf_water_data(&self) -> Result<Vec<BspLeafWaterData>, BspError> {
        let tex_info_count = self.records(LUMP_TEXINFO, 72)?.len() / 72;
        let bytes = self.records(LUMP_LEAFWATERDATA, 12)?;
        let mut records = Vec::with_capacity(bytes.len() / 12);
        for (record_index, record) in bytes.chunks_exact(12).enumerate() {
            let surface_tex_info = i16_at(record, 8);
            let valid_tex_info = usize::try_from(surface_tex_info)
                .ok()
                .is_some_and(|index| index < tex_info_count);
            if !valid_tex_info {
                return Err(BspError::InvalidLeafWaterTexInfo {
                    record: record_index,
                    tex_info: surface_tex_info,
                    available_tex_info: tex_info_count,
                });
            }
            records.push(BspLeafWaterData {
                surface_z: f32_at(record, 0),
                min_z: f32_at(record, 4),
                surface_tex_info,
            });
        }
        Ok(records)
    }

    /// Parses the entity lump's quoted key/value dictionaries.
    pub fn entities(&self) -> Result<Vec<BspEntity>, BspError> {
        let bytes = self.lump_bytes(LUMP_ENTITIES)?;
        let bytes = bytes.strip_suffix(&[0]).unwrap_or(bytes);
        let text = std::str::from_utf8(bytes).map_err(|_| BspError::InvalidEntityUtf8)?;
        EntityParser::new(text, self.limits).parse()
    }

    /// Lists embedded PAK entries without extracting them.
    pub fn pak_entries(&self) -> Result<Vec<BspPakEntry>, BspError> {
        if self.lump_bytes(LUMP_PAKFILE)?.is_empty() {
            return Ok(Vec::new());
        }
        let mut archive = self.pak_archive()?;
        if archive.len() > self.limits.max_pak_entries {
            return Err(BspError::PakEntryLimit {
                actual: archive.len(),
                limit: self.limits.max_pak_entries,
            });
        }
        let mut entries = Vec::with_capacity(archive.len());
        for index in 0..archive.len() {
            let entry = archive.by_index(index).map_err(BspError::Pak)?;
            entries.push(BspPakEntry {
                path: normalize_archive_path(entry.name()),
                uncompressed_bytes: entry.size(),
            });
        }
        Ok(entries)
    }

    /// Reads one embedded PAK entry case-insensitively and without filesystem extraction.
    pub fn pak_file(&self, path: &str) -> Result<Option<Vec<u8>>, BspError> {
        if self.lump_bytes(LUMP_PAKFILE)?.is_empty() {
            return Ok(None);
        }
        let wanted = normalize_archive_path(path);
        let mut archive = self.pak_archive()?;
        if archive.len() > self.limits.max_pak_entries {
            return Err(BspError::PakEntryLimit {
                actual: archive.len(),
                limit: self.limits.max_pak_entries,
            });
        }
        for index in 0..archive.len() {
            let mut entry = archive.by_index(index).map_err(BspError::Pak)?;
            if normalize_archive_path(entry.name()) != wanted {
                continue;
            }
            let size = usize::try_from(entry.size()).map_err(|_| BspError::PakFileLimit {
                path: wanted.clone(),
                actual: usize::MAX,
                limit: self.limits.max_pak_entry_bytes,
            })?;
            if size > self.limits.max_pak_entry_bytes {
                return Err(BspError::PakFileLimit {
                    path: wanted,
                    actual: size,
                    limit: self.limits.max_pak_entry_bytes,
                });
            }
            let mut bytes = Vec::with_capacity(size);
            entry.read_to_end(&mut bytes).map_err(BspError::PakIo)?;
            return Ok(Some(bytes));
        }
        Ok(None)
    }

    /// Reads all embedded files whose final extension matches one of `extensions`.
    ///
    /// The archive is opened once, making this the preferred API for batch
    /// material import. Extensions are compared case-insensitively without a
    /// leading dot.
    pub fn pak_files_by_extension(&self, extensions: &[&str]) -> Result<Vec<BspPakFile>, BspError> {
        if self.lump_bytes(LUMP_PAKFILE)?.is_empty() {
            return Ok(Vec::new());
        }
        let mut archive = self.pak_archive()?;
        if archive.len() > self.limits.max_pak_entries {
            return Err(BspError::PakEntryLimit {
                actual: archive.len(),
                limit: self.limits.max_pak_entries,
            });
        }
        let mut files = Vec::new();
        let mut total_bytes = 0_usize;
        for index in 0..archive.len() {
            let mut entry = archive.by_index(index).map_err(BspError::Pak)?;
            let path = normalize_archive_path(entry.name());
            let extension = path.rsplit_once('.').map(|(_, extension)| extension);
            if !extension.is_some_and(|candidate| {
                extensions
                    .iter()
                    .any(|wanted| candidate.eq_ignore_ascii_case(wanted.trim_start_matches('.')))
            }) {
                continue;
            }
            let size = usize::try_from(entry.size()).map_err(|_| BspError::PakFileLimit {
                path: path.clone(),
                actual: usize::MAX,
                limit: self.limits.max_pak_entry_bytes,
            })?;
            if size > self.limits.max_pak_entry_bytes {
                return Err(BspError::PakFileLimit {
                    path,
                    actual: size,
                    limit: self.limits.max_pak_entry_bytes,
                });
            }
            total_bytes = total_bytes
                .checked_add(size)
                .ok_or(BspError::PakTotalLimit {
                    actual: usize::MAX,
                    limit: self.limits.max_pak_total_bytes,
                })?;
            if total_bytes > self.limits.max_pak_total_bytes {
                return Err(BspError::PakTotalLimit {
                    actual: total_bytes,
                    limit: self.limits.max_pak_total_bytes,
                });
            }
            let mut bytes = Vec::with_capacity(size);
            entry.read_to_end(&mut bytes).map_err(BspError::PakIo)?;
            files.push(BspPakFile { path, bytes });
        }
        Ok(files)
    }

    fn pak_archive(&self) -> Result<ZipArchive<Cursor<&[u8]>>, BspError> {
        let bytes = self.lump_bytes(LUMP_PAKFILE)?;
        ZipArchive::new(Cursor::new(bytes)).map_err(BspError::Pak)
    }

    fn records(&self, lump: usize, stride: usize) -> Result<&[u8], BspError> {
        let bytes = self.lump_bytes(lump)?;
        if !bytes.len().is_multiple_of(stride) {
            return Err(BspError::InvalidRecordLump {
                lump,
                length: bytes.len(),
                stride,
            });
        }
        let records = bytes.len() / stride;
        if records > self.limits.max_records_per_lump {
            return Err(BspError::RecordLimit {
                lump,
                actual: records,
                limit: self.limits.max_records_per_lump,
            });
        }
        Ok(bytes)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BspVertex {
    pub position: [f32; 3],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BspEdge {
    pub vertices: [u16; 2],
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BspPlane {
    pub normal: [f32; 3],
    pub distance: f32,
    pub kind: i32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BspFace {
    pub plane: u16,
    pub side: bool,
    pub on_node: bool,
    pub first_edge: i32,
    pub edge_count: i16,
    pub tex_info: i16,
    pub displacement: i16,
    pub light_offset: i32,
    pub area: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BspModel {
    pub mins: [f32; 3],
    pub maxs: [f32; 3],
    pub origin: [f32; 3],
    pub head_node: i32,
    pub first_face: i32,
    pub face_count: i32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BspTexInfo {
    pub texture_vectors: [[f32; 4]; 2],
    pub lightmap_vectors: [[f32; 4]; 2],
    pub flags: i32,
    pub tex_data: i32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BspTexData {
    pub reflectivity: [f32; 3],
    pub name_string_table_id: i32,
    pub width: i32,
    pub height: i32,
    pub view_width: i32,
    pub view_height: i32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BspBrush {
    pub first_side: i32,
    pub side_count: i32,
    pub contents: i32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BspBrushSide {
    pub plane: u16,
    pub tex_info: i16,
    pub displacement: i16,
    pub bevel: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BspDisplacementInfo {
    pub start_position: [f32; 3],
    pub displacement_vertex_start: i32,
    pub power: i32,
    pub map_face: u16,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BspDisplacementVertex {
    pub vector: [f32; 3],
    pub distance: f32,
    pub alpha: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BspLeafWaterData {
    pub surface_z: f32,
    pub min_z: f32,
    pub surface_tex_info: i16,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BspEntity {
    properties: Vec<(String, String)>,
}

impl BspEntity {
    #[must_use]
    pub fn properties(&self) -> &[(String, String)] {
        &self.properties
    }

    #[must_use]
    pub fn property(&self, key: &str) -> Option<&str> {
        self.properties
            .iter()
            .find(|(candidate, _)| candidate.eq_ignore_ascii_case(key))
            .map(|(_, value)| value.as_str())
    }

    #[must_use]
    pub fn classname(&self) -> Option<&str> {
        self.property("classname")
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BspPakEntry {
    pub path: String,
    pub uncompressed_bytes: u64,
}

/// One safely decoded in-memory entry from the BSP PAKFILE lump.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BspPakFile {
    pub path: String,
    pub bytes: Vec<u8>,
}

struct EntityParser<'a> {
    input: &'a str,
    offset: usize,
    limits: BspLimits,
    property_count: usize,
}

impl<'a> EntityParser<'a> {
    fn new(input: &'a str, limits: BspLimits) -> Self {
        Self {
            input,
            offset: 0,
            limits,
            property_count: 0,
        }
    }

    fn parse(mut self) -> Result<Vec<BspEntity>, BspError> {
        let mut entities = Vec::new();
        loop {
            self.skip_space_and_comments();
            if self.peek().is_none() {
                break;
            }
            self.expect('{')?;
            let mut properties = Vec::new();
            loop {
                self.skip_space_and_comments();
                if self.peek() == Some('}') {
                    self.take();
                    break;
                }
                let key = self.quoted()?;
                self.skip_space_and_comments();
                let value = self.quoted()?;
                self.property_count += 1;
                if self.property_count > self.limits.max_entity_properties {
                    return Err(BspError::EntityPropertyLimit {
                        limit: self.limits.max_entity_properties,
                    });
                }
                properties.push((key, value));
            }
            entities.push(BspEntity { properties });
            if entities.len() > self.limits.max_entities {
                return Err(BspError::EntityLimit {
                    limit: self.limits.max_entities,
                });
            }
        }
        Ok(entities)
    }

    fn quoted(&mut self) -> Result<String, BspError> {
        self.expect('"')?;
        let mut value = String::new();
        loop {
            match self.take() {
                Some('"') => return Ok(value),
                Some('\\') => match self.take() {
                    Some('"') => value.push('"'),
                    Some('\\') => value.push('\\'),
                    Some(character) => {
                        value.push('\\');
                        value.push(character);
                    }
                    None => {
                        return Err(BspError::MalformedEntities {
                            offset: self.offset,
                        });
                    }
                },
                Some(character) => value.push(character),
                None => {
                    return Err(BspError::MalformedEntities {
                        offset: self.offset,
                    });
                }
            }
        }
    }

    fn expect(&mut self, expected: char) -> Result<(), BspError> {
        if self.take() == Some(expected) {
            Ok(())
        } else {
            Err(BspError::MalformedEntities {
                offset: self.offset,
            })
        }
    }

    fn skip_space_and_comments(&mut self) {
        loop {
            while self.peek().is_some_and(char::is_whitespace) {
                self.take();
            }
            if self.input[self.offset..].starts_with("//") {
                while self.take().is_some_and(|character| character != '\n') {}
            } else {
                break;
            }
        }
    }

    fn peek(&self) -> Option<char> {
        self.input[self.offset..].chars().next()
    }

    fn take(&mut self) -> Option<char> {
        let character = self.peek()?;
        self.offset += character.len_utf8();
        Some(character)
    }
}

#[derive(Debug)]
pub enum BspReadError {
    Io(std::io::Error),
    Bsp(BspError),
}

impl fmt::Display for BspReadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(source) => write!(formatter, "cannot read BSP: {source}"),
            Self::Bsp(source) => source.fmt(formatter),
        }
    }
}

impl Error for BspReadError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(source) => Some(source),
            Self::Bsp(source) => Some(source),
        }
    }
}

#[derive(Debug)]
pub enum BspError {
    InputLimit {
        actual: usize,
        limit: usize,
    },
    TruncatedHeader {
        actual: usize,
    },
    InvalidMagic([u8; 4]),
    UnsupportedVersion(i32),
    InvalidLumpRange {
        lump: usize,
        offset: i32,
        length: i32,
    },
    LumpOutOfBounds {
        lump: usize,
    },
    LumpLimit {
        lump: usize,
        actual: usize,
        limit: usize,
    },
    InvalidLumpIndex(usize),
    CompressedLump {
        lump: usize,
        uncompressed_bytes: i32,
    },
    InvalidRecordLump {
        lump: usize,
        length: usize,
        stride: usize,
    },
    InvalidModelFaceRange {
        model: usize,
        first_face: i32,
        face_count: i32,
        available_faces: usize,
    },
    InvalidLeafWaterTexInfo {
        record: usize,
        tex_info: i16,
        available_tex_info: usize,
    },
    RecordLimit {
        lump: usize,
        actual: usize,
        limit: usize,
    },
    InvalidTextureStringOffset {
        index: usize,
        offset: i32,
    },
    UnterminatedTextureString {
        index: usize,
    },
    InvalidTextureStringUtf8 {
        index: usize,
    },
    InvalidEntityUtf8,
    MalformedEntities {
        offset: usize,
    },
    EntityLimit {
        limit: usize,
    },
    EntityPropertyLimit {
        limit: usize,
    },
    Pak(zip::result::ZipError),
    PakIo(std::io::Error),
    PakEntryLimit {
        actual: usize,
        limit: usize,
    },
    PakFileLimit {
        path: String,
        actual: usize,
        limit: usize,
    },
    PakTotalLimit {
        actual: usize,
        limit: usize,
    },
}

impl fmt::Display for BspError {
    #[allow(
        clippy::too_many_lines,
        reason = "one exhaustive formatter keeps the public binary-validation error vocabulary explicit"
    )]
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InputLimit { actual, limit } => {
                write!(formatter, "BSP is {actual} bytes; limit is {limit}")
            }
            Self::TruncatedHeader { actual } => {
                write!(formatter, "BSP header is truncated at {actual} bytes")
            }
            Self::InvalidMagic(magic) => write!(formatter, "invalid BSP magic {magic:?}"),
            Self::UnsupportedVersion(version) => {
                write!(formatter, "unsupported Source BSP version {version}")
            }
            Self::InvalidLumpRange {
                lump,
                offset,
                length,
            } => write!(
                formatter,
                "BSP lump {lump} has invalid range {offset}+{length}"
            ),
            Self::LumpOutOfBounds { lump } => {
                write!(formatter, "BSP lump {lump} is outside the file")
            }
            Self::LumpLimit {
                lump,
                actual,
                limit,
            } => write!(
                formatter,
                "BSP lump {lump} is {actual} bytes; limit is {limit}"
            ),
            Self::InvalidLumpIndex(index) => {
                write!(formatter, "BSP lump index {index} is outside 0..64")
            }
            Self::CompressedLump {
                lump,
                uncompressed_bytes,
            } => write!(
                formatter,
                "BSP lump {lump} uses unsupported Valve LZMA compression ({uncompressed_bytes} bytes)"
            ),
            Self::InvalidRecordLump {
                lump,
                length,
                stride,
            } => write!(
                formatter,
                "BSP lump {lump} length {length} is not divisible by record size {stride}"
            ),
            Self::InvalidModelFaceRange {
                model,
                first_face,
                face_count,
                available_faces,
            } => write!(
                formatter,
                "BSP model {model} face range {first_face}+{face_count} is outside {available_faces} faces"
            ),
            Self::InvalidLeafWaterTexInfo {
                record,
                tex_info,
                available_tex_info,
            } => write!(
                formatter,
                "BSP leaf-water record {record} references texture info {tex_info}, but only {available_tex_info} records exist"
            ),
            Self::RecordLimit {
                lump,
                actual,
                limit,
            } => write!(
                formatter,
                "BSP lump {lump} has {actual} records; limit is {limit}"
            ),
            Self::InvalidTextureStringOffset { index, offset } => write!(
                formatter,
                "BSP texture string {index} has invalid offset {offset}"
            ),
            Self::UnterminatedTextureString { index } => {
                write!(formatter, "BSP texture string {index} has no terminator")
            }
            Self::InvalidTextureStringUtf8 { index } => {
                write!(formatter, "BSP texture string {index} is not UTF-8")
            }
            Self::InvalidEntityUtf8 => formatter.write_str("BSP entity lump is not UTF-8"),
            Self::MalformedEntities { offset } => {
                write!(formatter, "malformed BSP entity lump near byte {offset}")
            }
            Self::EntityLimit { limit } => write!(formatter, "BSP entity count exceeds {limit}"),
            Self::EntityPropertyLimit { limit } => {
                write!(formatter, "BSP entity property count exceeds {limit}")
            }
            Self::Pak(source) => write!(formatter, "invalid BSP PAKFILE: {source}"),
            Self::PakIo(source) => write!(formatter, "cannot read BSP PAKFILE entry: {source}"),
            Self::PakEntryLimit { actual, limit } => write!(
                formatter,
                "BSP PAKFILE has {actual} entries; limit is {limit}"
            ),
            Self::PakFileLimit {
                path,
                actual,
                limit,
            } => write!(
                formatter,
                "BSP PAKFILE entry {path} is {actual} bytes; limit is {limit}"
            ),
            Self::PakTotalLimit { actual, limit } => write!(
                formatter,
                "selected BSP PAKFILE entries total {actual} bytes; limit is {limit}"
            ),
        }
    }
}

impl Error for BspError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Pak(source) => Some(source),
            Self::PakIo(source) => Some(source),
            _ => None,
        }
    }
}

fn normalize_archive_path(path: &str) -> String {
    path.replace('\\', "/")
        .trim_start_matches('/')
        .to_ascii_lowercase()
}

fn i16_at(bytes: &[u8], offset: usize) -> i16 {
    i16::from_le_bytes(
        bytes[offset..offset + 2]
            .try_into()
            .expect("validated record"),
    )
}

fn u16_at(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(
        bytes[offset..offset + 2]
            .try_into()
            .expect("validated record"),
    )
}

fn i32_at(bytes: &[u8], offset: usize) -> i32 {
    i32::from_le_bytes(
        bytes[offset..offset + 4]
            .try_into()
            .expect("validated record"),
    )
}

fn f32_at(bytes: &[u8], offset: usize) -> f32 {
    f32::from_le_bytes(
        bytes[offset..offset + 4]
            .try_into()
            .expect("validated record"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_bsp() -> Vec<u8> {
        let mut bytes = vec![0; HEADER_BYTES];
        bytes[..4].copy_from_slice(BSP_MAGIC);
        bytes[4..8].copy_from_slice(&20_i32.to_le_bytes());
        bytes
    }

    fn append_lump(bytes: &mut Vec<u8>, lump: usize, data: &[u8]) {
        let offset = bytes.len();
        bytes.extend_from_slice(data);
        let descriptor = 8 + lump * 16;
        bytes[descriptor..descriptor + 4]
            .copy_from_slice(&i32::try_from(offset).expect("offset").to_le_bytes());
        bytes[descriptor + 4..descriptor + 8]
            .copy_from_slice(&i32::try_from(data.len()).expect("length").to_le_bytes());
    }

    fn model_record(first_face: i32, face_count: i32) -> [u8; 48] {
        let mut record = [0; 48];
        for (offset, value) in [
            (0, -1.0_f32),
            (4, -2.0),
            (8, -3.0),
            (12, 4.0),
            (16, 5.0),
            (20, 6.0),
            (24, 100.0),
            (28, 200.0),
            (32, 300.0),
        ] {
            record[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        record[36..40].copy_from_slice(&7_i32.to_le_bytes());
        record[40..44].copy_from_slice(&first_face.to_le_bytes());
        record[44..48].copy_from_slice(&face_count.to_le_bytes());
        record
    }

    fn leaf_water_record(surface_z: f32, min_z: f32, surface_tex_info: i16) -> [u8; 12] {
        let mut record = [0; 12];
        record[0..4].copy_from_slice(&surface_z.to_le_bytes());
        record[4..8].copy_from_slice(&min_z.to_le_bytes());
        record[8..10].copy_from_slice(&surface_tex_info.to_le_bytes());
        record
    }

    #[test]
    fn validates_header_and_empty_lumps() {
        let bsp = Bsp::parse(empty_bsp(), BspLimits::default()).expect("empty BSP header");
        assert_eq!(bsp.version(), 20);
        assert!(bsp.vertices().expect("vertices").is_empty());
        assert!(bsp.pak_entries().expect("empty PAKFILE").is_empty());
        assert!(
            bsp.pak_files_by_extension(&["vmt"])
                .expect("empty PAKFILE")
                .is_empty()
        );
    }

    #[test]
    fn rejects_lump_outside_file() {
        let mut bytes = empty_bsp();
        bytes[8..12].copy_from_slice(&20_000_i32.to_le_bytes());
        bytes[12..16].copy_from_slice(&4_i32.to_le_bytes());
        assert!(matches!(
            Bsp::parse(bytes, BspLimits::default()),
            Err(BspError::LumpOutOfBounds { lump: 0 })
        ));
    }

    #[test]
    fn parses_entity_keyvalues() {
        let entity = b"{\"classname\" \"info_player_start\" \"origin\" \"1 2 3\"}\0";
        let mut bytes = empty_bsp();
        let offset = bytes.len();
        bytes.extend_from_slice(entity);
        bytes[8..12].copy_from_slice(&i32::try_from(offset).expect("offset").to_le_bytes());
        bytes[12..16].copy_from_slice(&i32::try_from(entity.len()).expect("length").to_le_bytes());
        let bsp = Bsp::parse(bytes, BspLimits::default()).expect("BSP");
        let entities = bsp.entities().expect("entities");
        assert_eq!(entities[0].classname(), Some("info_player_start"));
        assert_eq!(entities[0].property("origin"), Some("1 2 3"));
    }

    #[test]
    fn parses_models_and_validates_face_ranges() {
        let mut bytes = empty_bsp();
        append_lump(&mut bytes, LUMP_FACES, &[0; 3 * 56]);
        append_lump(&mut bytes, LUMP_MODELS, &model_record(1, 2));
        let bsp = Bsp::parse(bytes, BspLimits::default()).expect("BSP");

        assert_eq!(
            bsp.models().expect("models"),
            vec![BspModel {
                mins: [-1.0, -2.0, -3.0],
                maxs: [4.0, 5.0, 6.0],
                origin: [100.0, 200.0, 300.0],
                head_node: 7,
                first_face: 1,
                face_count: 2,
            }]
        );

        let mut bytes = empty_bsp();
        append_lump(&mut bytes, LUMP_FACES, &[0; 3 * 56]);
        append_lump(&mut bytes, LUMP_MODELS, &model_record(2, 2));
        let bsp = Bsp::parse(bytes, BspLimits::default()).expect("BSP");
        assert!(matches!(
            bsp.models(),
            Err(BspError::InvalidModelFaceRange {
                model: 0,
                first_face: 2,
                face_count: 2,
                available_faces: 3,
            })
        ));
    }

    #[test]
    fn parses_leaf_water_data_and_validates_tex_info() {
        let mut bytes = empty_bsp();
        append_lump(&mut bytes, LUMP_TEXINFO, &[0; 9 * 72]);
        let mut water = Vec::new();
        water.extend_from_slice(&leaf_water_record(11_104.0, 10_832.0, 8));
        water.extend_from_slice(&leaf_water_record(11_247.0, 11_189.0, 8));
        append_lump(&mut bytes, LUMP_LEAFWATERDATA, &water);
        let bsp = Bsp::parse(bytes, BspLimits::default()).expect("BSP");

        assert_eq!(
            bsp.leaf_water_data().expect("leaf water data"),
            vec![
                BspLeafWaterData {
                    surface_z: 11_104.0,
                    min_z: 10_832.0,
                    surface_tex_info: 8,
                },
                BspLeafWaterData {
                    surface_z: 11_247.0,
                    min_z: 11_189.0,
                    surface_tex_info: 8,
                },
            ]
        );

        let mut bytes = empty_bsp();
        append_lump(&mut bytes, LUMP_TEXINFO, &[0; 9 * 72]);
        append_lump(
            &mut bytes,
            LUMP_LEAFWATERDATA,
            &leaf_water_record(1.0, 0.0, 9),
        );
        let bsp = Bsp::parse(bytes, BspLimits::default()).expect("BSP");
        assert!(matches!(
            bsp.leaf_water_data(),
            Err(BspError::InvalidLeafWaterTexInfo {
                record: 0,
                tex_info: 9,
                available_tex_info: 9,
            })
        ));
    }
}
