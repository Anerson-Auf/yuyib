//! Runtime specular IBL resources for the factor-only PBR preset.
//!
//! This module accepts **already prefiltered** cube mip chains and a split-sum
//! BRDF LUT. HDR equirectangular decode lives in [`crate::equirect`]; GGX
//! convolution cook that turns an equirect into these mips is a later slice.
//! Diffuse irradiance remains the typed L2 SH path on [`crate::PbrLighting3d`].

use std::{error::Error, fmt};

use yuyib_render::{RenderFrame, Renderer, wgpu};

/// WGPU cube face order: +X, −X, +Y, −Y, +Z, −Z.
pub const SPECULAR_IBL_FACE_COUNT: usize = 6;

/// CPU-prepared LDR prefiltered specular environment + BRDF LUT.
///
/// Face pixels are tightly packed RGBA8 (`width * height * 4` bytes per face
/// per mip). Face order matches [`SPECULAR_IBL_FACE_COUNT`]. Mip zero is the
/// sharpest reflection; higher mips must already encode roughness blur.
#[derive(Clone, Debug)]
pub struct PreparedSpecularIbl3d {
    face_size: u32,
    mip_level_count: u32,
    /// `faces[mip][face] -> rgba8`
    mips: Vec<[Vec<u8>; SPECULAR_IBL_FACE_COUNT]>,
    lut_size: u32,
    /// Row-major RGBA8 BRDF LUT; only R/G are sampled (`scale`, `bias`).
    brdf_lut: Vec<u8>,
}

/// Uploaded specular IBL bindable by [`crate::PbrMeshRenderer3d`].
pub struct GpuSpecularIbl3d {
    face_size: u32,
    mip_level_count: u32,
    lut_size: u32,
    _cube: wgpu::Texture,
    _lut: wgpu::Texture,
    cube_view: wgpu::TextureView,
    cube_sampler: wgpu::Sampler,
    lut_view: wgpu::TextureView,
    lut_sampler: wgpu::Sampler,
    bind_group: wgpu::BindGroup,
}

/// Invalid prefiltered specular IBL payload.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SpecularIblError {
    /// Face edge length must be a positive power of two.
    InvalidFaceSize,
    /// LUT edge length must be at least 2.
    InvalidLutSize,
    /// A face/mip byte payload did not match `face_size` / mip dimensions.
    InvalidFaceBytes {
        /// Mip index that failed validation.
        mip: u32,
        /// Face index in WGPU cube order.
        face: usize,
        /// Observed byte length.
        got: usize,
        /// Expected byte length.
        expected: usize,
    },
    /// BRDF LUT byte length did not match `lut_size² * 4`.
    InvalidLutBytes {
        /// Observed byte length.
        got: usize,
        /// Expected byte length.
        expected: usize,
    },
    /// Requested mip count does not match `floor(log2(face_size)) + 1`.
    InvalidMipLevelCount {
        /// Observed mip count.
        got: u32,
        /// Expected complete chain length.
        expected: u32,
    },
}

impl fmt::Display for SpecularIblError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidFaceSize => {
                formatter.write_str("specular IBL face size must be a positive power of two")
            }
            Self::InvalidLutSize => {
                formatter.write_str("specular IBL BRDF LUT size must be >= 2")
            }
            Self::InvalidFaceBytes {
                mip,
                face,
                got,
                expected,
            } => write!(
                formatter,
                "specular IBL mip {mip} face {face} has {got} bytes, expected {expected}"
            ),
            Self::InvalidLutBytes { got, expected } => write!(
                formatter,
                "specular IBL BRDF LUT has {got} bytes, expected {expected}"
            ),
            Self::InvalidMipLevelCount { got, expected } => write!(
                formatter,
                "specular IBL mip count {got} does not match complete chain {expected}"
            ),
        }
    }
}

impl Error for SpecularIblError {}

