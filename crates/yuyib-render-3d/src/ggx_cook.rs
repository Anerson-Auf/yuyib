//! CPU GGX specular prefilter: equirect → [`PreparedSpecularIbl3d`].
//!
//! Implements the Karis / split-sum importance-sample cook used by Filament and
//! LearnOpenGL. Output is LDR RGBA8 cube mips + BRDF LUT so it plugs into the
//! existing [`crate::GpuSpecularIbl3d`] upload path. This is an offline/CPU
//! cook — not a runtime GPU compute pass.

use crate::equirect::PreparedEquirectEnvironment3d;
use crate::ibl::{
    PreparedSpecularIbl3d, SpecularIblError, hammersley,
    importance_sample_ggx, integrate_brdf_lut_rgba8,
};

use std::{error::Error, fmt};

/// Failure while cooking a GGX prefiltered specular environment.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GgxCookError {
    /// Face edge must be a positive power of two.
    InvalidFaceSize,
    /// Importance-sample count must be >= 1.
    InvalidSampleCount,
    /// LUT edge must be >= 2.
    InvalidLutSize,
    /// Packed result failed [`PreparedSpecularIbl3d`] validation.
    Specular(SpecularIblError),
}

impl fmt::Display for GgxCookError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidFaceSize => {
                formatter.write_str("GGX cook face size must be a positive power of two")
            }
            Self::InvalidSampleCount => {
                formatter.write_str("GGX cook sample count must be >= 1")
            }
            Self::InvalidLutSize => formatter.write_str("GGX cook BRDF LUT size must be >= 2"),
            Self::Specular(error) => write!(formatter, "GGX cook specular pack failed: {error}"),
        }
    }
}

impl Error for GgxCookError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Specular(error) => Some(error),
            _ => None,
        }
    }
}

impl From<SpecularIblError> for GgxCookError {
    fn from(error: SpecularIblError) -> Self {
        Self::Specular(error)
    }
}

/// Options for [`cook_ggx_specular_ibl`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GgxCookConfig {
    face_size: u32,
    sample_count: u32,
    lut_size: u32,
}

impl GgxCookConfig {
    /// Smoke/default cook: 16² faces, 32 GGX samples, 64² LUT.
    #[must_use]
    pub const fn smoke() -> Self {
        Self {
            face_size: 16,
            sample_count: 32,
            lut_size: 64,
        }
    }

    /// Higher-quality CPU cook for interactive probe labs.
    #[must_use]
    pub const fn quality() -> Self {
        Self {
            face_size: 32,
            sample_count: 64,
            lut_size: 64,
        }
    }

    /// Creates an explicit cook configuration.
    #[must_use]
    pub const fn new(face_size: u32, sample_count: u32, lut_size: u32) -> Self {
        Self {
            face_size,
            sample_count,
            lut_size,
        }
    }

    /// Face edge length at mip zero.
    #[must_use]
    pub const fn face_size(self) -> u32 {
        self.face_size
    }

    /// GGX importance-sample count per texel.
    #[must_use]
    pub const fn sample_count(self) -> u32 {
        self.sample_count
    }

    /// Split-sum BRDF LUT edge length.
    #[must_use]
    pub const fn lut_size(self) -> u32 {
        self.lut_size
    }
}

