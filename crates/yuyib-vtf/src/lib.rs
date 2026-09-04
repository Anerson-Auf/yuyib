//! Source 1 VTF decoding into renderer-neutral RGBA8 pixels.
//!
//! Supports ordinary 2D, one-frame VTF 7.0 through 7.5 files and the common
//! RGBA8888, BGRA8888, BGR888, DXT1, DXT3 and DXT5 encodings.

#![forbid(unsafe_code)]
#![allow(missing_docs, reason = "Compact error variants mirror binary fields.")]

use std::{error::Error, fmt};

const SIGNATURE: &[u8; 4] = b"VTF\0";
const LEGACY_HEADER_SIZE: usize = 64;
const RESOURCE_HEADER_SIZE: usize = 80;
const HIGH_RES_RESOURCE: [u8; 3] = [0x30, 0, 0];

/// Bounds applied before decoding untrusted VTF bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VtfLimits {
    pub max_input_bytes: usize,
    pub max_dimension: u16,
    pub max_mip_count: u8,
    pub max_decoded_bytes: usize,
}
impl Default for VtfLimits {
    fn default() -> Self {
        Self {
            max_input_bytes: 256 * 1024 * 1024,
            max_dimension: 16_384,
            max_mip_count: 15,
            max_decoded_bytes: 256 * 1024 * 1024,
        }
    }
}

/// Original high-resolution VTF encoding.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VtfHighResFormat {
    Rgba8888,
    Bgr888,
    Bgra8888,
    Dxt1,
    Dxt3,
    Dxt5,
}
impl VtfHighResFormat {
    fn from_raw(raw: i32) -> Result<Self, VtfError> {
        match raw {
            0 => Ok(Self::Rgba8888),
            3 => Ok(Self::Bgr888),
            12 => Ok(Self::Bgra8888),
            13 => Ok(Self::Dxt1),
            14 => Ok(Self::Dxt3),
            15 => Ok(Self::Dxt5),
            format => Err(VtfError::UnsupportedHighResFormat { format }),
        }
    }
    fn mip_bytes(self, w: usize, h: usize) -> Option<usize> {
        match self {
            Self::Rgba8888 | Self::Bgra8888 => w.checked_mul(h)?.checked_mul(4),
            Self::Bgr888 => w.checked_mul(h)?.checked_mul(3),
            Self::Dxt1 => w.div_ceil(4).checked_mul(h.div_ceil(4))?.checked_mul(8),
            Self::Dxt3 | Self::Dxt5 => w.div_ceil(4).checked_mul(h.div_ceil(4))?.checked_mul(16),
        }
    }
}

/// Decoded largest-mip RGBA8 pixels.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VtfImage {
    width: u16,
    height: u16,
    pixels: Vec<u8>,
    source_format: VtfHighResFormat,
    mip_count: u8,
}
impl VtfImage {
    #[must_use]
    pub const fn width(&self) -> u16 {
        self.width
    }
    #[must_use]
    pub const fn height(&self) -> u16 {
        self.height
    }
    #[must_use]
    pub fn pixels_rgba8(&self) -> &[u8] {
        &self.pixels
    }
    #[must_use]
    pub const fn source_format(&self) -> VtfHighResFormat {
        self.source_format
    }
    #[must_use]
    pub const fn mip_count(&self) -> u8 {
        self.mip_count
    }
}

