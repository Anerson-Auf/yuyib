//! Headless offscreen GPU targets for smoke capture and reference screenshots.
//!
//! Presentation swapchains are not a portable `COPY_SRC` source. This helper
//! owns an ordinary colour texture (`RENDER_ATTACHMENT | COPY_SRC`) plus the
//! same depth format as [`crate::Renderer`], records passes through
//! [`crate::RenderFrame`], then exposes [`Self::capture_rgba8`].
//!
//! HDR colour post-processing is intentionally **not** applied here: smoke and
//! golden images capture the direct colour target (typically `Rgba8Unorm`).

use std::{collections::HashMap, error::Error, fmt, sync::Arc};

use wgpu::{
    Backends, CommandEncoderDescriptor, Device, DeviceDescriptor, DownlevelFlags, Extent3d,
    Features, Instance, InstanceDescriptor, LoadOp, PowerPreference, Queue, RequestAdapterOptions,
    TextureDescriptor, TextureDimension, TextureFormat, TextureUsages, TextureView,
    TextureViewDescriptor,
};

use crate::{
    ClearColor, DepthAttachment, MAX_CACHED_VIEWPORT_DEPTH_ATTACHMENTS, RenderFrame,
    RenderViewport, TextureReadbackError, TextureReadbackFormat, read_texture_rgba8,
    required_device_limits,
};

/// Default offscreen colour format for smoke / golden captures.
pub const OFFSCREEN_COLOR_FORMAT: TextureFormat = TextureFormat::Rgba8Unorm;

/// Hard ceiling keeps accidental huge captures from allocating multi-GB textures.
pub const MAX_OFFSCREEN_DIMENSION: u32 = 8192;

/// Headless device + offscreen colour/depth attachments.
pub struct OffscreenRenderer {
    device: Device,
    queue: Queue,
    width: u32,
    height: u32,
    color: wgpu::Texture,
    color_view: TextureView,
    depth: DepthAttachment,
    viewport_depths: HashMap<RenderViewport, Arc<DepthAttachment>>,
    anisotropic_filtering_supported: bool,
    frame_serial: u64,
}

/// Tightly packed RGBA8 pixels captured from an [`OffscreenRenderer`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapturedFrameRgba8 {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

impl CapturedFrameRgba8 {
    /// Capture width in pixels.
    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// Capture height in pixels.
    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// Row-major RGBA8 bytes (`width * height * 4`).
    #[must_use]
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// Consumes the capture and returns ownership of the pixel buffer.
    #[must_use]
    pub fn into_pixels(self) -> Vec<u8> {
        self.pixels
    }
}

impl OffscreenRenderer {
    /// Creates a headless GPU device and offscreen colour/depth targets.
    ///
    /// # Errors
    ///
    /// Returns [`OffscreenRendererInitError`] for zero/oversized dimensions or
    /// adapter/device acquisition failures.
    pub fn new(width: u32, height: u32) -> Result<Self, OffscreenRendererInitError> {
        pollster::block_on(Self::new_async(width, height))
    }

    /// Async counterpart of [`Self::new`].
    ///
    /// # Errors
    ///
    /// Same failures as [`Self::new`].
    pub async fn new_async(width: u32, height: u32) -> Result<Self, OffscreenRendererInitError> {
        validate_dimensions(width, height)?;
        let mut instance_descriptor = InstanceDescriptor::new_without_display_handle();
        instance_descriptor.backends = Backends::DX12 | Backends::VULKAN;
        let instance = Instance::new(instance_descriptor);
        let adapter = instance
            .request_adapter(&RequestAdapterOptions {
                power_preference: PowerPreference::HighPerformance,
                compatible_surface: None,
                force_fallback_adapter: false,
                ..Default::default()
            })
            .await
            .map_err(OffscreenRendererInitError::RequestAdapter)?;
        let anisotropic_filtering_supported = adapter
            .get_downlevel_capabilities()
            .flags
            .contains(DownlevelFlags::ANISOTROPIC_FILTERING);
        let (device, queue) = adapter
            .request_device(&DeviceDescriptor {
                label: Some("yuyib offscreen device"),
                required_features: Features::empty(),
                required_limits: required_device_limits(),
                ..Default::default()
            })
            .await
            .map_err(OffscreenRendererInitError::RequestDevice)?;
        let (color, color_view) = create_color_target(&device, width, height);
        let depth = DepthAttachment::new(&device, width, height);
        Ok(Self {
            device,
            queue,
            width,
            height,
            color,
            color_view,
            depth,
            viewport_depths: HashMap::new(),
            anisotropic_filtering_supported,
            frame_serial: 0,
        })
    }

    /// Physical capture size in pixels.
    #[must_use]
    pub const fn size(&self) -> [u32; 2] {
        [self.width, self.height]
    }

    /// Colour format of the offscreen target ([`OFFSCREEN_COLOR_FORMAT`]).
    #[must_use]
    pub const fn color_format(&self) -> TextureFormat {
        OFFSCREEN_COLOR_FORMAT
    }

    /// Depth format paired with this offscreen target.
    #[must_use]
    pub const fn depth_format(&self) -> TextureFormat {
        DepthAttachment::FORMAT
    }

    /// Borrow-only access to the headless device and queue.
    pub fn with_raw_gpu<Result>(
        &self,
        operation: impl FnOnce(&Device, &Queue) -> Result,
    ) -> Result {
        operation(&self.device, &self.queue)
    }

