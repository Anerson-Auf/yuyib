//! WGPU rendering foundation shared by Yuyib's future 2D, 3D and UI phases.
//!
//! This crate owns a presentable [`Renderer`] surface path and a headless
//! [`OffscreenRenderer`] for smoke capture / reference screenshots. It
//! deliberately has no dependency on ECS, native UI, asset import, or gameplay
//! code.

#![forbid(unsafe_code)]

mod graph;
mod offscreen;
mod post_process;
mod readback;

pub use graph::{
    BoxedRenderPassError, RenderGraph, RenderGraphBuildError, RenderGraphExecution,
    RenderGraphExecutionError, RenderPassDescriptor as GraphPassDescriptor, RenderPassId,
    RenderPassTiming, RenderPhase, RenderResourceId, RenderResourceIdError,
};
pub use offscreen::{
    CapturedFrameRgba8, MAX_OFFSCREEN_DIMENSION, OFFSCREEN_COLOR_FORMAT, OffscreenRenderer,
    OffscreenRendererInitError,
};
pub use post_process::{
    BLOOM_LEVELS, BloomConfig, ColorGradeConfig, ColorPostProcess, ColorPostProcessError,
    FxaaConfig, FxaaQuality, HDR_SCENE_FORMAT, MAX_EXPOSURE_EV, MIN_EXPOSURE_EV, ToneMapping,
};
pub use readback::{
    TEXTURE_READBACK_BYTES_PER_ROW_ALIGNMENT, TextureReadbackError, TextureReadbackFormat,
    non_zero_extent, padded_bytes_per_row, read_texture_rgba8,
};

use std::{collections::HashMap, error::Error, fmt, sync::Arc};

pub use wgpu;
use wgpu::{
    Backends, Color, CommandEncoderDescriptor, CurrentSurfaceTexture, Device, DeviceDescriptor,
    DownlevelFlags, Extent3d, Features, Instance, InstanceDescriptor, Limits, LoadOp, Operations,
    PowerPreference, Queue, RenderPassColorAttachment, RenderPassDepthStencilAttachment,
    RenderPassDescriptor, RequestAdapterOptions, StoreOp, Surface, SurfaceConfiguration,
    TextureDescriptor, TextureDimension, TextureFormat, TextureUsages, TextureView,
    TextureViewDescriptor,
};
use yuyib_platform::{Window, winit};

/// Minimum bind-group slots for textured PBR + specular IBL + directional shadow
/// (`@group(0..=4)`). wgpu's default downlevel limit is 4 (indices `0..=3`).
pub const REQUIRED_MAX_BIND_GROUPS: u32 = 5;

/// Device limits required by Yuyib's shared 3D shading paths.
#[must_use]
pub fn required_device_limits() -> Limits {
    let mut limits = Limits::default();
    limits.max_bind_groups = limits.max_bind_groups.max(REQUIRED_MAX_BIND_GROUPS);
    limits
}

/// RGBA clear color in linear color space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClearColor {
    /// Red channel.
    pub red: f64,
    /// Green channel.
    pub green: f64,
    /// Blue channel.
    pub blue: f64,
    /// Alpha channel.
    pub alpha: f64,
}

impl ClearColor {
    /// Creates a clear color from linear RGBA channels.
    #[must_use]
    pub const fn linear(red: f64, green: f64, blue: f64, alpha: f64) -> Self {
        Self {
            red,
            green,
            blue,
            alpha,
        }
    }
}

impl Default for ClearColor {
    fn default() -> Self {
        Self::linear(0.025, 0.035, 0.06, 1.0)
    }
}

impl From<ClearColor> for Color {
    fn from(value: ClearColor) -> Self {
        Self {
            r: value.red,
            g: value.green,
            b: value.blue,
            a: value.alpha,
        }
    }
}

/// Outcome of a render attempt that did not necessarily draw a frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum RenderStatus {
    /// A frame was submitted and presented.
    Presented,
    /// Rendering was skipped because the window has a zero-sized client area.
    SkippedMinimized,
    /// WGPU timed out while acquiring a presentation texture.
    SkippedTimeout,
    /// The window is occluded; rendering can be retried later.
    SkippedOccluded,
    /// The surface became outdated and was reconfigured before drawing.
    Reconfigured,
    /// The surface was lost and recreated before drawing.
    SurfaceRecreated,
    /// The surface was lost and could not be recreated by the renderer.
    SurfaceLost,
}

/// Observable lifecycle state of the presentation surface.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum RendererState {
    /// The surface and GPU device are ready to render.
    Ready,
    /// The window has no drawable client area.
    Minimized,
    /// The renderer is rebuilding a lost or outdated surface.
    Recovering,
    /// Automatic surface recovery failed.
    Failed,
}

