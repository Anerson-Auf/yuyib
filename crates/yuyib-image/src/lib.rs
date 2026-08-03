//! Safe, budgeted CPU decoding of common texture image formats.
//!
//! `yuyib-image` deliberately has no filesystem watcher, asynchronous runtime,
//! GPU upload code, or asset-store dependency. It validates untrusted encoded
//! bytes before decoding and normalises the result to RGBA8, ready for an asset
//! pipeline to store or upload. Encoding helpers exist for smoke/reference
//! screenshots and cooker diagnostics — not as a full authoring image editor.
//!
//! # Example
//!
//! ```no_run
//! use yuyib_image::{decode_path, encode_png_rgba8, DecodePolicy, ImageFormatPolicy};
//!
//! let policy = DecodePolicy::default()
//!     .with_formats(ImageFormatPolicy::PNG | ImageFormatPolicy::WEBP)
//!     .with_max_rgba_bytes(16 * 1024 * 1024);
//! let image = decode_path("assets/player.png", policy)?;
//! assert_eq!(image.texture().size().width(), 64);
//! let png = encode_png_rgba8(
//!     image.texture().size().width(),
//!     image.texture().size().height(),
//!     image.pixels(),
//! )?;
//! assert!(!png.is_empty());
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! # Limits and caveats
//!
//! Dimensions and the final RGBA8 byte size are checked from metadata before
//! calling the decoder. This avoids ordinary decompression bombs, but image
//! codecs may need temporary working memory while decoding. Configure budgets
//! conservatively for content received from an untrusted source.

#![forbid(unsafe_code)]

use std::{
    error::Error,
    fmt, fs,
    io::Cursor,
    path::{Path, PathBuf},
};

use image::{
    ExtendedColorType, ImageEncoder, ImageFormat as BackendImageFormat, ImageReader,
    codecs::png::PngEncoder,
};
use yuyib_2d::{Texture, TextureAlphaMode, TextureSize};

/// An RGBA8 texture decoded on the CPU.
///
/// Pixels are row-major, top-to-bottom, with four bytes per pixel in RGBA
/// order. The pixel vector is exactly `width * height * 4` bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecodedImage {
    texture: Texture,
    pixels: Vec<u8>,
    source_format: ImageFormat,
}

impl DecodedImage {
    /// Returns the texture metadata inferred during decoding.
    #[must_use]
    pub const fn texture(&self) -> &Texture {
        &self.texture
    }

    /// Returns the decoded RGBA8 pixels in row-major order.
    #[must_use]
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// Consumes the image and returns its RGBA8 pixel allocation.
    #[must_use]
    pub fn into_pixels(self) -> Vec<u8> {
        self.pixels
    }

    /// Returns the format detected from encoded image bytes.
    #[must_use]
    pub const fn source_format(&self) -> ImageFormat {
        self.source_format
    }
}

/// An encoded image format supported by this importer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImageFormat {
    /// Portable Network Graphics.
    Png,
    /// Joint Photographic Experts Group image data.
    Jpeg,
    /// WebP image data.
    WebP,
}

impl ImageFormat {
    fn from_backend(format: BackendImageFormat) -> Option<Self> {
        match format {
            BackendImageFormat::Png => Some(Self::Png),
            BackendImageFormat::Jpeg => Some(Self::Jpeg),
            BackendImageFormat::WebP => Some(Self::WebP),
            _ => None,
        }
    }
}

impl fmt::Display for ImageFormat {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Png => "PNG",
            Self::Jpeg => "JPEG",
            Self::WebP => "WebP",
        })
    }
}

/// A compact allow-list of formats accepted by [`DecodePolicy`].
///
/// Combine constants with `|`, for example
/// `ImageFormatPolicy::PNG | ImageFormatPolicy::WEBP`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ImageFormatPolicy(u8);

impl ImageFormatPolicy {
    /// Allows no encoded format.
    pub const NONE: Self = Self(0);
    /// Allows PNG images.
    pub const PNG: Self = Self(1 << 0);
    /// Allows JPEG images.
    pub const JPEG: Self = Self(1 << 1);
    /// Allows WebP images.
    pub const WEBP: Self = Self(1 << 2);
    /// Allows every format compiled into this importer.
    pub const ALL: Self = Self(Self::PNG.0 | Self::JPEG.0 | Self::WEBP.0);