/// Decodes a supported Source 1 VTF with default bounds.
///
/// # Errors
/// Returns a structured error for malformed, over-budget or unsupported data.
pub fn decode(bytes: &[u8]) -> Result<VtfImage, VtfError> {
    decode_with_limits(bytes, VtfLimits::default())
}
/// Decodes a supported Source 1 VTF with explicit bounds.
///
/// # Errors
/// Returns a structured error for malformed, over-budget or unsupported data.
pub fn decode_with_limits(bytes: &[u8], limits: VtfLimits) -> Result<VtfImage, VtfError> {
    if bytes.len() > limits.max_input_bytes {
        return Err(VtfError::LimitExceeded {
            limit: VtfLimit::InputBytes,
            maximum: limits.max_input_bytes,
        });
    }
    let header = Header::read(bytes)?;
    header.validate(limits)?;
    let format = VtfHighResFormat::from_raw(header.format)?;
    let header_size = usize::try_from(header.size).map_err(|_| VtfError::InvalidHeaderSize {
        actual: header.size,
    })?;
    if header_size < header.minimum_size() || header_size > bytes.len() {
        return Err(VtfError::InvalidHeaderSize {
            actual: header.size,
        });
    }
    let levels = mip_layout(header.width, header.height, header.mips, format)?;
    let total = levels.iter().try_fold(0_usize, |sum, level| {
        sum.checked_add(*level).ok_or(VtfError::Overflow {
            field: "mip payload bytes",
        })
    })?;
    let high_size = *levels.last().ok_or(VtfError::InvalidMipCount {
        mip_count: header.mips,
    })?;
    let decoded = usize::from(header.width)
        .checked_mul(usize::from(header.height))
        .and_then(|n| n.checked_mul(4))
        .ok_or(VtfError::Overflow {
            field: "decoded pixel bytes",
        })?;
    if decoded > limits.max_decoded_bytes {
        return Err(VtfError::LimitExceeded {
            limit: VtfLimit::DecodedBytes,
            maximum: limits.max_decoded_bytes,
        });
    }
    let start = header.high_res_offset(bytes, header_size)?;
    let available = bytes
        .len()
        .checked_sub(start)
        .ok_or(VtfError::TruncatedPayload {
            expected: total,
            available: 0,
        })?;
    if available < total {
        return Err(VtfError::TruncatedPayload {
            expected: total,
            available,
        });
    }
    let high_start = start
        .checked_add(total.checked_sub(high_size).ok_or(VtfError::Overflow {
            field: "high-res mip offset",
        })?)
        .ok_or(VtfError::Overflow {
            field: "high-res mip offset",
        })?;
    let source =
        bytes
            .get(high_start..high_start + high_size)
            .ok_or(VtfError::TruncatedPayload {
                expected: total,
                available,
            })?;
    Ok(VtfImage {
        width: header.width,
        height: header.height,
        pixels: decode_level(
            source,
            usize::from(header.width),
            usize::from(header.height),
            format,
        )?,
        source_format: format,
        mip_count: header.mips,
    })
}

