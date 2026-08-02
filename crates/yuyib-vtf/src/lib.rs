//! Bounded narrow Source 1 VTF binary decoding.
//!
//! The reader accepts little-endian VTF 7.2 headers for a single
//! 2D frame with no low-resolution thumbnail. It validates the complete
//! smallest-to-largest mip layout, then decodes the highest-resolution exact
//! RGBA8888 or BGRA8888 payload into RGBA8 output.
//!
//! It deliberately rejects VTF 7.3 resource chunks, cubemaps, multiple frames,
//! depth textures, low-res thumbnails, compressed formats, VPK/filesystem
//! loading and Source 2 texture formats.

#![forbid(unsafe_code)]
#![allow(
    clippy::doc_markdown,
    reason = "The public decoder names are intentionally plain in compact rustdoc."
)]

use std::{error::Error, fmt};

const SIGNATURE: &[u8; 4] = b"VTF\0";
const MIN_HEADER_SIZE: usize = 80;
const SUPPORTED_MINOR: u32 = 2;
const IMAGE_FORMAT_NONE: i32 = -1;
const IMAGE_FORMAT_RGBA8888: i32 = 0;
const IMAGE_FORMAT_BGRA8888: i32 = 12;

/// Bounds applied before decoding untrusted VTF bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VtfLimits {
    /// Maximum complete VTF input bytes.
    pub max_input_bytes: usize,
    /// Maximum width or height.
    pub max_dimension: u16,
    /// Maximum declared mip count.
    pub max_mip_count: u8,
    /// Maximum decoded highest-resolution RGBA8 bytes.
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

/// High-resolution Source VTF format accepted by this decoder.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VtfHighResFormat {
    /// Four payload bytes are red, green, blue, alpha.
    Rgba8888,
    /// Four payload bytes are blue, green, red, alpha.
    Bgra8888,
}

/// Decoded highest-resolution RGBA8 texture pixels.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VtfImage {
    width: u16,
    height: u16,
    pixels: Vec<u8>,
    source_format: VtfHighResFormat,
    mip_count: u8,
}

impl VtfImage {
    /// Returns highest-resolution image width.
    #[must_use]
    pub const fn width(&self) -> u16 {
        self.width
    }
    /// Returns highest-resolution image height.
    #[must_use]
    pub const fn height(&self) -> u16 {
        self.height
    }
    /// Returns tightly packed RGBA8 pixels in row-major order.
    #[must_use]
    pub fn pixels_rgba8(&self) -> &[u8] {
        &self.pixels
    }
    /// Returns original high-resolution VTF pixel ordering.
    #[must_use]
    pub const fn source_format(&self) -> VtfHighResFormat {
        self.source_format
    }
    /// Returns declared mip level count after validation.
    #[must_use]
    pub const fn mip_count(&self) -> u8 {
        self.mip_count
    }
}

/// Decodes a narrow supported Source 1 VTF file with default limits.
///
/// # Errors
///
/// Returns VtfError for header, format, overflow, mip layout or budget failure.
pub fn decode(bytes: &[u8]) -> Result<VtfImage, VtfError> {
    decode_with_limits(bytes, VtfLimits::default())
}