    /// Returns whether this policy allows `format`.
    #[must_use]
    pub const fn allows(self, format: ImageFormat) -> bool {
        let bit = match format {
            ImageFormat::Png => Self::PNG.0,
            ImageFormat::Jpeg => Self::JPEG.0,
            ImageFormat::WebP => Self::WEBP.0,
        };
        self.0 & bit != 0
    }
}

impl std::ops::BitOr for ImageFormatPolicy {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

/// Resource limits and accepted formats for a decode request.
///
/// The default limits fit typical game and application textures while
/// preventing accidental multi-gigabyte allocations: 64 MiB encoded input,
/// 8,192 pixels per dimension, 64 million pixels, and 256 MiB RGBA8 output.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DecodePolicy {
    formats: ImageFormatPolicy,
    max_encoded_bytes: usize,
    max_width: u32,
    max_height: u32,
    max_pixels: u64,
    max_rgba_bytes: usize,
}

impl DecodePolicy {
    /// Creates a policy with explicit limits.
    ///
    /// No field is silently clamped. Passing zero for a dimension, pixel, or
    /// byte budget intentionally rejects every image that needs that budget.
    #[must_use]
    pub const fn new(
        formats: ImageFormatPolicy,
        max_encoded_bytes: usize,
        max_width: u32,
        max_height: u32,
        max_pixels: u64,
        max_rgba_bytes: usize,
    ) -> Self {
        Self {
            formats,
            max_encoded_bytes,
            max_width,
            max_height,
            max_pixels,
            max_rgba_bytes,
        }
    }

    /// Replaces the accepted-format allow-list.
    #[must_use]
    pub const fn with_formats(mut self, formats: ImageFormatPolicy) -> Self {
        self.formats = formats;
        self
    }

    /// Replaces the encoded-input byte limit.
    #[must_use]
    pub const fn with_max_encoded_bytes(mut self, max_encoded_bytes: usize) -> Self {
        self.max_encoded_bytes = max_encoded_bytes;
        self
    }

    /// Replaces the maximum accepted width.
    #[must_use]
    pub const fn with_max_width(mut self, max_width: u32) -> Self {
        self.max_width = max_width;
        self
    }

    /// Replaces the maximum accepted height.
    #[must_use]
    pub const fn with_max_height(mut self, max_height: u32) -> Self {
        self.max_height = max_height;
        self
    }

    /// Replaces the maximum accepted pixel count.
    #[must_use]
    pub const fn with_max_pixels(mut self, max_pixels: u64) -> Self {
        self.max_pixels = max_pixels;
        self
    }

    /// Replaces the maximum output RGBA8 allocation size.
    #[must_use]
    pub const fn with_max_rgba_bytes(mut self, max_rgba_bytes: usize) -> Self {
        self.max_rgba_bytes = max_rgba_bytes;
        self
    }

    /// Returns the accepted-format allow-list.
    #[must_use]
    pub const fn formats(self) -> ImageFormatPolicy {
        self.formats
    }
}

impl Default for DecodePolicy {
    fn default() -> Self {
        Self::new(
            ImageFormatPolicy::ALL,
            64 * 1024 * 1024,
            8_192,
            8_192,
            64 * 1024 * 1024,
            256 * 1024 * 1024,
        )
    }
}

/// Encodes tightly packed RGBA8 pixels as a PNG byte stream.
///
/// This is the CPU half of M1 reference-screenshot / smoke capture. Pair it
/// with a GPU texture readback that returns row-major RGBA8. It does **not**
/// recolour, resize, or convert colour spaces.
///
/// # Errors
///
/// Returns [`ImageEncodeError`] when dimensions are zero, the pixel buffer
/// length does not match `width * height * 4`, or the PNG encoder fails.
pub fn encode_png_rgba8(
    width: u32,
    height: u32,
    pixels: &[u8],
) -> Result<Vec<u8>, ImageEncodeError> {
    if width == 0 || height == 0 {
        return Err(ImageEncodeError::EmptyDimensions { width, height });
    }
    let expected = usize::try_from(width)
        .ok()
        .and_then(|w| usize::try_from(height).ok().map(|h| (w, h)))
        .and_then(|(w, h)| w.checked_mul(h))
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or(ImageEncodeError::DimensionsOverflow { width, height })?;
    if pixels.len() != expected {
        return Err(ImageEncodeError::PixelLengthMismatch {
            expected,
            actual: pixels.len(),
            width,
            height,
        });
    }
    let mut bytes = Vec::new();
    PngEncoder::new(&mut bytes)
        .write_image(pixels, width, height, ExtendedColorType::Rgba8)
        .map_err(|source| ImageEncodeError::Encode { source })?;
    Ok(bytes)
}

