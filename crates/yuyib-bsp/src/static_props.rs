//! Typed, bounded Source 1 `GAME_LUMP` and `sprp` decoding.
//!
//! Static-prop instances only reference `StudioModel` paths. Rendering their real
//! meshes additionally requires the corresponding MDL, VVD and VTX files; this
//! module deliberately does not replace missing model data with proxy geometry.

use std::{error::Error, fmt};

use super::{Bsp, BspError, LUMP_GAME_LUMP};

/// Source's multi-character `GAMELUMP_STATIC_PROPS` identifier (`'sprp'`).
///
/// Multi-character constants are stored byte-reversed in little-endian BSP
/// files, so the raw directory bytes are `prps` and decode to this integer.
pub const GAME_LUMP_STATIC_PROPS: u32 = 0x7370_7270;

const GAME_LUMP_DIRECTORY_RECORD_BYTES: usize = 16;
const STATIC_PROP_MODEL_NAME_BYTES: usize = 128;

/// One validated entry in the BSP `GAME_LUMP` directory.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BspGameLumpEntry {
    /// Source multi-character lump identifier.
    pub id: u32,
    /// Source game-lump flags. Bit zero denotes compression.
    pub flags: u16,
    /// Format version of this game lump.
    pub version: u16,
    /// Absolute byte offset in the containing BSP.
    pub offset: usize,
    /// Encoded byte length.
    pub length: usize,
}

/// One decoded Source `StaticPropLump` instance.
#[derive(Clone, Debug, PartialEq)]
pub struct BspStaticProp {
    /// Source-space instance origin.
    pub origin: [f32; 3],
    /// Source-space pitch/yaw/roll angles in degrees.
    pub angles: [f32; 3],
    /// Index into [`BspStaticPropLump::model_names`].
    pub model_index: u16,
    /// First index in [`BspStaticPropLump::leaf_indices`].
    pub first_leaf: u16,
    /// Number of referenced BSP leaves.
    pub leaf_count: u16,
    /// Source `SolidType_t` byte.
    pub solid: u8,
    /// Version-normalized Source static-prop flags.
    pub flags: u32,
    /// Model skin index.
    pub skin: i32,
    /// Distance at which fading starts.
    pub fade_min_distance: f32,
    /// Distance at which fading completes.
    pub fade_max_distance: f32,
    /// Source-space lighting sample origin.
    pub lighting_origin: [f32; 3],
    /// Forced fade scale, present since static-prop version 5.
    pub forced_fade_scale: Option<f32>,
    /// Minimum and maximum legacy DirectX levels, present since version 6.
    pub dx_levels: Option<[u16; 2]>,
    /// Per-prop lightmap resolution, present in Source SDK 2013 version 10.
    pub lightmap_resolution: Option<[u16; 2]>,
}

/// Decoded Source static-prop dictionary, leaf list and instances.
#[derive(Clone, Debug, PartialEq)]
pub struct BspStaticPropLump {
    /// Source `sprp` format version.
    pub version: u16,
    /// `StudioModel` paths indexed by [`BspStaticProp::model_index`].
    pub model_names: Vec<String>,
    /// BSP leaf indices referenced by each prop's leaf range.
    pub leaf_indices: Vec<u16>,
    /// Static-prop instances in source order.
    pub props: Vec<BspStaticProp>,
}

impl BspStaticPropLump {
    /// Resolves the model path referenced by `prop`.
    #[must_use]
    pub fn model_name(&self, prop: &BspStaticProp) -> Option<&str> {
        self.model_names
            .get(usize::from(prop.model_index))
            .map(String::as_str)
    }

    /// Resolves the leaf slice referenced by `prop`.
    #[must_use]
    pub fn leaves(&self, prop: &BspStaticProp) -> Option<&[u16]> {
        let first = usize::from(prop.first_leaf);
        let end = first.checked_add(usize::from(prop.leaf_count))?;
        self.leaf_indices.get(first..end)
    }
}

