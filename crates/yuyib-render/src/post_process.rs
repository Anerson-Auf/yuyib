//! HDR colour post-processing shared by 2D, 3D and UI render phases.
//!
//! The renderer records the complete frame into a linear `Rgba16Float` target
//! when this feature is enabled, then optionally extracts a bright-pass bloom
//! pyramid and resolves to the presentation surface through exposure and tone
//! mapping. Keeping this ownership in the renderer means existing high-level
//! scene renderers do not need a second draw API.

use std::{error::Error, fmt};

use wgpu::{
    BindGroup, BindGroupLayout, Buffer, ColorTargetState, ColorWrites, Device, FragmentState,
    LoadOp, MultisampleState, Operations, PipelineCompilationOptions, PrimitiveState,
    RenderPassColorAttachment, RenderPassDescriptor, RenderPipeline, Sampler, ShaderStages,
    StoreOp, Texture, TextureDescriptor, TextureDimension, TextureFormat, TextureUsages,
    TextureView, TextureViewDescriptor, VertexState,
};

/// Floating-point scene target used before presentation tone mapping.
pub const HDR_SCENE_FORMAT: TextureFormat = TextureFormat::Rgba16Float;

/// Hard exposure limits keep accidental values from producing unusable frames.
pub const MIN_EXPOSURE_EV: f32 = -16.0;
/// Upper bound paired with [`MIN_EXPOSURE_EV`].
pub const MAX_EXPOSURE_EV: f32 = 16.0;

/// Number of half-resolution bloom pyramid levels (½, ¼, ⅛, …).
pub const BLOOM_LEVELS: usize = 4;

/// Mapping from exposed linear HDR colour to display-referred colour.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum ToneMapping {
    /// Clamp each exposed channel to `0..=1` without changing its curve.
    LinearClamp,
    /// Extended Reinhard curve, useful for a soft and neutral highlight rolloff.
    Reinhard,
    /// The compact ACES filmic approximation commonly used for real-time previews.
    #[default]
    AcesFilmic,
}

impl ToneMapping {
    const fn shader_id(self) -> u32 {
        match self {
            Self::LinearClamp => 0,
            Self::Reinhard => 1,
            Self::AcesFilmic => 2,
        }
    }
}

/// Bright-pass bloom settings applied before tone mapping.
///
/// Bloom only runs on the HDR path ([`ColorPostProcess`]). Threshold is in
/// linear luminance after scene lighting; intensity scales the composited glow.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BloomConfig {
    threshold: f32,
    soft_knee: f32,
    intensity: f32,
}

impl BloomConfig {
    /// Creates validated bloom settings.
    ///
    /// # Errors
    ///
    /// Returns [`ColorPostProcessError::InvalidBloom`] unless every parameter is
    /// finite and non-negative, and `soft_knee` is in `0.0..=1.0`.
    pub fn new(threshold: f32, soft_knee: f32, intensity: f32) -> Result<Self, ColorPostProcessError> {
        validate_bloom(threshold, soft_knee, intensity)?;
        Ok(Self {
            threshold,
            soft_knee,
            intensity,
        })
    }

    /// Soft outdoor / neon preset: threshold just above mid-grey, gentle mix.
    #[must_use]
    pub fn street_city() -> Self {
        Self::new(1.15, 0.55, 0.085).expect("street-city bloom preset")
    }

    /// Linear luminance threshold for the bright-pass.
    #[must_use]
    pub const fn threshold(self) -> f32 {
        self.threshold
    }

    /// Soft knee width as a fraction of [`Self::threshold`] (`0` = hard cut).
    #[must_use]
    pub const fn soft_knee(self) -> f32 {
        self.soft_knee
    }

    /// Composited glow strength.
    #[must_use]
    pub const fn intensity(self) -> f32 {
        self.intensity
    }
}

/// Fast approximate anti-aliasing applied after tone mapping (LDR).
///
/// FXAA needs no motion vectors or history. Prefer this as the playable AA
/// until a temporal path (TAA) exists. Runs only on the HDR presentation path.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FxaaConfig {
    quality: FxaaQuality,
}

/// Search / edge quality for [`FxaaConfig`].
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum FxaaQuality {
    /// Fewer edge walks; cheapest.
    Low,
    /// Balanced default for playable views.
    #[default]
    Medium,
    /// Extra edge search steps; slightly softer neon silhouettes.
    High,
}

impl FxaaConfig {
    /// Creates FXAA with the given quality preset.
    #[must_use]
    pub const fn new(quality: FxaaQuality) -> Self {
        Self { quality }
    }

    /// Playable preset: medium FXAA after ACES.
    #[must_use]
    pub const fn street_city() -> Self {
        Self::new(FxaaQuality::Medium)
    }

    /// Selected quality preset.
    #[must_use]
    pub const fn quality(self) -> FxaaQuality {
        self.quality
    }
}

/// Display-referred colour grading applied after tone mapping (before FXAA).
///
/// Parametric MVP (contrast / saturation / temperature / tint). A cooked 3D LUT
/// asset path remains open; this look is intentionally cheap and editor-safe.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ColorGradeConfig {
    contrast: f32,
    saturation: f32,
    temperature: f32,
    tint: f32,
}

impl ColorGradeConfig {
    /// Creates validated grading settings.
    ///
    /// # Errors
    ///
    /// Returns [`ColorPostProcessError::InvalidGrade`] unless every parameter is
    /// finite, `contrast`/`saturation` are in `0.0..=2.0`, and
    /// `temperature`/`tint` are in `-1.0..=1.0`.
    pub fn new(
        contrast: f32,
        saturation: f32,
        temperature: f32,
        tint: f32,
    ) -> Result<Self, ColorPostProcessError> {
        validate_grade(contrast, saturation, temperature, tint)?;
        Ok(Self {
            contrast,
            saturation,
            temperature,
            tint,
        })
    }