/// Failure during GPU device or surface initialisation.
#[derive(Debug)]
#[non_exhaustive]
pub enum RendererInitError {
    /// The native window could not be used as a GPU presentation surface.
    CreateSurface(wgpu::CreateSurfaceError),
    /// No available adapter supports the requested window surface.
    RequestAdapter(wgpu::RequestAdapterError),
    /// The selected adapter rejected the requested device defaults.
    RequestDevice(wgpu::RequestDeviceError),
    /// The adapter exposes no usable default configuration for this surface.
    SurfaceUnsupported,
}

impl fmt::Display for RendererInitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CreateSurface(error) => {
                write!(formatter, "could not create GPU surface: {error}")
            }
            Self::RequestAdapter(error) => {
                write!(formatter, "could not select GPU adapter: {error}")
            }
            Self::RequestDevice(error) => write!(formatter, "could not create GPU device: {error}"),
            Self::SurfaceUnsupported => {
                formatter.write_str("GPU adapter does not support this surface")
            }
        }
    }
}

impl Error for RendererInitError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::CreateSurface(error) => Some(error),
            Self::RequestAdapter(error) => Some(error),
            Self::RequestDevice(error) => Some(error),
            Self::SurfaceUnsupported => None,
        }
    }
}

/// Failure reported by WGPU while acquiring a presentation texture.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SurfaceValidationError;

impl fmt::Display for SurfaceValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("WGPU rejected the surface configuration")
    }
}

impl Error for SurfaceValidationError {}

/// A presentable GPU surface with deliberate low-level WGPU access.
pub struct Renderer {
    instance: Instance,
    window: Arc<winit::window::Window>,
    surface: Surface<'static>,
    device: Device,
    queue: Queue,
    configuration: SurfaceConfiguration,
    depth: DepthAttachment,
    viewport_depths: HashMap<RenderViewport, Arc<DepthAttachment>>,
    minimized: bool,
    reconfigure_pending: bool,
    state: RendererState,
    color_post_process: Option<ColorPostProcess>,
    post_process_resources: Option<post_process::PostProcessResources>,
    anisotropic_filtering_supported: bool,
}

/// The private depth texture paired with a configured presentation surface.
///
/// It owns the WGPU texture while exposing only its view to [`RenderFrame`].
/// This prevents a rendering callback from retaining a stale attachment across a
/// resize or surface reconfiguration.
struct DepthAttachment {
    _texture: wgpu::Texture,
    view: TextureView,
}

const MAX_CACHED_VIEWPORT_DEPTH_ATTACHMENTS: usize = 4;

impl DepthAttachment {
    /// The format used for Yuyib's per-surface depth attachment.
    const FORMAT: TextureFormat = TextureFormat::Depth32Float;

    fn new(device: &Device, width: u32, height: u32) -> Self {
        let texture = device.create_texture(&TextureDescriptor {
            label: Some("yuyib surface depth"),
            size: Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: TextureDimension::D2,
            format: Self::FORMAT,
            usage: TextureUsages::RENDER_ATTACHMENT | TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&TextureViewDescriptor::default());
        Self {
            _texture: texture,
            view,
        }
    }
}

/// A physical-pixel draw rectangle inside one presentation surface.
///
/// The rectangle is renderer-neutral authoring data until
/// [`RenderFrame::with_viewport`] validates it against the current surface.
/// Keeping it explicit lets editor panels share one native window without
/// teaching scene renderers about docking or `WebView` ownership.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct RenderViewport {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

impl RenderViewport {
    /// Creates a non-empty physical rectangle.
    ///
    /// # Errors
    ///
    /// Returns [`RenderViewportError::Empty`] for a zero dimension and
    /// [`RenderViewportError::Overflow`] when either far edge cannot be
    /// represented as `u32`.
    pub fn new(x: u32, y: u32, width: u32, height: u32) -> Result<Self, RenderViewportError> {
        if width == 0 || height == 0 {
            return Err(RenderViewportError::Empty);
        }
        x.checked_add(width)
            .and_then(|_| y.checked_add(height))
            .ok_or(RenderViewportError::Overflow)?;
        Ok(Self {
            x,
            y,
            width,
            height,
        })
    }