/// Cooks a linear equirect into a complete prefiltered specular IBL chain.
///
/// Mip `0` is a near-mirror projection of the environment. Higher mips use
/// GGX importance sampling with roughness `mip / (mip_count - 1)`. RGB is
/// stored as Reinhard+sRGB8 for the existing LDR specular runtime.
///
/// # Errors
///
/// Returns [`GgxCookError`] when config is invalid or packing fails.
pub fn cook_ggx_specular_ibl(
    environment: &PreparedEquirectEnvironment3d,
    config: GgxCookConfig,
) -> Result<PreparedSpecularIbl3d, GgxCookError> {
    if config.face_size == 0 || !config.face_size.is_power_of_two() {
        return Err(GgxCookError::InvalidFaceSize);
    }
    if config.sample_count == 0 {
        return Err(GgxCookError::InvalidSampleCount);
    }
    if config.lut_size < 2 {
        return Err(GgxCookError::InvalidLutSize);
    }

    let mip_count = config.face_size.ilog2() + 1;
    let mut mips = Vec::with_capacity(mip_count as usize);
    for mip in 0..mip_count {
        let edge = (config.face_size >> mip).max(1);
        let roughness = if mip_count == 1 {
            0.0
        } else {
            mip as f32 / (mip_count - 1) as f32
        };
        let faces = core::array::from_fn(|face| {
            prefilter_face(environment, face, edge, roughness, config.sample_count)
        });
        mips.push(faces);
    }
    let brdf_lut = integrate_brdf_lut_rgba8(config.lut_size);
    Ok(PreparedSpecularIbl3d::from_rgba8_mips(
        config.face_size,
        mips,
        config.lut_size,
        brdf_lut,
    )?)
}

fn prefilter_face(
    environment: &PreparedEquirectEnvironment3d,
    face: usize,
    edge: u32,
    roughness: f32,
    sample_count: u32,
) -> Vec<u8> {
    let mut pixels = Vec::with_capacity((edge as usize).saturating_mul(edge as usize) * 4);
    for y in 0..edge {
        for x in 0..edge {
            let u = (2.0 * (x as f32 + 0.5) / edge as f32) - 1.0;
            let v = (2.0 * (y as f32 + 0.5) / edge as f32) - 1.0;
            let direction = normalize3(cube_face_direction(face, u, v));
            let rgb = if roughness <= 1.0e-4 {
                environment.sample_direction(direction)
            } else {
                prefilter_direction(environment, direction, roughness, sample_count)
            };
            let rgba = linear_to_rgba8(rgb);
            pixels.extend_from_slice(&rgba);
        }
    }
    pixels
}

fn prefilter_direction(
    environment: &PreparedEquirectEnvironment3d,
    normal: [f32; 3],
    roughness: f32,
    sample_count: u32,
) -> [f32; 3] {
    let view = normal;
    let mut color = [0.0_f32; 3];
    let mut weight = 0.0_f32;
    for i in 0..sample_count {
        let xi = hammersley(i, sample_count);
        let half = importance_sample_ggx_oriented(xi, normal, roughness);
        let dot_vh = dot3(view, half);
        let light = [
            2.0 * dot_vh * half[0] - view[0],
            2.0 * dot_vh * half[1] - view[1],
            2.0 * dot_vh * half[2] - view[2],
        ];
        let n_dot_l = dot3(normal, light).max(0.0);
        if n_dot_l > 0.0 {
            let sample = environment.sample_direction(normalize3(light));
            color[0] += sample[0] * n_dot_l;
            color[1] += sample[1] * n_dot_l;
            color[2] += sample[2] * n_dot_l;
            weight += n_dot_l;
        }
    }
    if weight > 1.0e-5 {
        [color[0] / weight, color[1] / weight, color[2] / weight]
    } else {
        environment.sample_direction(normal)
    }
}

fn importance_sample_ggx_oriented(xi: [f32; 2], normal: [f32; 3], roughness: f32) -> [f32; 3] {
    let h_tangent = importance_sample_ggx(xi, roughness);
    let (tangent, bitangent) = orthonormal_basis(normal);
    normalize3([
        tangent[0] * h_tangent[0] + bitangent[0] * h_tangent[1] + normal[0] * h_tangent[2],
        tangent[1] * h_tangent[0] + bitangent[1] * h_tangent[1] + normal[1] * h_tangent[2],
        tangent[2] * h_tangent[0] + bitangent[2] * h_tangent[1] + normal[2] * h_tangent[2],
    ])
}