    /// Mild sci-fi look: slightly punchier contrast, cool temperature.
    #[must_use]
    pub fn street_city() -> Self {
        Self::new(1.08, 0.95, -0.06, 0.02).expect("street-city grade preset")
    }

    /// LDR contrast around mid-grey (`1` = unchanged).
    #[must_use]
    pub const fn contrast(self) -> f32 {
        self.contrast
    }

    /// Saturation (`1` = unchanged, `0` = luma).
    #[must_use]
    pub const fn saturation(self) -> f32 {
        self.saturation
    }

    /// Warm (+) / cool (−) balance.
    #[must_use]
    pub const fn temperature(self) -> f32 {
        self.temperature
    }

    /// Green (+) / magenta (−) balance.
    #[must_use]
    pub const fn tint(self) -> f32 {
        self.tint
    }
}

/// Validated HDR presentation policy for one [`crate::Renderer`].
///
/// Merely constructing this value does not allocate GPU resources. Install it
/// with [`crate::Renderer::set_color_post_process`] before creating cached GPU
/// pipelines. `Application` users can use its high-level builder method.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ColorPostProcess {
    exposure_ev: f32,
    tone_mapping: ToneMapping,
    bloom: Option<BloomConfig>,
    fxaa: Option<FxaaConfig>,
    grade: Option<ColorGradeConfig>,
}

impl ColorPostProcess {
    /// Creates a validated exposure and tone-mapping policy (effects off).
    ///
    /// # Errors
    ///
    /// Returns [`ColorPostProcessError`] unless exposure is finite and within
    /// [`MIN_EXPOSURE_EV`]..=[`MAX_EXPOSURE_EV`].
    pub fn new(exposure_ev: f32, tone_mapping: ToneMapping) -> Result<Self, ColorPostProcessError> {
        validate_exposure(exposure_ev)?;
        Ok(Self {
            exposure_ev,
            tone_mapping,
            bloom: None,
            fxaa: None,
            grade: None,
        })
    }

    /// Filmic opt-in preset: zero exposure compensation and ACES highlight rolloff.
    #[must_use]
    pub const fn filmic() -> Self {
        Self {
            exposure_ev: 0.0,
            tone_mapping: ToneMapping::AcesFilmic,
            bloom: None,
            fxaa: None,
            grade: None,
        }
    }

    /// Exposure compensation in photographic stops (`2^EV` multiplier).
    #[must_use]
    pub const fn exposure_ev(self) -> f32 {
        self.exposure_ev
    }

    /// Selected display mapping curve.
    #[must_use]
    pub const fn tone_mapping(self) -> ToneMapping {
        self.tone_mapping
    }

    /// Optional bright-pass bloom applied before tone mapping.
    #[must_use]
    pub const fn bloom(self) -> Option<BloomConfig> {
        self.bloom
    }

    /// Optional FXAA applied after tone mapping (and grading).
    #[must_use]
    pub const fn fxaa(self) -> Option<FxaaConfig> {
        self.fxaa
    }

    /// Optional colour grading applied after tone mapping.
    #[must_use]
    pub const fn color_grade(self) -> Option<ColorGradeConfig> {
        self.grade
    }

    /// Returns a copy with different validated exposure compensation.
    ///
    /// # Errors
    ///
    /// Returns [`ColorPostProcessError`] under the same conditions as [`Self::new`].
    pub fn with_exposure_ev(mut self, exposure_ev: f32) -> Result<Self, ColorPostProcessError> {
        validate_exposure(exposure_ev)?;
        self.exposure_ev = exposure_ev;
        Ok(self)
    }

    /// Returns a copy using another tone-mapping curve.
    #[must_use]
    pub const fn with_tone_mapping(mut self, tone_mapping: ToneMapping) -> Self {
        self.tone_mapping = tone_mapping;
        self
    }

    /// Enables bloom with the given settings.
    #[must_use]
    pub const fn with_bloom(mut self, bloom: BloomConfig) -> Self {
        self.bloom = Some(bloom);
        self
    }

    /// Disables bloom.
    #[must_use]
    pub const fn without_bloom(mut self) -> Self {
        self.bloom = None;
        self
    }

    /// Enables FXAA after tone mapping / grading.
    #[must_use]
    pub const fn with_fxaa(mut self, fxaa: FxaaConfig) -> Self {
        self.fxaa = Some(fxaa);
        self
    }

    /// Disables FXAA.
    #[must_use]
    pub const fn without_fxaa(mut self) -> Self {
        self.fxaa = None;
        self
    }

    /// Enables colour grading after tone mapping.
    #[must_use]
    pub const fn with_color_grade(mut self, grade: ColorGradeConfig) -> Self {
        self.grade = Some(grade);
        self
    }

    /// Disables colour grading.
    #[must_use]
    pub const fn without_color_grade(mut self) -> Self {
        self.grade = None;
        self
    }

    /// Linear multiplier applied before tone mapping.
    #[must_use]
    pub fn exposure_multiplier(self) -> f32 {
        self.exposure_ev.exp2()
    }
}

impl Default for ColorPostProcess {
    fn default() -> Self {
        Self::filmic()
    }
}

/// Invalid high-level colour post-processing configuration.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ColorPostProcessError {
    /// Exposure was not finite or exceeded the safe supported range.
    ExposureOutOfRange(f32),
    /// Bloom threshold / knee / intensity failed validation.
    InvalidBloom,
    /// Colour-grade contrast / saturation / temperature / tint failed validation.
    InvalidGrade,
}