/// Rec.709 luminance for one linearised sRGB channel triple in `0.0..=1.0`.
#[must_use]
#[inline]
pub fn rgba8_luminance(r: f32, g: f32, b: f32) -> f32 {
    0.2126_f32.mul_add(r, 0.7152_f32.mul_add(g, 0.0722_f32 * b))
}

/// Compresses opaque (`alpha > 0`) RGBA8 luminance toward the mean.
///
/// For each opaque pixel:
/// - `L = 0.2126 R + 0.7152 G + 0.0722 B` (channels in `0..1`)
/// - `L' = mean + (L - mean) * contrast`
/// - `rgb' = rgb * (L' / max(L, ε))`, clamped to `0..255`
///
/// Transparent pixels (`alpha == 0`) and alpha channels are left unchanged.
/// An empty opaque set is a no-op. Typical `contrast` for softening baked
/// lighting islands in diffuse albedo is around `0.40`.
///
/// # Panics
///
/// Panics when `pixels.len()` is not a multiple of four.
pub fn compress_rgba8_luminance(pixels: &mut [u8], contrast: f32) {
    assert!(
        pixels.len() % 4 == 0,
        "RGBA8 luminance compress requires a multiple of 4 bytes, got {}",
        pixels.len()
    );

    let mut luminance_sum = 0.0_f32;
    let mut opaque_count = 0_usize;
    for pixel in pixels.chunks_exact(4) {
        if pixel[3] == 0 {
            continue;
        }
        let r = f32::from(pixel[0]) / 255.0;
        let g = f32::from(pixel[1]) / 255.0;
        let b = f32::from(pixel[2]) / 255.0;
        luminance_sum += rgba8_luminance(r, g, b);
        opaque_count += 1;
    }
    if opaque_count == 0 {
        return;
    }
    let mean = luminance_sum / opaque_count as f32;
    const EPSILON: f32 = 1.0e-6;

    for pixel in pixels.chunks_exact_mut(4) {
        if pixel[3] == 0 {
            continue;
        }
        let r = f32::from(pixel[0]) / 255.0;
        let g = f32::from(pixel[1]) / 255.0;
        let b = f32::from(pixel[2]) / 255.0;
        let luminance = rgba8_luminance(r, g, b);
        let compressed = mean + (luminance - mean) * contrast;
        let scale = compressed / luminance.max(EPSILON);
        pixel[0] = (r * scale * 255.0).round().clamp(0.0, 255.0) as u8;
        pixel[1] = (g * scale * 255.0).round().clamp(0.0, 255.0) as u8;
        pixel[2] = (b * scale * 255.0).round().clamp(0.0, 255.0) as u8;
    }
}

/// Writes tightly packed RGBA8 pixels to a PNG file.
///
/// # Errors
///
/// Returns [`ImageEncodeError`] for the same validation failures as
/// [`encode_png_rgba8`], or when the destination file cannot be written.
pub fn write_png_rgba8(
    path: impl AsRef<Path>,
    width: u32,
    height: u32,
    pixels: &[u8],
) -> Result<(), ImageEncodeError> {
    let bytes = encode_png_rgba8(width, height, pixels)?;
    fs::write(path.as_ref(), bytes).map_err(|source| ImageEncodeError::WriteFile {
        path: path.as_ref().to_path_buf(),
        source,
    })
}

/// Soft, driver-tolerant metrics for an RGBA8 reference screenshot.
///
/// Luminance uses the Rec. 709 RGB coefficients over the captured byte samples.
/// The histogram contains non-clear pixels in the ranges `[0.00, 0.25)`,
/// `[0.25, 0.50)`, `[0.50, 0.75)`, and `[0.75, 1.00]`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rgba8ReferenceMetrics {
    /// Pixels whose largest RGBA channel delta from the clear colour exceeds
    /// the configured threshold.
    pub non_clear_pixels: usize,
    /// Mean RGB luminance of the non-clear pixels, normalized to `0.0..=1.0`.
    ///
    /// This is zero when no pixel differs from the clear colour.
    pub mean_non_clear_luminance: f32,
    /// Four-bin brightness histogram for the non-clear pixels.
    pub non_clear_brightness_histogram: [usize; 4],
}