/// Failure while decoding the Source `GAME_LUMP` directory or `sprp` payload.
#[derive(Debug)]
pub enum BspStaticPropError {
    /// The enclosing BSP lump could not be read.
    Bsp(BspError),
    /// A counted section ends before all declared records are present.
    Truncated {
        /// Section being decoded.
        section: &'static str,
        /// Required end offset relative to the decoded buffer.
        needed: usize,
        /// Available bytes.
        available: usize,
    },
    /// A declared record count exceeds the BSP's bounded-work policy.
    RecordLimit {
        /// Section being decoded.
        section: &'static str,
        /// Declared count.
        actual: usize,
        /// Configured maximum.
        limit: usize,
    },
    /// A signed Source record count was negative.
    NegativeCount {
        /// Section being decoded.
        section: &'static str,
        /// Invalid signed count.
        value: i32,
    },
    /// A game-lump directory entry points outside the containing BSP.
    InvalidGameLumpRange {
        /// Directory index.
        entry: usize,
        /// Raw signed file offset.
        offset: i32,
        /// Raw signed length.
        length: i32,
    },
    /// Compressed game lumps are kept explicit rather than misdecoded.
    CompressedStaticProps {
        /// Raw game-lump flags.
        flags: u16,
    },
    /// More than one static-prop lump was declared.
    DuplicateStaticProps,
    /// This branch/version has no unambiguous record layout here.
    UnsupportedVersion(u16),
    /// Static-prop records do not exactly fill the payload.
    InvalidRecordBytes {
        /// Format version.
        version: u16,
        /// Expected bytes for the declared instance count.
        expected: usize,
        /// Remaining bytes in the payload.
        actual: usize,
    },
    /// A fixed-width model dictionary entry is not valid UTF-8.
    InvalidModelName {
        /// Dictionary index.
        index: usize,
    },
    /// A prop references a model dictionary index that does not exist.
    InvalidModelIndex {
        /// Prop instance index.
        prop: usize,
        /// Referenced model index.
        model: u16,
        /// Model dictionary length.
        available: usize,
    },
    /// A prop's leaf range is outside the decoded leaf table.
    InvalidLeafRange {
        /// Prop instance index.
        prop: usize,
        /// First referenced leaf-list index.
        first: u16,
        /// Referenced leaf count.
        count: u16,
        /// Leaf-list length.
        available: usize,
    },
}

impl fmt::Display for BspStaticPropError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bsp(source) => source.fmt(formatter),
            Self::Truncated {
                section,
                needed,
                available,
            } => write!(
                formatter,
                "Source static-prop {section} needs {needed} bytes; only {available} available"
            ),
            Self::RecordLimit {
                section,
                actual,
                limit,
            } => write!(
                formatter,
                "Source static-prop {section} has {actual} records; limit is {limit}"
            ),
            Self::NegativeCount { section, value } => write!(
                formatter,
                "Source static-prop {section} has invalid negative count {value}"
            ),
            Self::InvalidGameLumpRange {
                entry,
                offset,
                length,
            } => write!(
                formatter,
                "Source game lump {entry} has invalid BSP range {offset}+{length}"
            ),
            Self::CompressedStaticProps { flags } => write!(
                formatter,
                "Source static-prop game lump uses unsupported compression flags {flags:#06x}"
            ),
            Self::DuplicateStaticProps => {
                formatter.write_str("BSP declares more than one static-prop game lump")
            }
            Self::UnsupportedVersion(version) => {
                write!(
                    formatter,
                    "unsupported Source static-prop version {version}"
                )
            }
            Self::InvalidRecordBytes {
                version,
                expected,
                actual,
            } => write!(
                formatter,
                "Source static-prop v{version} records need {expected} bytes; payload has {actual}"
            ),
            Self::InvalidModelName { index } => {
                write!(
                    formatter,
                    "Source static-prop model name {index} is not UTF-8"
                )
            }
            Self::InvalidModelIndex {
                prop,
                model,
                available,
            } => write!(
                formatter,
                "Source static prop {prop} references model {model}; dictionary has {available} entries"
            ),
            Self::InvalidLeafRange {
                prop,
                first,
                count,
                available,
            } => write!(
                formatter,
                "Source static prop {prop} leaf range {first}+{count} exceeds {available} leaves"
            ),
        }
    }
}

impl Error for BspStaticPropError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Bsp(source) => Some(source),
            _ => None,
        }
    }
}

impl From<BspError> for BspStaticPropError {
    fn from(source: BspError) -> Self {
        Self::Bsp(source)
    }
}