fn orthonormal_basis(normal: [f32; 3]) -> ([f32; 3], [f32; 3]) {
    let up = if normal[1].abs() < 0.999 {
        [0.0, 1.0, 0.0]
    } else {
        [1.0, 0.0, 0.0]
    };
    let tangent = normalize3(cross3(up, normal));
    let bitangent = cross3(normal, tangent);
    (tangent, bitangent)
}

/// WGPU / OpenGL cube face directions for `u,v ∈ [-1, 1]`.
fn cube_face_direction(face: usize, u: f32, v: f32) -> [f32; 3] {
    match face {
        0 => [1.0, -v, -u],  // +X
        1 => [-1.0, -v, u],  // -X
        2 => [u, 1.0, v],    // +Y
        3 => [u, -1.0, -v],  // -Y
        4 => [u, -v, 1.0],   // +Z
        _ => [-u, -v, -1.0], // -Z
    }
}

fn linear_to_rgba8(rgb: [f32; 3]) -> [u8; 4] {
    let encode = |channel: f32| {
        let tonemapped = channel.max(0.0) / (1.0 + channel.max(0.0));
        (tonemapped.powf(1.0 / 2.2) * 255.0).round().clamp(0.0, 255.0) as u8
    };
    [encode(rgb[0]), encode(rgb[1]), encode(rgb[2]), 255]
}

fn normalize3(v: [f32; 3]) -> [f32; 3] {
    let len_sq = dot3(v, v);
    if !len_sq.is_finite() || len_sq <= f32::EPSILON {
        return [0.0, 1.0, 0.0];
    }
    let inv = len_sq.sqrt().recip();
    [v[0] * inv, v[1] * inv, v[2] * inv]
}

fn dot3(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0].mul_add(b[0], a[1].mul_add(b[1], a[2] * b[2]))
}

fn cross3(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

#[cfg(test)]
mod tests {
    use super::{GgxCookConfig, GgxCookError, cook_ggx_specular_ibl};
    use crate::equirect::PreparedEquirectEnvironment3d;
    use crate::ibl::SPECULAR_IBL_FACE_COUNT;

    fn face_mean_luma(pixels: &[u8]) -> f32 {
        let mut sum = 0.0_f32;
        let mut count = 0_u32;
        for texel in pixels.chunks_exact(4) {
            sum += 0.2126 * f32::from(texel[0])
                + 0.7152 * f32::from(texel[1])
                + 0.0722 * f32::from(texel[2]);
            count += 1;
        }
        sum / count.max(1) as f32
    }

    #[test]
    fn cook_smoke_config_produces_valid_chain() {
        let env = PreparedEquirectEnvironment3d::synthetic_outdoor_probe().expect("equirect");
        let prepared = cook_ggx_specular_ibl(&env, GgxCookConfig::smoke()).expect("cook");
        assert_eq!(prepared.face_size(), 16);
        assert_eq!(prepared.mip_level_count(), 5);
        assert_eq!(prepared.lut_size(), 64);
    }

    #[test]
    fn cooked_plus_y_brighter_than_minus_y_on_mirror_mip() {
        let env = PreparedEquirectEnvironment3d::synthetic_outdoor_probe().expect("equirect");
        let prepared = cook_ggx_specular_ibl(&env, GgxCookConfig::smoke()).expect("cook");
        let plus_y = prepared.mip_face_rgba8(0, 2).expect("+Y face");
        let minus_y = prepared.mip_face_rgba8(0, 3).expect("-Y face");
        let up = face_mean_luma(plus_y);
        let down = face_mean_luma(minus_y);
        assert!(
            up > down,
            "+Y luma {up} should exceed -Y luma {down} for outdoor probe"
        );
        assert_eq!(SPECULAR_IBL_FACE_COUNT, 6);
    }

    #[test]
    fn rejects_bad_face_size() {
        let env = PreparedEquirectEnvironment3d::synthetic_outdoor_probe().expect("equirect");
        let error = cook_ggx_specular_ibl(&env, GgxCookConfig::new(3, 16, 32)).expect_err("size");
        assert_eq!(error, GgxCookError::InvalidFaceSize);
    }
}