/// Computes soft reference-screenshot metrics from tightly packed RGBA8 pixels.
///
/// `clear` is the expected RGBA8 clear colour. A pixel is non-clear when its
/// largest channel delta exceeds `non_clear_threshold`. This is intended for
/// smoke assertions that tolerate ordinary GPU and driver variance, not
/// pixel-perfect golden-image comparison.
///
/// # Panics
///
/// Panics when `pixels` is not tightly packed RGBA8 data.
#[must_use]
pub fn reference_metrics_rgba8(
    pixels: &[u8],
    clear: [u8; 4],
    non_clear_threshold: u8,
) -> Rgba8ReferenceMetrics {
    assert!(
        pixels.len() % 4 == 0,
        "RGBA8 metrics require a pixel buffer whose length is divisible by four"
    );

    let mut non_clear_pixels = 0_usize;
    let mut total_luminance = 0.0_f32;
    let mut non_clear_brightness_histogram = [0_usize; 4];

    for pixel in pixels.chunks_exact(4) {
        let max_delta = pixel
            .iter()
            .zip(clear)
            .map(|(sample, expected)| sample.abs_diff(expected))
            .max()
            .unwrap_or(0);
        if max_delta <= non_clear_threshold {
            continue;
        }

        let luminance = (0.2126 * f32::from(pixel[0])
            + 0.7152 * f32::from(pixel[1])
            + 0.0722 * f32::from(pixel[2]))
            / 255.0;
        non_clear_pixels += 1;
        total_luminance += luminance;
        let bin = (luminance * 4.0).floor() as usize;
        non_clear_brightness_histogram[bin.min(3)] += 1;
    }

    Rgba8ReferenceMetrics {
        non_clear_pixels,
        mean_non_clear_luminance: if non_clear_pixels == 0 {
            0.0
        } else {
            total_luminance / non_clear_pixels as f32
        },
        non_clear_brightness_histogram,
    }
}

/// Failure while encoding CPU RGBA8 pixels to PNG.
#[derive(Debug)]
pub enum ImageEncodeError {
    /// Width or height was zero.
    EmptyDimensions {
        /// Requested width.
        width: u32,
        /// Requested height.
        height: u32,
    },
    /// `width * height * 4` overflowed `usize`.
    DimensionsOverflow {
        /// Requested width.
        width: u32,
        /// Requested height.
        height: u32,
    },
    /// Pixel buffer length did not match the requested dimensions.
    PixelLengthMismatch {
        /// Expected byte count.
        expected: usize,
        /// Observed byte count.
        actual: usize,
        /// Requested width.
        width: u32,
        /// Requested height.
        height: u32,
    },
    /// The PNG codec rejected the pixel stream.
    Encode {
        /// Underlying codec failure.
        source: image::ImageError,
    },
    /// The destination path could not be written.
    WriteFile {
        /// Destination path.
        path: PathBuf,
        /// Underlying I/O failure.
        source: std::io::Error,
    },
}

impl fmt::Display for ImageEncodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyDimensions { width, height } => write!(
                formatter,
                "cannot encode PNG with empty dimensions {width}x{height}"
            ),
            Self::DimensionsOverflow { width, height } => write!(
                formatter,
                "PNG dimensions {width}x{height} overflow host address size"
            ),
            Self::PixelLengthMismatch {
                expected,
                actual,
                width,
                height,
            } => write!(
                formatter,
                "RGBA8 buffer is {actual} bytes for {width}x{height}; expected {expected}"
            ),
            Self::Encode { .. } => formatter.write_str("PNG encoder failed"),
            Self::WriteFile { path, .. } => {
                write!(formatter, "could not write PNG file {}", path.display())
            }
        }
    }
}

impl Error for ImageEncodeError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Encode { source } => Some(source),
            Self::WriteFile { source, .. } => Some(source),
            Self::EmptyDimensions { .. }
            | Self::DimensionsOverflow { .. }
            | Self::PixelLengthMismatch { .. } => None,
        }
    }
}