impl fmt::Display for ColorPostProcessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ExposureOutOfRange(value) => write!(
                formatter,
                "post-process exposure {value} EV must be finite and within {MIN_EXPOSURE_EV}..={MAX_EXPOSURE_EV}"
            ),
            Self::InvalidBloom => formatter.write_str(
                "bloom threshold and intensity must be finite and >= 0; soft_knee must be in 0..=1",
            ),
            Self::InvalidGrade => formatter.write_str(
                "grade contrast/saturation must be finite in 0..=2; temperature/tint must be in -1..=1",
            ),
        }
    }
}

impl Error for ColorPostProcessError {}

fn validate_exposure(exposure_ev: f32) -> Result<(), ColorPostProcessError> {
    if !exposure_ev.is_finite() || !(MIN_EXPOSURE_EV..=MAX_EXPOSURE_EV).contains(&exposure_ev) {
        return Err(ColorPostProcessError::ExposureOutOfRange(exposure_ev));
    }
    Ok(())
}

fn validate_bloom(
    threshold: f32,
    soft_knee: f32,
    intensity: f32,
) -> Result<(), ColorPostProcessError> {
    if !threshold.is_finite()
        || threshold < 0.0
        || !intensity.is_finite()
        || intensity < 0.0
        || !soft_knee.is_finite()
        || !(0.0..=1.0).contains(&soft_knee)
    {
        return Err(ColorPostProcessError::InvalidBloom);
    }
    Ok(())
}

fn validate_grade(
    contrast: f32,
    saturation: f32,
    temperature: f32,
    tint: f32,
) -> Result<(), ColorPostProcessError> {
    if !contrast.is_finite()
        || !(0.0..=2.0).contains(&contrast)
        || !saturation.is_finite()
        || !(0.0..=2.0).contains(&saturation)
        || !temperature.is_finite()
        || !(-1.0..=1.0).contains(&temperature)
        || !tint.is_finite()
        || !(-1.0..=1.0).contains(&tint)
    {
        return Err(ColorPostProcessError::InvalidGrade);
    }
    Ok(())
}

pub(crate) struct PostProcessResources {
    width: u32,
    height: u32,
    surface_format: TextureFormat,
    bloom_enabled: bool,
    fxaa_enabled: bool,
    _hdr_texture: Texture,
    hdr_view: TextureView,
    bloom: Option<BloomResources>,
    fxaa: Option<FxaaResources>,
    parameters: Buffer,
    bind_group: BindGroup,
    pipeline: RenderPipeline,
}

struct BloomResources {
    levels: Vec<BloomLevel>,
    extract_pipeline: RenderPipeline,
    downsample_pipeline: RenderPipeline,
    upsample_pipeline: RenderPipeline,
    extract_params: Buffer,
    sampler: Sampler,
}

struct BloomLevel {
    _texture: Texture,
    view: TextureView,
}

struct FxaaResources {
    _tone_mapped: Texture,
    tone_mapped_view: TextureView,
    parameters: Buffer,
    bind_group: BindGroup,
    pipeline: RenderPipeline,
}