    /// Converts finite logical bounds to one canonical physical-pixel rectangle.
    ///
    /// Origins and far edges are rounded independently. This prevents adjacent
    /// native/WebView panels from accumulating a one-pixel gap at fractional
    /// DPI scales.
    ///
    /// # Errors
    ///
    /// Rejects non-finite/negative bounds, a non-positive scale, conversion
    /// overflow, an empty rounded rectangle, or a rectangle outside the surface.
    pub fn from_logical(
        x: f64,
        y: f64,
        width: f64,
        height: f64,
        scale: f64,
        surface: [u32; 2],
    ) -> Result<Self, RenderViewportError> {
        if !x.is_finite()
            || !y.is_finite()
            || !width.is_finite()
            || !height.is_finite()
            || !scale.is_finite()
            || x < 0.0
            || y < 0.0
            || width <= 0.0
            || height <= 0.0
            || scale <= 0.0
        {
            return Err(RenderViewportError::InvalidLogicalBounds);
        }
        let to_pixel = |value: f64| -> Result<u32, RenderViewportError> {
            let value = (value * scale).round();
            if !(0.0..=f64::from(u32::MAX)).contains(&value) {
                return Err(RenderViewportError::Overflow);
            }
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            Ok(value as u32)
        };
        let left = to_pixel(x)?;
        let top = to_pixel(y)?;
        let right = to_pixel(x + width)?;
        let bottom = to_pixel(y + height)?;
        let viewport = Self::new(
            left,
            top,
            right
                .checked_sub(left)
                .ok_or(RenderViewportError::Overflow)?,
            bottom
                .checked_sub(top)
                .ok_or(RenderViewportError::Overflow)?,
        )?;
        viewport.validate_within(surface)?;
        Ok(viewport)
    }

    const fn full(size: [u32; 2]) -> Self {
        Self {
            x: 0,
            y: 0,
            width: size[0],
            height: size[1],
        }
    }

    /// Returns the horizontal origin in physical pixels.
    #[must_use]
    pub const fn x(self) -> u32 {
        self.x
    }

    /// Returns the vertical origin in physical pixels.
    #[must_use]
    pub const fn y(self) -> u32 {
        self.y
    }

    /// Returns the rectangle width in physical pixels.
    #[must_use]
    pub const fn width(self) -> u32 {
        self.width
    }

    /// Returns the rectangle height in physical pixels.
    #[must_use]
    pub const fn height(self) -> u32 {
        self.height
    }

    /// Converts a global physical pointer position to viewport-local pixels.
    #[must_use]
    pub fn local_position(self, x: f64, y: f64) -> Option<[f64; 2]> {
        let local_x = x - f64::from(self.x);
        let local_y = y - f64::from(self.y);
        (local_x >= 0.0
            && local_y >= 0.0
            && local_x < f64::from(self.width)
            && local_y < f64::from(self.height))
        .then_some([local_x, local_y])
    }

    fn validate_within(self, surface: [u32; 2]) -> Result<(), RenderViewportError> {
        let right = self
            .x
            .checked_add(self.width)
            .ok_or(RenderViewportError::Overflow)?;
        let bottom = self
            .y
            .checked_add(self.height)
            .ok_or(RenderViewportError::Overflow)?;
        if right > surface[0] || bottom > surface[1] {
            return Err(RenderViewportError::OutsideSurface {
                viewport: self,
                surface,
            });
        }
        Ok(())
    }

    fn validate_within_parent(self, parent: Self) -> Result<(), RenderViewportError> {
        let right = self
            .x
            .checked_add(self.width)
            .ok_or(RenderViewportError::Overflow)?;
        let bottom = self
            .y
            .checked_add(self.height)
            .ok_or(RenderViewportError::Overflow)?;
        let parent_right = parent
            .x
            .checked_add(parent.width)
            .ok_or(RenderViewportError::Overflow)?;
        let parent_bottom = parent
            .y
            .checked_add(parent.height)
            .ok_or(RenderViewportError::Overflow)?;
        if self.x < parent.x || self.y < parent.y || right > parent_right || bottom > parent_bottom
        {
            return Err(RenderViewportError::OutsideParent {
                viewport: self,
                parent,
            });
        }
        Ok(())
    }
}

/// Invalid scoped presentation viewport.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RenderViewportError {
    /// Logical bounds or DPI scale are non-finite or non-positive.
    InvalidLogicalBounds,
    /// Width or height was zero.
    Empty,
    /// A rectangle edge overflowed `u32`.
    Overflow,
    /// The rectangle extends beyond the current presentation surface.
    OutsideSurface {
        /// Rectangle requested by the caller.
        viewport: RenderViewport,
        /// Current physical surface dimensions.
        surface: [u32; 2],
    },
    /// A nested rectangle extends beyond its parent viewport.
    OutsideParent {
        /// Rectangle requested by the caller.
        viewport: RenderViewport,
        /// Active parent viewport.
        parent: RenderViewport,
    },
}

