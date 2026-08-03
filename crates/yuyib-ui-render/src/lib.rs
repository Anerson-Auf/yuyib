//! First WGPU bridge for retained Yuyib native UI.
//!
//! This backend converts yuyib-ui layout and semantic styles into ordered
//! coloured rectangle fills, optional rectangular borders, a keyboard-focus
//! outline, and an optional vertical `ScrollView` thumb indicator, then records
//! a colour-only pass through yuyib-render. An optional rectangular clip
//! performs CPU geometry intersection and a bounded WGPU scissor pass. It
//! deliberately does not provide text glyphs, fonts, scrollbar drag/inertia,
//! rounded corners, GPU textured image sampling, accessibility, windows,
//! Winit integration, HTML, CSS, `WebView`, or a UI application loop.
//! Hosts may still extract opaque [`UiImageQuad`] lists via
//! [`extract_image_draw_list`] and bind textures themselves.

#![forbid(unsafe_code)]

use std::{error::Error, fmt, mem::size_of};

use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt;
use yuyib_render::{RenderFrame, Renderer, wgpu};
use yuyib_ui::{
    Color, SCROLL_THUMB_THICKNESS, UiImageId, UiInputState, UiLayout, UiTokens, Widget, WidgetId,
    WidgetKind, vertical_scroll_thumb_bounds,
};

/// An explicit logical-pixel clipping rectangle for one UI render pass.
///
/// During retained-tree extraction, each generated fill, border, and focus
/// rectangle is intersected with this rectangle on the CPU. The same bounds
/// are retained on every resulting command so the WGPU backend can set a
/// bounded scissor rectangle. Empty and off-surface clips simply emit no GPU
/// command; they are not errors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UiClipRect {
    bounds: yuyib_ui::Rect,
}

impl UiClipRect {
    /// Creates a clip from logical-pixel bounds.
    #[must_use]
    pub const fn new(bounds: yuyib_ui::Rect) -> Self {
        Self { bounds }
    }

    /// Returns the logical-pixel clip bounds.
    #[must_use]
    pub const fn bounds(self) -> yuyib_ui::Rect {
        self.bounds
    }
}

/// One renderer-neutral coloured UI rectangle in paint order.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct UiRectangle {
    /// Retained widget that produced this rectangle.
    pub widget: WidgetId,
    /// Logical-pixel rectangle from UI layout.
    pub bounds: yuyib_ui::Rect,
    /// Fully resolved sRGBA fill colour.
    pub color: Color,
    /// Optional pass clip retained for bounded GPU scissor recording.
    ///
    /// Extraction APIs crop `bounds` to this rectangle before it reaches the
    /// draw list. Direct draw-list producers should preserve that invariant
    /// where possible; the renderer still bounds the scissor safely.
    pub clip: Option<UiClipRect>,
}

/// One inside-aligned rectangular border or focus outline.
///
/// The renderer expands a border into non-overlapping edge rectangles. This
/// preserves ordinary alpha composition without a separate line primitive.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UiBorder {
    color: Color,
    thickness: u32,
}

impl UiBorder {
    /// Creates a visible border with a positive logical-pixel thickness.
    ///
    /// # Errors
    ///
    /// Returns an error when thickness is zero.
    pub const fn new(color: Color, thickness: u32) -> Result<Self, UiBorderError> {
        if thickness == 0 {
            return Err(UiBorderError::ZeroThickness);
        }
        Ok(Self { color, thickness })
    }

    /// Returns the resolved sRGBA border colour.
    #[must_use]
    pub const fn color(self) -> Color {
        self.color
    }

    /// Returns the requested logical-pixel border thickness.
    #[must_use]
    pub const fn thickness(self) -> u32 {
        self.thickness
    }
}

/// Invalid rectangular border configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UiBorderError {
    /// A zero-pixel border would not produce a visual.
    ZeroThickness,
}

impl fmt::Display for UiBorderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroThickness => formatter.write_str("UI border thickness must be positive"),
        }
    }
}

impl Error for UiBorderError {}

/// Optional visual decorations extracted by the rectangle UI backend.
///
/// Widget borders are emitted after each widget fill but before child widgets,
/// so retained-tree painter order is unchanged. The focus outline is emitted
/// after the tree so a keyboard-focused control remains visible above sibling
/// content. Both use standard alpha blending in the existing WGPU pipeline.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UiVisualStyle {
    widget_border: Option<UiBorder>,
    focus_outline: Option<UiBorder>,
    scroll_thumb: Option<Color>,
}

impl UiVisualStyle {
    /// Returns no borders, focus visual, or scroll thumb.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            widget_border: None,
            focus_outline: None,
            scroll_thumb: None,
        }
    }

    /// Sets the border emitted for every widget that has a background fill.
    #[must_use]
    pub const fn with_widget_border(mut self, border: Option<UiBorder>) -> Self {
        self.widget_border = border;
        self
    }

    /// Sets the visual emitted for the current keyboard-focused widget.
    #[must_use]
    pub const fn with_focus_outline(mut self, outline: Option<UiBorder>) -> Self {
        self.focus_outline = outline;
        self
    }

    /// Sets the optional vertical `ScrollView` thumb indicator colour.
    ///
    /// `None` disables the thumb even when content overflows. The default style
    /// enables a translucent text-coloured thumb.
    #[must_use]
    pub const fn with_scroll_thumb(mut self, color: Option<Color>) -> Self {
        self.scroll_thumb = color;
        self
    }

    /// Returns the background-widget border configuration.
    #[must_use]
    pub const fn widget_border(self) -> Option<UiBorder> {
        self.widget_border
    }

    /// Returns the keyboard focus-outline configuration.
    #[must_use]
    pub const fn focus_outline(self) -> Option<UiBorder> {
        self.focus_outline
    }

    /// Returns the scroll thumb colour when overflow indicators are enabled.
    #[must_use]
    pub const fn scroll_thumb(self) -> Option<Color> {
        self.scroll_thumb
    }
}

impl Default for UiVisualStyle {
    fn default() -> Self {
        Self {
            widget_border: None,
            focus_outline: Some(UiBorder {
                color: Color::rgb(250, 204, 21),
                thickness: 2,
            }),
            scroll_thumb: Some(Color::rgba(239, 242, 247, 140)),
        }
    }
}

/// Input and visual configuration for one input-aware UI renderer pass.
///
/// The input reference remains owned by the application; this value only
/// borrows it while CPU extraction happens on the caller thread.
#[derive(Clone, Copy, Debug)]
pub struct UiInputRenderOptions<'a> {
    input: &'a UiInputState,
    visuals: UiVisualStyle,
    limits: UiRenderLimits,
    clip: Option<UiClipRect>,
}

impl<'a> UiInputRenderOptions<'a> {
    /// Starts an input-aware pass with visible default focus styling.
    #[must_use]
    pub fn new(input: &'a UiInputState) -> Self {
        Self {
            input,
            visuals: UiVisualStyle::default(),
            limits: UiRenderLimits::default(),
            clip: None,
        }
    }