impl PreparedSpecularIbl3d {
    /// Validates a complete prefiltered cube mip chain and BRDF LUT.
    ///
    /// # Errors
    ///
    /// Returns [`SpecularIblError`] when dimensions or payload sizes are wrong.
    pub fn from_rgba8_mips(
        face_size: u32,
        mips: Vec<[Vec<u8>; SPECULAR_IBL_FACE_COUNT]>,
        lut_size: u32,
        brdf_lut: Vec<u8>,
    ) -> Result<Self, SpecularIblError> {
        if face_size == 0 || !face_size.is_power_of_two() {
            return Err(SpecularIblError::InvalidFaceSize);
        }
        if lut_size < 2 {
            return Err(SpecularIblError::InvalidLutSize);
        }
        let expected_mips = face_size.ilog2() + 1;
        let mip_level_count = u32::try_from(mips.len()).unwrap_or(u32::MAX);
        if mip_level_count != expected_mips {
            return Err(SpecularIblError::InvalidMipLevelCount {
                got: mip_level_count,
                expected: expected_mips,
            });
        }
        for (mip_index, faces) in mips.iter().enumerate() {
            let mip = u32::try_from(mip_index).unwrap_or(u32::MAX);
            let edge = (face_size >> mip_index).max(1);
            let expected = (edge as usize).saturating_mul(edge as usize).saturating_mul(4);
            for (face, pixels) in faces.iter().enumerate() {
                if pixels.len() != expected {
                    return Err(SpecularIblError::InvalidFaceBytes {
                        mip,
                        face,
                        got: pixels.len(),
                        expected,
                    });
                }
            }
        }
        let expected_lut = (lut_size as usize)
            .saturating_mul(lut_size as usize)
            .saturating_mul(4);
        if brdf_lut.len() != expected_lut {
            return Err(SpecularIblError::InvalidLutBytes {
                got: brdf_lut.len(),
                expected: expected_lut,
            });
        }
        Ok(Self {
            face_size,
            mip_level_count,
            mips,
            lut_size,
            brdf_lut,
        })
    }

    /// Builds a tiny asymmetric LDR environment for tests and headless smoke.
    ///
    /// Each face has a distinct colour so orientation bugs are visible. Higher
    /// mips are box-filtered toward the face average (stand-in for GGX
    /// prefilter). The BRDF LUT is a coarse CPU split-sum table.
    ///
    /// # Errors
    ///
    /// Returns [`SpecularIblError`] only if internal packing is inconsistent
    /// (should not happen for the fixed synthetic sizes).
    pub fn synthetic_asymmetric() -> Result<Self, SpecularIblError> {
        const FACE_SIZE: u32 = 16;
        const LUT_SIZE: u32 = 64;
        let face_colors: [[u8; 3]; SPECULAR_IBL_FACE_COUNT] = [
            [220, 40, 40],  // +X
            [40, 40, 220],  // -X
            [180, 210, 255], // +Y sky
            [90, 60, 40],   // -Y ground
            [40, 200, 80],  // +Z
            [200, 180, 40], // -Z
        ];
        let mip_count = FACE_SIZE.ilog2() + 1;
        let mut mips = Vec::with_capacity(mip_count as usize);
        let mut level0 = core::array::from_fn(|face| {
            flat_rgba_face(FACE_SIZE, face_colors[face][0], face_colors[face][1], face_colors[face][2])
        });
        // Mark the centre of +Y so LOD0 reflections stay orientation-sensitive.
        paint_centre_marker(&mut level0[2], FACE_SIZE, [255, 255, 255]);
        mips.push(level0);
        while let Some(previous) = mips.last() {
            let prev_edge = (FACE_SIZE >> (mips.len() - 1)).max(1);
            if prev_edge == 1 {
                break;
            }
            let next_edge = (prev_edge / 2).max(1);
            let next = core::array::from_fn(|face| {
                downsample_rgba8_box(&previous[face], prev_edge, next_edge)
            });
            mips.push(next);
        }
        let brdf_lut = integrate_brdf_lut_rgba8(LUT_SIZE);
        Self::from_rgba8_mips(FACE_SIZE, mips, LUT_SIZE, brdf_lut)
    }

    /// Neutral black environment that contributes no specular when strength > 0
    /// is still near-zero visually; used as the factor-PBR default bind.
    ///
    /// # Errors
    ///
    /// Returns [`SpecularIblError`] on internal packing failure.
    pub fn neutral_black() -> Result<Self, SpecularIblError> {
        const FACE_SIZE: u32 = 1;
        const LUT_SIZE: u32 = 2;
        let mips = vec![core::array::from_fn(|_| flat_rgba_face(FACE_SIZE, 0, 0, 0))];
        let brdf_lut = vec![0_u8; (LUT_SIZE as usize) * (LUT_SIZE as usize) * 4];
        Self::from_rgba8_mips(FACE_SIZE, mips, LUT_SIZE, brdf_lut)
    }

    /// Face edge length at mip zero.
    #[must_use]
    pub const fn face_size(&self) -> u32 {
        self.face_size
    }