impl fmt::Display for RenderViewportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLogicalBounds => formatter
                .write_str("logical viewport bounds and DPI scale must be finite and positive"),
            Self::Empty => formatter.write_str("render viewport dimensions must be non-zero"),
            Self::Overflow => formatter.write_str("render viewport edge overflowed u32"),
            Self::OutsideSurface { viewport, surface } => write!(
                formatter,
                "render viewport {viewport:?} extends beyond surface {surface:?}"
            ),
            Self::OutsideParent { viewport, parent } => write!(
                formatter,
                "render viewport {viewport:?} extends beyond parent viewport {parent:?}"
            ),
        }
    }
}

impl Error for RenderViewportError {}

fn apply_render_viewport(pass: &mut wgpu::RenderPass<'_>, viewport: RenderViewport) {
    // WGPU's viewport API is float-based. The source values are already
    // validated physical u32 surface coordinates.
    #[allow(clippy::cast_precision_loss)]
    pass.set_viewport(
        viewport.x as f32,
        viewport.y as f32,
        viewport.width as f32,
        viewport.height as f32,
        0.0,
        1.0,
    );
    pass.set_scissor_rect(viewport.x, viewport.y, viewport.width, viewport.height);
}

/// The exclusive command-recording boundary of one presentable frame.
///
/// The renderer owns texture acquisition, submission and presentation. A frame
/// may encode passes but cannot retain the surface texture or present it.
pub struct RenderFrame<'frame> {
    device: &'frame Device,
    queue: &'frame Queue,
    encoder: &'frame mut wgpu::CommandEncoder,
    surface_view: &'frame wgpu::TextureView,
    width: u32,
    height: u32,
    viewport: RenderViewport,
    format: wgpu::TextureFormat,
    depth_view: &'frame TextureView,
    viewport_depths: &'frame mut HashMap<RenderViewport, Arc<DepthAttachment>>,
    anisotropic_filtering_supported: bool,
}