    /// Clears the offscreen target, records passes, and submits the commands.
    ///
    /// Does not present anything. Call [`Self::capture_rgba8`] afterwards (or
    /// use [`Self::render_and_capture_rgba8`]).
    pub fn render_frame(&mut self, clear: ClearColor, record: impl FnOnce(&mut RenderFrame<'_>)) {
        self.frame_serial = self
            .frame_serial
            .checked_add(1)
            .expect("offscreen frame serial exhausted u64");
        let mut encoder = self
            .device
            .create_command_encoder(&CommandEncoderDescriptor {
                label: Some("yuyib offscreen frame"),
            });
        {
            let mut frame = RenderFrame {
                device: &self.device,
                queue: &self.queue,
                encoder: &mut encoder,
                surface_view: &self.color_view,
                width: self.width,
                height: self.height,
                viewport: RenderViewport::full([self.width, self.height]),
                format: OFFSCREEN_COLOR_FORMAT,
                depth_view: &self.depth.view,
                viewport_depths: &mut self.viewport_depths,
                anisotropic_filtering_supported: self.anisotropic_filtering_supported,
                frame_serial: self.frame_serial,
            };
            frame.with_surface_pass(LoadOp::Clear(clear.into()), |_| {});
            record(&mut frame);
        }
        if self.viewport_depths.len() > MAX_CACHED_VIEWPORT_DEPTH_ATTACHMENTS {
            self.viewport_depths.clear();
        }
        self.queue.submit([encoder.finish()]);
    }

    /// Copies the current offscreen colour target into tightly packed RGBA8.
    ///
    /// # Errors
    ///
    /// Returns [`TextureReadbackError`] when the GPU map fails.
    pub fn capture_rgba8(&self) -> Result<CapturedFrameRgba8, TextureReadbackError> {
        let pixels = read_texture_rgba8(
            &self.device,
            &self.queue,
            &self.color,
            self.width,
            self.height,
            TextureReadbackFormat::Rgba8,
        )?;
        Ok(CapturedFrameRgba8 {
            width: self.width,
            height: self.height,
            pixels,
        })
    }

    /// Records one frame and immediately captures RGBA8 pixels.
    ///
    /// # Errors
    ///
    /// Returns [`TextureReadbackError`] from [`Self::capture_rgba8`].
    pub fn render_and_capture_rgba8(
        &mut self,
        clear: ClearColor,
        record: impl FnOnce(&mut RenderFrame<'_>),
    ) -> Result<CapturedFrameRgba8, TextureReadbackError> {
        self.render_frame(clear, record);
        self.capture_rgba8()
    }
}

fn validate_dimensions(width: u32, height: u32) -> Result<(), OffscreenRendererInitError> {
    if width == 0 || height == 0 {
        return Err(OffscreenRendererInitError::EmptyDimensions { width, height });
    }
    if width > MAX_OFFSCREEN_DIMENSION || height > MAX_OFFSCREEN_DIMENSION {
        return Err(OffscreenRendererInitError::DimensionsExceeded {
            width,
            height,
            maximum: MAX_OFFSCREEN_DIMENSION,
        });
    }
    Ok(())
}

fn create_color_target(device: &Device, width: u32, height: u32) -> (wgpu::Texture, TextureView) {
    let color = device.create_texture(&TextureDescriptor {
        label: Some("yuyib offscreen color"),
        size: Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: TextureDimension::D2,
        format: OFFSCREEN_COLOR_FORMAT,
        usage: TextureUsages::RENDER_ATTACHMENT | TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let color_view = color.create_view(&TextureViewDescriptor::default());
    (color, color_view)
}

/// Failure while creating a headless [`OffscreenRenderer`].
#[derive(Debug)]
pub enum OffscreenRendererInitError {
    /// Width or height was zero.
    EmptyDimensions {
        /// Requested width.
        width: u32,
        /// Requested height.
        height: u32,
    },
    /// A dimension exceeded [`MAX_OFFSCREEN_DIMENSION`].
    DimensionsExceeded {
        /// Requested width.
        width: u32,
        /// Requested height.
        height: u32,
        /// Configured maximum.
        maximum: u32,
    },
    /// No compatible GPU adapter was available.
    RequestAdapter(wgpu::RequestAdapterError),
    /// The adapter rejected device creation.
    RequestDevice(wgpu::RequestDeviceError),
}

impl fmt::Display for OffscreenRendererInitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyDimensions { width, height } => write!(
                formatter,
                "cannot create offscreen renderer with empty dimensions {width}x{height}"
            ),
            Self::DimensionsExceeded {
                width,
                height,
                maximum,
            } => write!(
                formatter,
                "offscreen dimensions {width}x{height} exceed limit {maximum}"
            ),
            Self::RequestAdapter(error) => {
                write!(
                    formatter,
                    "could not acquire offscreen GPU adapter: {error}"
                )
            }
            Self::RequestDevice(error) => {
                write!(formatter, "could not create offscreen GPU device: {error}")
            }
        }
    }
}

impl Error for OffscreenRendererInitError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::RequestAdapter(error) => Some(error),
            Self::RequestDevice(error) => Some(error),
            Self::EmptyDimensions { .. } | Self::DimensionsExceeded { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_OFFSCREEN_DIMENSION, OffscreenRenderer, OffscreenRendererInitError};

    #[test]
    fn rejects_empty_and_oversized_dimensions() {
        assert!(matches!(
            OffscreenRenderer::new(0, 16),
            Err(OffscreenRendererInitError::EmptyDimensions { .. })
        ));
        assert!(matches!(
            OffscreenRenderer::new(MAX_OFFSCREEN_DIMENSION + 1, 16),
            Err(OffscreenRendererInitError::DimensionsExceeded { .. })
        ));
    }
}
