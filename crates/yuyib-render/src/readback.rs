//! GPU → CPU texture readback for smoke captures and diagnostics.
//!
//! Swapchain surfaces are not a portable `COPY_SRC` source. Capture must render
//! (or blit) into an ordinary texture created with
//! `TextureUsages::RENDER_ATTACHMENT | TextureUsages::COPY_SRC`, then call
//! [`read_texture_rgba8`]. Pair the returned pixels with
//! `yuyib_image::encode_png_rgba8` / `write_png_rgba8` for M1 reference
//! screenshots.

use std::{error::Error, fmt, num::NonZeroU32};

use wgpu::{
    BufferDescriptor, BufferUsages, CommandEncoderDescriptor, Device, Extent3d, MapMode, Queue,
    TexelCopyBufferInfo, TexelCopyBufferLayout, TexelCopyTextureInfo, Texture, TextureAspect,
    TextureFormat,
};

/// Minimum bytes-per-row alignment required by `copy_texture_to_buffer`.
pub const TEXTURE_READBACK_BYTES_PER_ROW_ALIGNMENT: u32 = 256;

/// Supported colour formats for tightly packed RGBA8 readback.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TextureReadbackFormat {
    /// `Rgba8Unorm` / `Rgba8UnormSrgb` — copied without channel swizzle.
    Rgba8,
    /// `Bgra8Unorm` / `Bgra8UnormSrgb` — swizzled to RGBA8 on the CPU.
    Bgra8,
}

impl TextureReadbackFormat {
    /// Maps a WGPU texture format to a readback contract.
    ///
    /// Returns `None` for formats that are not 8-bit 4-channel colour targets.
    #[must_use]
    pub const fn from_texture_format(format: TextureFormat) -> Option<Self> {
        match format {
            TextureFormat::Rgba8Unorm | TextureFormat::Rgba8UnormSrgb => Some(Self::Rgba8),
            TextureFormat::Bgra8Unorm | TextureFormat::Bgra8UnormSrgb => Some(Self::Bgra8),
            _ => None,
        }
    }

    const fn bytes_per_pixel(self) -> u32 {
        4
    }
}

/// Computes the padded `bytes_per_row` for a texture copy of `width` texels.
#[must_use]
pub const fn padded_bytes_per_row(width: u32, bytes_per_pixel: u32) -> u32 {
    let unpadded = width.saturating_mul(bytes_per_pixel);
    let align = TEXTURE_READBACK_BYTES_PER_ROW_ALIGNMENT;
    unpadded.saturating_add(align.saturating_sub(1)) & !(align.saturating_sub(1))
}

/// Copies one mip-0 2D colour texture into tightly packed row-major RGBA8.
///
/// The texture must have been created with `TextureUsages::COPY_SRC`. This call
/// submits its own command buffer and blocks until the map completes — suitable
/// for smoke/reference capture, not per-frame gameplay streaming.
///
/// # Errors
///
/// Returns [`TextureReadbackError`] for unsupported formats, empty sizes,
/// overflow, or GPU map failures.
pub fn read_texture_rgba8(
    device: &Device,
    queue: &Queue,
    texture: &Texture,
    width: u32,
    height: u32,
    format: TextureReadbackFormat,
) -> Result<Vec<u8>, TextureReadbackError> {
    if width == 0 || height == 0 {
        return Err(TextureReadbackError::EmptyDimensions { width, height });
    }
    let bytes_per_pixel = format.bytes_per_pixel();
    let padded_bpr = padded_bytes_per_row(width, bytes_per_pixel);
    let buffer_size = u64::from(padded_bpr)
        .checked_mul(u64::from(height))
        .ok_or(TextureReadbackError::BufferSizeOverflow { width, height })?;
    let output_len = usize::try_from(width)
        .ok()
        .and_then(|w| usize::try_from(height).ok().map(|h| (w, h)))
        .and_then(|(w, h)| w.checked_mul(h))
        .and_then(|texels| texels.checked_mul(usize::try_from(bytes_per_pixel).ok()?))
        .ok_or(TextureReadbackError::BufferSizeOverflow { width, height })?;

    let buffer = device.create_buffer(&BufferDescriptor {
        label: Some("yuyib texture readback"),
        size: buffer_size,
        usage: BufferUsages::COPY_DST | BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&CommandEncoderDescriptor {
        label: Some("yuyib texture readback encoder"),
    });
    encoder.copy_texture_to_buffer(
        TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: TextureAspect::All,
        },
        TexelCopyBufferInfo {
            buffer: &buffer,
            layout: TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded_bpr),
                rows_per_image: Some(height),
            },
        },
        Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(Some(encoder.finish()));

    let slice = buffer.slice(..);
    let (sender, receiver) = std::sync::mpsc::channel();
    slice.map_async(MapMode::Read, move |result| {
        let _ = sender.send(result);
    });
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .map_err(|source| TextureReadbackError::DevicePoll {
            message: source.to_string(),
        })?;
    receiver
        .recv()
        .map_err(|_| TextureReadbackError::MapChannelClosed)?
        .map_err(|source| TextureReadbackError::MapFailed { source })?;

    let mapped = slice
        .get_mapped_range()
        .map_err(|source| TextureReadbackError::MappedRange {
            message: source.to_string(),
        })?;
    let mut pixels = Vec::with_capacity(output_len);
    let unpadded = (width as usize).saturating_mul(bytes_per_pixel as usize);
    for row in 0..height as usize {
        let start = row.saturating_mul(padded_bpr as usize);
        let end = start.saturating_add(unpadded);
        let row_bytes =
            mapped
                .get(start..end)
                .ok_or(TextureReadbackError::MappedRangeTruncated {
                    start,
                    end,
                    mapped: mapped.len(),
                })?;
        match format {
            TextureReadbackFormat::Rgba8 => pixels.extend_from_slice(row_bytes),
            TextureReadbackFormat::Bgra8 => {
                for chunk in row_bytes.as_chunks::<4>().0 {
                    pixels.extend_from_slice(&[chunk[2], chunk[1], chunk[0], chunk[3]]);
                }
            }
        }
    }
    drop(mapped);
    buffer.unmap();
    Ok(pixels)
}