impl RenderFrame<'_> {
    /// Returns the WGPU device used by this frame.
    #[must_use]
    pub const fn device(&self) -> &Device {
        self.device
    }

    /// Returns the WGPU queue used by this frame.
    #[must_use]
    pub const fn queue(&self) -> &Queue {
        self.queue
    }

    /// Returns whether the adapter reports active anisotropic sampling.
    #[must_use]
    pub const fn supports_anisotropic_filtering(&self) -> bool {
        self.anisotropic_filtering_supported
    }

    /// Returns the physical presentation size in pixels.
    #[must_use]
    pub const fn surface_size(&self) -> [u32; 2] {
        [self.width, self.height]
    }

    /// Returns the active draw-region dimensions in physical pixels.
    ///
    /// This equals [`Self::surface_size`] for an ordinary frame. Inside
    /// [`Self::with_viewport`] it returns the scoped viewport size so camera
    /// projection and screen/world conversion use the visible editor region
    /// rather than the complete presentation surface.
    #[must_use]
    pub const fn draw_size(&self) -> [u32; 2] {
        [self.viewport.width, self.viewport.height]
    }

    /// Returns the active physical draw rectangle.
    #[must_use]
    pub const fn viewport(&self) -> RenderViewport {
        self.viewport
    }

    /// Runs rendering code inside one validated presentation sub-rectangle.
    ///
    /// Surface passes recorded by the scoped frame automatically set matching
    /// WGPU viewport and scissor state before invoking their callback. The
    /// nested frame retains the same device, queue, attachments and command
    /// encoder; it cannot outlive this call or present independently.
    ///
    /// # Errors
    ///
    /// Returns [`RenderViewportError`] when the rectangle is empty, overflows,
    /// or extends beyond the current presentation surface.
    pub fn with_viewport<Result>(
        &mut self,
        viewport: RenderViewport,
        render: impl FnOnce(&mut RenderFrame<'_>) -> Result,
    ) -> std::result::Result<Result, RenderViewportError> {
        viewport.validate_within([self.width, self.height])?;
        viewport.validate_within_parent(self.viewport)?;
        if !self.viewport_depths.contains_key(&viewport)
            && self.viewport_depths.len() >= MAX_CACHED_VIEWPORT_DEPTH_ATTACHMENTS
        {
            self.viewport_depths.clear();
        }
        let depth = Arc::clone(self.viewport_depths.entry(viewport).or_insert_with(|| {
            Arc::new(DepthAttachment::new(self.device, self.width, self.height))
        }));
        let mut nested = RenderFrame {
            device: self.device,
            queue: self.queue,
            encoder: &mut *self.encoder,
            surface_view: self.surface_view,
            width: self.width,
            height: self.height,
            viewport,
            format: self.format,
            depth_view: &depth.view,
            viewport_depths: &mut *self.viewport_depths,
            anisotropic_filtering_supported: self.anisotropic_filtering_supported,
        };
        Ok(render(&mut nested))
    }

    /// Returns the presentation texture format selected for this frame.
    ///
    /// GPU helpers that create a render pipeline inside an application
    /// callback should use this format, rather than guessing a platform
    /// default. The format remains valid for the lifetime of this frame only.
    #[must_use]
    pub const fn surface_format(&self) -> wgpu::TextureFormat {
        self.format
    }

    /// Returns the depth texture format paired with this surface frame.
    ///
    /// Pipelines recorded through [`Self::with_surface_pass_with_depth`] must
    /// use this format in their `depth_stencil` state.
    #[must_use]
    pub const fn depth_format(&self) -> TextureFormat {
        DepthAttachment::FORMAT
    }

    /// Sampleable camera-depth view for post effects (SSAO, soft particles).
    ///
    /// Valid only for the duration of this [`RenderFrame`]. Do not retain across
    /// frames. The texture is `Depth32Float` with `TEXTURE_BINDING`; sample it
    /// from a colour-only pass (not while it is also bound as a depth attachment).
    #[must_use]
    pub const fn camera_depth_view(&self) -> &TextureView {
        self.depth_view
    }

    /// Records a pass targeting the presentation surface.
    ///
    /// The first phase normally uses `LoadOp::Clear`; later phases use
    /// `LoadOp::Load` to compose over existing results.
    pub fn with_surface_pass(
        &mut self,
        load: LoadOp<Color>,
        record: impl FnOnce(&mut wgpu::RenderPass<'_>),
    ) {
        let mut pass = self.encoder.begin_render_pass(&RenderPassDescriptor {
            label: Some("yuyib surface pass"),
            color_attachments: &[Some(RenderPassColorAttachment {
                view: self.surface_view,
                depth_slice: None,
                resolve_target: None,
                ops: Operations {
                    load,
                    store: StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        apply_render_viewport(&mut pass, self.viewport);
        record(&mut pass);
    }

    /// Records a surface pass with the frame-local depth attachment enabled.
    ///
    /// `depth_load` controls the depth part of the pass: use
    /// `LoadOp::Clear(1.0)` for the first depth-writing phase and
    /// `LoadOp::Load` for a later compatible phase. Depth values follow WGPU's
    /// normalized `0.0..=1.0` range; a conventional camera clears to `1.0` and
    /// uses a `Less` comparison.
    ///
    /// The renderer recreates this attachment whenever it reconfigures the
    /// presentation surface. Do not cache bind groups or pipelines that depend
    /// on a different depth format; obtain the format from
    /// [`Self::depth_format`] while building the pipeline.
    ///
    /// For a color-only pass, use [`Self::with_surface_pass`] instead. That is
    /// the explicit opt-out for 2D/UI phases which should not bind or mutate
    /// depth state.
    pub fn with_surface_pass_with_depth(
        &mut self,
        color_load: LoadOp<Color>,
        depth_load: LoadOp<f32>,
        record: impl FnOnce(&mut wgpu::RenderPass<'_>),
    ) {
        let mut pass = self.encoder.begin_render_pass(&RenderPassDescriptor {
            label: Some("yuyib surface pass with depth"),
            color_attachments: &[Some(RenderPassColorAttachment {
                view: self.surface_view,
                depth_slice: None,
                resolve_target: None,
                ops: Operations {
                    load: color_load,
                    store: StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(RenderPassDepthStencilAttachment {
                view: self.depth_view,
                depth_ops: Some(Operations {
                    load: depth_load,
                    store: StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            ..Default::default()
        });
        apply_render_viewport(&mut pass, self.viewport);
        record(&mut pass);
    }

    /// Records a depth-only pass into a caller-owned depth attachment.
    ///
    /// Use this for shadow maps and other off-surface depth targets. The pass
    /// binds **no** colour attachment, so it cannot clear or overwrite the
    /// presentation surface. `depth_view` must match [`Self::depth_format`]
    /// (`Depth32Float`) and remain valid for the duration of `record`.
    ///
    /// Prefer `LoadOp::Clear(1.0)` when beginning a shadow map, then
    /// `LoadOp::Load` for additional caster draws that share the same target.
    pub fn with_depth_only_pass(
        &mut self,
        depth_view: &TextureView,
        depth_load: LoadOp<f32>,
        record: impl FnOnce(&mut wgpu::RenderPass<'_>),
    ) {
        let mut pass = self.encoder.begin_render_pass(&RenderPassDescriptor {
            label: Some("yuyib depth-only pass"),
            color_attachments: &[],
            depth_stencil_attachment: Some(RenderPassDepthStencilAttachment {
                view: depth_view,
                depth_ops: Some(Operations {
                    load: depth_load,
                    store: StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            ..Default::default()
        });
        record(&mut pass);
    }

    /// Records a colour-only pass into a caller-owned colour attachment.
    ///
    /// Use this for SSAO extract buffers and other off-surface colour targets.
    /// The pass does not touch the presentation depth attachment.
    pub fn with_color_only_pass(
        &mut self,
        color_view: &TextureView,
        color_load: LoadOp<Color>,
        record: impl FnOnce(&mut wgpu::RenderPass<'_>),
    ) {
        let mut pass = self.encoder.begin_render_pass(&RenderPassDescriptor {
            label: Some("yuyib colour-only pass"),
            color_attachments: &[Some(RenderPassColorAttachment {
                view: color_view,
                depth_slice: None,
                resolve_target: None,
                ops: Operations {
                    load: color_load,
                    store: StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            ..Default::default()
        });
        record(&mut pass);
    }
}

impl Renderer {
    /// Creates and configures a GPU surface synchronously for simple apps and demos.
    ///
    /// Production hosts with their own async lifecycle may prefer
    /// [`Self::new_async`]. This call blocks only while an adapter and device are
    /// selected; it must not be made from a per-frame update callback.
    ///
    /// # Errors
    ///
    /// Returns [`RendererInitError`] when the window cannot obtain a compatible
    /// WGPU surface, adapter, device or presentation configuration.
    pub fn new(window: &Window) -> Result<Self, RendererInitError> {
        pollster::block_on(Self::new_async(window))
    }

    /// Creates a GPU surface without imposing an executor.
    ///
    /// # Errors
    ///
    /// Returns [`RendererInitError`] when the window cannot obtain a compatible
    /// WGPU surface, adapter, device or presentation configuration.
    pub async fn new_async(window: &Window) -> Result<Self, RendererInitError> {
        let mut instance_descriptor = InstanceDescriptor::new_without_display_handle();
        instance_descriptor.backends = Backends::DX12 | Backends::VULKAN;
        let instance = Instance::new(instance_descriptor);
        let native_window = window.raw().clone();
        let surface = instance
            .create_surface(native_window.clone())
            .map_err(RendererInitError::CreateSurface)?;
        let adapter = instance
            .request_adapter(&RequestAdapterOptions {
                power_preference: PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
                ..Default::default()
            })
            .await
            .map_err(RendererInitError::RequestAdapter)?;
        let anisotropic_filtering_supported = adapter
            .get_downlevel_capabilities()
            .flags
            .contains(DownlevelFlags::ANISOTROPIC_FILTERING);
        let (device, queue) = adapter
            .request_device(&DeviceDescriptor {
                label: Some("yuyib primary device"),
                required_features: Features::empty(),
                required_limits: required_device_limits(),
                ..Default::default()
            })
            .await
            .map_err(RendererInitError::RequestDevice)?;

        let size = window.physical_size();
        let minimized = size.width == 0 || size.height == 0;
        let configuration = surface
            .get_default_config(&adapter, size.width.max(1), size.height.max(1))
            .ok_or(RendererInitError::SurfaceUnsupported)?;
        if !minimized {
            surface.configure(&device, &configuration);
        }
        let depth = DepthAttachment::new(&device, configuration.width, configuration.height);

        Ok(Self {
            instance,
            window: native_window,
            surface,
            device,
            queue,
            configuration,
            depth,
            viewport_depths: HashMap::new(),
            minimized,
            reconfigure_pending: minimized,
            state: if minimized {
                RendererState::Minimized
            } else {
                RendererState::Ready
            },
            color_post_process: None,
            post_process_resources: None,
            anisotropic_filtering_supported,
        })
    }

    /// Records a resize. The surface is configured before the next acquisition.
    pub fn resize(&mut self, width: u32, height: u32) {
        self.minimized = width == 0 || height == 0;
        if self.minimized {
            self.state = RendererState::Minimized;
            return;
        }
        self.configuration.width = width;
        self.configuration.height = height;
        self.reconfigure_pending = true;
        self.state = RendererState::Recovering;
    }

    /// Returns the current presentation lifecycle state.
    #[must_use]
    pub const fn state(&self) -> RendererState {
        self.state
    }

    /// Returns the format of the depth attachment recreated with this surface.
    ///
    /// A render pipeline intended for
    /// [`RenderFrame::with_surface_pass_with_depth`] must use this exact format
    /// in its `depth_stencil` state. It is stable for the lifetime of this
    /// renderer; the attachment view itself remains frame-local.
    #[must_use]
    pub const fn depth_format(&self) -> TextureFormat {
        DepthAttachment::FORMAT
    }

    /// Returns the color format expected by pipelines created outside a frame.
    ///
    /// This is the presentation format normally and [`HDR_SCENE_FORMAT`] while
    /// color post-processing is enabled.
    #[must_use]
    pub const fn color_target_format(&self) -> TextureFormat {
        if self.color_post_process.is_some() {
            HDR_SCENE_FORMAT
        } else {
            self.configuration.format
        }
    }

    /// Enables or disables renderer-owned HDR exposure and tone mapping.
    ///
    /// Set this before constructing cached render pipelines. Frame-local
    /// renderers automatically observe the active target format.
    pub fn set_color_post_process(&mut self, config: Option<ColorPostProcess>) {
        let previous = self.color_post_process;
        let target_format_changes = previous.is_some() != config.is_some();
        let bloom_changes = previous.and_then(ColorPostProcess::bloom).is_some()
            != config.and_then(ColorPostProcess::bloom).is_some();
        let fxaa_changes = previous.and_then(ColorPostProcess::fxaa).is_some()
            != config.and_then(ColorPostProcess::fxaa).is_some();
        self.color_post_process = config;
        if target_format_changes || bloom_changes || fxaa_changes {
            self.post_process_resources = None;
        }
    }

    /// Returns the active opt-in color post-processing policy.
    #[must_use]
    pub const fn color_post_process(&self) -> Option<ColorPostProcess> {
        self.color_post_process
    }

    /// Returns whether the selected adapter reports active anisotropic sampling.
    #[must_use]
    pub const fn supports_anisotropic_filtering(&self) -> bool {
        self.anisotropic_filtering_supported
    }

    /// Renders and presents a single clear pass.
    ///
    /// # Errors
    ///
    /// Returns [`SurfaceValidationError`] when WGPU rejects the active surface
    /// configuration during texture acquisition.
    pub fn clear(&mut self, color: ClearColor) -> Result<RenderStatus, SurfaceValidationError> {
        self.render_frame(color, |_| {})
    }

    /// Acquires one presentation texture, records passes, then submits and presents it.
    ///
    /// # Errors
    ///
    /// Returns [`SurfaceValidationError`] when WGPU rejects the active surface
    /// configuration during texture acquisition.
    #[allow(clippy::too_many_lines)]
    pub fn render_frame(
        &mut self,
        clear: ClearColor,
        record: impl FnOnce(&mut RenderFrame<'_>),
    ) -> Result<RenderStatus, SurfaceValidationError> {
        if self.minimized {
            self.state = RendererState::Minimized;
            return Ok(RenderStatus::SkippedMinimized);
        }
        if self.reconfigure_pending {
            self.configure();
            return Ok(RenderStatus::Reconfigured);
        }

        let (surface_texture, reconfigure_after_present) = match self.surface.get_current_texture()
        {
            CurrentSurfaceTexture::Success(texture) => (texture, false),
            CurrentSurfaceTexture::Suboptimal(texture) => (texture, true),
            CurrentSurfaceTexture::Timeout => return Ok(RenderStatus::SkippedTimeout),
            CurrentSurfaceTexture::Occluded => return Ok(RenderStatus::SkippedOccluded),
            CurrentSurfaceTexture::Outdated => {
                self.configure();
                return Ok(RenderStatus::Reconfigured);
            }
            CurrentSurfaceTexture::Lost => {
                self.state = RendererState::Recovering;
                let Ok(surface) = self.instance.create_surface(self.window.clone()) else {
                    self.state = RendererState::Failed;
                    return Ok(RenderStatus::SurfaceLost);
                };
                self.surface = surface;
                self.configure();
                return Ok(RenderStatus::SurfaceRecreated);
            }
            CurrentSurfaceTexture::Validation => {
                self.state = RendererState::Failed;
                return Err(SurfaceValidationError);
            }
        };
        let view = surface_texture
            .texture
            .create_view(&TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&CommandEncoderDescriptor {
                label: Some("yuyib frame"),
            });
        if self.color_post_process.is_some()
            && self
                .post_process_resources
                .as_ref()
                .is_none_or(|resources| {
                    !resources.matches(
                        self.configuration.width,
                        self.configuration.height,
                        self.configuration.format,
                        self.color_post_process
                            .and_then(ColorPostProcess::bloom)
                            .is_some(),
                        self.color_post_process
                            .and_then(ColorPostProcess::fxaa)
                            .is_some(),
                    )
                })
        {
            self.post_process_resources = Some(post_process::PostProcessResources::new(
                &self.device,
                self.configuration.width,
                self.configuration.height,
                self.configuration.format,
                self.color_post_process.and_then(ColorPostProcess::bloom),
                self.color_post_process.and_then(ColorPostProcess::fxaa),
            ));
        }
        if let (Some(config), Some(resources)) =
            (self.color_post_process, &self.post_process_resources)
        {
            resources.write_parameters(&self.queue, config);
        }
        {
            let color_view = self
                .post_process_resources
                .as_ref()
                .map_or(&view, post_process::PostProcessResources::hdr_view);
            let color_format = if self.post_process_resources.is_some() {
                HDR_SCENE_FORMAT
            } else {
                self.configuration.format
            };
            let mut frame = RenderFrame {
                device: &self.device,
                queue: &self.queue,
                encoder: &mut encoder,
                surface_view: color_view,
                width: self.configuration.width,
                height: self.configuration.height,
                viewport: RenderViewport::full([
                    self.configuration.width,
                    self.configuration.height,
                ]),
                format: color_format,
                depth_view: &self.depth.view,
                viewport_depths: &mut self.viewport_depths,
                anisotropic_filtering_supported: self.anisotropic_filtering_supported,
            };
            frame.with_surface_pass(LoadOp::Clear(clear.into()), |_| {});
            record(&mut frame);
        }
        if let (Some(config), Some(resources)) =
            (self.color_post_process, &self.post_process_resources)
        {
            resources.resolve(&mut encoder, &self.device, &view, config);
        }
        self.queue.submit([encoder.finish()]);
        self.window.pre_present_notify();
        self.queue.present(surface_texture);
        self.reconfigure_pending = reconfigure_after_present;
        self.state = RendererState::Ready;
        Ok(RenderStatus::Presented)
    }

    /// Calls `operation` with borrow-only access to the selected GPU objects.
    ///
    /// The renderer retains ownership of surface lifecycle and presentation.
    pub fn with_raw_gpu<Result>(
        &self,
        operation: impl FnOnce(&Device, &Queue, &SurfaceConfiguration) -> Result,
    ) -> Result {
        operation(&self.device, &self.queue, &self.configuration)
    }

    fn configure(&mut self) {
        self.state = RendererState::Recovering;
        self.surface.configure(&self.device, &self.configuration);
        self.depth = DepthAttachment::new(
            &self.device,
            self.configuration.width,
            self.configuration.height,
        );
        self.viewport_depths.clear();
        self.reconfigure_pending = false;
        self.state = RendererState::Ready;
    }
}

#[cfg(test)]
mod tests {
    use super::{DepthAttachment, RenderViewport, RenderViewportError, TextureFormat};

    #[test]
    fn surface_depth_format_is_depth32_float() {
        assert_eq!(DepthAttachment::FORMAT, TextureFormat::Depth32Float);
    }

    #[test]
    fn render_viewport_validates_shape_and_surface_bounds() {
        assert_eq!(
            RenderViewport::new(0, 0, 0, 10),
            Err(RenderViewportError::Empty)
        );
        assert_eq!(
            RenderViewport::new(u32::MAX, 0, 2, 1),
            Err(RenderViewportError::Overflow)
        );

        let viewport = RenderViewport::new(40, 20, 640, 480).expect("valid viewport");
        assert_eq!(viewport.validate_within([1280, 720]), Ok(()));
        assert_eq!(
            viewport.validate_within([600, 720]),
            Err(RenderViewportError::OutsideSurface {
                viewport,
                surface: [600, 720],
            })
        );
    }

    #[test]
    fn nested_viewport_must_remain_inside_parent() {
        let parent = RenderViewport::new(100, 40, 600, 400).expect("parent");
        let child = RenderViewport::new(120, 60, 200, 100).expect("child");
        assert_eq!(child.validate_within_parent(parent), Ok(()));

        let escaped = RenderViewport::new(80, 60, 200, 100).expect("escaped child");
        assert_eq!(
            escaped.validate_within_parent(parent),
            Err(RenderViewportError::OutsideParent {
                viewport: escaped,
                parent,
            })
        );
    }

    #[test]
    fn logical_conversion_and_pointer_localization_share_pixel_rounding() {
        let viewport = RenderViewport::from_logical(10.0, 5.0, 100.0, 50.0, 1.25, [800, 600])
            .expect("logical viewport");
        assert_eq!(
            viewport,
            RenderViewport::new(13, 6, 125, 63).expect("physical")
        );
        assert_eq!(viewport.local_position(20.0, 10.0), Some([7.0, 4.0]));
        assert_eq!(viewport.local_position(12.0, 10.0), None);
    }
}