    /// Sets optional widget borders and keyboard-focus styling.
    #[must_use]
    pub const fn with_visuals(mut self, visuals: UiVisualStyle) -> Self {
        self.visuals = visuals;
        self
    }

    /// Sets the bounded rectangle extraction limit.
    #[must_use]
    pub const fn with_limits(mut self, limits: UiRenderLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Sets an optional explicit pass clip for fills and input decorations.
    ///
    /// The extraction path crops geometry on the CPU; GPU recording additionally
    /// applies the clip as a bounded scissor rectangle.
    #[must_use]
    pub const fn with_clip(mut self, clip: Option<UiClipRect>) -> Self {
        self.clip = clip;
        self
    }
}

/// Ordered CPU draw list for the rectangle-fill UI backend.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct UiDrawList {
    rectangles: Vec<UiRectangle>,
}

impl UiDrawList {
    /// Returns fill rectangles in exact retained tree paint order.
    #[must_use]
    pub fn rectangles(&self) -> &[UiRectangle] {
        &self.rectangles
    }

    /// Returns whether this list contains no visible fills.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rectangles.is_empty()
    }
}

/// Limits retained UI rectangle and image-quad upload work.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UiRenderLimits {
    /// Maximum coloured rectangles accepted in one draw list.
    pub max_rectangles: usize,
    /// Maximum image quads accepted in one [`UiImageDrawList`].
    pub max_images: usize,
}

impl Default for UiRenderLimits {
    fn default() -> Self {
        Self {
            max_rectangles: 100_000,
            max_images: 100_000,
        }
    }
}

/// One renderer-neutral image/icon quad in paint order.
///
/// Bounds are layout logical pixels, optionally CPU-cropped to `clip`. The
/// host resolves [`UiImageId`] to a GPU texture or CPU bitmap; this crate does
/// not sample textures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UiImageQuad {
    /// Retained widget that produced this quad.
    pub widget: WidgetId,
    /// Application-owned image key from [`WidgetKind::Image`].
    pub image: UiImageId,
    /// Logical-pixel destination rectangle after clip intersection.
    pub bounds: yuyib_ui::Rect,
    /// Optional pass / scroll clip retained for a host scissor stage.
    pub clip: Option<UiClipRect>,
}

/// Ordered image/icon quads extracted from a retained UI tree.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UiImageDrawList {
    quads: Vec<UiImageQuad>,
}

impl UiImageDrawList {
    /// Returns image quads in retained-tree paint order.
    #[must_use]
    pub fn quads(&self) -> &[UiImageQuad] {
        &self.quads
    }

    /// Returns whether this list contains no image quads.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.quads.is_empty()
    }
}

/// CPU extraction, GPU upload, or recording failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UiRenderError {
    /// The layout did not include a widget from the supplied retained tree.
    MissingLayoutBounds(WidgetId),
    /// The draw list exceeded the configured bounded-work limit.
    TooManyRectangles {
        /// Observed rectangle count.
        actual: usize,
        /// Configured maximum.
        limit: usize,
    },
    /// The image draw list exceeded the configured bounded-work limit.
    TooManyImages {
        /// Observed image-quad count.
        actual: usize,
        /// Configured maximum.
        limit: usize,
    },
    /// Frame dimensions cannot represent an NDC transform.
    ZeroSurfaceSize,
    /// Vertex buffer count or byte size exceeded WGPU address space.
    TooManyVertices,
}

impl fmt::Display for UiRenderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingLayoutBounds(id) => {
                write!(formatter, "UI layout is missing widget {}", id.get())
            }
            Self::TooManyRectangles { actual, limit } => {
                write!(
                    formatter,
                    "UI rectangle count {actual} exceeds limit {limit}"
                )
            }
            Self::TooManyImages { actual, limit } => {
                write!(formatter, "UI image count {actual} exceeds limit {limit}")
            }
            Self::ZeroSurfaceSize => {
                formatter.write_str("UI cannot render to a zero-sized surface")
            }
            Self::TooManyVertices => {
                formatter.write_str("UI vertex buffer exceeds WGPU address space")
            }
        }
    }
}

impl Error for UiRenderError {}

/// Counts commands issued by one UI rectangle pass.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct UiRenderStats {
    /// Fill, border, and focus rectangles issued after clipping/scissoring.
    pub rectangles: u32,
    /// Triangles issued after clipping/scissoring.
    pub triangles: u32,
    /// GPU draw calls; zero when no rectangle has a non-empty bounded scissor.
    ///
    /// A clipped draw list may require one call per rectangle because each
    /// command can carry a different scissor rectangle.
    pub draw_calls: u32,
}

/// Extracts background-coloured retained widgets into paint-order rectangles.
///
/// A button uses its default accent background. A panel is simply a container
/// whose widget style has a background token. Labels currently produce no fill
/// unless their caller gives them a background style.
///
/// # Errors
///
/// Returns a structured error for missing layout rectangles or bounded list
/// overflow. This function performs no GPU work and is suitable for tests or a
/// future alternate renderer.
pub fn extract_draw_list(
    root: &Widget,
    layout: &UiLayout,
    tokens: UiTokens,
    limits: UiRenderLimits,
) -> Result<UiDrawList, UiRenderError> {
    extract_draw_list_clipped(root, layout, tokens, None, limits)
}

/// Extracts background-coloured widgets with an optional explicit pass clip.
///
/// The returned geometry is CPU-intersected with `clip`. When a clip is
/// present, each retained rectangle also carries it for the backend's bounded
/// WGPU scissor stage. An empty intersection is omitted without consuming the
/// rectangle limit.
///
/// # Errors
///
/// Returns a structured error for missing layout rectangles or bounded list
/// overflow. This function performs no GPU work.
pub fn extract_draw_list_clipped(
    root: &Widget,
    layout: &UiLayout,
    tokens: UiTokens,
    clip: Option<UiClipRect>,
    limits: UiRenderLimits,
) -> Result<UiDrawList, UiRenderError> {
    extract_draw_list_with_input_clipped(
        root,
        layout,
        tokens,
        &UiInputState::default(),
        UiVisualStyle::none(),
        clip,
        limits,
    )
}

/// Extracts retained widget fills, optional borders, scroll thumbs, and
/// keyboard focus visual.
///
/// Fills, widget borders, and scroll thumbs remain in depth-first retained-tree
/// painter order. A focused button outline is appended last so it remains
/// visible; a stale focus identifier simply produces no outline, matching the
/// input model's stale-focus behavior.
///
/// # Errors
///
/// Returns a structured error for missing layout rectangles or bounded list
/// overflow. This function performs no GPU work and is suitable for tests or a
/// future alternate renderer.
pub fn extract_draw_list_with_input(
    root: &Widget,
    layout: &UiLayout,
    tokens: UiTokens,
    input: &UiInputState,
    visuals: UiVisualStyle,
    limits: UiRenderLimits,
) -> Result<UiDrawList, UiRenderError> {
    extract_draw_list_with_input_clipped(root, layout, tokens, input, visuals, None, limits)
}

