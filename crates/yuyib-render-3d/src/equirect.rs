//! CPU HDR equirectangular environment ingest for M2 IBL.
//!
//! This module decodes Radiance `.hdr` (RGBE) bytes into linear RGB32F and
//! stores them as a lat-long equirect map. GGX prefilter lives in
//! [`crate::ggx_cook`]; this module only produces the source probe.

use std::{error::Error, fmt, io::Cursor};

use image::codecs::hdr::HdrDecoder;
use image::{ImageDecoder, ImageError};

/// Lat-long equirectangular environment in linear RGB32F.
///
/// Pixel layout is row-major, top-to-bottom, three `f32` channels per texel
/// (`width * height * 3`). Longitude `φ ∈ [0, 2π)` maps to `u`, latitude
/// `θ ∈ [0, π]` (0 = +Y) maps to `v`, matching the common glTF / Filament
/// equirect convention used by later cube projection.
#[derive(Clone, Debug, PartialEq)]
pub struct PreparedEquirectEnvironment3d {
    width: u32,
    height: u32,
    rgb: Vec<f32>,
}

/// Failure while ingesting an equirectangular HDR environment.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EquirectEnvironmentError {
    /// Encoded payload could not be decoded as Radiance HDR.
    Decode(String),
    /// Width or height was zero.
    EmptyDimensions,
    /// Width×height overflowed or exceeded the configured pixel budget.
    PixelBudget {
        /// Requested pixel count.
        pixels: u64,
        /// Allowed maximum.
        max_pixels: u64,
    },
    /// `rgb.len()` did not equal `width * height * 3`.
    InvalidRgbLength {
        /// Observed float count.
        got: usize,
        /// Expected float count.
        expected: usize,
    },
    /// RGB payload contained a non-finite channel.
    NonFiniteRgb,
}

impl fmt::Display for EquirectEnvironmentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Decode(message) => write!(formatter, "HDR equirect decode failed: {message}"),
            Self::EmptyDimensions => {
                formatter.write_str("HDR equirect width and height must be non-zero")
            }
            Self::PixelBudget { pixels, max_pixels } => write!(
                formatter,
                "HDR equirect pixel count {pixels} exceeds budget {max_pixels}"
            ),
            Self::InvalidRgbLength { got, expected } => write!(
                formatter,
                "HDR equirect RGB length {got} does not match width*height*3 = {expected}"
            ),
            Self::NonFiniteRgb => {
                formatter.write_str("HDR equirect RGB must contain only finite floats")
            }
        }
    }
}

impl Error for EquirectEnvironmentError {}

impl From<ImageError> for EquirectEnvironmentError {
    fn from(error: ImageError) -> Self {
        Self::Decode(error.to_string())
    }
}

impl PreparedEquirectEnvironment3d {
    /// Default pixel budget: 16k × 8k equirect (common sky probe upper bound).
    pub const DEFAULT_MAX_PIXELS: u64 = 16_384 * 8_192;

    /// Builds an environment from already-linear RGB32F rows.
    ///
    /// # Errors
    ///
    /// Returns [`EquirectEnvironmentError`] when dimensions, length or finiteness
    /// checks fail.
    pub fn from_linear_rgb_f32(
        width: u32,
        height: u32,
        rgb: Vec<f32>,
    ) -> Result<Self, EquirectEnvironmentError> {
        Self::from_linear_rgb_f32_with_budget(width, height, rgb, Self::DEFAULT_MAX_PIXELS)
    }

    /// Same as [`Self::from_linear_rgb_f32`] with an explicit pixel budget.
    ///
    /// # Errors
    ///
    /// Returns [`EquirectEnvironmentError`] on validation failure.
    pub fn from_linear_rgb_f32_with_budget(
        width: u32,
        height: u32,
        rgb: Vec<f32>,
        max_pixels: u64,
    ) -> Result<Self, EquirectEnvironmentError> {
        if width == 0 || height == 0 {
            return Err(EquirectEnvironmentError::EmptyDimensions);
        }
        let pixels = u64::from(width).saturating_mul(u64::from(height));
        if pixels == 0 || pixels > max_pixels {
            return Err(EquirectEnvironmentError::PixelBudget { pixels, max_pixels });
        }
        let expected = usize::try_from(pixels.saturating_mul(3)).unwrap_or(usize::MAX);
        if rgb.len() != expected {
            return Err(EquirectEnvironmentError::InvalidRgbLength {
                got: rgb.len(),
                expected,
            });
        }
        if rgb.iter().any(|channel| !channel.is_finite()) {
            return Err(EquirectEnvironmentError::NonFiniteRgb);
        }
        Ok(Self {
            width,
            height,
            rgb,
        })
    }