impl PostProcessResources {
    pub(crate) fn new(
        device: &Device,
        width: u32,
        height: u32,
        surface_format: TextureFormat,
        bloom: Option<BloomConfig>,
        fxaa: Option<FxaaConfig>,
    ) -> Self {
        let hdr_texture = device.create_texture(&TextureDescriptor {
            label: Some("yuyib HDR scene colour"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: TextureDimension::D2,
            format: HDR_SCENE_FORMAT,
            usage: TextureUsages::RENDER_ATTACHMENT | TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let hdr_view = hdr_texture.create_view(&TextureViewDescriptor::default());
        let bloom_resources = bloom.map(|_| create_bloom_resources(device, width, height));
        let fxaa_resources =
            fxaa.map(|config| create_fxaa_resources(device, width, height, surface_format, config));
        let parameters = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("yuyib colour post-process parameters"),
            size: 64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let resolve_layout = resolve_bind_group_layout(device);
        let bloom_view = bloom_resources
            .as_ref()
            .map(|bloom| &bloom.levels[0].view)
            .unwrap_or(&hdr_view);
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("yuyib colour post-process inputs"),
            layout: &resolve_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: parameters.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&hdr_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(bloom_view),
                },
            ],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("yuyib colour post-process WGSL"),
            source: wgpu::ShaderSource::Wgsl(POST_PROCESS_SHADER.into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("yuyib colour post-process pipeline layout"),
            bind_group_layouts: &[Some(&resolve_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("yuyib colour post-process pipeline"),
            layout: Some(&pipeline_layout),
            vertex: VertexState {
                module: &shader,
                entry_point: Some("vertex_main"),
                compilation_options: PipelineCompilationOptions::default(),
                buffers: &[],
            },
            fragment: Some(FragmentState {
                module: &shader,
                entry_point: Some("fragment_main"),
                compilation_options: PipelineCompilationOptions::default(),
                targets: &[Some(ColorTargetState {
                    format: surface_format,
                    blend: None,
                    write_mask: ColorWrites::ALL,
                })],
            }),
            primitive: PrimitiveState::default(),
            depth_stencil: None,
            multisample: MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        Self {
            width,
            height,
            surface_format,
            bloom_enabled: bloom.is_some(),
            fxaa_enabled: fxaa.is_some(),
            _hdr_texture: hdr_texture,
            hdr_view,
            bloom: bloom_resources,
            fxaa: fxaa_resources,
            parameters,
            bind_group,
            pipeline,
        }
    }

    pub(crate) fn matches(
        &self,
        width: u32,
        height: u32,
        surface_format: TextureFormat,
        bloom_enabled: bool,
        fxaa_enabled: bool,
    ) -> bool {
        self.width == width
            && self.height == height
            && self.surface_format == surface_format
            && self.bloom_enabled == bloom_enabled
            && self.fxaa_enabled == fxaa_enabled
    }

    pub(crate) const fn hdr_view(&self) -> &TextureView {
        &self.hdr_view
    }

    pub(crate) fn write_parameters(&self, queue: &wgpu::Queue, config: ColorPostProcess) {
        let bloom = config.bloom().unwrap_or(BloomConfig {
            threshold: 0.0,
            soft_knee: 0.0,
            intensity: 0.0,
        });
        let grade = config.color_grade().unwrap_or(ColorGradeConfig {
            contrast: 1.0,
            saturation: 1.0,
            temperature: 0.0,
            tint: 0.0,
        });
        let mut bytes = [0_u8; 64];
        bytes[0..4].copy_from_slice(&config.exposure_multiplier().to_ne_bytes());
        bytes[4..8].copy_from_slice(&config.tone_mapping.shader_id().to_ne_bytes());
        let bloom_enabled = u32::from(config.bloom().is_some());
        bytes[8..12].copy_from_slice(&bloom_enabled.to_ne_bytes());
        bytes[12..16].copy_from_slice(&bloom.intensity.to_ne_bytes());
        bytes[16..20].copy_from_slice(&bloom.threshold.to_ne_bytes());
        bytes[20..24].copy_from_slice(&bloom.soft_knee.to_ne_bytes());
        let grade_enabled = u32::from(config.color_grade().is_some());
        bytes[24..28].copy_from_slice(&grade_enabled.to_ne_bytes());
        bytes[28..32].copy_from_slice(&grade.contrast.to_ne_bytes());
        bytes[32..36].copy_from_slice(&grade.saturation.to_ne_bytes());
        bytes[36..40].copy_from_slice(&grade.temperature.to_ne_bytes());
        bytes[40..44].copy_from_slice(&grade.tint.to_ne_bytes());
        queue.write_buffer(&self.parameters, 0, &bytes);
        if let (Some(bloom_cfg), Some(bloom)) = (config.bloom(), &self.bloom) {
            let mut extract = [0_u8; 16];
            extract[0..4].copy_from_slice(&bloom_cfg.threshold.to_ne_bytes());
            extract[4..8].copy_from_slice(&bloom_cfg.soft_knee.to_ne_bytes());
            queue.write_buffer(&bloom.extract_params, 0, &extract);
        }
        if let (Some(fxaa_cfg), Some(fxaa)) = (config.fxaa(), &self.fxaa) {
            let quality = match fxaa_cfg.quality() {
                FxaaQuality::Low => 0_u32,
                FxaaQuality::Medium => 1_u32,
                FxaaQuality::High => 2_u32,
            };
            let mut fxaa_bytes = [0_u8; 16];
            fxaa_bytes[0..4].copy_from_slice(&(1.0 / self.width as f32).to_ne_bytes());
            fxaa_bytes[4..8].copy_from_slice(&(1.0 / self.height as f32).to_ne_bytes());
            fxaa_bytes[8..12].copy_from_slice(&quality.to_ne_bytes());
            queue.write_buffer(&fxaa.parameters, 0, &fxaa_bytes);
        }
    }

    pub(crate) fn resolve(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        device: &Device,
        surface_view: &TextureView,
        config: ColorPostProcess,
    ) {
        if config.bloom().is_some() {
            if let Some(bloom) = &self.bloom {
                self.encode_bloom(encoder, device, bloom);
            }
        }
        let resolve_target = self
            .fxaa
            .as_ref()
            .map_or(surface_view, |fxaa| &fxaa.tone_mapped_view);
        {
            let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
                label: Some("yuyib colour post-process resolve"),
                color_attachments: &[Some(RenderPassColorAttachment {
                    view: resolve_target,
                    depth_slice: None,
                    resolve_target: None,
                    ops: Operations {
                        load: LoadOp::Clear(wgpu::Color::BLACK),
                        store: StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, Some(&self.bind_group), &[]);
            pass.draw(0..3, 0..1);
        }
        if config.fxaa().is_some() {
            if let Some(fxaa) = &self.fxaa {
                let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
                    label: Some("yuyib FXAA"),
                    color_attachments: &[Some(RenderPassColorAttachment {
                        view: surface_view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: Operations {
                            load: LoadOp::Clear(wgpu::Color::BLACK),
                            store: StoreOp::Store,
                        },
                    })],
                    ..Default::default()
                });
                pass.set_pipeline(&fxaa.pipeline);
                pass.set_bind_group(0, Some(&fxaa.bind_group), &[]);
                pass.draw(0..3, 0..1);
            }
        }
    }

    fn encode_bloom(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        device: &Device,
        bloom: &BloomResources,
    ) {
        // Bright extract → level 0.
        let extract_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("yuyib bloom extract bind group"),
            layout: &bloom_src_layout(device),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: bloom.extract_params.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&self.hdr_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&bloom.sampler),
                },
            ],
        });
        {
            let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
                label: Some("yuyib bloom extract"),
                color_attachments: &[Some(RenderPassColorAttachment {
                    view: &bloom.levels[0].view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: Operations {
                        load: LoadOp::Clear(wgpu::Color::BLACK),
                        store: StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            pass.set_pipeline(&bloom.extract_pipeline);
            pass.set_bind_group(0, Some(&extract_bg), &[]);
            pass.draw(0..3, 0..1);
        }

        // Downsample chain.
        for index in 0..bloom.levels.len().saturating_sub(1) {
            let src = &bloom.levels[index];
            let dst = &bloom.levels[index + 1];
            let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("yuyib bloom downsample bind group"),
                layout: &bloom_src_layout(device),
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: bloom.extract_params.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&src.view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(&bloom.sampler),
                    },
                ],
            });
            let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
                label: Some("yuyib bloom downsample"),
                color_attachments: &[Some(RenderPassColorAttachment {
                    view: &dst.view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: Operations {
                        load: LoadOp::Clear(wgpu::Color::BLACK),
                        store: StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            pass.set_pipeline(&bloom.downsample_pipeline);
            pass.set_bind_group(0, Some(&bg), &[]);
            pass.draw(0..3, 0..1);
        }

        // Upsample: additively blend coarse contribution into finer levels.
        // Only the coarse texture is sampled — fine is the color attachment.
        for index in (0..bloom.levels.len().saturating_sub(1)).rev() {
            let coarse = &bloom.levels[index + 1];
            let fine = &bloom.levels[index];
            let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("yuyib bloom upsample bind group"),
                layout: &bloom_upsample_layout(device),
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&coarse.view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&bloom.sampler),
                    },
                ],
            });
            let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
                label: Some("yuyib bloom upsample"),
                color_attachments: &[Some(RenderPassColorAttachment {
                    view: &fine.view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: Operations {
                        load: LoadOp::Load,
                        store: StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            pass.set_pipeline(&bloom.upsample_pipeline);
            pass.set_bind_group(0, Some(&bg), &[]);
            pass.draw(0..3, 0..1);
        }
    }
}

fn create_bloom_resources(device: &Device, width: u32, height: u32) -> BloomResources {
    let mut levels = Vec::with_capacity(BLOOM_LEVELS);
    let mut level_w = (width / 2).max(1);
    let mut level_h = (height / 2).max(1);
    for level in 0..BLOOM_LEVELS {
        let texture = device.create_texture(&TextureDescriptor {
            label: Some("yuyib bloom level"),
            size: wgpu::Extent3d {
                width: level_w,
                height: level_h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: TextureDimension::D2,
            format: HDR_SCENE_FORMAT,
            usage: TextureUsages::RENDER_ATTACHMENT | TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&TextureViewDescriptor::default());
        levels.push(BloomLevel {
            _texture: texture,
            view,
        });
        let _ = level;
        level_w = (level_w / 2).max(1);
        level_h = (level_h / 2).max(1);
    }
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("yuyib bloom sampler"),
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        address_mode_w: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        mipmap_filter: wgpu::MipmapFilterMode::Nearest,
        ..Default::default()
    });
    let extract_params = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("yuyib bloom extract params"),
        size: 16,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let src_layout = bloom_src_layout(device);
    let bloom_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("yuyib bloom WGSL"),
        source: wgpu::ShaderSource::Wgsl(BLOOM_SHADER.into()),
    });
    let extract_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("yuyib bloom extract layout"),
        bind_group_layouts: &[Some(&src_layout)],
        immediate_size: 0,
    });
    let extract_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("yuyib bloom extract"),
        layout: Some(&extract_layout),
        vertex: VertexState {
            module: &bloom_shader,
            entry_point: Some("vertex_main"),
            compilation_options: PipelineCompilationOptions::default(),
            buffers: &[],
        },
        fragment: Some(FragmentState {
            module: &bloom_shader,
            entry_point: Some("extract_main"),
            compilation_options: PipelineCompilationOptions::default(),
            targets: &[Some(ColorTargetState {
                format: HDR_SCENE_FORMAT,
                blend: None,
                write_mask: ColorWrites::ALL,
            })],
        }),
        primitive: PrimitiveState::default(),
        depth_stencil: None,
        multisample: MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    });
    let downsample_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("yuyib bloom downsample"),
        layout: Some(&extract_layout),
        vertex: VertexState {
            module: &bloom_shader,
            entry_point: Some("vertex_main"),
            compilation_options: PipelineCompilationOptions::default(),
            buffers: &[],
        },
        fragment: Some(FragmentState {
            module: &bloom_shader,
            entry_point: Some("downsample_main"),
            compilation_options: PipelineCompilationOptions::default(),
            targets: &[Some(ColorTargetState {
                format: HDR_SCENE_FORMAT,
                blend: None,
                write_mask: ColorWrites::ALL,
            })],
        }),
        primitive: PrimitiveState::default(),
        depth_stencil: None,
        multisample: MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    });
    let upsample_layout = bloom_upsample_layout(device);
    let upsample_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("yuyib bloom upsample pipeline layout"),
        bind_group_layouts: &[Some(&upsample_layout)],
        immediate_size: 0,
    });
    let upsample_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("yuyib bloom upsample WGSL"),
        source: wgpu::ShaderSource::Wgsl(BLOOM_UPSAMPLE_SHADER.into()),
    });
    let upsample_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("yuyib bloom upsample"),
        layout: Some(&upsample_pipeline_layout),
        vertex: VertexState {
            module: &upsample_shader,
            entry_point: Some("vertex_main"),
            compilation_options: PipelineCompilationOptions::default(),
            buffers: &[],
        },
        fragment: Some(FragmentState {
            module: &upsample_shader,
            entry_point: Some("upsample_main"),
            compilation_options: PipelineCompilationOptions::default(),
            targets: &[Some(ColorTargetState {
                format: HDR_SCENE_FORMAT,
                blend: Some(wgpu::BlendState {
                    color: wgpu::BlendComponent {
                        src_factor: wgpu::BlendFactor::One,
                        dst_factor: wgpu::BlendFactor::One,
                        operation: wgpu::BlendOperation::Add,
                    },
                    alpha: wgpu::BlendComponent {
                        src_factor: wgpu::BlendFactor::One,
                        dst_factor: wgpu::BlendFactor::One,
                        operation: wgpu::BlendOperation::Add,
                    },
                }),
                write_mask: ColorWrites::ALL,
            })],
        }),
        primitive: PrimitiveState::default(),
        depth_stencil: None,
        multisample: MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    });
    BloomResources {
        levels,
        extract_pipeline,
        downsample_pipeline,
        upsample_pipeline,
        extract_params,
        sampler,
    }
}