#[derive(Clone, Copy)]
struct Header {
    major: u32,
    minor: u32,
    size: u32,
    width: u16,
    height: u16,
    frames: u16,
    format: i32,
    mips: u8,
    depth: u16,
    low_res_format: i32,
    low_res_width: u8,
    low_res_height: u8,
}
impl Header {
    fn read(bytes: &[u8]) -> Result<Self, VtfError> {
        if bytes.len() < LEGACY_HEADER_SIZE {
            return Err(VtfError::TruncatedHeader {
                available: bytes.len(),
            });
        }
        if bytes.get(..4) != Some(SIGNATURE.as_slice()) {
            return Err(VtfError::InvalidSignature);
        }
        let major = u32_at(bytes, 4)?;
        let minor = u32_at(bytes, 8)?;
        Ok(Self {
            major,
            minor,
            size: u32_at(bytes, 12)?,
            width: u16_at(bytes, 16)?,
            height: u16_at(bytes, 18)?,
            frames: u16_at(bytes, 24)?,
            format: i32_at(bytes, 52)?,
            mips: byte_at(bytes, 56)?,
            // Depth was added in VTF 7.2. Byte 63 is already image payload in
            // a valid 64-byte VTF 7.0/7.1 header, so it must not be read there.
            depth: if major == 7 && minor >= 2 {
                u16_at(bytes, 63)?
            } else {
                1
            },
            low_res_format: i32_at(bytes, 57)?,
            low_res_width: byte_at(bytes, 61)?,
            low_res_height: byte_at(bytes, 62)?,
        })
    }
    fn validate(self, limits: VtfLimits) -> Result<(), VtfError> {
        if self.major != 7 || self.minor > 5 {
            return Err(VtfError::UnsupportedVersion {
                major: self.major,
                minor: self.minor,
            });
        }
        if self.width == 0
            || self.height == 0
            || self.width > limits.max_dimension
            || self.height > limits.max_dimension
        {
            return Err(VtfError::InvalidDimensions {
                width: self.width,
                height: self.height,
            });
        }
        if self.frames != 1 {
            return Err(VtfError::UnsupportedFrames {
                frames: self.frames,
            });
        }
        if self.depth != 1 {
            return Err(VtfError::UnsupportedDepth { depth: self.depth });
        }
        if self.mips == 0 || self.mips > limits.max_mip_count {
            return Err(VtfError::InvalidMipCount {
                mip_count: self.mips,
            });
        }
        Ok(())
    }
    const fn minimum_size(self) -> usize {
        if self.minor >= 2 {
            RESOURCE_HEADER_SIZE
        } else {
            LEGACY_HEADER_SIZE
        }
    }
    fn high_res_offset(self, bytes: &[u8], header_size: usize) -> Result<usize, VtfError> {
        if self.minor < 3 {
            let low_res_size = if self.low_res_width == 0 || self.low_res_height == 0 {
                0
            } else {
                VtfHighResFormat::from_raw(self.low_res_format)?
                    .mip_bytes(
                        usize::from(self.low_res_width),
                        usize::from(self.low_res_height),
                    )
                    .ok_or(VtfError::Overflow {
                        field: "low-res image bytes",
                    })?
            };
            return header_size
                .checked_add(low_res_size)
                .filter(|offset| *offset <= bytes.len())
                .ok_or(VtfError::TruncatedPayload {
                    expected: low_res_size,
                    available: bytes.len().saturating_sub(header_size),
                });
        }
        let count = usize::try_from(u32_at(bytes, 68)?).map_err(|_| VtfError::Overflow {
            field: "resource count",
        })?;
        let end = RESOURCE_HEADER_SIZE
            .checked_add(count.checked_mul(8).ok_or(VtfError::Overflow {
                field: "resource table",
            })?)
            .ok_or(VtfError::Overflow {
                field: "resource table",
            })?;
        if end > header_size || end > bytes.len() {
            return Err(VtfError::InvalidResourceDirectory);
        }
        for item in 0..count {
            let at = RESOURCE_HEADER_SIZE + item * 8;
            if bytes.get(at..at + 3) == Some(HIGH_RES_RESOURCE.as_slice()) {
                let value =
                    usize::try_from(u32_at(bytes, at + 4)?).map_err(|_| VtfError::Overflow {
                        field: "high-res resource offset",
                    })?;
                if value < header_size || value >= bytes.len() {
                    return Err(VtfError::InvalidResourceDirectory);
                }
                return Ok(value);
            }
        }
        Err(VtfError::MissingHighResResource)
    }
}
fn mip_layout(w: u16, h: u16, count: u8, format: VtfHighResFormat) -> Result<Vec<usize>, VtfError> {
    let mut levels = Vec::with_capacity(usize::from(count));
    for level in 0..count {
        let width = usize::from(w)
            .checked_shr(u32::from(level))
            .unwrap_or(0)
            .max(1);
        let height = usize::from(h)
            .checked_shr(u32::from(level))
            .unwrap_or(0)
            .max(1);
        levels.push(format.mip_bytes(width, height).ok_or(VtfError::Overflow {
            field: "mip level bytes",
        })?);
    }
    levels.reverse();
    Ok(levels)
}
fn decode_level(
    source: &[u8],
    w: usize,
    h: usize,
    format: VtfHighResFormat,
) -> Result<Vec<u8>, VtfError> {
    let mut out = vec![
        0;
        w.checked_mul(h).and_then(|n| n.checked_mul(4)).ok_or(
            VtfError::Overflow {
                field: "decoded pixel bytes"
            }
        )?
    ];
    match format {
        VtfHighResFormat::Rgba8888 => {
            for (d, s) in out.chunks_exact_mut(4).zip(source.chunks_exact(4)) {
                d.copy_from_slice(s);
            }
        }
        VtfHighResFormat::Bgra8888 => {
            for (d, s) in out.chunks_exact_mut(4).zip(source.chunks_exact(4)) {
                d.copy_from_slice(&[s[2], s[1], s[0], s[3]]);
            }
        }
        VtfHighResFormat::Bgr888 => {
            for (d, s) in out.chunks_exact_mut(4).zip(source.chunks_exact(3)) {
                d.copy_from_slice(&[s[2], s[1], s[0], 255]);
            }
        }
        format => decode_dxt(source, w, h, format, &mut out),
    };
    Ok(out)
}
fn decode_dxt(src: &[u8], w: usize, h: usize, format: VtfHighResFormat, out: &mut [u8]) {
    let block_len = if format == VtfHighResFormat::Dxt1 {
        8
    } else {
        16
    };
    let blocks_x = w.div_ceil(4);
    for (block_index, block) in src.chunks_exact(block_len).enumerate() {
        let x = block_index % blocks_x * 4;
        let y = block_index / blocks_x * 4;
        let (alpha, at) = match format {
            VtfHighResFormat::Dxt1 => (None, 0),
            VtfHighResFormat::Dxt3 => (Some(dxt3_alpha(&block[..8])), 8),
            VtfHighResFormat::Dxt5 => (Some(dxt5_alpha(&block[..8])), 8),
            _ => unreachable!(),
        };
        let colors = dxt_colors(&block[at..at + 4], format == VtfHighResFormat::Dxt1);
        let bits = u32::from_le_bytes(block[at + 4..at + 8].try_into().expect("fixed DXT block"));
        for py in 0..4 {
            for px in 0..4 {
                let dx = x + px;
                let dy = y + py;
                if dx >= w || dy >= h {
                    continue;
                }
                let i = py * 4 + px;
                let mut rgba = colors[((bits >> (i * 2)) & 3) as usize];
                if let Some(alpha) = alpha {
                    rgba[3] = alpha[i];
                }
                out[(dy * w + dx) * 4..][..4].copy_from_slice(&rgba);
            }
        }
    }
}
fn dxt_colors(src: &[u8], dxt1: bool) -> [[u8; 4]; 4] {
    let a = u16::from_le_bytes(src[..2].try_into().expect("fixed color"));
    let b = u16::from_le_bytes(src[2..4].try_into().expect("fixed color"));
    let rgb = |v: u16| {
        [
            (((v >> 11) & 31) * 255 / 31) as u8,
            (((v >> 5) & 63) * 255 / 63) as u8,
            ((v & 31) * 255 / 31) as u8,
            255,
        ]
    };
    let first = rgb(a);
    let second = rgb(b);
    let mix = |x: u8, y: u8, nx: u16, ny: u16, d: u16| {
        ((u16::from(x) * nx + u16::from(y) * ny) / d) as u8
    };
    if dxt1 && a <= b {
        [
            first,
            second,
            [
                mix(first[0], second[0], 1, 1, 2),
                mix(first[1], second[1], 1, 1, 2),
                mix(first[2], second[2], 1, 1, 2),
                255,
            ],
            [0, 0, 0, 0],
        ]
    } else {
        [
            first,
            second,
            [
                mix(first[0], second[0], 2, 1, 3),
                mix(first[1], second[1], 2, 1, 3),
                mix(first[2], second[2], 2, 1, 3),
                255,
            ],
            [
                mix(first[0], second[0], 1, 2, 3),
                mix(first[1], second[1], 1, 2, 3),
                mix(first[2], second[2], 1, 2, 3),
                255,
            ],
        ]
    }
}
fn dxt3_alpha(src: &[u8]) -> [u8; 16] {
    let bits = u64::from_le_bytes(src.try_into().expect("fixed alpha"));
    std::array::from_fn(|i| (((bits >> (i * 4)) & 15) as u8) * 17)
}
fn dxt5_alpha(src: &[u8]) -> [u8; 16] {
    let a0 = src[0];
    let a1 = src[1];
    let mut table = [0; 8];
    table[0] = a0;
    table[1] = a1;
    if a0 > a1 {
        for (i, value) in table.iter_mut().enumerate().skip(2) {
            *value = (((8 - i) as u16 * u16::from(a0) + (i - 1) as u16 * u16::from(a1)) / 7) as u8;
        }
    } else {
        for (i, value) in table.iter_mut().enumerate().take(6).skip(2) {
            *value = (((6 - i) as u16 * u16::from(a0) + (i - 1) as u16 * u16::from(a1)) / 5) as u8;
        }
        table[6] = 0;
        table[7] = 255;
    }
    let bits = src[2..8]
        .iter()
        .enumerate()
        .fold(0_u64, |all, (i, byte)| all | u64::from(*byte) << (i * 8));
    std::array::from_fn(|i| table[((bits >> (i * 3)) & 7) as usize])
}
fn byte_at(bytes: &[u8], at: usize) -> Result<u8, VtfError> {
    bytes.get(at).copied().ok_or(VtfError::TruncatedHeader {
        available: bytes.len(),
    })
}
fn u16_at(bytes: &[u8], at: usize) -> Result<u16, VtfError> {
    Ok(u16::from_le_bytes(
        bytes
            .get(at..at + 2)
            .ok_or(VtfError::TruncatedHeader {
                available: bytes.len(),
            })?
            .try_into()
            .expect("fixed slice"),
    ))
}
fn u32_at(bytes: &[u8], at: usize) -> Result<u32, VtfError> {
    Ok(u32::from_le_bytes(
        bytes
            .get(at..at + 4)
            .ok_or(VtfError::TruncatedHeader {
                available: bytes.len(),
            })?
            .try_into()
            .expect("fixed slice"),
    ))
}
fn i32_at(bytes: &[u8], at: usize) -> Result<i32, VtfError> {
    Ok(i32::from_le_bytes(
        bytes
            .get(at..at + 4)
            .ok_or(VtfError::TruncatedHeader {
                available: bytes.len(),
            })?
            .try_into()
            .expect("fixed slice"),
    ))
}
/// VTF decoder error.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VtfError {
    InvalidSignature,
    TruncatedHeader { available: usize },
    UnsupportedVersion { major: u32, minor: u32 },
    InvalidHeaderSize { actual: u32 },
    InvalidDimensions { width: u16, height: u16 },
    UnsupportedFrames { frames: u16 },
    UnsupportedDepth { depth: u16 },
    InvalidMipCount { mip_count: u8 },
    UnsupportedHighResFormat { format: i32 },
    MissingHighResResource,
    InvalidResourceDirectory,
    TruncatedPayload { expected: usize, available: usize },
    Overflow { field: &'static str },
    LimitExceeded { limit: VtfLimit, maximum: usize },
}
impl fmt::Display for VtfError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl Error for VtfError {}
/// Resource controlled by [`VtfLimits`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VtfLimit {
    InputBytes,
    DecodedBytes,
}
#[cfg(test)]
mod tests {
    use super::*;
    fn vtf(format: i32, w: u16, h: u16, pixels: &[u8]) -> Vec<u8> {
        let mut bytes = vec![0; 80];
        bytes[..4].copy_from_slice(b"VTF\0");
        bytes[4..8].copy_from_slice(&7_u32.to_le_bytes());
        bytes[8..12].copy_from_slice(&2_u32.to_le_bytes());
        bytes[12..16].copy_from_slice(&80_u32.to_le_bytes());
        bytes[16..18].copy_from_slice(&w.to_le_bytes());
        bytes[18..20].copy_from_slice(&h.to_le_bytes());
        bytes[24..26].copy_from_slice(&1_u16.to_le_bytes());
        bytes[52..56].copy_from_slice(&format.to_le_bytes());
        bytes[56] = 1;
        bytes[57..61].copy_from_slice(&(-1_i32).to_le_bytes());
        bytes[63..65].copy_from_slice(&1_u16.to_le_bytes());
        bytes.extend_from_slice(pixels);
        bytes
    }
    #[test]
    fn decodes_common_formats() {
        assert_eq!(
            decode(&vtf(3, 1, 1, &[3, 2, 1]))
                .expect("BGR")
                .pixels_rgba8(),
            [1, 2, 3, 255]
        );
        let color = [0, 0xf8, 0x1f, 0, 0, 0, 0, 0];
        assert_eq!(
            &decode(&vtf(13, 4, 4, &color)).expect("DXT1").pixels_rgba8()[..4],
            [255, 0, 0, 255]
        );
        let mut dxt3 = vec![0xff; 8];
        dxt3.extend(color);
        assert_eq!(
            &decode(&vtf(14, 4, 4, &dxt3)).expect("DXT3").pixels_rgba8()[..4],
            [255, 0, 0, 255]
        );
        let mut dxt5 = vec![255, 0, 0, 0, 0, 0, 0, 0];
        dxt5.extend(color);
        assert_eq!(
            &decode(&vtf(15, 4, 4, &dxt5)).expect("DXT5").pixels_rgba8()[..4],
            [255, 0, 0, 255]
        );
    }
    #[test]
    fn reads_v74_resource_directory() {
        let mut bytes = vtf(13, 4, 4, &[]);
        bytes[8..12].copy_from_slice(&4_u32.to_le_bytes());
        bytes[12..16].copy_from_slice(&96_u32.to_le_bytes());
        bytes.resize(96, 0);
        bytes[68..72].copy_from_slice(&2_u32.to_le_bytes());
        bytes[80..83].copy_from_slice(&HIGH_RES_RESOURCE);
        bytes[84..88].copy_from_slice(&96_u32.to_le_bytes());
        bytes.extend([0, 0xf8, 0x1f, 0, 0, 0, 0, 0]);
        assert_eq!(
            decode(&bytes).expect("v74").source_format(),
            VtfHighResFormat::Dxt1
        );
    }