/// Decodes an image file after reading it into the policy's encoded-input
/// budget.
///
/// Use [`decode_bytes`] when the asset layer already owns the encoded bytes or
/// when loading from an archive, network stream, or virtual filesystem.
///
/// # Errors
///
/// Returns [`ImageImportError::ReadFile`] for filesystem failures, otherwise
/// the same validation and decode errors as [`decode_bytes`].
pub fn decode_path(
    path: impl AsRef<Path>,
    policy: DecodePolicy,
) -> Result<DecodedImage, ImageImportError> {
    let path = path.as_ref();
    let bytes = fs::read(path).map_err(|source| ImageImportError::ReadFile {
        path: path.to_path_buf(),
        source,
    })?;
    decode_bytes(&bytes, policy)
}

/// Decodes encoded PNG, JPEG, or WebP bytes into a validated RGBA8 image.
///
/// The input limit is checked before metadata parsing. Dimensions, pixel count,
/// and final RGBA8 bytes are checked after reading headers but before asking the
/// codec to decode pixels.
///
/// # Errors
///
/// Returns a structured [`ImageImportError`] describing the rejected budget,
/// format policy, header parsing failure, or codec failure.
pub fn decode_bytes(bytes: &[u8], policy: DecodePolicy) -> Result<DecodedImage, ImageImportError> {
    if bytes.len() > policy.max_encoded_bytes {
        return Err(ImageImportError::EncodedBytesExceeded {
            actual: bytes.len(),
            maximum: policy.max_encoded_bytes,
        });
    }

    let reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(ImageImportError::ReadFormatHint)?;
    let backend_format = reader.format().ok_or(ImageImportError::UnknownFormat)?;
    let source_format = ImageFormat::from_backend(backend_format)
        .ok_or(ImageImportError::UnsupportedFormat { backend_format })?;
    if !policy.formats.allows(source_format) {
        return Err(ImageImportError::DisallowedFormat {
            format: source_format,
            allowed: policy.formats,
        });
    }

    let (width, height) = reader
        .into_dimensions()
        .map_err(ImageImportError::ReadMetadata)?;
    validate_dimensions(width, height, policy)?;

    // Build a fresh reader because `into_dimensions` consumes the first one.
    let decoded = ImageReader::with_format(Cursor::new(bytes), backend_format)
        .decode()
        .map_err(ImageImportError::Decode)?;
    let pixels = decoded.into_rgba8().into_raw();
    let size = TextureSize::new(width, height)
        .map_err(|_| ImageImportError::InvalidDimensions { width, height })?;
    let alpha_mode = match source_format {
        ImageFormat::Jpeg => TextureAlphaMode::Opaque,
        ImageFormat::Png | ImageFormat::WebP => TextureAlphaMode::Straight,
    };
    Ok(DecodedImage {
        texture: Texture::new(size).with_alpha_mode(alpha_mode),
        pixels,
        source_format,
    })
}

fn validate_dimensions(
    width: u32,
    height: u32,
    policy: DecodePolicy,
) -> Result<(), ImageImportError> {
    if width == 0 || height == 0 {
        return Err(ImageImportError::InvalidDimensions { width, height });
    }
    if width > policy.max_width || height > policy.max_height {
        return Err(ImageImportError::DimensionsExceeded {
            width,
            height,
            max_width: policy.max_width,
            max_height: policy.max_height,
        });
    }
    let pixels = u64::from(width) * u64::from(height);
    if pixels > policy.max_pixels {
        return Err(ImageImportError::PixelsExceeded {
            width,
            height,
            actual: pixels,
            maximum: policy.max_pixels,
        });
    }
    let rgba_bytes = pixels
        .checked_mul(4)
        .and_then(|value| usize::try_from(value).ok())
        .ok_or(ImageImportError::RgbaByteSizeOverflow { width, height })?;
    if rgba_bytes > policy.max_rgba_bytes {
        return Err(ImageImportError::RgbaBytesExceeded {
            width,
            height,
            actual: rgba_bytes,
            maximum: policy.max_rgba_bytes,
        });
    }
    Ok(())
}