fn create_fxaa_resources(
    device: &Device,
    width: u32,
    height: u32,
    surface_format: TextureFormat,
    _config: FxaaConfig,
) -> FxaaResources {
    let tone_mapped = device.create_texture(&TextureDescriptor {
        label: Some("yuyib FXAA tone-mapped colour"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: TextureDimension::D2,
        format: surface_format,
        usage: TextureUsages::RENDER_ATTACHMENT | TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let tone_mapped_view = tone_mapped.create_view(&TextureViewDescriptor::default());
    let parameters = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("yuyib FXAA parameters"),
        size: 16,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("yuyib FXAA sampler"),
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        address_mode_w: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        mipmap_filter: wgpu::MipmapFilterMode::Nearest,
        ..Default::default()
    });
    let layout = fxaa_bind_group_layout(device);
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("yuyib FXAA inputs"),
        layout: &layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: parameters.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(&tone_mapped_view),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::Sampler(&sampler),
            },
        ],
    });
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("yuyib FXAA WGSL"),
        source: wgpu::ShaderSource::Wgsl(FXAA_SHADER.into()),
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("yuyib FXAA pipeline layout"),
        bind_group_layouts: &[Some(&layout)],
        immediate_size: 0,
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("yuyib FXAA pipeline"),
        layout: Some(&pipeline_layout),
        vertex: VertexState {
            module: &shader,
            entry_point: Some("vertex_main"),
            compilation_options: PipelineCompilationOptions::default(),
            buffers: &[],
        },
        fragment: Some(FragmentState {
            module: &shader,
            entry_point: Some("fragment_main"),
            compilation_options: PipelineCompilationOptions::default(),
            targets: &[Some(ColorTargetState {
                format: surface_format,
                blend: None,
                write_mask: ColorWrites::ALL,
            })],
        }),
        primitive: PrimitiveState::default(),
        depth_stencil: None,
        multisample: MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    });
    FxaaResources {
        _tone_mapped: tone_mapped,
        tone_mapped_view,
        parameters,
        bind_group,
        pipeline,
    }
}