    /// Decodes Radiance `.hdr` / RGBE bytes into a linear equirect map.
    ///
    /// # Errors
    ///
    /// Returns [`EquirectEnvironmentError`] when decode or budget checks fail.
    pub fn from_radiance_hdr_bytes(bytes: &[u8]) -> Result<Self, EquirectEnvironmentError> {
        Self::from_radiance_hdr_bytes_with_budget(bytes, Self::DEFAULT_MAX_PIXELS)
    }

    /// Same as [`Self::from_radiance_hdr_bytes`] with an explicit pixel budget.
    ///
    /// # Errors
    ///
    /// Returns [`EquirectEnvironmentError`] when decode or budget checks fail.
    pub fn from_radiance_hdr_bytes_with_budget(
        bytes: &[u8],
        max_pixels: u64,
    ) -> Result<Self, EquirectEnvironmentError> {
        let decoder = HdrDecoder::new(Cursor::new(bytes))?;
        let (width, height) = decoder.dimensions();
        let pixels = u64::from(width).saturating_mul(u64::from(height));
        if width == 0 || height == 0 {
            return Err(EquirectEnvironmentError::EmptyDimensions);
        }
        if pixels == 0 || pixels > max_pixels {
            return Err(EquirectEnvironmentError::PixelBudget { pixels, max_pixels });
        }
        let byte_len = usize::try_from(decoder.total_bytes()).unwrap_or(usize::MAX);
        let mut raw = vec![0_u8; byte_len];
        decoder.read_image(&mut raw)?;
        if raw.len() % 4 != 0 {
            return Err(EquirectEnvironmentError::Decode(
                "HDR RGB32F payload is not a multiple of 4 bytes".into(),
            ));
        }
        let mut rgb = Vec::with_capacity(raw.len() / 4);
        for chunk in raw.chunks_exact(4) {
            rgb.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
        }
        Self::from_linear_rgb_f32_with_budget(width, height, rgb, max_pixels)
    }

    /// Returns the equirect width in texels.
    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// Returns the equirect height in texels.
    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// Returns tightly packed linear RGB32F samples.
    #[must_use]
    pub fn rgb(&self) -> &[f32] {
        &self.rgb
    }

    /// Builds a tiny synthetic outdoor equirect for unit/smoke tests.
    ///
    /// Upper hemisphere is a cool sky, lower hemisphere a warm ground bounce;
    /// the +Z lobe is brighter so later cube projection has an asymmetric cue.
    ///
    /// # Errors
    ///
    /// Returns [`EquirectEnvironmentError`] only if internal packing is wrong.
    pub fn synthetic_outdoor_probe() -> Result<Self, EquirectEnvironmentError> {
        const WIDTH: u32 = 64;
        const HEIGHT: u32 = 32;
        let mut rgb = Vec::with_capacity((WIDTH * HEIGHT * 3) as usize);
        for y in 0..HEIGHT {
            let v = (y as f32 + 0.5) / HEIGHT as f32;
            let theta = v * std::f32::consts::PI;
            for x in 0..WIDTH {
                let u = (x as f32 + 0.5) / WIDTH as f32;
                let phi = u * std::f32::consts::TAU;
                let dir = [
                    theta.sin() * phi.cos(),
                    theta.cos(),
                    theta.sin() * phi.sin(),
                ];
                let sky = dir[1].max(0.0);
                let ground = (-dir[1]).max(0.0);
                let sun_hint = (dir[2] * 0.5 + 0.5).clamp(0.0, 1.0);
                rgb.push(0.08 + sky * 0.55 + ground * 0.25 + sun_hint * 0.35);
                rgb.push(0.10 + sky * 0.70 + ground * 0.18 + sun_hint * 0.20);
                rgb.push(0.14 + sky * 0.95 + ground * 0.08 + sun_hint * 0.05);
            }
        }
        Self::from_linear_rgb_f32(WIDTH, HEIGHT, rgb)
    }