/// Extracts [`WidgetKind::Image`] widgets into paint-order destination quads.
///
/// The list is separate from colour [`UiDrawList`] so hosts can bind textures
/// without mixing them into the solid-colour WGPU pass. Empty trees and trees
/// without images succeed with an empty list.
///
/// # Errors
///
/// Returns a structured error for missing layout rectangles or bounded list
/// overflow. This function performs no GPU work.
pub fn extract_image_draw_list(
    root: &Widget,
    layout: &UiLayout,
    limits: UiRenderLimits,
) -> Result<UiImageDrawList, UiRenderError> {
    extract_image_draw_list_clipped(root, layout, None, limits)
}

/// Extracts image widgets with an optional explicit pass clip.
///
/// Geometry is CPU-intersected with `clip` and with any [`UiLayout::clip`] from
/// nested `ScrollView` viewports. Empty intersections are omitted without
/// consuming the image limit.
///
/// # Errors
///
/// Returns a structured error for missing layout rectangles or bounded list
/// overflow. This function performs no GPU work.
pub fn extract_image_draw_list_clipped(
    root: &Widget,
    layout: &UiLayout,
    clip: Option<UiClipRect>,
    limits: UiRenderLimits,
) -> Result<UiImageDrawList, UiRenderError> {
    let mut quads = Vec::new();
    extract_image_widget(root, layout, clip, limits, &mut quads)?;
    Ok(UiImageDrawList { quads })
}

/// Extracts retained fills and input decorations with an optional pass clip.
///
/// CPU clipping applies uniformly to backgrounds, widget borders, scroll
/// thumbs, and the focus outline. The result keeps one explicit clip per
/// rectangle for a later bounded WGPU scissor. Nested scroll viewport clips
/// still come from [`UiLayout::clip`]; this argument is an additional pass
/// clip, not a full clip-stack API.
///
/// # Errors
///
/// Returns a structured error for missing layout rectangles or bounded list
/// overflow. This function performs no GPU work.
pub fn extract_draw_list_with_input_clipped(
    root: &Widget,
    layout: &UiLayout,
    tokens: UiTokens,
    input: &UiInputState,
    visuals: UiVisualStyle,
    clip: Option<UiClipRect>,
    limits: UiRenderLimits,
) -> Result<UiDrawList, UiRenderError> {
    let mut rectangles = Vec::new();
    {
        let mut context = ExtractionContext {
            tokens,
            widget_border: visuals.widget_border,
            scroll_thumb: visuals.scroll_thumb,
            input,
            focused: input.focused(),
            limits,
            clip,
            rectangles: &mut rectangles,
            focused_bounds: None,
        };
        extract_widget(root, layout, &mut context)?;
        if let (Some(focused), Some(bounds), Some(outline)) = (
            context.focused,
            context.focused_bounds,
            visuals.focus_outline,
        ) {
            append_border(
                focused,
                bounds,
                outline,
                context.clip,
                context.limits,
                context.rectangles,
            )?;
        }
    }
    Ok(UiDrawList { rectangles })
}

/// WGPU rectangle-fill renderer for a fixed Yuyib presentation format.
///
/// Construct once during renderer setup. It records a colour-only pass using
/// `LoadOp::Load`, so it overlays existing scene output without touching depth.
pub struct UiRenderer {
    pipeline: wgpu::RenderPipeline,
}

impl UiRenderer {
    /// Creates a backend pipeline for the renderer presentation format.
    #[must_use]
    pub fn new(renderer: &Renderer) -> Self {
        let color_format = renderer.color_target_format();
        renderer.with_raw_gpu(|device, _queue, _configuration| Self::create(device, color_format))
    }

    /// Creates a backend pipeline from an active frame.
    #[must_use]
    pub fn new_for_frame(frame: &RenderFrame<'_>) -> Self {
        Self::create(frame.device(), frame.surface_format())
    }

    /// Extracts, uploads, and records one UI rectangle-fill overlay pass.
    ///
    /// The UI layout viewport must use the same physical logical-pixel size as
    /// the frame surface. High-DPI policy belongs to the caller at the layout
    /// boundary; this first backend intentionally has no DPI conversion layer.
    ///
    /// # Errors
    ///
    /// Returns structured extraction, sizing, or bounded upload errors.
    pub fn draw(
        &self,
        frame: &mut RenderFrame<'_>,
        root: &Widget,
        layout: &UiLayout,
        tokens: UiTokens,
        limits: UiRenderLimits,
    ) -> Result<UiRenderStats, UiRenderError> {
        let draw_list = extract_draw_list(root, layout, tokens, limits)?;
        self.draw_list(frame, &draw_list)
    }

    /// Extracts, clips, uploads, and records one UI rectangle-fill overlay.
    ///
    /// Clipping is performed twice by design: CPU extraction intersects the
    /// uploaded geometry and WGPU applies the same clip as a bounded scissor.
    /// The API is intentionally one pass wide; it does not create scrolling
    /// state or a nested clip hierarchy.
    ///
    /// # Errors
    ///
    /// Returns structured extraction, sizing, or bounded upload errors.
    pub fn draw_clipped(
        &self,
        frame: &mut RenderFrame<'_>,
        root: &Widget,
        layout: &UiLayout,
        tokens: UiTokens,
        clip: Option<UiClipRect>,
        limits: UiRenderLimits,
    ) -> Result<UiRenderStats, UiRenderError> {
        let draw_list = extract_draw_list_clipped(root, layout, tokens, clip, limits)?;
        self.draw_list(frame, &draw_list)
    }

    /// Extracts input-aware decorations, uploads them, and records one overlay pass.
    ///
    /// The focused widget from `UiInputState` receives the configured visual
    /// outline after normal tree painter order. Overflowing `ScrollView`
    /// widgets emit a vertical thumb when [`UiVisualStyle::scroll_thumb`] is
    /// set. Configure an explicit single pass clip with
    /// [`UiInputRenderOptions::with_clip`]. This method still has no text,
    /// scrollbar drag, image, or window-loop responsibilities.
    ///
    /// # Errors
    ///
    /// Returns structured extraction, sizing, or bounded upload errors.
    pub fn draw_with_input(
        &self,
        frame: &mut RenderFrame<'_>,
        root: &Widget,
        layout: &UiLayout,
        tokens: UiTokens,
        options: UiInputRenderOptions<'_>,
    ) -> Result<UiRenderStats, UiRenderError> {
        let draw_list = extract_draw_list_with_input_clipped(
            root,
            layout,
            tokens,
            options.input,
            options.visuals,
            options.clip,
            options.limits,
        )?;
        self.draw_list(frame, &draw_list)
    }