fn fxaa_bind_group_layout(device: &Device) -> BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("yuyib FXAA layout"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(16),
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    })
}

fn bloom_src_layout(device: &Device) -> BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("yuyib bloom src layout"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(16),
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    })
}

fn bloom_upsample_layout(device: &Device) -> BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("yuyib bloom upsample layout"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    })
}

fn resolve_bind_group_layout(device: &Device) -> BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("yuyib colour post-process layout"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(64),
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
        ],
    })
}

const POST_PROCESS_SHADER: &str = r"
struct Parameters {
    exposure_multiplier: f32,
    tone_mapper: u32,
    bloom_enabled: u32,
    bloom_intensity: f32,
    bloom_threshold: f32,
    bloom_soft_knee: f32,
    grade_enabled: u32,
    grade_contrast: f32,
    grade_saturation: f32,
    grade_temperature: f32,
    grade_tint: f32,
    _pad0: f32,
};

@group(0) @binding(0) var<uniform> parameters: Parameters;
@group(0) @binding(1) var hdr_scene: texture_2d<f32>;
@group(0) @binding(2) var bloom_tex: texture_2d<f32>;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
};

@vertex
fn vertex_main(@builtin(vertex_index) index: u32) -> VertexOutput {
    var positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    var output: VertexOutput;
    output.position = vec4<f32>(positions[index], 0.0, 1.0);
    return output;
}

fn aces_filmic(value: vec3<f32>) -> vec3<f32> {
    let a = 2.51;
    let b = 0.03;
    let c = 2.43;
    let d = 0.59;
    let e = 0.14;
    return clamp((value * (a * value + b)) / (value * (c * value + d) + e), vec3<f32>(0.0), vec3<f32>(1.0));
}

fn apply_grade(color: vec3<f32>) -> vec3<f32> {
    let mid = 0.5;
    var graded = (color - vec3<f32>(mid)) * parameters.grade_contrast + vec3<f32>(mid);
    let luma = dot(graded, vec3<f32>(0.2126, 0.7152, 0.0722));
    graded = mix(vec3<f32>(luma), graded, parameters.grade_saturation);
    graded.r = graded.r * (1.0 + parameters.grade_temperature * 0.35);
    graded.b = graded.b * (1.0 - parameters.grade_temperature * 0.35);
    graded.g = graded.g * (1.0 + parameters.grade_tint * 0.25);
    return clamp(graded, vec3<f32>(0.0), vec3<f32>(1.0));
}

@fragment
fn fragment_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let pixel = vec2<i32>(input.position.xy);
    var source = max(textureLoad(hdr_scene, pixel, 0).rgb, vec3<f32>(0.0));
    if (parameters.bloom_enabled != 0u) {
        let bloom_size = vec2<i32>(textureDimensions(bloom_tex, 0));
        let scene_size = vec2<i32>(textureDimensions(hdr_scene, 0));
        let bloom_pixel = vec2<i32>(
            clamp(i32(f32(pixel.x) * f32(bloom_size.x) / f32(scene_size.x)), 0, bloom_size.x - 1),
            clamp(i32(f32(pixel.y) * f32(bloom_size.y) / f32(scene_size.y)), 0, bloom_size.y - 1),
        );
        let glow = max(textureLoad(bloom_tex, bloom_pixel, 0).rgb, vec3<f32>(0.0));
        source = source + glow * parameters.bloom_intensity;
    }
    let exposed = source * parameters.exposure_multiplier;
    var mapped: vec3<f32>;
    switch parameters.tone_mapper {
        case 0u: { mapped = clamp(exposed, vec3<f32>(0.0), vec3<f32>(1.0)); }
        case 1u: { mapped = exposed / (vec3<f32>(1.0) + exposed); }
        default: { mapped = aces_filmic(exposed); }
    }
    if (parameters.grade_enabled != 0u) {
        mapped = apply_grade(mapped);
    }
    return vec4<f32>(mapped, 1.0);
}
";