/// Decodes a narrow supported Source 1 VTF file with explicit limits.
///
/// Header fields are little-endian. The VTF high-resolution mip chain is
/// interpreted in Source 1 storage order, smallest level first and largest
/// level last. Only one frame, one face and depth one are accepted, so no
/// frame/face/depth stride is guessed.
///
/// # Errors
///
/// Returns VtfError for header, format, overflow, mip layout or budget failure.
pub fn decode_with_limits(bytes: &[u8], limits: VtfLimits) -> Result<VtfImage, VtfError> {
    if bytes.len() > limits.max_input_bytes {
        return Err(VtfError::LimitExceeded {
            limit: VtfLimit::InputBytes,
            maximum: limits.max_input_bytes,
        });
    }
    let header = Header::read(bytes)?;
    header.validate(limits)?;
    let format = match header.high_res_format {
        IMAGE_FORMAT_RGBA8888 => VtfHighResFormat::Rgba8888,
        IMAGE_FORMAT_BGRA8888 => VtfHighResFormat::Bgra8888,
        format => return Err(VtfError::UnsupportedHighResFormat { format }),
    };
    let header_size = usize::try_from(header.size).map_err(|_| VtfError::InvalidHeaderSize {
        actual: header.size,
    })?;
    if header_size < MIN_HEADER_SIZE || header_size > bytes.len() {
        return Err(VtfError::InvalidHeaderSize {
            actual: header.size,
        });
    }
    let levels = mip_layout(header.width, header.height, header.mip_count)?;
    let total = levels.iter().try_fold(0_usize, |total, level| {
        total.checked_add(level.bytes).ok_or(VtfError::Overflow {
            field: "mip payload bytes",
        })
    })?;
    let available = bytes
        .len()
        .checked_sub(header_size)
        .ok_or(VtfError::InvalidHeaderSize {
            actual: header.size,
        })?;
    if available < total {
        return Err(VtfError::TruncatedPayload {
            expected: total,
            available,
        });
    }
    let high = levels.last().ok_or(VtfError::InvalidMipCount {
        mip_count: header.mip_count,
    })?;
    if high.bytes > limits.max_decoded_bytes {
        return Err(VtfError::LimitExceeded {
            limit: VtfLimit::DecodedBytes,
            maximum: limits.max_decoded_bytes,
        });
    }
    let high_offset = header_size
        .checked_add(total.checked_sub(high.bytes).ok_or(VtfError::Overflow {
            field: "high-res mip offset",
        })?)
        .ok_or(VtfError::Overflow {
            field: "high-res mip offset",
        })?;
    let source =
        bytes
            .get(high_offset..high_offset + high.bytes)
            .ok_or(VtfError::TruncatedPayload {
                expected: total,
                available,
            })?;
    let mut pixels = Vec::with_capacity(high.bytes);
    let (source_pixels, remainder) = source.as_chunks::<4>();
    debug_assert!(remainder.is_empty());
    for pixel in source_pixels {
        match format {
            VtfHighResFormat::Rgba8888 => pixels.extend_from_slice(pixel),
            VtfHighResFormat::Bgra8888 => pixels.extend([pixel[2], pixel[1], pixel[0], pixel[3]]),
        }
    }
    Ok(VtfImage {
        width: header.width,
        height: header.height,
        pixels,
        source_format: format,
        mip_count: header.mip_count,
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
    high_res_format: i32,
    mip_count: u8,
    low_res_format: i32,
    low_res_width: u8,
    low_res_height: u8,
    depth: u16,
}

impl Header {
    fn read(bytes: &[u8]) -> Result<Self, VtfError> {
        if bytes.len() < MIN_HEADER_SIZE {
            return Err(VtfError::TruncatedHeader {
                available: bytes.len(),
            });
        }
        if bytes.get(..4) != Some(SIGNATURE.as_slice()) {
            return Err(VtfError::InvalidSignature);
        }
        Ok(Self {
            major: read_u32(bytes, 4)?,
            minor: read_u32(bytes, 8)?,
            size: read_u32(bytes, 12)?,
            width: read_u16(bytes, 16)?,
            height: read_u16(bytes, 18)?,
            frames: read_u16(bytes, 24)?,
            high_res_format: read_i32(bytes, 52)?,
            mip_count: *bytes.get(56).ok_or(VtfError::TruncatedHeader {
                available: bytes.len(),
            })?,
            low_res_format: read_i32(bytes, 57)?,
            low_res_width: *bytes.get(61).ok_or(VtfError::TruncatedHeader {
                available: bytes.len(),
            })?,
            low_res_height: *bytes.get(62).ok_or(VtfError::TruncatedHeader {
                available: bytes.len(),
            })?,
            depth: read_u16(bytes, 63)?,
        })
    }
    fn validate(self, limits: VtfLimits) -> Result<(), VtfError> {
        if self.major != 7 || self.minor != SUPPORTED_MINOR {
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
        if self.mip_count == 0 || self.mip_count > limits.max_mip_count {
            return Err(VtfError::InvalidMipCount {
                mip_count: self.mip_count,
            });
        }
        if self.low_res_format != IMAGE_FORMAT_NONE
            || self.low_res_width != 0
            || self.low_res_height != 0
        {
            return Err(VtfError::UnsupportedLowResImage);
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
struct MipLevel {
    bytes: usize,
}

fn mip_layout(width: u16, height: u16, count: u8) -> Result<Vec<MipLevel>, VtfError> {
    let mut levels = Vec::new();
    for level in 0..count {
        let shift = u32::from(level);
        let level_width = usize::from(width).checked_shr(shift).unwrap_or(0).max(1);
        let level_height = usize::from(height).checked_shr(shift).unwrap_or(0).max(1);
        let bytes = level_width
            .checked_mul(level_height)
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or(VtfError::Overflow {
                field: "mip level bytes",
            })?;
        levels.push(MipLevel { bytes });
    }
    levels.reverse();
    Ok(levels)
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, VtfError> {
    let array: [u8; 2] = bytes
        .get(offset..offset + 2)
        .ok_or(VtfError::TruncatedHeader {
            available: bytes.len(),
        })?
        .try_into()
        .expect("fixed slice length");
    Ok(u16::from_le_bytes(array))
}
fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, VtfError> {
    let array: [u8; 4] = bytes
        .get(offset..offset + 4)
        .ok_or(VtfError::TruncatedHeader {
            available: bytes.len(),
        })?
        .try_into()
        .expect("fixed slice length");
    Ok(u32::from_le_bytes(array))
}
fn read_i32(bytes: &[u8], offset: usize) -> Result<i32, VtfError> {
    let array: [u8; 4] = bytes
        .get(offset..offset + 4)
        .ok_or(VtfError::TruncatedHeader {
            available: bytes.len(),
        })?
        .try_into()
        .expect("fixed slice length");
    Ok(i32::from_le_bytes(array))
}

/// Decoder failure with explicit unsupported scope and bounds.
#[allow(
    missing_docs,
    reason = "Variant field names are self-describing and the variants document their semantics."
)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VtfError {
    /// File did not start with exact VTF zero signature.
    InvalidSignature,
    /// Bytes were too short for fixed Source 1 header fields.
    TruncatedHeader { available: usize },
    /// VTF version is outside verified 7.2 scope.
    UnsupportedVersion { major: u32, minor: u32 },
    /// Declared header size was too small or beyond input.
    InvalidHeaderSize { actual: u32 },
    /// Dimensions were zero or exceeded configured bounds.
    InvalidDimensions { width: u16, height: u16 },
    /// Multiple frames are unsupported.
    UnsupportedFrames { frames: u16 },
    /// Non-2D texture depth is unsupported.
    UnsupportedDepth { depth: u16 },
    /// Mip count was zero or exceeded configured bounds.
    InvalidMipCount { mip_count: u8 },
    /// Low-res thumbnail data is outside this narrow reader scope.
    UnsupportedLowResImage,
    /// High-res image format is outside RGBA8888/BGRA8888 subset.
    UnsupportedHighResFormat { format: i32 },
    /// Input payload did not contain full validated mip layout.
    TruncatedPayload { expected: usize, available: usize },
    /// Arithmetic overflow occurred while evaluating layout.
    Overflow { field: &'static str },
    /// Configured resource maximum was exceeded.
    LimitExceeded { limit: VtfLimit, maximum: usize },
}
impl fmt::Display for VtfError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl Error for VtfError {}

/// Resource controlled by VtfLimits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VtfLimit {
    /// Whole input bytes.
    InputBytes,
    /// Output RGBA8 bytes.
    DecodedBytes,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vtf(format: i32, pixels: &[u8]) -> Vec<u8> {
        let mut bytes = vec![0_u8; 80];
        bytes[..4].copy_from_slice(b"VTF\0");
        bytes[4..8].copy_from_slice(&7_u32.to_le_bytes());
        bytes[8..12].copy_from_slice(&2_u32.to_le_bytes());
        bytes[12..16].copy_from_slice(&80_u32.to_le_bytes());
        bytes[16..18].copy_from_slice(&2_u16.to_le_bytes());
        bytes[18..20].copy_from_slice(&1_u16.to_le_bytes());
        bytes[24..26].copy_from_slice(&1_u16.to_le_bytes());
        bytes[52..56].copy_from_slice(&format.to_le_bytes());
        bytes[56] = 1;
        bytes[57..61].copy_from_slice(&(-1_i32).to_le_bytes());
        bytes[63..65].copy_from_slice(&1_u16.to_le_bytes());
        bytes.extend_from_slice(pixels);
        bytes
    }

    #[test]
    fn decodes_rgba_and_bgra_headers() {
        let rgba = decode(&vtf(0, &[1, 2, 3, 4, 5, 6, 7, 8])).expect("RGBA");
        assert_eq!(rgba.pixels_rgba8(), [1, 2, 3, 4, 5, 6, 7, 8]);
        let bgra = decode(&vtf(12, &[3, 2, 1, 4, 7, 6, 5, 8])).expect("BGRA");
        assert_eq!(bgra.pixels_rgba8(), [1, 2, 3, 4, 5, 6, 7, 8]);
    }
    #[test]
    fn rejects_truncated_and_budgeted_data() {
        assert!(matches!(
            decode(b"VTF\0"),
            Err(VtfError::TruncatedHeader { .. })
        ));
        let error = decode_with_limits(
            &vtf(0, &[0; 8]),
            VtfLimits {
                max_decoded_bytes: 1,
                ..VtfLimits::default()
            },
        )
        .expect_err("budget");
        assert!(matches!(
            error,
            VtfError::LimitExceeded {
                limit: VtfLimit::DecodedBytes,
                ..
            }
        ));
    }
}