impl Bsp {
    /// Returns the validated Source game-lump directory.
    ///
    /// # Errors
    ///
    /// Rejects truncated directories, excessive counts and ranges outside the
    /// containing BSP. Individual payload compression is reported when that
    /// payload is decoded.
    pub fn game_lumps(&self) -> Result<Vec<BspGameLumpEntry>, BspStaticPropError> {
        let bytes = self.lump_bytes(LUMP_GAME_LUMP)?;
        if bytes.is_empty() {
            return Ok(Vec::new());
        }
        require(bytes, 0, 4, "game-lump header")?;
        let count = non_negative_count(i32_at(bytes, 0), "game-lump directory")?;
        bounded(
            count,
            self.limits.max_records_per_lump,
            "game-lump directory",
        )?;
        let records_bytes = count.checked_mul(GAME_LUMP_DIRECTORY_RECORD_BYTES).ok_or(
            BspStaticPropError::RecordLimit {
                section: "game-lump directory",
                actual: count,
                limit: self.limits.max_records_per_lump,
            },
        )?;
        require(bytes, 4, records_bytes, "game-lump directory")?;

        let mut entries = Vec::with_capacity(count);
        for index in 0..count {
            let cursor = 4 + index * GAME_LUMP_DIRECTORY_RECORD_BYTES;
            let raw_offset = i32_at(bytes, cursor + 8);
            let raw_length = i32_at(bytes, cursor + 12);
            let (Ok(offset), Ok(length)) =
                (usize::try_from(raw_offset), usize::try_from(raw_length))
            else {
                return Err(BspStaticPropError::InvalidGameLumpRange {
                    entry: index,
                    offset: raw_offset,
                    length: raw_length,
                });
            };
            let Some(end) = offset.checked_add(length) else {
                return Err(BspStaticPropError::InvalidGameLumpRange {
                    entry: index,
                    offset: raw_offset,
                    length: raw_length,
                });
            };
            if length > self.limits.max_lump_bytes || end > self.bytes.len() {
                return Err(BspStaticPropError::InvalidGameLumpRange {
                    entry: index,
                    offset: raw_offset,
                    length: raw_length,
                });
            }
            entries.push(BspGameLumpEntry {
                id: u32_at(bytes, cursor),
                flags: u16_at(bytes, cursor + 4),
                version: u16_at(bytes, cursor + 6),
                offset,
                length,
            });
        }
        Ok(entries)
    }

    /// Decodes the optional Source static-prop (`sprp`) game lump.
    ///
    /// # Errors
    ///
    /// Rejects ambiguous versions, compression, malformed counts/references,
    /// excessive records and payload ranges outside the BSP.
    pub fn static_props(&self) -> Result<Option<BspStaticPropLump>, BspStaticPropError> {
        let mut found = None;
        for entry in self.game_lumps()? {
            if entry.id != GAME_LUMP_STATIC_PROPS {
                continue;
            }
            if found.replace(entry).is_some() {
                return Err(BspStaticPropError::DuplicateStaticProps);
            }
        }
        let Some(entry) = found else {
            return Ok(None);
        };
        if entry.flags & 1 != 0 {
            return Err(BspStaticPropError::CompressedStaticProps { flags: entry.flags });
        }
        let bytes = &self.bytes[entry.offset..entry.offset + entry.length];
        parse_static_prop_game_lump(bytes, entry.version, self.limits.max_records_per_lump)
            .map(Some)
    }
}