const FXAA_SHADER: &str = r"
struct FxaaParams {
    inv_resolution: vec2<f32>,
    quality: u32,
    _pad0: u32,
};

@group(0) @binding(0) var<uniform> params: FxaaParams;
@group(0) @binding(1) var source_tex: texture_2d<f32>;
@group(0) @binding(2) var source_sampler: sampler;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vertex_main(@builtin(vertex_index) index: u32) -> VertexOutput {
    var positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    var output: VertexOutput;
    let pos = positions[index];
    output.position = vec4<f32>(pos, 0.0, 1.0);
    output.uv = pos * vec2<f32>(0.5, -0.5) + vec2<f32>(0.5, 0.5);
    return output;
}

fn luma(rgb: vec3<f32>) -> f32 {
    return dot(rgb, vec3<f32>(0.299, 0.587, 0.114));
}

fn sample_rgb(uv: vec2<f32>) -> vec3<f32> {
    return textureSampleLevel(source_tex, source_sampler, uv, 0.0).rgb;
}

@fragment
fn fragment_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let texel = params.inv_resolution;
    let rgb_m = sample_rgb(input.uv);
    let luma_m = luma(rgb_m);
    let luma_nw = luma(sample_rgb(input.uv + vec2<f32>(-texel.x, -texel.y)));
    let luma_ne = luma(sample_rgb(input.uv + vec2<f32>( texel.x, -texel.y)));
    let luma_sw = luma(sample_rgb(input.uv + vec2<f32>(-texel.x,  texel.y)));
    let luma_se = luma(sample_rgb(input.uv + vec2<f32>( texel.x,  texel.y)));

    let luma_min = min(luma_m, min(min(luma_nw, luma_ne), min(luma_sw, luma_se)));
    let luma_max = max(luma_m, max(max(luma_nw, luma_ne), max(luma_sw, luma_se)));
    let luma_range = luma_max - luma_min;

    var edge_threshold = 0.166;
    var edge_threshold_min = 0.0833;
    var subpix_trim = 0.25;
    if (params.quality == 0u) {
        edge_threshold = 0.333;
        edge_threshold_min = 0.125;
        subpix_trim = 0.5;
    } else if (params.quality == 2u) {
        edge_threshold = 0.125;
        edge_threshold_min = 0.0625;
        subpix_trim = 0.125;
    }

    if (luma_range < max(edge_threshold_min, luma_max * edge_threshold)) {
        return vec4<f32>(rgb_m, 1.0);
    }

    let luma_l = (luma_nw + luma_ne + luma_sw + luma_se) * 0.25;
    let range_l = abs(luma_l - luma_m);
    let blend_l = max(0.0, (range_l / luma_range) - subpix_trim) * (1.0 / (1.0 - subpix_trim));
    let blend = min(1.0, blend_l * blend_l);

    let dir_x = -((luma_nw + luma_ne) - (luma_sw + luma_se));
    let dir_y =  ((luma_nw + luma_sw) - (luma_ne + luma_se));
    let dir_reduce = max((luma_nw + luma_ne + luma_sw + luma_se) * 0.03125, 0.0078125);
    let rcp_dir_min = 1.0 / (min(abs(dir_x), abs(dir_y)) + dir_reduce);
    var dir = clamp(vec2<f32>(dir_x, dir_y) * rcp_dir_min, vec2<f32>(-8.0), vec2<f32>(8.0)) * texel;

    let rgb_a = 0.5 * (
        sample_rgb(input.uv + dir * (1.0 / 3.0 - 0.5)) +
        sample_rgb(input.uv + dir * (2.0 / 3.0 - 0.5))
    );
    let rgb_b = rgb_a * 0.5 + 0.25 * (
        sample_rgb(input.uv + dir * -0.5) +
        sample_rgb(input.uv + dir * 0.5)
    );
    let luma_b = luma(rgb_b);
    var rgb_out = rgb_b;
    if (luma_b < luma_min || luma_b > luma_max) {
        rgb_out = rgb_a;
    }
    rgb_out = mix(rgb_m, rgb_out, blend);
    return vec4<f32>(rgb_out, 1.0);
}
";

const BLOOM_SHADER: &str = r"
struct ExtractParams {
    threshold: f32,
    soft_knee: f32,
    _pad0: f32,
    _pad1: f32,
};

@group(0) @binding(0) var<uniform> params: ExtractParams;
@group(0) @binding(1) var source_tex: texture_2d<f32>;
@group(0) @binding(2) var source_sampler: sampler;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vertex_main(@builtin(vertex_index) index: u32) -> VertexOutput {
    var positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    var output: VertexOutput;
    let pos = positions[index];
    output.position = vec4<f32>(pos, 0.0, 1.0);
    output.uv = pos * vec2<f32>(0.5, -0.5) + vec2<f32>(0.5, 0.5);
    return output;
}

fn soft_threshold(color: vec3<f32>, threshold: f32, knee: f32) -> vec3<f32> {
    let brightness = max(max(color.r, color.g), color.b);
    let soft = brightness - threshold + knee;
    let soft_clamped = clamp(soft, 0.0, 2.0 * knee);
    let soft_factor = (soft_clamped * soft_clamped) / max(4.0 * knee + 0.00001, 0.00001);
    let contribution = max(brightness - threshold, soft_factor) / max(brightness, 0.00001);
    return color * max(contribution, 0.0);
}