/// A filesystem, format policy, budget validation, or codec error.
#[derive(Debug)]
pub enum ImageImportError {
    /// The encoded image file could not be read.
    ReadFile {
        /// Requested source path.
        path: PathBuf,
        /// Underlying filesystem error.
        source: std::io::Error,
    },
    /// Encoded input already exceeds its configured byte budget.
    EncodedBytesExceeded {
        /// Observed encoded byte count.
        actual: usize,
        /// Maximum allowed encoded byte count.
        maximum: usize,
    },
    /// The image format could not be identified from its bytes.
    UnknownFormat,
    /// The encoded stream could not be inspected to identify its format.
    ReadFormatHint(std::io::Error),
    /// The identified backend format is not enabled by this crate.
    UnsupportedFormat {
        /// Format reported by the image backend.
        backend_format: BackendImageFormat,
    },
    /// A supported format is forbidden by the request policy.
    DisallowedFormat {
        /// Detected input format.
        format: ImageFormat,
        /// Formats allowed by the request policy.
        allowed: ImageFormatPolicy,
    },
    /// The image header could not be parsed safely.
    ReadMetadata(image::ImageError),
    /// Image metadata declared an invalid empty dimension.
    InvalidDimensions {
        /// Reported width.
        width: u32,
        /// Reported height.
        height: u32,
    },
    /// Width or height exceeds its configured maximum.
    DimensionsExceeded {
        /// Reported width.
        width: u32,
        /// Reported height.
        height: u32,
        /// Maximum allowed width.
        max_width: u32,
        /// Maximum allowed height.
        max_height: u32,
    },
    /// The width multiplied by height exceeds the pixel budget.
    PixelsExceeded {
        /// Reported width.
        width: u32,
        /// Reported height.
        height: u32,
        /// Observed pixel count.
        actual: u64,
        /// Maximum allowed pixel count.
        maximum: u64,
    },
    /// The final RGBA8 size cannot be represented as a `usize`.
    RgbaByteSizeOverflow {
        /// Reported width.
        width: u32,
        /// Reported height.
        height: u32,
    },
    /// The final RGBA8 allocation would exceed the request's byte budget.
    RgbaBytesExceeded {
        /// Reported width.
        width: u32,
        /// Reported height.
        height: u32,
        /// Required output byte count.
        actual: usize,
        /// Maximum allowed output byte count.
        maximum: usize,
    },
    /// The image codec failed while decoding pixel data.
    Decode(image::ImageError),
}

impl fmt::Display for ImageImportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReadFile { path, .. } => {
                write!(formatter, "could not read image file `{}`", path.display())
            }
            Self::EncodedBytesExceeded { actual, maximum } => write!(
                formatter,
                "encoded image is {actual} bytes; limit is {maximum}"
            ),
            Self::UnknownFormat => formatter.write_str("image format could not be identified"),
            Self::ReadFormatHint(_) => formatter.write_str("could not inspect image format"),
            Self::UnsupportedFormat { backend_format } => write!(
                formatter,
                "image format {backend_format:?} is not supported by this importer"
            ),
            Self::DisallowedFormat { format, .. } => {
                write!(formatter, "{format} is disallowed by the decode policy")
            }
            Self::ReadMetadata(_) => formatter.write_str("could not read image metadata"),
            Self::InvalidDimensions { width, height } => write!(
                formatter,
                "image dimensions must be non-zero, got {width}x{height}"
            ),
            Self::DimensionsExceeded {
                width,
                height,
                max_width,
                max_height,
            } => write!(
                formatter,
                "image dimensions {width}x{height} exceed limit {max_width}x{max_height}"
            ),
            Self::PixelsExceeded {
                actual, maximum, ..
            } => write!(formatter, "image has {actual} pixels; limit is {maximum}"),
            Self::RgbaByteSizeOverflow { width, height } => write!(
                formatter,
                "RGBA8 byte size overflows for {width}x{height} image"
            ),
            Self::RgbaBytesExceeded {
                actual, maximum, ..
            } => write!(
                formatter,
                "RGBA8 image needs {actual} bytes; limit is {maximum}"
            ),
            Self::Decode(_) => formatter.write_str("could not decode image pixels"),
        }
    }
}