    /// Nearest-neighbour samples the equirect for a world-space unit direction.
    ///
    /// Non-finite or near-zero directions return `[0, 0, 0]`.
    #[must_use]
    pub fn sample_direction(&self, direction: [f32; 3]) -> [f32; 3] {
        let length_sq = direction[0].mul_add(
            direction[0],
            direction[1].mul_add(direction[1], direction[2] * direction[2]),
        );
        if !length_sq.is_finite() || length_sq <= f32::EPSILON {
            return [0.0; 3];
        }
        let inv = length_sq.sqrt().recip();
        let dir = [
            direction[0] * inv,
            direction[1] * inv,
            direction[2] * inv,
        ];
        let phi = dir[2].atan2(dir[0]);
        let u = (phi / std::f32::consts::TAU + 1.0).rem_euclid(1.0);
        let theta = dir[1].clamp(-1.0, 1.0).acos();
        let v = (theta / std::f32::consts::PI).clamp(0.0, 1.0);
        let x = ((u * self.width as f32) as u32).min(self.width.saturating_sub(1));
        let y = ((v * self.height as f32) as u32).min(self.height.saturating_sub(1));
        let index = ((y * self.width + x) * 3) as usize;
        [
            self.rgb[index],
            self.rgb[index + 1],
            self.rgb[index + 2],
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::{EquirectEnvironmentError, PreparedEquirectEnvironment3d};
    use image::Rgb;
    use image::codecs::hdr::HdrEncoder;

    #[test]
    fn synthetic_probe_has_brighter_upper_hemisphere() {
        let env = PreparedEquirectEnvironment3d::synthetic_outdoor_probe().expect("synthetic");
        assert_eq!(env.width(), 64);
        assert_eq!(env.height(), 32);
        let up = env.sample_direction([0.0, 1.0, 0.0]);
        let down = env.sample_direction([0.0, -1.0, 0.0]);
        let up_luma = 0.2126 * up[0] + 0.7152 * up[1] + 0.0722 * up[2];
        let down_luma = 0.2126 * down[0] + 0.7152 * down[1] + 0.0722 * down[2];
        assert!(
            up_luma > down_luma,
            "sky {up_luma} should exceed ground {down_luma}"
        );
    }

    #[test]
    fn radiance_hdr_bytes_roundtrip() {
        let pixels = [
            Rgb([1.25_f32, 0.5, 0.25]),
            Rgb([0.1, 0.2, 0.35]),
            Rgb([2.0, 1.5, 0.75]),
            Rgb([0.05, 0.05, 0.2]),
        ];
        let mut encoded = Vec::new();
        HdrEncoder::new(&mut encoded)
            .encode(&pixels, 2, 2)
            .expect("encode radiance HDR");
        let env = PreparedEquirectEnvironment3d::from_radiance_hdr_bytes(&encoded)
            .expect("decode radiance HDR");
        assert_eq!(env.width(), 2);
        assert_eq!(env.height(), 2);
        assert_eq!(env.rgb().len(), 12);
        // RGBE is lossy; allow a modest relative error on bright texels.
        assert!((env.rgb()[0] - 1.25).abs() < 0.08);
        assert!(env.sample_direction([0.0, 1.0, 0.0])[0].is_finite());
    }

    #[test]
    fn rejects_empty_dimensions() {
        let error = PreparedEquirectEnvironment3d::from_linear_rgb_f32(0, 4, vec![])
            .expect_err("empty width");
        assert_eq!(error, EquirectEnvironmentError::EmptyDimensions);
    }

    #[test]
    fn rejects_pixel_budget() {
        let error = PreparedEquirectEnvironment3d::from_linear_rgb_f32_with_budget(
            8,
            8,
            vec![0.0; 8 * 8 * 3],
            16,
        )
        .expect_err("over budget");
        assert!(matches!(
            error,
            EquirectEnvironmentError::PixelBudget { .. }
        ));
    }
}