    /// Complete mip count including level zero.
    #[must_use]
    pub const fn mip_level_count(&self) -> u32 {
        self.mip_level_count
    }

    /// BRDF LUT edge length.
    #[must_use]
    pub const fn lut_size(&self) -> u32 {
        self.lut_size
    }

    /// Returns tightly packed RGBA8 pixels for one cube face at `mip`.
    #[must_use]
    pub fn mip_face_rgba8(&self, mip: u32, face: usize) -> Option<&[u8]> {
        self.mips
            .get(mip as usize)
            .and_then(|faces| faces.get(face))
            .map(Vec::as_slice)
    }

    /// Maps perceptual roughness `0..=1` onto the prefiltered mip range.
    #[must_use]
    pub fn roughness_to_lod(&self, roughness: f32) -> f32 {
        let max_lod = (self.mip_level_count.saturating_sub(1)) as f32;
        roughness.clamp(0.0, 1.0) * max_lod
    }
}

impl GpuSpecularIbl3d {
    /// Uploads [`PreparedSpecularIbl3d`] to `renderer`'s device.
    #[must_use]
    pub fn upload(renderer: &Renderer, prepared: &PreparedSpecularIbl3d) -> Self {
        renderer.with_raw_gpu(|device, queue, _configuration| {
            Self::upload_with(device, queue, prepared)
        })
    }

    /// Uploads through a frame's device/queue.
    #[must_use]
    pub fn upload_for_frame(frame: &RenderFrame<'_>, prepared: &PreparedSpecularIbl3d) -> Self {
        Self::upload_with(frame.device(), frame.queue(), prepared)
    }

    /// Face edge length at mip zero.
    #[must_use]
    pub const fn face_size(&self) -> u32 {
        self.face_size
    }

    /// Complete mip count including level zero.
    #[must_use]
    pub const fn mip_level_count(&self) -> u32 {
        self.mip_level_count
    }

    /// BRDF LUT edge length.
    #[must_use]
    pub const fn lut_size(&self) -> u32 {
        self.lut_size
    }

    /// Bind group for `@group(2)` on the factor-only PBR pipeline.
    #[must_use]
    pub const fn bind_group(&self) -> &wgpu::BindGroup {
        &self.bind_group
    }

    pub(crate) fn bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
        device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("yuyib specular IBL layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::Cube,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        })
    }

    pub(crate) fn upload_with(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        prepared: &PreparedSpecularIbl3d,
    ) -> Self {
        let cube = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("yuyib specular IBL cube"),
            size: wgpu::Extent3d {
                width: prepared.face_size,
                height: prepared.face_size,
                depth_or_array_layers: SPECULAR_IBL_FACE_COUNT as u32,
            },
            mip_level_count: prepared.mip_level_count,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        for (mip_index, faces) in prepared.mips.iter().enumerate() {
            let mip = u32::try_from(mip_index).expect("mip fits u32");
            let edge = (prepared.face_size >> mip_index).max(1);
            for (face, pixels) in faces.iter().enumerate() {
                queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &cube,
                        mip_level: mip,
                        origin: wgpu::Origin3d {
                            x: 0,
                            y: 0,
                            z: u32::try_from(face).expect("face fits u32"),
                        },
                        aspect: wgpu::TextureAspect::All,
                    },
                    pixels,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(edge * 4),
                        rows_per_image: Some(edge),
                    },
                    wgpu::Extent3d {
                        width: edge,
                        height: edge,
                        depth_or_array_layers: 1,
                    },
                );
            }
        }
        let cube_view = cube.create_view(&wgpu::TextureViewDescriptor {
            label: Some("yuyib specular IBL cube view"),
            dimension: Some(wgpu::TextureViewDimension::Cube),
            ..Default::default()
        });
        let cube_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("yuyib specular IBL cube sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            ..Default::default()
        });

        let lut = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("yuyib specular IBL BRDF LUT"),
            size: wgpu::Extent3d {
                width: prepared.lut_size,
                height: prepared.lut_size,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &lut,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &prepared.brdf_lut,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(prepared.lut_size * 4),
                rows_per_image: Some(prepared.lut_size),
            },
            wgpu::Extent3d {
                width: prepared.lut_size,
                height: prepared.lut_size,
                depth_or_array_layers: 1,
            },
        );
        let lut_view = lut.create_view(&wgpu::TextureViewDescriptor {
            label: Some("yuyib specular IBL BRDF LUT view"),
            ..Default::default()
        });
        let lut_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("yuyib specular IBL BRDF LUT sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });

        let layout = Self::bind_group_layout(device);
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("yuyib specular IBL bind group"),
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&cube_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&cube_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&lut_view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Sampler(&lut_sampler),
                },
            ],
        });

        Self {
            face_size: prepared.face_size,
            mip_level_count: prepared.mip_level_count,
            lut_size: prepared.lut_size,
            _cube: cube,
            _lut: lut,
            cube_view,
            cube_sampler,
            lut_view,
            lut_sampler,
            bind_group,
        }
    }

    pub(crate) const fn cube_view(&self) -> &wgpu::TextureView {
        &self.cube_view
    }

    pub(crate) const fn cube_sampler(&self) -> &wgpu::Sampler {
        &self.cube_sampler
    }

    pub(crate) const fn lut_view(&self) -> &wgpu::TextureView {
        &self.lut_view
    }

    pub(crate) const fn lut_sampler(&self) -> &wgpu::Sampler {
        &self.lut_sampler
    }
}