impl Error for ImageImportError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::ReadFile { source, .. } | Self::ReadFormatHint(source) => Some(source),
            Self::ReadMetadata(source) | Self::Decode(source) => Some(source),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png_bytes(width: u32, height: u32) -> Vec<u8> {
        encode_png_rgba8(
            width,
            height,
            &[10, 20, 30, 40].repeat((width * height) as usize),
        )
        .expect("test PNG encoding succeeds")
    }

    #[test]
    fn encode_png_round_trips_through_decode() {
        let pixels = [1_u8, 2, 3, 255, 4, 5, 6, 255];
        let encoded = encode_png_rgba8(2, 1, &pixels).expect("encode");
        let decoded = decode_bytes(&encoded, DecodePolicy::default()).expect("decode");
        assert_eq!(decoded.pixels(), pixels);
    }

    #[test]
    fn compress_luminance_pulls_bright_and_dark_toward_mean() {
        let mut pixels = [
            255, 255, 255, 255, // bright
            10, 10, 10, 255, // dark
            0, 0, 0, 0, // transparent — ignored
        ];
        let bright_l = rgba8_luminance(1.0, 1.0, 1.0);
        let dark_l = rgba8_luminance(10.0 / 255.0, 10.0 / 255.0, 10.0 / 255.0);
        let mean = (bright_l + dark_l) * 0.5;
        let contrast = 0.40_f32;

        compress_rgba8_luminance(&mut pixels, contrast);

        let after_bright = rgba8_luminance(
            f32::from(pixels[0]) / 255.0,
            f32::from(pixels[1]) / 255.0,
            f32::from(pixels[2]) / 255.0,
        );
        let after_dark = rgba8_luminance(
            f32::from(pixels[4]) / 255.0,
            f32::from(pixels[5]) / 255.0,
            f32::from(pixels[6]) / 255.0,
        );
        let expected_bright = mean + (bright_l - mean) * contrast;
        let expected_dark = mean + (dark_l - mean) * contrast;

        assert!((after_bright - expected_bright).abs() < 0.01);
        assert!((after_dark - expected_dark).abs() < 0.01);
        assert!(after_bright < bright_l);
        assert!(after_dark > dark_l);
        assert_eq!(&pixels[8..12], &[0, 0, 0, 0]);
    }

    #[test]
    fn encode_png_rejects_length_mismatch() {
        let error = encode_png_rgba8(2, 2, &[0; 4]).expect_err("length must match");
        assert!(matches!(
            error,
            ImageEncodeError::PixelLengthMismatch {
                expected: 16,
                actual: 4,
                ..
            }
        ));
    }

    #[test]
    fn reference_metrics_exclude_clear_pixels_and_bin_luminance() {
        let metrics = reference_metrics_rgba8(
            &[
                10, 20, 30, 255, // clear
                0, 0, 0, 255, // bin 0
                128, 128, 128, 255, // bin 2
                255, 255, 255, 255, // bin 3
            ],
            [10, 20, 30, 255],
            8,
        );

        assert_eq!(metrics.non_clear_pixels, 3);
        assert_eq!(metrics.non_clear_brightness_histogram, [1, 0, 1, 1]);
        assert!((metrics.mean_non_clear_luminance - 0.500_65).abs() < 0.000_01);
    }

    #[test]
    #[should_panic(expected = "RGBA8 metrics require")]
    fn reference_metrics_reject_non_rgba8_input() {
        let _ = reference_metrics_rgba8(&[0, 0, 0], [0, 0, 0, 255], 0);
    }

    #[test]
    fn decodes_png_as_rgba8_texture() {
        let decoded = decode_bytes(&png_bytes(2, 3), DecodePolicy::default()).expect("valid PNG");

        assert_eq!(decoded.source_format(), ImageFormat::Png);
        assert_eq!(decoded.texture().size().width(), 2);
        assert_eq!(decoded.texture().size().height(), 3);
        assert_eq!(decoded.texture().alpha_mode(), TextureAlphaMode::Straight);
        assert_eq!(decoded.pixels(), [10, 20, 30, 40].repeat(6));
    }

    #[test]
    fn format_policy_is_checked_before_pixel_decode() {
        let error = decode_bytes(
            &png_bytes(1, 1),
            DecodePolicy::default().with_formats(ImageFormatPolicy::JPEG),
        )
        .expect_err("PNG is explicitly forbidden");

        assert!(matches!(
            error,
            ImageImportError::DisallowedFormat {
                format: ImageFormat::Png,
                ..
            }
        ));
    }

    #[test]
    fn validates_dimensions_before_decode() {
        let error = decode_bytes(&png_bytes(3, 2), DecodePolicy::default().with_max_width(2))
            .expect_err("header width exceeds budget");

        assert!(matches!(error, ImageImportError::DimensionsExceeded { .. }));
    }

    #[test]
    fn encoded_budget_is_checked_before_header_parsing() {
        let error = decode_bytes(&[0; 9], DecodePolicy::default().with_max_encoded_bytes(8))
            .expect_err("input is too large");

        assert_eq!(error.to_string(), "encoded image is 9 bytes; limit is 8");
    }
}