    /// Uploads an existing ordered draw list and records its overlay pass.
    ///
    /// # Errors
    ///
    /// Returns an error when frame dimensions or representable vertex counts
    /// are invalid.
    pub fn draw_list(
        &self,
        frame: &mut RenderFrame<'_>,
        draw_list: &UiDrawList,
    ) -> Result<UiRenderStats, UiRenderError> {
        let surface = frame.surface_size();
        if surface[0] == 0 || surface[1] == 0 {
            return Err(UiRenderError::ZeroSurfaceSize);
        }
        if draw_list.is_empty() {
            return Ok(UiRenderStats::default());
        }
        let commands = draw_commands_for(draw_list, surface)?;
        if commands.is_empty() {
            return Ok(UiRenderStats::default());
        }
        let vertices = vertices_for(draw_list, surface)?;
        let rectangle_count =
            u32::try_from(commands.len()).map_err(|_| UiRenderError::TooManyVertices)?;
        let buffer = frame
            .device()
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("yuyib UI rectangle vertices"),
                contents: bytemuck::cast_slice(&vertices),
                usage: wgpu::BufferUsages::VERTEX,
            });
        frame.with_surface_pass(wgpu::LoadOp::Load, |pass| {
            pass.set_pipeline(&self.pipeline);
            pass.set_vertex_buffer(0, buffer.slice(..));
            for command in commands {
                pass.set_scissor_rect(
                    command.scissor.x,
                    command.scissor.y,
                    command.scissor.width,
                    command.scissor.height,
                );
                pass.draw(
                    command.first_vertex..command.first_vertex.saturating_add(6),
                    0..1,
                );
            }
        });
        Ok(UiRenderStats {
            rectangles: rectangle_count,
            triangles: rectangle_count.saturating_mul(2),
            draw_calls: rectangle_count,
        })
    }

    /// Draws caller-owned rectangles in their supplied painter order.
    ///
    /// This is the low-level counterpart of [`Self::draw`]. It is useful for
    /// an overlay that must interleave rectangle widgets with another renderer
    /// (for example, native glyph text) while preserving retained-tree order.
    /// The caller remains responsible for using the same logical-pixel and
    /// surface coordinate policy as the frame.
    ///
    /// # Errors
    ///
    /// Returns the same bounded GPU/geometry errors as [`Self::draw_list`].
    pub fn draw_rectangles(
        &self,
        frame: &mut RenderFrame<'_>,
        rectangles: &[UiRectangle],
    ) -> Result<UiRenderStats, UiRenderError> {
        self.draw_list(
            frame,
            &UiDrawList {
                rectangles: rectangles.to_vec(),
            },
        )
    }

    fn create(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("yuyib UI rectangle WGSL"),
            source: wgpu::ShaderSource::Wgsl(RECTANGLE_WGSL.into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("yuyib UI rectangle pipeline layout"),
            bind_group_layouts: &[],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("yuyib UI rectangle pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[Some(VERTEX_LAYOUT)],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        Self { pipeline }
    }
}

struct ExtractionContext<'a> {
    tokens: UiTokens,
    widget_border: Option<UiBorder>,
    scroll_thumb: Option<Color>,
    input: &'a UiInputState,
    focused: Option<WidgetId>,
    limits: UiRenderLimits,
    clip: Option<UiClipRect>,
    rectangles: &'a mut Vec<UiRectangle>,
    focused_bounds: Option<yuyib_ui::Rect>,
}

fn extract_widget(
    widget: &Widget,
    layout: &UiLayout,
    context: &mut ExtractionContext<'_>,
) -> Result<(), UiRenderError> {
    let bounds = layout
        .bounds(widget.id())
        .ok_or(UiRenderError::MissingLayoutBounds(widget.id()))?;
    let layout_clip = layout.clip(widget.id()).map(UiClipRect::new);
    if let (Some(first), Some(second)) = (context.clip, layout_clip)
        && intersect_rectangles(first.bounds(), second.bounds()).is_none()
    {
        return Ok(());
    }
    let clip = merge_clips(context.clip, layout_clip);
    if context.focused == Some(widget.id()) {
        context.focused_bounds = Some(bounds);
    }
    if let Some(token) = widget.style().background {
        push_rectangle(
            UiRectangle {
                widget: widget.id(),
                bounds,
                color: context.tokens.colors.resolve(token),
                clip,
            },
            context.limits,
            context.rectangles,
        )?;
        if let Some(border) = context.widget_border {
            append_border(
                widget.id(),
                bounds,
                border,
                clip,
                context.limits,
                context.rectangles,
            )?;
        }
    }
    for child in widget.children() {
        extract_widget(child, layout, context)?;
    }
    if matches!(widget.kind(), WidgetKind::ScrollView)
        && let Some(color) = context.scroll_thumb
    {
        append_scroll_thumb(widget, layout, color, clip, context)?;
    }
    Ok(())
}

fn extract_image_widget(
    widget: &Widget,
    layout: &UiLayout,
    pass_clip: Option<UiClipRect>,
    limits: UiRenderLimits,
    quads: &mut Vec<UiImageQuad>,
) -> Result<(), UiRenderError> {
    let bounds = layout
        .bounds(widget.id())
        .ok_or(UiRenderError::MissingLayoutBounds(widget.id()))?;
    let layout_clip = layout.clip(widget.id()).map(UiClipRect::new);
    if let (Some(first), Some(second)) = (pass_clip, layout_clip)
        && intersect_rectangles(first.bounds(), second.bounds()).is_none()
    {
        return Ok(());
    }
    let clip = merge_clips(pass_clip, layout_clip);
    if let Some(image) = widget.image_id() {
        push_image_quad(
            UiImageQuad {
                widget: widget.id(),
                image,
                bounds,
                clip,
            },
            limits,
            quads,
        )?;
    }
    for child in widget.children() {
        extract_image_widget(child, layout, pass_clip, limits, quads)?;
    }
    Ok(())
}

fn push_image_quad(
    mut quad: UiImageQuad,
    limits: UiRenderLimits,
    quads: &mut Vec<UiImageQuad>,
) -> Result<(), UiRenderError> {
    if let Some(clip) = quad.clip {
        let Some(bounds) = intersect_rectangles(quad.bounds, clip.bounds()) else {
            return Ok(());
        };
        quad.bounds = bounds;
    }
    if quads.len() >= limits.max_images {
        return Err(UiRenderError::TooManyImages {
            actual: quads.len().saturating_add(1),
            limit: limits.max_images,
        });
    }
    quads.push(quad);
    Ok(())
}