fn flat_rgba_face(edge: u32, r: u8, g: u8, b: u8) -> Vec<u8> {
    let count = (edge as usize).saturating_mul(edge as usize);
    let mut pixels = Vec::with_capacity(count.saturating_mul(4));
    for _ in 0..count {
        pixels.extend_from_slice(&[r, g, b, 255]);
    }
    pixels
}

fn paint_centre_marker(pixels: &mut [u8], edge: u32, rgb: [u8; 3]) {
    if edge == 0 {
        return;
    }
    let cx = edge / 2;
    let cy = edge / 2;
    let index = ((cy * edge + cx) * 4) as usize;
    if index + 3 < pixels.len() {
        pixels[index] = rgb[0];
        pixels[index + 1] = rgb[1];
        pixels[index + 2] = rgb[2];
        pixels[index + 3] = 255;
    }
}

fn downsample_rgba8_box(source: &[u8], src_edge: u32, dst_edge: u32) -> Vec<u8> {
    let mut out = flat_rgba_face(dst_edge, 0, 0, 0);
    for y in 0..dst_edge {
        for x in 0..dst_edge {
            let mut sum = [0_u32; 3];
            let mut count = 0_u32;
            for oy in 0..2 {
                for ox in 0..2 {
                    let sx = (x * 2 + ox).min(src_edge - 1);
                    let sy = (y * 2 + oy).min(src_edge - 1);
                    let idx = ((sy * src_edge + sx) * 4) as usize;
                    sum[0] += u32::from(source[idx]);
                    sum[1] += u32::from(source[idx + 1]);
                    sum[2] += u32::from(source[idx + 2]);
                    count += 1;
                }
            }
            let dst = ((y * dst_edge + x) * 4) as usize;
            out[dst] = (sum[0] / count) as u8;
            out[dst + 1] = (sum[1] / count) as u8;
            out[dst + 2] = (sum[2] / count) as u8;
            out[dst + 3] = 255;
        }
    }
    out
}

pub(crate) fn integrate_brdf_lut_rgba8(size: u32) -> Vec<u8> {
    // Coarse split-sum BRDF integration (Karis). Enough for smoke/evidence;
    // cook pipeline can replace with a higher-sample offline table later.
    const SAMPLE_COUNT: u32 = 64;
    let mut pixels = vec![0_u8; (size as usize) * (size as usize) * 4];
    for y in 0..size {
        for x in 0..size {
            let n_dot_v = (x as f32 + 0.5) / size as f32;
            let roughness = (y as f32 + 0.5) / size as f32;
            let (scale, bias) = integrate_brdf(n_dot_v.max(0.001), roughness, SAMPLE_COUNT);
            let idx = ((y * size + x) * 4) as usize;
            pixels[idx] = (scale.clamp(0.0, 1.0) * 255.0).round() as u8;
            pixels[idx + 1] = (bias.clamp(0.0, 1.0) * 255.0).round() as u8;
            pixels[idx + 2] = 0;
            pixels[idx + 3] = 255;
        }
    }
    pixels
}