/// Decodes a standalone uncompressed Source static-prop game-lump payload.
///
/// Versions 4, 5, 6 and Source SDK 2013 version 10 have explicit, stable
/// layouts. Versions 7-9 differ between Source branches and are rejected rather
/// than guessed.
///
/// # Errors
///
/// Rejects unsupported versions, truncated sections, excessive record counts,
/// invalid UTF-8 model paths and invalid model/leaf references.
#[allow(
    clippy::too_many_lines,
    reason = "the bounded binary parser keeps every version-specific field and reference validation adjacent"
)]
pub fn parse_static_prop_game_lump(
    bytes: &[u8],
    version: u16,
    max_records: usize,
) -> Result<BspStaticPropLump, BspStaticPropError> {
    let stride = match version {
        4 => 56,
        5 => 60,
        6 => 64,
        10 => 72,
        _ => return Err(BspStaticPropError::UnsupportedVersion(version)),
    };
    let mut cursor = 0;
    let model_count = take_count(bytes, &mut cursor, "model dictionary", max_records)?;
    let model_bytes = model_count
        .checked_mul(STATIC_PROP_MODEL_NAME_BYTES)
        .ok_or(BspStaticPropError::RecordLimit {
            section: "model dictionary",
            actual: model_count,
            limit: max_records,
        })?;
    require(bytes, cursor, model_bytes, "model dictionary")?;
    let mut model_names = Vec::with_capacity(model_count);
    for index in 0..model_count {
        let start = cursor + index * STATIC_PROP_MODEL_NAME_BYTES;
        let fixed = &bytes[start..start + STATIC_PROP_MODEL_NAME_BYTES];
        let end = fixed
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(fixed.len());
        let name = std::str::from_utf8(&fixed[..end])
            .map_err(|_| BspStaticPropError::InvalidModelName { index })?;
        model_names.push(name.replace('\\', "/"));
    }
    cursor += model_bytes;

    let leaf_count = take_count(bytes, &mut cursor, "leaf table", max_records)?;
    let leaf_bytes = leaf_count
        .checked_mul(2)
        .ok_or(BspStaticPropError::RecordLimit {
            section: "leaf table",
            actual: leaf_count,
            limit: max_records,
        })?;
    require(bytes, cursor, leaf_bytes, "leaf table")?;
    let leaf_indices = bytes[cursor..cursor + leaf_bytes]
        .chunks_exact(2)
        .map(|record| u16_at(record, 0))
        .collect::<Vec<_>>();
    cursor += leaf_bytes;

    let prop_count = take_count(bytes, &mut cursor, "instance table", max_records)?;
    let expected = prop_count
        .checked_mul(stride)
        .ok_or(BspStaticPropError::RecordLimit {
            section: "instance table",
            actual: prop_count,
            limit: max_records,
        })?;
    let actual = bytes.len().saturating_sub(cursor);
    if actual != expected {
        return Err(BspStaticPropError::InvalidRecordBytes {
            version,
            expected,
            actual,
        });
    }

    let mut props = Vec::with_capacity(prop_count);
    for (index, record) in bytes[cursor..].chunks_exact(stride).enumerate() {
        let model_index = u16_at(record, 24);
        if usize::from(model_index) >= model_names.len() {
            return Err(BspStaticPropError::InvalidModelIndex {
                prop: index,
                model: model_index,
                available: model_names.len(),
            });
        }
        let first_leaf = u16_at(record, 26);
        let prop_leaf_count = u16_at(record, 28);
        let leaf_end = usize::from(first_leaf).saturating_add(usize::from(prop_leaf_count));
        if leaf_end > leaf_indices.len() {
            return Err(BspStaticPropError::InvalidLeafRange {
                prop: index,
                first: first_leaf,
                count: prop_leaf_count,
                available: leaf_indices.len(),
            });
        }
        props.push(BspStaticProp {
            origin: vector_at(record, 0),
            angles: vector_at(record, 12),
            model_index,
            first_leaf,
            leaf_count: prop_leaf_count,
            solid: record[30],
            flags: if version == 10 {
                u32_at(record, 64)
            } else {
                u32::from(record[31])
            },
            skin: i32_at(record, 32),
            fade_min_distance: f32_at(record, 36),
            fade_max_distance: f32_at(record, 40),
            lighting_origin: vector_at(record, 44),
            forced_fade_scale: (version >= 5).then(|| f32_at(record, 56)),
            dx_levels: (version >= 6).then(|| [u16_at(record, 60), u16_at(record, 62)]),
            lightmap_resolution: (version == 10).then(|| [u16_at(record, 68), u16_at(record, 70)]),
        });
    }

    Ok(BspStaticPropLump {
        version,
        model_names,
        leaf_indices,
        props,
    })
}

fn take_count(
    bytes: &[u8],
    cursor: &mut usize,
    section: &'static str,
    limit: usize,
) -> Result<usize, BspStaticPropError> {
    require(bytes, *cursor, 4, section)?;
    let count = non_negative_count(i32_at(bytes, *cursor), section)?;
    bounded(count, limit, section)?;
    *cursor += 4;
    Ok(count)
}

fn non_negative_count(value: i32, section: &'static str) -> Result<usize, BspStaticPropError> {
    usize::try_from(value).map_err(|_| BspStaticPropError::NegativeCount { section, value })
}