fn append_scroll_thumb(
    widget: &Widget,
    layout: &UiLayout,
    color: Color,
    clip: Option<UiClipRect>,
    context: &mut ExtractionContext<'_>,
) -> Result<(), UiRenderError> {
    let Some(content) = widget.children().first() else {
        return Ok(());
    };
    let viewport = layout
        .bounds(widget.id())
        .ok_or(UiRenderError::MissingLayoutBounds(widget.id()))?;
    let content_bounds = layout
        .bounds(content.id())
        .ok_or(UiRenderError::MissingLayoutBounds(content.id()))?;
    let Some(thumb) = vertical_scroll_thumb_bounds(
        viewport,
        content_bounds.size.height,
        context.input.scroll_offset(widget.id()),
        SCROLL_THUMB_THICKNESS,
    ) else {
        return Ok(());
    };
    push_rectangle(
        UiRectangle {
            widget: widget.id(),
            bounds: thumb,
            color,
            clip,
        },
        context.limits,
        context.rectangles,
    )
}

fn merge_clips(first: Option<UiClipRect>, second: Option<UiClipRect>) -> Option<UiClipRect> {
    match (first, second) {
        (Some(first), Some(second)) => {
            intersect_rectangles(first.bounds(), second.bounds()).map(UiClipRect::new)
        }
        (Some(clip), None) | (None, Some(clip)) => Some(clip),
        (None, None) => None,
    }
}

fn push_rectangle(
    mut rectangle: UiRectangle,
    limits: UiRenderLimits,
    rectangles: &mut Vec<UiRectangle>,
) -> Result<(), UiRenderError> {
    if let Some(clip) = rectangle.clip {
        let Some(bounds) = intersect_rectangles(rectangle.bounds, clip.bounds()) else {
            return Ok(());
        };
        rectangle.bounds = bounds;
    }
    if rectangles.len() >= limits.max_rectangles {
        return Err(UiRenderError::TooManyRectangles {
            actual: rectangles.len().saturating_add(1),
            limit: limits.max_rectangles,
        });
    }
    rectangles.push(rectangle);
    Ok(())
}

fn append_border(
    widget: WidgetId,
    bounds: yuyib_ui::Rect,
    border: UiBorder,
    clip: Option<UiClipRect>,
    limits: UiRenderLimits,
    rectangles: &mut Vec<UiRectangle>,
) -> Result<(), UiRenderError> {
    let width = border.thickness.min(bounds.size.width);
    let height = border.thickness.min(bounds.size.height);
    if width == 0 || height == 0 {
        return Ok(());
    }
    push_rectangle(
        UiRectangle {
            widget,
            bounds: yuyib_ui::Rect {
                origin: bounds.origin,
                size: yuyib_ui::Size::new(bounds.size.width, height),
            },
            color: border.color,
            clip,
        },
        limits,
        rectangles,
    )?;
    if bounds.size.height > height {
        push_rectangle(
            UiRectangle {
                widget,
                bounds: yuyib_ui::Rect {
                    origin: yuyib_ui::Point::new(
                        bounds.origin.x,
                        bounds
                            .origin
                            .y
                            .saturating_add(to_i32(bounds.size.height.saturating_sub(height))),
                    ),
                    size: yuyib_ui::Size::new(bounds.size.width, height),
                },
                color: border.color,
                clip,
            },
            limits,
            rectangles,
        )?;
    }
    let middle_height = bounds.size.height.saturating_sub(height.saturating_mul(2));
    if middle_height == 0 {
        return Ok(());
    }
    push_rectangle(
        UiRectangle {
            widget,
            bounds: yuyib_ui::Rect {
                origin: yuyib_ui::Point::new(
                    bounds.origin.x,
                    bounds.origin.y.saturating_add(to_i32(height)),
                ),
                size: yuyib_ui::Size::new(width, middle_height),
            },
            color: border.color,
            clip,
        },
        limits,
        rectangles,
    )?;
    if bounds.size.width > width {
        push_rectangle(
            UiRectangle {
                widget,
                bounds: yuyib_ui::Rect {
                    origin: yuyib_ui::Point::new(
                        bounds
                            .origin
                            .x
                            .saturating_add(to_i32(bounds.size.width.saturating_sub(width))),
                        bounds.origin.y.saturating_add(to_i32(height)),
                    ),
                    size: yuyib_ui::Size::new(width, middle_height),
                },
                color: border.color,
                clip,
            },
            limits,
            rectangles,
        )?;
    }
    Ok(())
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Vertex {
    position: [f32; 2],
    color: [f32; 4],
}

const VERTEX_LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
    array_stride: size_of::<Vertex>() as u64,
    step_mode: wgpu::VertexStepMode::Vertex,
    attributes: &[
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32x2,
            offset: 0,
            shader_location: 0,
        },
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32x4,
            offset: size_of::<[f32; 2]>() as u64,
            shader_location: 1,
        },
    ],
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ScissorRect {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DrawCommand {
    first_vertex: u32,
    scissor: ScissorRect,
}

/// Intersects two half-open logical-pixel rectangles without overflowing their
/// signed origin plus unsigned extent representation.
fn intersect_rectangles(first: yuyib_ui::Rect, second: yuyib_ui::Rect) -> Option<yuyib_ui::Rect> {
    let first_left = i64::from(first.origin.x);
    let first_top = i64::from(first.origin.y);
    let first_right = first_left + i64::from(first.size.width);
    let first_bottom = first_top + i64::from(first.size.height);
    let second_left = i64::from(second.origin.x);
    let second_top = i64::from(second.origin.y);
    let second_right = second_left + i64::from(second.size.width);
    let second_bottom = second_top + i64::from(second.size.height);

    let left = first_left.max(second_left);
    let top = first_top.max(second_top);
    let right = first_right.min(second_right);
    let bottom = first_bottom.min(second_bottom);
    if right <= left || bottom <= top {
        return None;
    }

    Some(yuyib_ui::Rect {
        origin: yuyib_ui::Point::new(i32::try_from(left).ok()?, i32::try_from(top).ok()?),
        size: yuyib_ui::Size::new(
            u32::try_from(right - left).ok()?,
            u32::try_from(bottom - top).ok()?,
        ),
    })
}

/// Converts an optional logical clip into a scissor bounded to the surface.
///
/// `None` means a full-surface scissor, while an empty or entirely outside
/// clip means the command must be skipped. This is kept CPU-only so its edge
/// cases remain directly testable without a WGPU adapter.
fn bounded_scissor(clip: Option<UiClipRect>, surface: [u32; 2]) -> Option<ScissorRect> {
    if surface[0] == 0 || surface[1] == 0 {
        return None;
    }
    let surface_bounds = yuyib_ui::Rect {
        origin: yuyib_ui::Point::new(0, 0),
        size: yuyib_ui::Size::new(surface[0], surface[1]),
    };
    let bounds = match clip {
        Some(clip) => intersect_rectangles(clip.bounds(), surface_bounds)?,
        None => surface_bounds,
    };
    Some(ScissorRect {
        x: u32::try_from(bounds.origin.x).ok()?,
        y: u32::try_from(bounds.origin.y).ok()?,
        width: bounds.size.width,
        height: bounds.size.height,
    })
}

fn draw_commands_for(
    draw_list: &UiDrawList,
    surface: [u32; 2],
) -> Result<Vec<DrawCommand>, UiRenderError> {
    let mut commands = Vec::with_capacity(draw_list.rectangles.len());
    for (index, rectangle) in draw_list.rectangles.iter().enumerate() {
        if rectangle.bounds.size.width == 0 || rectangle.bounds.size.height == 0 {
            continue;
        }
        let Some(scissor) = bounded_scissor(rectangle.clip, surface) else {
            continue;
        };
        let first_vertex = index.checked_mul(6).ok_or(UiRenderError::TooManyVertices)?;
        commands.push(DrawCommand {
            first_vertex: u32::try_from(first_vertex)
                .map_err(|_| UiRenderError::TooManyVertices)?,
            scissor,
        });
    }
    Ok(commands)
}

fn vertices_for(draw_list: &UiDrawList, surface: [u32; 2]) -> Result<Vec<Vertex>, UiRenderError> {
    let capacity = draw_list
        .rectangles
        .len()
        .checked_mul(6)
        .ok_or(UiRenderError::TooManyVertices)?;
    u32::try_from(capacity).map_err(|_| UiRenderError::TooManyVertices)?;
    let mut vertices = Vec::with_capacity(capacity);
    for rectangle in &draw_list.rectangles {
        let left = pixel_to_ndc(rectangle.bounds.origin.x, surface[0], false)?;
        let top = pixel_to_ndc(rectangle.bounds.origin.y, surface[1], true)?;
        let right = pixel_to_ndc(
            rectangle
                .bounds
                .origin
                .x
                .saturating_add(to_i32(rectangle.bounds.size.width)),
            surface[0],
            false,
        )?;
        let bottom = pixel_to_ndc(
            rectangle
                .bounds
                .origin
                .y
                .saturating_add(to_i32(rectangle.bounds.size.height)),
            surface[1],
            true,
        )?;
        let color = color_to_linear(rectangle.color);
        vertices.extend([
            Vertex {
                position: [left, top],
                color,
            },
            Vertex {
                position: [right, top],
                color,
            },
            Vertex {
                position: [right, bottom],
                color,
            },
            Vertex {
                position: [left, top],
                color,
            },
            Vertex {
                position: [right, bottom],
                color,
            },
            Vertex {
                position: [left, bottom],
                color,
            },
        ]);
    }
    Ok(vertices)
}

#[allow(clippy::cast_precision_loss)] // Surface pixel sizes are bounded by WGPU presentation dimensions.
fn pixel_to_ndc(value: i32, extent: u32, invert: bool) -> Result<f32, UiRenderError> {
    if extent == 0 {
        return Err(UiRenderError::ZeroSurfaceSize);
    }
    let normalized = value as f32 / extent as f32;
    let ndc = normalized.mul_add(2.0, -1.0);
    Ok(if invert { -ndc } else { ndc })
}

#[allow(clippy::cast_precision_loss)] // Eight-bit channels are exactly representable by f32.
fn color_to_linear(color: Color) -> [f32; 4] {
    [
        f32::from(color.red) / f32::from(u8::MAX),
        f32::from(color.green) / f32::from(u8::MAX),
        f32::from(color.blue) / f32::from(u8::MAX),
        f32::from(color.alpha) / f32::from(u8::MAX),
    ]
}

fn to_i32(value: u32) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX)
}