fn integrate_brdf(n_dot_v: f32, roughness: f32, sample_count: u32) -> (f32, f32) {
    let mut a = 0.0_f32;
    let mut b = 0.0_f32;
    // View vector in tangent space: N = (0,0,1), cos_theta = n_dot_v.
    let view = [(1.0 - n_dot_v * n_dot_v).max(0.0).sqrt(), 0.0, n_dot_v];
    for i in 0..sample_count {
        let xi = hammersley(i, sample_count);
        let h = importance_sample_ggx(xi, roughness);
        let dot_vh = view[0] * h[0] + view[1] * h[1] + view[2] * h[2];
        let light = [
            2.0 * dot_vh * h[0] - view[0],
            2.0 * dot_vh * h[1] - view[1],
            2.0 * dot_vh * h[2] - view[2],
        ];
        let n_dot_l = light[2].max(0.0);
        let n_dot_h = h[2].max(0.0);
        let v_dot_h = dot_vh.max(0.0);
        if n_dot_l > 0.0 {
            let g = geometry_smith(n_dot_v, n_dot_l, roughness);
            let g_vis = (g * v_dot_h) / (n_dot_h * n_dot_v).max(1.0e-5);
            let fc = (1.0 - v_dot_h).powi(5);
            a += (1.0 - fc) * g_vis;
            b += fc * g_vis;
        }
    }
    let inv = (sample_count as f32).recip();
    (a * inv, b * inv)
}

pub(crate) fn hammersley(i: u32, n: u32) -> [f32; 2] {
    [i as f32 / n as f32, radical_inverse_vdc(i)]
}

fn radical_inverse_vdc(mut bits: u32) -> f32 {
    bits = (bits << 16) | (bits >> 16);
    bits = ((bits & 0x5555_5555) << 1) | ((bits & 0xAAAA_AAAA) >> 1);
    bits = ((bits & 0x3333_3333) << 2) | ((bits & 0xCCCC_CCCC) >> 2);
    bits = ((bits & 0x0F0F_0F0F) << 4) | ((bits & 0xF0F0_F0F0) >> 4);
    bits = ((bits & 0x00FF_00FF) << 8) | ((bits & 0xFF00_FF00) >> 8);
    bits as f32 * 2.328_306_4e-10
}

pub(crate) fn importance_sample_ggx(xi: [f32; 2], roughness: f32) -> [f32; 3] {
    let a = roughness * roughness;
    let phi = 2.0 * std::f32::consts::PI * xi[0];
    let cos_theta = ((1.0 - xi[1]) / (xi[1] * (a * a - 1.0) + 1.0))
        .max(0.0)
        .sqrt();
    let sin_theta = (1.0 - cos_theta * cos_theta).max(0.0).sqrt();
    [phi.cos() * sin_theta, phi.sin() * sin_theta, cos_theta]
}

fn geometry_schlick_ggx(n_dot: f32, roughness: f32) -> f32 {
    let k = (roughness * roughness) / 2.0;
    n_dot / (n_dot * (1.0 - k) + k).max(1.0e-5)
}

fn geometry_smith(n_dot_v: f32, n_dot_l: f32, roughness: f32) -> f32 {
    geometry_schlick_ggx(n_dot_v, roughness) * geometry_schlick_ggx(n_dot_l, roughness)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_prefiltered_chain_validates() {
        let prepared = PreparedSpecularIbl3d::synthetic_asymmetric().expect("synthetic");
        assert_eq!(prepared.face_size(), 16);
        assert_eq!(prepared.mip_level_count(), 5);
        assert_eq!(prepared.lut_size(), 64);
        assert!((prepared.roughness_to_lod(0.0) - 0.0).abs() < f32::EPSILON);
        assert!((prepared.roughness_to_lod(1.0) - 4.0).abs() < f32::EPSILON);
    }

    #[test]
    fn rejects_non_power_of_two_face() {
        let err = PreparedSpecularIbl3d::from_rgba8_mips(
            3,
            vec![core::array::from_fn(|_| flat_rgba_face(3, 0, 0, 0))],
            8,
            vec![0; 8 * 8 * 4],
        )
        .expect_err("face size");
        assert_eq!(err, SpecularIblError::InvalidFaceSize);
    }

    #[test]
    fn rejects_truncated_mip_chain() {
        let err = PreparedSpecularIbl3d::from_rgba8_mips(
            4,
            vec![core::array::from_fn(|_| flat_rgba_face(4, 0, 0, 0))],
            8,
            vec![0; 8 * 8 * 4],
        )
        .expect_err("incomplete mips");
        assert!(matches!(
            err,
            SpecularIblError::InvalidMipLevelCount {
                got: 1,
                expected: 3
            }
        ));
    }
}