fn bounded(actual: usize, limit: usize, section: &'static str) -> Result<(), BspStaticPropError> {
    if actual > limit {
        Err(BspStaticPropError::RecordLimit {
            section,
            actual,
            limit,
        })
    } else {
        Ok(())
    }
}

fn require(
    bytes: &[u8],
    offset: usize,
    length: usize,
    section: &'static str,
) -> Result<(), BspStaticPropError> {
    let needed = offset.saturating_add(length);
    if needed > bytes.len() {
        Err(BspStaticPropError::Truncated {
            section,
            needed,
            available: bytes.len(),
        })
    } else {
        Ok(())
    }
}

fn vector_at(bytes: &[u8], offset: usize) -> [f32; 3] {
    [
        f32_at(bytes, offset),
        f32_at(bytes, offset + 4),
        f32_at(bytes, offset + 8),
    ]
}

fn u16_at(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(
        bytes[offset..offset + 2]
            .try_into()
            .expect("validated field"),
    )
}

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(
        bytes[offset..offset + 4]
            .try_into()
            .expect("validated field"),
    )
}

fn i32_at(bytes: &[u8], offset: usize) -> i32 {
    i32::from_le_bytes(
        bytes[offset..offset + 4]
            .try_into()
            .expect("validated field"),
    )
}

fn f32_at(bytes: &[u8], offset: usize) -> f32 {
    f32::from_le_bytes(
        bytes[offset..offset + 4]
            .try_into()
            .expect("validated field"),
    )
}

#[cfg(test)]
mod tests {
    use crate::BspLimits;

    use super::*;

    #[test]
    fn decodes_source_sdk_2013_v10_static_props() {
        let mut bytes = Vec::new();
        push_i32(&mut bytes, 1);
        let mut name = [0_u8; STATIC_PROP_MODEL_NAME_BYTES];
        let model_name = b"models/props/tree01.mdl";
        name[..model_name.len()].copy_from_slice(model_name);
        bytes.extend_from_slice(&name);
        push_i32(&mut bytes, 2);
        push_u16(&mut bytes, 7);
        push_u16(&mut bytes, 11);
        push_i32(&mut bytes, 1);

        push_vector(&mut bytes, [1.0, 2.0, 3.0]);
        push_vector(&mut bytes, [10.0, 20.0, 30.0]);
        push_u16(&mut bytes, 0);
        push_u16(&mut bytes, 0);
        push_u16(&mut bytes, 2);
        bytes.push(6);
        bytes.push(0);
        push_i32(&mut bytes, 4);
        push_f32(&mut bytes, 100.0);
        push_f32(&mut bytes, 200.0);
        push_vector(&mut bytes, [4.0, 5.0, 6.0]);
        push_f32(&mut bytes, 1.5);
        push_u16(&mut bytes, 80);
        push_u16(&mut bytes, 95);
        bytes.extend_from_slice(&0x101_u32.to_le_bytes());
        push_u16(&mut bytes, 32);
        push_u16(&mut bytes, 64);

        let lump = parse_static_prop_game_lump(&bytes, 10, 100).expect("valid v10 lump");
        assert_eq!(lump.model_names, ["models/props/tree01.mdl"]);
        assert_eq!(lump.leaf_indices, [7, 11]);
        assert_eq!(lump.props.len(), 1);
        let prop = &lump.props[0];
        assert_eq!(lump.model_name(prop), Some("models/props/tree01.mdl"));
        assert_eq!(lump.leaves(prop), Some([7, 11].as_slice()));
        assert_eq!(prop.origin, [1.0, 2.0, 3.0]);
        assert_eq!(prop.angles, [10.0, 20.0, 30.0]);
        assert_eq!(prop.solid, 6);
        assert_eq!(prop.flags, 0x101);
        assert_eq!(prop.skin, 4);
        assert_eq!(prop.forced_fade_scale, Some(1.5));
        assert_eq!(prop.dx_levels, Some([80, 95]));
        assert_eq!(prop.lightmap_resolution, Some([32, 64]));
    }