const RECTANGLE_WGSL: &str = r"
struct VertexInput {
    @location(0) position: vec2<f32>,
    @location(1) color: vec4<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) color: vec4<f32>,
};

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    var output: VertexOutput;
    output.clip_position = vec4<f32>(input.position, 0.0, 1.0);
    output.color = input.color;
    return output;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    return input.color;
}
";

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use yuyib_ui::{
        ColorToken, KeyboardInput, LayoutKind, Size, UiBuilder, UiInputState, Widget, WidgetStyle,
        handle_keyboard_input, layout_with_input_state,
    };

    use super::*;

    fn id(key: &str) -> WidgetId {
        WidgetId::from_key(key)
    }

    #[test]
    fn extraction_follows_tree_paint_order_and_resolves_tokens() {
        let tree = UiBuilder::new(id("root"), LayoutKind::Column)
            .child(
                Widget::container(id("panel"), LayoutKind::Column)
                    .with_style(WidgetStyle::default().with_background(ColorToken::SurfaceMuted)),
            )
            .child(Widget::button(id("button"), "Play"))
            .build()
            .expect("tree");
        let layout = yuyib_ui::layout(&tree, Size::new(100, 100)).expect("layout");
        let list = extract_draw_list(
            tree.root(),
            &layout,
            UiTokens::default(),
            UiRenderLimits::default(),
        )
        .expect("draw list");
        assert_eq!(
            list.rectangles()
                .iter()
                .map(|rectangle| rectangle.widget)
                .collect::<Vec<_>>(),
            vec![id("panel"), id("button")]
        );
        assert_eq!(
            list.rectangles()[0].color,
            UiTokens::default().colors.surface_muted
        );
    }

    #[test]
    fn extraction_rejects_bounded_rectangle_overflow() {
        let tree = UiBuilder::new(id("root"), LayoutKind::Column)
            .child(Widget::button(id("one"), "One"))
            .child(Widget::button(id("two"), "Two"))
            .build()
            .expect("tree");
        let layout = yuyib_ui::layout(&tree, Size::new(100, 100)).expect("layout");
        assert!(matches!(
            extract_draw_list(
                tree.root(),
                &layout,
                UiTokens::default(),
                UiRenderLimits {
                    max_rectangles: 1,
                    max_images: 100_000,
                },
            ),
            Err(UiRenderError::TooManyRectangles { .. })
        ));
    }

    #[test]
    fn input_aware_extraction_keeps_borders_in_order_and_focus_on_top() {
        let tree = UiBuilder::new(id("root"), LayoutKind::Column)
            .child(Widget::button(id("play"), "Play"))
            .build()
            .expect("tree");
        let layout = yuyib_ui::layout(&tree, Size::new(100, 100)).expect("layout");
        let mut input = UiInputState::default();
        handle_keyboard_input(&tree, &layout, &mut input, KeyboardInput::Tab).expect("focus");
        assert_eq!(input.focused(), Some(id("play")));

        let border = UiBorder::new(Color::rgb(10, 20, 30), 1).expect("border");
        let focus = UiBorder::new(Color::rgb(250, 204, 21), 2).expect("focus");
        let list = extract_draw_list_with_input(
            tree.root(),
            &layout,
            UiTokens::default(),
            &input,
            UiVisualStyle::none()
                .with_widget_border(Some(border))
                .with_focus_outline(Some(focus)),
            UiRenderLimits::default(),
        )
        .expect("draw list");

        assert_eq!(list.rectangles().len(), 9);
        assert_eq!(list.rectangles()[0].widget, id("play"));
        assert_eq!(
            list.rectangles()[0].color,
            UiTokens::default().colors.accent
        );
        assert!(
            list.rectangles()[1..5]
                .iter()
                .all(|rectangle| rectangle.color == border.color())
        );
        assert!(
            list.rectangles()[5..]
                .iter()
                .all(|rectangle| rectangle.color == focus.color())
        );
    }

    #[test]
    fn border_edges_obey_rectangle_limit() {
        let tree = UiBuilder::new(id("root"), LayoutKind::Column)
            .child(Widget::button(id("play"), "Play"))
            .build()
            .expect("tree");
        let layout = yuyib_ui::layout(&tree, Size::new(100, 100)).expect("layout");
        let border = UiBorder::new(Color::rgb(1, 2, 3), 1).expect("border");
        assert!(matches!(
            extract_draw_list_with_input(
                tree.root(),
                &layout,
                UiTokens::default(),
                &UiInputState::default(),
                UiVisualStyle::none().with_widget_border(Some(border)),
                UiRenderLimits {
                    max_rectangles: 4,
                    max_images: 100_000,
                },
            ),
            Err(UiRenderError::TooManyRectangles {
                actual: 5,
                limit: 4
            })
        ));
    }

    #[test]
    fn clipped_extraction_intersects_geometry_and_preserves_the_clip() {
        let tree = UiBuilder::new(id("root"), LayoutKind::Column)
            .child(Widget::button(id("play"), "Play"))
            .build()
            .expect("tree");
        let layout = yuyib_ui::layout(&tree, Size::new(100, 100)).expect("layout");
        let clip = UiClipRect::new(yuyib_ui::Rect {
            origin: yuyib_ui::Point::new(10, 5),
            size: Size::new(20, 10),
        });

        let list = extract_draw_list_clipped(
            tree.root(),
            &layout,
            UiTokens::default(),
            Some(clip),
            UiRenderLimits::default(),
        )
        .expect("clipped draw list");

        assert_eq!(list.rectangles().len(), 1);
        assert_eq!(list.rectangles()[0].clip, Some(clip));
        assert_eq!(
            list.rectangles()[0].bounds,
            yuyib_ui::Rect {
                origin: yuyib_ui::Point::new(10, 5),
                size: Size::new(20, 10),
            }
        );
    }

    #[test]
    fn empty_clip_intersection_is_omitted_before_rectangle_limits() {
        let tree = UiBuilder::new(id("root"), LayoutKind::Column)
            .child(Widget::button(id("play"), "Play"))
            .build()
            .expect("tree");
        let layout = yuyib_ui::layout(&tree, Size::new(100, 100)).expect("layout");
        let clip = UiClipRect::new(yuyib_ui::Rect {
            origin: yuyib_ui::Point::new(500, 500),
            size: Size::new(10, 10),
        });

        let list = extract_draw_list_clipped(
            tree.root(),
            &layout,
            UiTokens::default(),
            Some(clip),
            UiRenderLimits {
                max_rectangles: 0,
                max_images: 100_000,
            },
        )
        .expect("empty clipped draw list");

        assert!(list.is_empty());
    }

    #[test]
    fn scissor_is_bounded_and_empty_or_outside_clips_are_skipped() {
        let partial = UiClipRect::new(yuyib_ui::Rect {
            origin: yuyib_ui::Point::new(-8, -4),
            size: Size::new(20, 10),
        });
        assert_eq!(
            bounded_scissor(Some(partial), [100, 100]),
            Some(ScissorRect {
                x: 0,
                y: 0,
                width: 12,
                height: 6,
            })
        );

        let outside = UiClipRect::new(yuyib_ui::Rect {
            origin: yuyib_ui::Point::new(100, 0),
            size: Size::new(1, 1),
        });
        let empty = UiClipRect::new(yuyib_ui::Rect {
            origin: yuyib_ui::Point::new(0, 0),
            size: Size::new(0, 10),
        });
        assert_eq!(bounded_scissor(Some(outside), [100, 100]), None);
        assert_eq!(bounded_scissor(Some(empty), [100, 100]), None);
        assert_eq!(bounded_scissor(None, [0, 100]), None);
    }

    #[test]
    fn draw_commands_skip_empty_scissors_without_reindexing_vertices() {
        let outside = UiClipRect::new(yuyib_ui::Rect {
            origin: yuyib_ui::Point::new(200, 200),
            size: Size::new(1, 1),
        });
        let bounds = yuyib_ui::Rect {
            origin: yuyib_ui::Point::new(0, 0),
            size: Size::new(10, 10),
        };
        let list = UiDrawList {
            rectangles: vec![
                UiRectangle {
                    widget: id("outside"),
                    bounds,
                    color: Color::rgb(1, 2, 3),
                    clip: Some(outside),
                },
                UiRectangle {
                    widget: id("visible"),
                    bounds,
                    color: Color::rgb(4, 5, 6),
                    clip: None,
                },
            ],
        };

        assert_eq!(
            draw_commands_for(&list, [100, 100]).expect("commands"),
            vec![DrawCommand {
                first_vertex: 6,
                scissor: ScissorRect {
                    x: 0,
                    y: 0,
                    width: 100,
                    height: 100,
                },
            }]
        );
    }

    #[test]
    fn scroll_view_children_inherit_viewport_scissor() {
        let tree = UiBuilder::new(id("root"), LayoutKind::Column)
            .child(
                Widget::scroll_view(id("scroll"))
                    .with_constraints(
                        yuyib_ui::LayoutConstraints::auto()
                            .with_width(yuyib_ui::Dimension::Fill)
                            .with_height(yuyib_ui::Dimension::Points(20)),
                    )
                    .with_children(vec![
                        Widget::container(id("content"), LayoutKind::Column)
                            .with_constraints(
                                yuyib_ui::LayoutConstraints::auto()
                                    .with_height(yuyib_ui::Dimension::Points(80)),
                            )
                            .with_children(vec![
                                Widget::button(id("visible"), "Visible"),
                                Widget::button(id("clipped"), "Clipped"),
                            ]),
                    ]),
            )
            .build()
            .expect("scroll tree");
        let layout = layout_with_input_state(&tree, Size::new(100, 100), &UiInputState::default())
            .expect("layout");
        let list = extract_draw_list(
            tree.root(),
            &layout,
            UiTokens::default(),
            UiRenderLimits::default(),
        )
        .expect("draw list");

        assert_eq!(list.rectangles().len(), 1);
        assert_eq!(list.rectangles()[0].widget, id("visible"));
        assert_eq!(
            list.rectangles()[0].clip,
            Some(UiClipRect::new(
                layout.bounds(id("scroll")).expect("viewport")
            ))
        );
    }

    fn overflow_scroll_tree() -> yuyib_ui::UiTree {
        UiBuilder::new(id("root"), LayoutKind::Column)
            .child(
                Widget::scroll_view(id("scroll"))
                    .with_constraints(
                        yuyib_ui::LayoutConstraints::auto()
                            .with_width(yuyib_ui::Dimension::Points(100))
                            .with_height(yuyib_ui::Dimension::Points(40)),
                    )
                    .with_children(vec![
                        Widget::container(id("content"), LayoutKind::Column)
                            .with_constraints(
                                yuyib_ui::LayoutConstraints::auto()
                                    .with_width(yuyib_ui::Dimension::Points(100))
                                    .with_height(yuyib_ui::Dimension::Points(120)),
                            )
                            .with_children(vec![Widget::button(id("item"), "Item")]),
                    ]),
            )
            .build()
            .expect("overflow scroll tree")
    }

    #[test]
    fn scroll_view_thumb_emitted_for_overflow_on_input_extract() {
        let tree = overflow_scroll_tree();
        let mut state = UiInputState::default();
        let layout = layout_with_input_state(&tree, Size::new(100, 100), &state).expect("layout");
        yuyib_ui::handle_scroll_input(&tree, &layout, &mut state, yuyib_ui::Point::new(2, 2), -40)
            .expect("scroll");
        let layout = layout_with_input_state(&tree, Size::new(100, 100), &state).expect("scrolled");
        let list = extract_draw_list_with_input(
            tree.root(),
            &layout,
            UiTokens::default(),
            &state,
            UiVisualStyle::default(),
            UiRenderLimits::default(),
        )
        .expect("draw list");
        let thumb_color = UiVisualStyle::default().scroll_thumb().expect("default thumb");
        let thumb = list
            .rectangles()
            .iter()
            .find(|rectangle| rectangle.widget == id("scroll") && rectangle.color == thumb_color)
            .expect("scroll thumb");
        let viewport = layout.bounds(id("scroll")).expect("viewport");
        let expected = vertical_scroll_thumb_bounds(
            viewport,
            layout.bounds(id("content")).expect("content").size.height,
            state.scroll_offset(id("scroll")),
            SCROLL_THUMB_THICKNESS,
        )
        .expect("thumb geometry");
        assert_eq!(thumb.bounds, expected);
        assert_eq!(thumb.clip, Some(UiClipRect::new(viewport)));
    }

    #[test]
    fn scroll_view_thumb_can_be_disabled_and_absent_without_overflow() {
        let tree = overflow_scroll_tree();
        let state = UiInputState::default();
        let layout = layout_with_input_state(&tree, Size::new(100, 100), &state).expect("layout");
        let disabled = extract_draw_list_with_input(
            tree.root(),
            &layout,
            UiTokens::default(),
            &state,
            UiVisualStyle::default().with_scroll_thumb(None),
            UiRenderLimits::default(),
        )
        .expect("disabled");
        assert!(
            disabled
                .rectangles()
                .iter()
                .all(|rectangle| rectangle.widget != id("scroll")
                    || rectangle.color != Color::rgba(239, 242, 247, 140))
        );

        let no_overflow = UiBuilder::new(id("root"), LayoutKind::Column)
            .child(
                Widget::scroll_view(id("scroll"))
                    .with_constraints(
                        yuyib_ui::LayoutConstraints::auto()
                            .with_width(yuyib_ui::Dimension::Points(100))
                            .with_height(yuyib_ui::Dimension::Points(40)),
                    )
                    .with_children(vec![
                        Widget::container(id("content"), LayoutKind::Column)
                            .with_constraints(
                                yuyib_ui::LayoutConstraints::auto()
                                    .with_height(yuyib_ui::Dimension::Points(20)),
                            )
                            .with_children(vec![Widget::button(id("item"), "Item")]),
                    ]),
            )
            .build()
            .expect("fit tree");
        let fit_layout =
            layout_with_input_state(&no_overflow, Size::new(100, 100), &state).expect("fit");
        let fit = extract_draw_list_with_input(
            no_overflow.root(),
            &fit_layout,
            UiTokens::default(),
            &state,
            UiVisualStyle::default(),
            UiRenderLimits::default(),
        )
        .expect("fit list");
        assert!(fit.rectangles().iter().all(|rectangle| {
            rectangle.widget != id("scroll")
                || rectangle.color != UiVisualStyle::default().scroll_thumb().unwrap()
        }));
    }

    #[test]
    fn ndc_vertex_geometry_has_two_triangles_per_rectangle() {
        let list = UiDrawList {
            rectangles: vec![UiRectangle {
                widget: id("rect"),
                bounds: yuyib_ui::Rect {
                    origin: yuyib_ui::Point::new(0, 0),
                    size: yuyib_ui::Size::new(50, 25),
                },
                color: Color::rgb(255, 0, 0),
                clip: None,
            }],
        };
        let vertices = vertices_for(&list, [100, 100]).expect("vertices");
        assert_eq!(vertices.len(), 6);
        assert_eq!(vertices[0].position, [-1.0, 1.0]);
        assert_eq!(vertices[2].position, [0.0, 0.5]);
    }

    #[test]
    fn image_extraction_follows_paint_order_and_honours_clip() {
        let first = yuyib_ui::UiImageId::new(11);
        let second = yuyib_ui::UiImageId::new(22);
        let tree = UiBuilder::new(id("root"), LayoutKind::Row)
            .child(Widget::image(id("a"), first))
            .child(Widget::button(id("button"), "Play"))
            .child(Widget::image(id("b"), second))
            .build()
            .expect("tree");
        let layout = yuyib_ui::layout(&tree, Size::new(200, 40)).expect("layout");
        let list = extract_image_draw_list(tree.root(), &layout, UiRenderLimits::default())
            .expect("images");
        assert_eq!(
            list.quads()
                .iter()
                .map(|quad| (quad.widget, quad.image))
                .collect::<Vec<_>>(),
            vec![(id("a"), first), (id("b"), second)]
        );
        assert_eq!(list.quads()[0].bounds.size, Size::new(24, 24));

        let clip = UiClipRect::new(yuyib_ui::Rect {
            origin: yuyib_ui::Point::new(0, 0),
            size: Size::new(12, 12),
        });
        let clipped =
            extract_image_draw_list_clipped(tree.root(), &layout, Some(clip), UiRenderLimits::default())
                .expect("clipped images");
        assert_eq!(clipped.quads().len(), 1);
        assert_eq!(clipped.quads()[0].widget, id("a"));
        assert_eq!(clipped.quads()[0].bounds.size, Size::new(12, 12));
        assert_eq!(clipped.quads()[0].clip, Some(clip));
    }

    #[test]
    fn image_extraction_rejects_bounded_overflow() {
        let tree = UiBuilder::new(id("root"), LayoutKind::Column)
            .child(Widget::image(id("one"), yuyib_ui::UiImageId::new(1)))
            .child(Widget::image(id("two"), yuyib_ui::UiImageId::new(2)))
            .build()
            .expect("tree");
        let layout = yuyib_ui::layout(&tree, Size::new(100, 100)).expect("layout");
        assert!(matches!(
            extract_image_draw_list(
                tree.root(),
                &layout,
                UiRenderLimits {
                    max_rectangles: 100_000,
                    max_images: 1,
                },
            ),
            Err(UiRenderError::TooManyImages { actual: 2, limit: 1 })
        ));
    }
}