    #[test]
    fn reads_v75_resource_directory() {
        let mut bytes = vtf(13, 4, 4, &[]);
        bytes[8..12].copy_from_slice(&5_u32.to_le_bytes());
        bytes[12..16].copy_from_slice(&96_u32.to_le_bytes());
        bytes.resize(96, 0);
        bytes[68..72].copy_from_slice(&1_u32.to_le_bytes());
        bytes[80..83].copy_from_slice(&HIGH_RES_RESOURCE);
        bytes[84..88].copy_from_slice(&96_u32.to_le_bytes());
        bytes.extend([0, 0xf8, 0x1f, 0, 0, 0, 0, 0]);

        assert_eq!(
            decode(&bytes).expect("VTF 7.5").source_format(),
            VtfHighResFormat::Dxt1
        );
    }

    #[test]
    fn reads_v70_legacy_header_after_low_res_thumbnail() {
        let mut bytes = vec![0; LEGACY_HEADER_SIZE];
        bytes[..4].copy_from_slice(b"VTF\0");
        bytes[4..8].copy_from_slice(&7_u32.to_le_bytes());
        bytes[8..12].copy_from_slice(&0_u32.to_le_bytes());
        bytes[12..16].copy_from_slice(
            &u32::try_from(LEGACY_HEADER_SIZE)
                .expect("legacy header size fits u32")
                .to_le_bytes(),
        );
        bytes[16..18].copy_from_slice(&4_u16.to_le_bytes());
        bytes[18..20].copy_from_slice(&4_u16.to_le_bytes());
        bytes[24..26].copy_from_slice(&1_u16.to_le_bytes());
        bytes[52..56].copy_from_slice(&13_i32.to_le_bytes());
        bytes[56] = 1;
        bytes[57..61].copy_from_slice(&13_i32.to_le_bytes());
        bytes[61] = 4;
        bytes[62] = 4;

        // The low-resolution thumbnail is blue; the actual largest mip is red.
        bytes.extend([0x1f, 0, 0x1f, 0, 0, 0, 0, 0]);
        bytes.extend([0, 0xf8, 0, 0xf8, 0, 0, 0, 0]);

        let image = decode(&bytes).expect("VTF 7.0");
        assert_eq!(image.source_format(), VtfHighResFormat::Dxt1);
        assert_eq!(&image.pixels_rgba8()[..4], [255, 0, 0, 255]);
    }
}