@fragment
fn extract_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let color = max(textureSampleLevel(source_tex, source_sampler, input.uv, 0.0).rgb, vec3<f32>(0.0));
    let knee = params.threshold * params.soft_knee;
    return vec4<f32>(soft_threshold(color, params.threshold, knee), 1.0);
}

@fragment
fn downsample_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let texel = 1.0 / vec2<f32>(textureDimensions(source_tex, 0));
    var color = vec3<f32>(0.0);
    color += textureSampleLevel(source_tex, source_sampler, input.uv + texel * vec2<f32>(-1.0, -1.0), 0.0).rgb;
    color += textureSampleLevel(source_tex, source_sampler, input.uv + texel * vec2<f32>( 1.0, -1.0), 0.0).rgb;
    color += textureSampleLevel(source_tex, source_sampler, input.uv + texel * vec2<f32>(-1.0,  1.0), 0.0).rgb;
    color += textureSampleLevel(source_tex, source_sampler, input.uv + texel * vec2<f32>( 1.0,  1.0), 0.0).rgb;
    return vec4<f32>(color * 0.25, 1.0);
}
";

const BLOOM_UPSAMPLE_SHADER: &str = r"
@group(0) @binding(0) var coarse_tex: texture_2d<f32>;
@group(0) @binding(1) var source_sampler: sampler;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vertex_main(@builtin(vertex_index) index: u32) -> VertexOutput {
    var positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    var output: VertexOutput;
    let pos = positions[index];
    output.position = vec4<f32>(pos, 0.0, 1.0);
    output.uv = pos * vec2<f32>(0.5, -0.5) + vec2<f32>(0.5, 0.5);
    return output;
}

@fragment
fn upsample_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let texel = 1.0 / vec2<f32>(textureDimensions(coarse_tex, 0));
    var color = vec3<f32>(0.0);
    color += textureSampleLevel(coarse_tex, source_sampler, input.uv + texel * vec2<f32>(-1.0,  0.0), 0.0).rgb;
    color += textureSampleLevel(coarse_tex, source_sampler, input.uv + texel * vec2<f32>( 1.0,  0.0), 0.0).rgb;
    color += textureSampleLevel(coarse_tex, source_sampler, input.uv + texel * vec2<f32>( 0.0, -1.0), 0.0).rgb;
    color += textureSampleLevel(coarse_tex, source_sampler, input.uv + texel * vec2<f32>( 0.0,  1.0), 0.0).rgb;
    color += textureSampleLevel(coarse_tex, source_sampler, input.uv, 0.0).rgb * 2.0;
    return vec4<f32>(color * (1.0 / 6.0), 1.0);
}
";

#[cfg(test)]
mod tests {
    use super::{
        BloomConfig, ColorGradeConfig, ColorPostProcess, ColorPostProcessError, FxaaConfig,
        FxaaQuality, MAX_EXPOSURE_EV, MIN_EXPOSURE_EV, ToneMapping,
    };

    #[test]
    fn filmic_preset_is_explicit_and_neutral_exposure() {
        let config = ColorPostProcess::filmic();
        assert_eq!(config.exposure_ev(), 0.0);
        assert_eq!(config.exposure_multiplier(), 1.0);
        assert_eq!(config.tone_mapping(), ToneMapping::AcesFilmic);
        assert!(config.bloom().is_none());
    }

    #[test]
    fn photographic_stops_become_power_of_two_multiplier() {
        let config = ColorPostProcess::new(2.0, ToneMapping::Reinhard).expect("valid exposure");
        assert_eq!(config.exposure_multiplier(), 4.0);
    }

    #[test]
    fn exposure_rejects_non_finite_and_unbounded_values() {
        for exposure in [
            f32::NAN,
            f32::INFINITY,
            MIN_EXPOSURE_EV - 0.01,
            MAX_EXPOSURE_EV + 0.01,
        ] {
            assert!(matches!(
                ColorPostProcess::new(exposure, ToneMapping::AcesFilmic),
                Err(ColorPostProcessError::ExposureOutOfRange(value)) if value.is_nan() || value == exposure
            ));
        }
    }

    #[test]
    fn bloom_preset_attaches_to_filmic() {
        let config = ColorPostProcess::filmic().with_bloom(BloomConfig::street_city());
        let bloom = config.bloom().expect("bloom enabled");
        assert!(bloom.intensity() > 0.0);
        assert!(bloom.threshold() > 0.0);
    }

    #[test]
    fn bloom_rejects_invalid_knee() {
        assert_eq!(
            BloomConfig::new(1.0, 1.5, 0.1),
            Err(ColorPostProcessError::InvalidBloom)
        );
    }

    #[test]
    fn fxaa_preset_attaches_to_filmic() {
        let config = ColorPostProcess::filmic().with_fxaa(FxaaConfig::street_city());
        let fxaa = config.fxaa().expect("fxaa enabled");
        assert_eq!(fxaa.quality(), FxaaQuality::Medium);
        assert!(config.bloom().is_none());
    }

    #[test]
    fn color_grade_preset_attaches_to_filmic() {
        let config = ColorPostProcess::filmic().with_color_grade(ColorGradeConfig::street_city());
        let grade = config.color_grade().expect("grade enabled");
        assert!(grade.contrast() > 1.0);
        assert!(grade.temperature() < 0.0);
    }

    #[test]
    fn color_grade_rejects_out_of_range() {
        assert_eq!(
            ColorGradeConfig::new(3.0, 1.0, 0.0, 0.0),
            Err(ColorPostProcessError::InvalidGrade)
        );
    }
}