/// Failure while copying a GPU colour texture to tightly packed RGBA8.
#[derive(Debug)]
pub enum TextureReadbackError {
    /// Width or height was zero.
    EmptyDimensions {
        /// Requested width.
        width: u32,
        /// Requested height.
        height: u32,
    },
    /// Padded buffer size overflowed `u64` / `usize`.
    BufferSizeOverflow {
        /// Requested width.
        width: u32,
        /// Requested height.
        height: u32,
    },
    /// Device poll failed while waiting for the map.
    DevicePoll {
        /// Displayable poll failure.
        message: String,
    },
    /// The map-async completion channel closed unexpectedly.
    MapChannelClosed,
    /// WGPU rejected the buffer map.
    MapFailed {
        /// Underlying map failure.
        source: wgpu::BufferAsyncError,
    },
    /// WGPU rejected creating a mapped view of the buffer.
    MappedRange {
        /// Displayable map-range failure.
        message: String,
    },
    /// Mapped bytes were shorter than the padded layout required.
    MappedRangeTruncated {
        /// Inclusive start of the missing row slice.
        start: usize,
        /// Exclusive end of the missing row slice.
        end: usize,
        /// Observed mapped length.
        mapped: usize,
    },
}

impl fmt::Display for TextureReadbackError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyDimensions { width, height } => write!(
                formatter,
                "cannot read back empty texture dimensions {width}x{height}"
            ),
            Self::BufferSizeOverflow { width, height } => write!(
                formatter,
                "texture readback buffer for {width}x{height} overflows host size limits"
            ),
            Self::DevicePoll { message } => {
                write!(formatter, "texture readback device poll failed: {message}")
            }
            Self::MapChannelClosed => {
                formatter.write_str("texture readback map completion channel closed")
            }
            Self::MapFailed { .. } => formatter.write_str("texture readback buffer map failed"),
            Self::MappedRange { message } => {
                write!(formatter, "texture readback mapped range failed: {message}")
            }
            Self::MappedRangeTruncated { start, end, mapped } => write!(
                formatter,
                "texture readback mapped range {start}..{end} exceeds mapped length {mapped}"
            ),
        }
    }
}

impl Error for TextureReadbackError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::MapFailed { source } => Some(source),
            Self::EmptyDimensions { .. }
            | Self::BufferSizeOverflow { .. }
            | Self::DevicePoll { .. }
            | Self::MapChannelClosed
            | Self::MappedRange { .. }
            | Self::MappedRangeTruncated { .. } => None,
        }
    }
}

/// Validates that a non-zero extent fits in `u32` row packing helpers.
#[must_use]
pub fn non_zero_extent(width: u32, height: u32) -> Option<(NonZeroU32, NonZeroU32)> {
    Some((NonZeroU32::new(width)?, NonZeroU32::new(height)?))
}

#[cfg(test)]
mod tests {
    use super::{TEXTURE_READBACK_BYTES_PER_ROW_ALIGNMENT, padded_bytes_per_row};

    #[test]
    fn padded_bytes_per_row_obeys_copy_alignment() {
        assert_eq!(
            padded_bytes_per_row(1, 4),
            TEXTURE_READBACK_BYTES_PER_ROW_ALIGNMENT
        );
        assert_eq!(padded_bytes_per_row(64, 4), 256);
        assert_eq!(padded_bytes_per_row(65, 4), 512);
    }
}