    #[test]
    fn rejects_invalid_model_and_leaf_references() {
        let mut bytes = Vec::new();
        push_i32(&mut bytes, 0);
        push_i32(&mut bytes, 0);
        push_i32(&mut bytes, 1);
        bytes.resize(bytes.len() + 56, 0);
        bytes[12 + 24..12 + 26].copy_from_slice(&1_u16.to_le_bytes());
        assert!(matches!(
            parse_static_prop_game_lump(&bytes, 4, 10),
            Err(BspStaticPropError::InvalidModelIndex {
                prop: 0,
                model: 1,
                ..
            })
        ));

        let mut bytes = Vec::new();
        push_i32(&mut bytes, 1);
        bytes.resize(bytes.len() + STATIC_PROP_MODEL_NAME_BYTES, 0);
        push_i32(&mut bytes, 0);
        push_i32(&mut bytes, 1);
        let record = bytes.len();
        bytes.resize(record + 56, 0);
        bytes[record + 26..record + 28].copy_from_slice(&1_u16.to_le_bytes());
        bytes[record + 28..record + 30].copy_from_slice(&1_u16.to_le_bytes());
        assert!(matches!(
            parse_static_prop_game_lump(&bytes, 4, 10),
            Err(BspStaticPropError::InvalidLeafRange {
                prop: 0,
                first: 1,
                count: 1,
                ..
            })
        ));
    }

    #[test]
    fn rejects_branch_ambiguous_versions_and_trailing_bytes() {
        assert!(matches!(
            parse_static_prop_game_lump(&[], 9, 10),
            Err(BspStaticPropError::UnsupportedVersion(9))
        ));
        let mut empty = Vec::new();
        push_i32(&mut empty, 0);
        push_i32(&mut empty, 0);
        push_i32(&mut empty, 0);
        empty.push(0);
        assert!(matches!(
            parse_static_prop_game_lump(&empty, 10, 10),
            Err(BspStaticPropError::InvalidRecordBytes {
                expected: 0,
                actual: 1,
                ..
            })
        ));
    }

    #[test]
    fn finds_static_props_through_absolute_game_lump_directory_offsets() {
        let mut payload = Vec::new();
        push_i32(&mut payload, 0);
        push_i32(&mut payload, 0);
        push_i32(&mut payload, 0);

        let header_bytes = 8 + 64 * 16 + 4;
        let directory_offset = header_bytes;
        let payload_offset = directory_offset + 4 + GAME_LUMP_DIRECTORY_RECORD_BYTES;
        let mut bytes = vec![0_u8; header_bytes];
        bytes[..4].copy_from_slice(b"VBSP");
        bytes[4..8].copy_from_slice(&20_i32.to_le_bytes());
        push_i32(&mut bytes, 1);
        bytes.extend_from_slice(&GAME_LUMP_STATIC_PROPS.to_le_bytes());
        push_u16(&mut bytes, 0);
        push_u16(&mut bytes, 10);
        push_i32(
            &mut bytes,
            i32::try_from(payload_offset).expect("payload offset"),
        );
        push_i32(
            &mut bytes,
            i32::try_from(payload.len()).expect("payload length"),
        );
        bytes.extend_from_slice(&payload);
        let descriptor = 8 + LUMP_GAME_LUMP * 16;
        bytes[descriptor..descriptor + 4].copy_from_slice(
            &i32::try_from(directory_offset)
                .expect("directory offset")
                .to_le_bytes(),
        );
        bytes[descriptor + 4..descriptor + 8].copy_from_slice(
            &i32::try_from(4 + GAME_LUMP_DIRECTORY_RECORD_BYTES)
                .expect("directory length")
                .to_le_bytes(),
        );

        let bsp = Bsp::parse(bytes, BspLimits::default()).expect("valid BSP");
        let directory = bsp.game_lumps().expect("game lump directory");
        assert_eq!(directory.len(), 1);
        assert_eq!(directory[0].id, GAME_LUMP_STATIC_PROPS);
        let props = bsp
            .static_props()
            .expect("sprp parse")
            .expect("sprp exists");
        assert_eq!(props.version, 10);
        assert!(props.model_names.is_empty());
        assert!(props.props.is_empty());
    }

    fn push_vector(bytes: &mut Vec<u8>, value: [f32; 3]) {
        for component in value {
            push_f32(bytes, component);
        }
    }

    fn push_f32(bytes: &mut Vec<u8>, value: f32) {
        bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn push_i32(bytes: &mut Vec<u8>, value: i32) {
        bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn push_u16(bytes: &mut Vec<u8>, value: u16) {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
}
