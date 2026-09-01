//! Retained renderer-agnostic native UI data, layout, pointer, and keyboard semantics.
//!
//! This crate owns no window, GPU renderer, Winit integration, text shaping,
//! accessibility bridge, HTML, CSS, or `WebView`. It instead provides a stable
//! tree that such future layers can consume.

#![forbid(unsafe_code)]

use std::{
    collections::{BTreeMap, HashSet},
    fmt,
    hash::{Hash, Hasher},
};

/// Stable widget identifier derived from application-owned source keys.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct WidgetId(u64);

impl WidgetId {
    /// Creates an explicit numeric identifier.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Deterministically creates an FNV-1a identifier from a stable key.
    #[must_use]
    pub fn from_key(key: &str) -> Self {
        let mut value = 14_695_981_039_346_656_037_u64;
        for byte in key.as_bytes() {
            value ^= u64::from(*byte);
            value = value.wrapping_mul(1_099_511_628_211);
        }
        Self(value)
    }

    /// Returns the raw identifier.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// An sRGBA colour.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Color {
    /// Red channel.
    pub red: u8,
    /// Green channel.
    pub green: u8,
    /// Blue channel.
    pub blue: u8,
    /// Alpha channel.
    pub alpha: u8,
}

impl Color {
    /// Creates an opaque colour.
    #[must_use]
    pub const fn rgb(red: u8, green: u8, blue: u8) -> Self {
        Self {
            red,
            green,
            blue,
            alpha: u8::MAX,
        }
    }

    /// Creates a colour with an explicit alpha channel.
    #[must_use]
    pub const fn rgba(red: u8, green: u8, blue: u8, alpha: u8) -> Self {
        Self {
            red,
            green,
            blue,
            alpha,
        }
    }
}

/// Default vertical scrollbar thumb thickness in logical pixels.
pub const SCROLL_THUMB_THICKNESS: u32 = 8;

/// Hit-test width for the thumb/track (may exceed the painted thickness).
pub const SCROLL_THUMB_HIT_THICKNESS: u32 = 16;

/// Minimum thumb height so a tiny overflow remains visible.
const SCROLL_THUMB_MIN_HEIGHT: u32 = 8;

/// Semantic colour role retained by widgets.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ColorToken {
    /// Main background.
    Surface,
    /// Secondary background.
    SurfaceMuted,
    /// Action accent.
    Accent,
    /// Foreground/text.
    Text,
    /// Explicit exceptional colour.
    Custom(Color),
}

/// Concrete palette used by future renderers to resolve tokens.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ColorTokens {
    /// Main surface.
    pub surface: Color,
    /// Muted surface.
    pub surface_muted: Color,
    /// Accent.
    pub accent: Color,
    /// Text.
    pub text: Color,
}

impl Default for ColorTokens {
    fn default() -> Self {
        Self {
            surface: Color::rgb(28, 31, 38),
            surface_muted: Color::rgb(42, 46, 56),
            accent: Color::rgb(82, 137, 255),
            text: Color::rgb(239, 242, 247),
        }
    }
}

impl ColorTokens {
    /// Resolves a semantic role without any renderer dependency.
    #[must_use]
    pub const fn resolve(self, token: ColorToken) -> Color {
        match token {
            ColorToken::Surface => self.surface,
            ColorToken::SurfaceMuted => self.surface_muted,
            ColorToken::Accent => self.accent,
            ColorToken::Text => self.text,
            ColorToken::Custom(color) => color,
        }
    }
}

/// Named spacing scale retained independently from a renderer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SpacingTokens {
    /// Compact spacing.
    pub small: u32,
    /// Normal spacing.
    pub medium: u32,
    /// Group separation.
    pub large: u32,
}

impl Default for SpacingTokens {
    fn default() -> Self {
        Self {
            small: 4,
            medium: 8,
            large: 16,
        }
    }
}

/// Complete palette and spacing token set.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct UiTokens {
    /// Colour palette.
    pub colors: ColorTokens,
    /// Spacing scale.
    pub spacing: SpacingTokens,
}

/// Logical-pixel size.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Size {
    /// Width.
    pub width: u32,
    /// Height.
    pub height: u32,
}

impl Size {
    /// Creates a size.
    #[must_use]
    pub const fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }
}

/// Logical-pixel point.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Point {
    /// Horizontal coordinate.
    pub x: i32,
    /// Vertical coordinate.
    pub y: i32,
}

impl Point {
    /// Creates a point.
    #[must_use]
    pub const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }
}

/// Logical-pixel rectangle.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Rect {
    /// Top-left position.
    pub origin: Point,
    /// Size.
    pub size: Size,
}

impl Rect {
    /// Returns whether the half-open rectangle contains a point.
    #[must_use]
    pub fn contains(self, point: Point) -> bool {
        let right = self.origin.x.saturating_add(to_i32(self.size.width));
        let bottom = self.origin.y.saturating_add(to_i32(self.size.height));
        point.x >= self.origin.x && point.x < right && point.y >= self.origin.y && point.y < bottom
    }

    fn inset(self, inset: Insets) -> Self {
        Self {
            origin: Point::new(
                self.origin.x.saturating_add(to_i32(inset.left)),
                self.origin.y.saturating_add(to_i32(inset.top)),
            ),
            size: Size::new(
                self.size
                    .width
                    .saturating_sub(inset.left.saturating_add(inset.right)),
                self.size
                    .height
                    .saturating_sub(inset.top.saturating_add(inset.bottom)),
            ),
        }
    }
}

/// Padding in logical pixels.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Insets {
    /// Top.
    pub top: u32,
    /// Right.
    pub right: u32,
    /// Bottom.
    pub bottom: u32,
    /// Left.
    pub left: u32,
}

impl Insets {
    /// Uses the same padding at every edge.
    #[must_use]
    pub const fn all(value: u32) -> Self {
        Self {
            top: value,
            right: value,
            bottom: value,
            left: value,
        }
    }
}

/// One width/height layout rule.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Dimension {
    /// Uses widget intrinsic minimum.
    #[default]
    Auto,
    /// Uses exact logical pixels.
    Points(u32),
    /// Shares remaining parent axis space with other fill siblings.
    Fill,
}

/// Widget layout constraints.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LayoutConstraints {
    /// Width rule.
    pub width: Dimension,
    /// Height rule.
    pub height: Dimension,
    /// Required position inside an absolute container.
    pub absolute_position: Option<Point>,
}

impl LayoutConstraints {
    /// Starts with automatic dimensions.
    #[must_use]
    pub const fn auto() -> Self {
        Self {
            width: Dimension::Auto,
            height: Dimension::Auto,
            absolute_position: None,
        }
    }

    /// Sets width.
    #[must_use]
    pub const fn with_width(mut self, width: Dimension) -> Self {
        self.width = width;
        self
    }

    /// Sets height.
    #[must_use]
    pub const fn with_height(mut self, height: Dimension) -> Self {
        self.height = height;
        self
    }

    /// Sets absolute position.
    #[must_use]
    pub const fn with_absolute_position(mut self, position: Point) -> Self {
        self.absolute_position = Some(position);
        self
    }
}

/// Container placement mode.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LayoutKind {
    /// Horizontal flow.
    Row,
    /// Vertical flow.
    Column,
    /// Explicit child positions.
    Absolute,
}

/// Renderer-neutral widget style.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct WidgetStyle {
    /// Optional background colour role.
    pub background: Option<ColorToken>,
    /// Optional foreground colour role.
    pub foreground: Option<ColorToken>,
    /// Inner padding.
    pub padding: Insets,
    /// Flow child gap.
    pub gap: u32,
    /// Intrinsic size before constraints.
    pub min_size: Size,
}

impl WidgetStyle {
    /// Sets a background token.
    #[must_use]
    pub const fn with_background(mut self, background: ColorToken) -> Self {
        self.background = Some(background);
        self
    }

    /// Sets a foreground token, normally used by text and icons.
    #[must_use]
    pub const fn with_foreground(mut self, foreground: ColorToken) -> Self {
        self.foreground = Some(foreground);
        self
    }

    /// Sets padding.
    #[must_use]
    pub const fn with_padding(mut self, padding: Insets) -> Self {
        self.padding = padding;
        self
    }

    /// Sets flow gap.
    #[must_use]
    pub const fn with_gap(mut self, gap: u32) -> Self {
        self.gap = gap;
        self
    }

    /// Sets intrinsic size.
    #[must_use]
    pub const fn with_min_size(mut self, min_size: Size) -> Self {
        self.min_size = min_size;
        self
    }
}

/// Opaque application-owned image/icon identifier for retained UI.
///
/// The UI tree stores only this id. Decode, GPU upload and atlas packing stay
/// with the host (or a later `ApplicationUi` image registry).
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct UiImageId(u64);

impl UiImageId {
    /// Creates an application-defined image id.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the raw identifier.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for UiImageId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "ui-image-{}", self.0)
    }
}

/// Semantic widget type.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WidgetKind {
    /// Structural container.
    Container(LayoutKind),
    /// Bounded vertical viewport with exactly one column content child.
    ScrollView,
    /// Thin divider between layout areas.
    Separator,
    /// Spacer for fixed logical gaps that participates only in layout.
    Spacer,
    /// Non-interactive text semantic.
    Label(String),
    /// Pressable semantic action.
    Button(String),
    /// Boolean control that typically flips state and emits [`UiAction::Toggled`].
    Checkbox(String, bool),
    /// Boolean control that typically flips state and emits [`UiAction::Toggled`].
    Toggle(String, bool),
    /// Non-interactive image/icon keyed by [`UiImageId`].
    Image(UiImageId),
}

impl WidgetKind {
    fn interactive(&self) -> bool {
        matches!(
            self,
            Self::Button(_) | Self::Checkbox(_, _) | Self::Toggle(_, _)
        )
    }
}

/// One retained widget node.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Widget {
    id: WidgetId,
    kind: WidgetKind,
    enabled: bool,
    constraints: LayoutConstraints,
    style: WidgetStyle,
    children: Vec<Widget>,
}

impl Widget {
    /// Creates a container.
    #[must_use]
    pub fn container(id: WidgetId, kind: LayoutKind) -> Self {
        Self {
            id,
            kind: WidgetKind::Container(kind),
            enabled: true,
            constraints: LayoutConstraints::default(),
            style: WidgetStyle::default(),
            children: Vec::new(),
        }
    }

    /// Creates a label.
    #[must_use]
    pub fn label(id: WidgetId, text: impl Into<String>) -> Self {
        Self {
            id,
            kind: WidgetKind::Label(text.into()),
            enabled: true,
            constraints: LayoutConstraints::default(),
            style: WidgetStyle::default()
                .with_foreground(ColorToken::Text)
                .with_min_size(Size::new(0, 20)),
            children: Vec::new(),
        }
    }

    /// Creates a button with a useful native-prototype intrinsic size.
    #[must_use]
    pub fn button(id: WidgetId, text: impl Into<String>) -> Self {
        Self {
            id,
            kind: WidgetKind::Button(text.into()),
            enabled: true,
            constraints: LayoutConstraints::default(),
            style: WidgetStyle::default()
                .with_background(ColorToken::Accent)
                .with_foreground(ColorToken::Text)
                .with_padding(Insets::all(8))
                .with_min_size(Size::new(80, 32)),
            children: Vec::new(),
        }
    }

    /// Creates a non-interactive thin separator.
    #[must_use]
    pub fn separator(id: WidgetId) -> Self {
        Self {
            id,
            kind: WidgetKind::Separator,
            enabled: true,
            constraints: LayoutConstraints::default(),
            style: WidgetStyle::default()
                .with_background(ColorToken::SurfaceMuted)
                .with_min_size(Size::new(0, 2)),
            children: Vec::new(),
        }
    }

    /// Creates a layout spacer.
    #[must_use]
    pub fn spacer(id: WidgetId) -> Self {
        Self {
            id,
            kind: WidgetKind::Spacer,
            enabled: true,
            constraints: LayoutConstraints::default(),
            style: WidgetStyle::default().with_min_size(Size::new(8, 8)),
            children: Vec::new(),
        }
    }

    /// Creates a checkbox control with explicit checked state.
    #[must_use]
    pub fn checkbox(id: WidgetId, text: impl Into<String>, checked: bool) -> Self {
        Self {
            id,
            kind: WidgetKind::Checkbox(text.into(), checked),
            enabled: true,
            constraints: LayoutConstraints::default(),
            style: WidgetStyle::default()
                .with_foreground(ColorToken::Text)
                .with_min_size(Size::new(0, 26)),
            children: Vec::new(),
        }
    }

    /// Creates a toggle control with explicit checked state.
    #[must_use]
    pub fn toggle(id: WidgetId, text: impl Into<String>, checked: bool) -> Self {
        Self {
            id,
            kind: WidgetKind::Toggle(text.into(), checked),
            enabled: true,
            constraints: LayoutConstraints::default(),
            style: WidgetStyle::default()
                .with_foreground(ColorToken::Text)
                .with_min_size(Size::new(0, 26)),
            children: Vec::new(),
        }
    }

    /// Creates a non-interactive image/icon slot.
    ///
    /// Intrinsic size defaults to 24×24; override with
    /// [`Self::with_constraints`] / style `min_size`. The host resolves
    /// [`UiImageId`] to pixels or a GPU texture outside this crate.
    #[must_use]
    pub fn image(id: WidgetId, image: UiImageId) -> Self {
        Self {
            id,
            kind: WidgetKind::Image(image),
            enabled: true,
            constraints: LayoutConstraints::default(),
            style: WidgetStyle::default().with_min_size(Size::new(24, 24)),
            children: Vec::new(),
        }
    }

    /// Creates a bounded vertical scroll viewport.
    #[must_use]
    pub fn scroll_view(id: WidgetId) -> Self {
        Self {
            id,
            kind: WidgetKind::ScrollView,
            enabled: true,
            constraints: LayoutConstraints::default(),
            style: WidgetStyle::default(),
            children: Vec::new(),
        }
    }

    /// Applies layout constraints.
    #[must_use]
    pub const fn with_constraints(mut self, constraints: LayoutConstraints) -> Self {
        self.constraints = constraints;
        self
    }

    /// Sets whether this widget may receive user interaction.
    ///
    /// Disabled buttons remain in layout and paint order but are excluded from
    /// pointer hit-testing and keyboard focus traversal. Containers, labels,
    /// media, separators and spacers are non-interactive regardless of this
    /// flag.
    #[must_use]
    pub const fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Applies style.
    #[must_use]
    pub const fn with_style(mut self, style: WidgetStyle) -> Self {
        self.style = style;
        self
    }

    /// Appends children.
    #[must_use]
    pub fn with_children(mut self, children: Vec<Self>) -> Self {
        self.children = children;
        self
    }

    /// Returns stable ID.
    #[must_use]
    pub const fn id(&self) -> WidgetId {
        self.id
    }

    /// Returns semantic type.
    #[must_use]
    pub const fn kind(&self) -> &WidgetKind {
        &self.kind
    }

    /// Returns image id when this widget is an [`WidgetKind::Image`].
    #[must_use]
    pub const fn image_id(&self) -> Option<UiImageId> {
        match &self.kind {
            WidgetKind::Image(id) => Some(*id),
            WidgetKind::Container(_)
            | WidgetKind::ScrollView
            | WidgetKind::Separator
            | WidgetKind::Spacer
            | WidgetKind::Label(_)
            | WidgetKind::Button(_)
            | WidgetKind::Checkbox(_, _)
            | WidgetKind::Toggle(_, _) => None,
        }
    }

    /// Returns the label or button caption, if this widget has one.
    ///
    /// Renderers should prefer this method over matching [`WidgetKind`] so
    /// the semantic representation may grow without forcing every backend to
    /// duplicate that match.
    #[must_use]
    pub fn text(&self) -> Option<&str> {
        match &self.kind {
            WidgetKind::Label(text)
            | WidgetKind::Button(text)
            | WidgetKind::Checkbox(text, _)
            | WidgetKind::Toggle(text, _) => Some(text),
            WidgetKind::Container(_)
            | WidgetKind::ScrollView
            | WidgetKind::Separator
            | WidgetKind::Spacer
            | WidgetKind::Image(_) => None,
        }
    }

    /// Returns `true` when this widget is checked.
    #[must_use]
    pub const fn is_checked(&self) -> Option<bool> {
        match &self.kind {
            WidgetKind::Checkbox(_, checked) | WidgetKind::Toggle(_, checked) => Some(*checked),
            WidgetKind::Container(_)
            | WidgetKind::ScrollView
            | WidgetKind::Separator
            | WidgetKind::Spacer
            | WidgetKind::Label(_)
            | WidgetKind::Button(_)
            | WidgetKind::Image(_) => None,
        }
    }

    /// Returns constraints.
    #[must_use]
    pub const fn constraints(&self) -> LayoutConstraints {
        self.constraints
    }

    /// Returns style.
    #[must_use]
    pub const fn style(&self) -> WidgetStyle {
        self.style
    }

    /// Returns whether this widget is enabled for interaction.
    #[must_use]
    pub const fn enabled(&self) -> bool {
        self.enabled
    }

    /// Returns children.
    #[must_use]
    pub fn children(&self) -> &[Self] {
        &self.children
    }
}

/// Bounded UI tree limits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UiLimits {
    /// Maximum widget count.
    pub max_widgets: usize,
    /// Maximum nested depth.
    pub max_depth: usize,
}

impl Default for UiLimits {
    fn default() -> Self {
        Self {
            max_widgets: 100_000,
            max_depth: 256,
        }
    }
}

/// Validated immutable retained UI tree.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UiTree {
    root: Widget,
}

impl UiTree {
    /// Returns root widget.
    #[must_use]
    pub const fn root(&self) -> &Widget {
        &self.root
    }
}

/// Compact high-level retained UI builder.
pub struct UiBuilder {
    root: Widget,
    limits: UiLimits,
}

impl UiBuilder {
    /// Starts a root container.
    #[must_use]
    pub fn new(id: WidgetId, layout: LayoutKind) -> Self {
        Self {
            root: Widget::container(id, layout),
            limits: UiLimits::default(),
        }
    }

    /// Sets tree validation limits.
    #[must_use]
    pub const fn with_limits(mut self, limits: UiLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Adds root child.
    #[must_use]
    pub fn child(mut self, child: Widget) -> Self {
        self.root.children.push(child);
        self
    }

    /// Adds nested container through a short child-builder closure.
    #[must_use]
    pub fn container(
        mut self,
        id: WidgetId,
        layout: LayoutKind,
        style: WidgetStyle,
        build: impl FnOnce(&mut ChildrenBuilder),
    ) -> Self {
        let mut children = ChildrenBuilder::default();
        build(&mut children);
        self.root.children.push(
            Widget::container(id, layout)
                .with_style(style)
                .with_children(children.children),
        );
        self
    }

    /// Validates and produces tree.
    ///
    /// # Errors
    ///
    /// Returns UI tree validation errors.
    pub fn build(self) -> Result<UiTree, UiError> {
        let mut ids = HashSet::new();
        let mut count = 0;
        validate_widget(&self.root, 1, self.limits, &mut ids, &mut count)?;
        Ok(UiTree { root: self.root })
    }
}

/// Nested child builder used by the high-level container method.
#[derive(Default)]
pub struct ChildrenBuilder {
    children: Vec<Widget>,
}

impl ChildrenBuilder {
    /// Adds arbitrary child.
    pub fn child(&mut self, child: Widget) -> &mut Self {
        self.children.push(child);
        self
    }

    /// Adds label child.
    pub fn label(&mut self, id: WidgetId, text: impl Into<String>) -> &mut Self {
        self.child(Widget::label(id, text))
    }

    /// Adds button child.
    pub fn button(&mut self, id: WidgetId, text: impl Into<String>) -> &mut Self {
        self.child(Widget::button(id, text))
    }

    /// Adds checkbox child.
    pub fn checkbox(&mut self, id: WidgetId, text: impl Into<String>, checked: bool) -> &mut Self {
        self.child(Widget::checkbox(id, text, checked))
    }

    /// Adds toggle child.
    pub fn toggle(&mut self, id: WidgetId, text: impl Into<String>, checked: bool) -> &mut Self {
        self.child(Widget::toggle(id, text, checked))
    }

    /// Adds separator child.
    pub fn separator(&mut self, id: WidgetId) -> &mut Self {
        self.child(Widget::separator(id))
    }

    /// Adds spacer child.
    pub fn spacer(&mut self, id: WidgetId) -> &mut Self {
        self.child(Widget::spacer(id))
    }
}

/// Deterministic layout output.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct UiLayout {
    bounds: BTreeMap<WidgetId, Rect>,
    clips: BTreeMap<WidgetId, Rect>,
    paint_order: Vec<WidgetId>,
    interactive: HashSet<WidgetId>,
}

impl UiLayout {
    /// Returns widget rectangle.
    #[must_use]
    pub fn bounds(&self, id: WidgetId) -> Option<Rect> {
        self.bounds.get(&id).copied()
    }

    /// Returns the effective scroll viewport clip for a widget, if any.
    #[must_use]
    pub fn clip(&self, id: WidgetId) -> Option<Rect> {
        self.clips.get(&id).copied()
    }

    /// Returns stable tree paint order.
    #[must_use]
    pub fn paint_order(&self) -> &[WidgetId] {
        &self.paint_order
    }

    /// Iterates enabled interactive widgets in deterministic keyboard-focus order.
    ///
    /// The order is a depth-first preorder traversal of the validated retained
    /// tree, matching [`Self::paint_order`]. Widget identifiers are unique by
    /// tree validation, so this does not need an ambiguous ID tie-breaker.
    pub fn focus_order(&self) -> impl Iterator<Item = WidgetId> + '_ {
        self.paint_order
            .iter()
            .copied()
            .filter(|id| self.interactive.contains(id))
    }
}

/// Calculates retained tree layout in a logical-pixel viewport.
///
/// # Errors
///
/// Returns an error for absolute children without positions or coordinate overflow.
pub fn layout(tree: &UiTree, viewport: Size) -> Result<UiLayout, UiError> {
    let mut output = UiLayout::default();
    layout_widget(
        tree.root(),
        Rect {
            origin: Point::default(),
            size: viewport,
        },
        &mut output,
    )?;
    Ok(output)
}

/// Calculates layout using retained scroll offsets from `state`.
#[must_use]
pub fn layout_with_input_state(
    tree: &UiTree,
    viewport: Size,
    state: &UiInputState,
) -> Result<UiLayout, UiError> {
    let mut output = layout(tree, viewport)?;
    apply_scroll_views(tree.root(), &mut output, state, None)?;
    Ok(output)
}

/// Supplies intrinsic content size for semantic widgets during layout.
///
/// The layout engine calls this only for widgets that expose text
/// and have at least
/// one [`Dimension::Auto`] axis. `available` is the inner size of the direct
/// parent. A measurer may be called more than once while flow space is
/// calculated, so it should be deterministic and normally cache expensive
/// work such as text shaping itself.
pub trait UiMeasurer {
    /// Error reported while measuring one widget.
    type Error;

    /// Measures content before widget padding is added by the layout engine.
    ///
    /// # Errors
    ///
    /// Returns an implementation-defined measurement error. The layout caller
    /// receives it unchanged in [`LayoutWithMeasureError::Measure`].
    fn measure(&mut self, widget: &Widget, available: Size) -> Result<Size, Self::Error>;
}

/// Failure from [`layout_with_measurer`].
#[derive(Debug)]
pub enum LayoutWithMeasureError<E> {
    /// Ordinary tree constraint or coordinate failure.
    Layout(UiError),
    /// The supplied content measurer failed for this semantic widget.
    Measure {
        /// Stable identifier of the widget that was being measured.
        widget: WidgetId,
        /// Original measuring failure; it is never stringified or discarded.
        source: E,
    },
}

impl<E: std::fmt::Display> std::fmt::Display for LayoutWithMeasureError<E> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Layout(source) => write!(formatter, "UI layout failed: {source}"),
            Self::Measure { widget, source } => {
                write!(
                    formatter,
                    "could not measure UI widget {}: {source}",
                    widget.get()
                )
            }
        }
    }
}

impl<E: std::error::Error + 'static> std::error::Error for LayoutWithMeasureError<E> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Layout(source) => Some(source),
            Self::Measure { source, .. } => Some(source),
        }
    }
}

/// Calculates layout while using caller-provided intrinsic measurements.
///
/// This is the low-level extension point for native text, icons, and other
/// content whose natural size is not known to `yuyib-ui`. Explicit `Points`
/// and `Fill` dimensions keep their existing priority; measured text affects
/// only `Auto` axes and is combined with `WidgetStyle::min_size` and padding.
/// [`layout`] remains available for rectangle-only UIs.
///
/// # Errors
///
/// Returns the originating UI error or preserves the caller's measurement
/// error together with the widget identifier.
pub fn layout_with_measurer<M: UiMeasurer>(
    tree: &UiTree,
    viewport: Size,
    measurer: &mut M,
) -> Result<UiLayout, LayoutWithMeasureError<M::Error>> {
    let mut output = UiLayout::default();
    layout_widget_with_measurer(
        tree.root(),
        Rect {
            origin: Point::default(),
            size: viewport,
        },
        &mut output,
        measurer,
    )?;
    Ok(output)
}

/// Measured counterpart of [`layout_with_input_state`].
pub fn layout_with_measurer_and_input_state<M: UiMeasurer>(
    tree: &UiTree,
    viewport: Size,
    measurer: &mut M,
    state: &UiInputState,
) -> Result<UiLayout, LayoutWithMeasureError<M::Error>> {
    let mut output = layout_with_measurer(tree, viewport, measurer)?;
    apply_scroll_views(tree.root(), &mut output, state, None)
        .map_err(LayoutWithMeasureError::Layout)?;
    Ok(output)
}

/// Pointer sample.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PointerInput {
    /// Pointer move.
    Move(Point),
    /// Primary down.
    PrimaryDown(Point),
    /// Primary up.
    PrimaryUp(Point),
}

/// Platform-normalised keyboard command for retained UI focus semantics.
///
/// A windowing adapter decides when a physical key produces one of these
/// commands, including its repeat policy. Does not
/// process text composition, IME, keyboard layout, or accessibility APIs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KeyboardInput {
    /// Moves focus to the next enabled interactive widget, wrapping after the last one.
    Tab,
    /// Moves focus to the previous enabled interactive widget, wrapping before the first one.
    ShiftTab,
    /// Activates the focused enabled interactive widget.
    Enter,
    /// Activates the focused enabled interactive widget.
    Space,
}

/// Retained transient pointer state.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct UiInputState {
    hovered: Option<WidgetId>,
    pressed: Option<WidgetId>,
    focused: Option<WidgetId>,
    scroll_offsets: BTreeMap<WidgetId, u32>,
    scroll_drag: Option<ScrollThumbDrag>,
}

/// Active vertical scrollbar thumb drag.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ScrollThumbDrag {
    scroll_view: WidgetId,
    /// Pointer Y minus thumb top at grab time (keeps the grab point stable).
    grab_offset_y: i32,
}

impl UiInputState {
    /// Clears pointer hover/press/drag state without changing keyboard focus.
    ///
    /// A platform adapter normally calls this after its cursor leaves the UI
    /// surface. It intentionally emits no synthetic action.
    pub fn clear_pointer(&mut self) {
        self.hovered = None;
        self.pressed = None;
        self.scroll_drag = None;
    }

    /// Clears pointer and keyboard-focus state without emitting an action.
    ///
    /// A platform adapter normally calls this after native window focus loss.
    pub fn clear(&mut self) {
        self.clear_pointer();
        self.focused = None;
    }

    /// Returns a stable fingerprint of scroll offsets that affect retained layout.
    ///
    /// Hover/press/focus are excluded: they change overlays only, not widget
    /// bounds or scroll clips. Hosts use this to skip full layout rebuilds on
    /// pointer motion.
    #[must_use]
    pub fn scroll_layout_fingerprint(&self) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        for (id, offset) in &self.scroll_offsets {
            id.get().hash(&mut hasher);
            offset.hash(&mut hasher);
        }
        // Thumb drag changes scroll continuously; include presence so a zeroed
        // map during drag start still invalidates when offsets update.
        self.scroll_drag.is_some().hash(&mut hasher);
        hasher.finish()
    }

    /// Returns hovered widget.
    #[must_use]
    pub const fn hovered(&self) -> Option<WidgetId> {
        self.hovered
    }

    /// Returns pressed widget.
    #[must_use]
    pub const fn pressed(&self) -> Option<WidgetId> {
        self.pressed
    }

    /// Returns the keyboard-focused enabled interactive widget, if any.
    #[must_use]
    pub const fn focused(&self) -> Option<WidgetId> {
        self.focused
    }

    /// Returns a scroll viewport's retained vertical offset.
    #[must_use]
    pub fn scroll_offset(&self, id: WidgetId) -> u32 {
        self.scroll_offsets.get(&id).copied().unwrap_or(0)
    }

    /// Returns the scroll viewport currently captured by a thumb drag, if any.
    #[must_use]
    pub const fn scroll_dragging(&self) -> Option<WidgetId> {
        match self.scroll_drag {
            Some(drag) => Some(drag.scroll_view),
            None => None,
        }
    }
}

/// Semantic interaction transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UiAction {
    /// Pointer entered button.
    Hovered(WidgetId),
    /// Pointer pressed button.
    Pressed(WidgetId),
    /// Pointer released on the same button.
    Clicked(WidgetId),
    /// Checkbox/Toggle was activated and its checked value was toggled.
    Toggled(WidgetId, bool),
    /// Keyboard focus moved to an enabled interactive widget.
    Focused(WidgetId),
    /// Focused interactive widget was activated with Enter or Space.
    Activated(WidgetId),
    /// A viewport accepted wheel or scrollbar-thumb input and changed its
    /// retained offset.
    Scrolled(WidgetId),
}

/// Input response.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct UiResponse {
    target: Option<WidgetId>,
    actions: Vec<UiAction>,
}

impl UiResponse {
    /// Returns the target of this pointer or keyboard transition.
    #[must_use]
    pub const fn target(&self) -> Option<WidgetId> {
        self.target
    }

    /// Returns ordered actions.
    #[must_use]
    pub fn actions(&self) -> &[UiAction] {
        &self.actions
    }
}

/// Hit-tests a pointer sample and emits semantic interactive / scrollbar events.
///
/// Scrollbar thumb drag has pointer capture: while a drag is active, Move and
/// PrimaryUp update that viewport and do not hit-test interactive widgets.
/// PrimaryDown on
/// the thumb starts a drag; PrimaryDown on the track jumps the thumb under the
/// pointer and starts a drag. Wheel scrolling remains [`handle_scroll_input`].
///
/// # Errors
///
/// Returns an error if layout does not represent every widget in tree.
pub fn handle_input(
    tree: &UiTree,
    layout: &UiLayout,
    state: &mut UiInputState,
    input: PointerInput,
) -> Result<UiResponse, UiError> {
    validate_layout(tree.root(), layout)?;
    let point = match input {
        PointerInput::Move(point)
        | PointerInput::PrimaryDown(point)
        | PointerInput::PrimaryUp(point) => point,
    };

    if let Some(drag) = state.scroll_drag {
        let response = apply_scroll_thumb_drag(tree, layout, state, drag, point)?;
        if matches!(input, PointerInput::PrimaryUp(_)) {
            state.scroll_drag = None;
            state.pressed = None;
        }
        return Ok(response);
    }

    if let PointerInput::PrimaryDown(point) = input
        && let Some(hit) = hit_scrollbar(tree, layout, state, point)?
    {
        state.pressed = None;
        match hit {
            ScrollbarHit::Thumb {
                scroll_view,
                grab_offset_y,
            } => {
                state.scroll_drag = Some(ScrollThumbDrag {
                    scroll_view,
                    grab_offset_y,
                });
                return Ok(UiResponse {
                    target: Some(scroll_view),
                    actions: Vec::new(),
                });
            }
            ScrollbarHit::Track { scroll_view } => {
                let response = jump_scroll_to_track_point(tree, layout, state, scroll_view, point)?;
                if let Some(thumb) = scroll_thumb_bounds(tree, layout, state, scroll_view)? {
                    state.scroll_drag = Some(ScrollThumbDrag {
                        scroll_view,
                        grab_offset_y: point.y.saturating_sub(thumb.origin.y),
                    });
                }
                return Ok(response);
            }
        }
    }

    let target = layout.paint_order.iter().rev().copied().find(|id| {
        layout.interactive.contains(id)
            && layout.bounds(*id).is_some_and(|rect| rect.contains(point))
            && layout.clip(*id).is_none_or(|clip| clip.contains(point))
    });
    let mut actions = Vec::new();
    if target != state.hovered {
        state.hovered = target;
        if let Some(id) = target {
            actions.push(UiAction::Hovered(id));
        }
    }
    match input {
        PointerInput::Move(_) => {}
        PointerInput::PrimaryDown(_) => {
            state.pressed = target;
            if let Some(id) = target {
                actions.push(UiAction::Pressed(id));
            }
        }
        PointerInput::PrimaryUp(_) => {
            if state.pressed == target
                && let Some(id) = target
            {
                actions.push(UiAction::Clicked(id));
                if let Some(checked) = toggled_state_for_id(tree.root(), id) {
                    actions.push(UiAction::Toggled(id, checked));
                }
            }
            state.pressed = None;
        }
    }
    Ok(UiResponse { target, actions })
}

/// Applies one semantic vertical wheel delta to the scroll viewport at `point`.
///
/// Positive deltas move content towards the top; negative deltas move it
/// towards the bottom. The offset always remains within content overflow.
pub fn handle_scroll_input(
    tree: &UiTree,
    layout: &UiLayout,
    state: &mut UiInputState,
    point: Point,
    vertical_delta: i32,
) -> Result<UiResponse, UiError> {
    validate_layout(tree.root(), layout)?;
    let target = layout.paint_order.iter().rev().copied().find(|id| {
        scroll_view_by_id(tree.root(), *id).is_some()
            && layout.bounds(*id).is_some_and(|rect| rect.contains(point))
            && layout.clip(*id).is_none_or(|clip| clip.contains(point))
    });
    let Some(id) = target else {
        return Ok(UiResponse::default());
    };
    let viewport = layout.bounds(id).ok_or(UiError::UnknownLayoutWidget(id))?;
    let content = scroll_view_by_id(tree.root(), id)
        .and_then(|widget| widget.children.first())
        .and_then(|child| layout.bounds(child.id))
        .ok_or(UiError::UnknownLayoutWidget(id))?;
    let maximum = content.size.height.saturating_sub(viewport.size.height);
    let current = state.scroll_offset(id);
    let next = if vertical_delta.is_positive() {
        current.saturating_sub(vertical_delta.unsigned_abs())
    } else {
        current
            .saturating_add(vertical_delta.unsigned_abs())
            .min(maximum)
    };
    if next == current {
        return Ok(UiResponse {
            target: Some(id),
            actions: Vec::new(),
        });
    }
    state.scroll_offsets.insert(id, next);
    Ok(UiResponse {
        target: Some(id),
        actions: vec![UiAction::Scrolled(id)],
    })
}

/// Computes the vertical scrollbar thumb inside a scroll viewport.
///
/// Returns [`None`] when content does not overflow the viewport. `scroll_offset`
/// is clamped to the same overflow range as [`handle_scroll_input`]. This is a
/// pure geometry helper for renderers; it does not hit-test or mutate input.
#[must_use]
pub fn vertical_scroll_thumb_bounds(
    viewport: Rect,
    content_height: u32,
    scroll_offset: u32,
    thickness: u32,
) -> Option<Rect> {
    if thickness == 0 || viewport.size.width == 0 || viewport.size.height == 0 {
        return None;
    }
    let overflow = content_height.saturating_sub(viewport.size.height);
    if overflow == 0 {
        return None;
    }
    let thumb_height = ((u64::from(viewport.size.height) * u64::from(viewport.size.height))
        / u64::from(content_height.max(1)))
    .clamp(
        u64::from(SCROLL_THUMB_MIN_HEIGHT.min(viewport.size.height)),
        u64::from(viewport.size.height),
    );
    let thumb_height = u32::try_from(thumb_height).unwrap_or(viewport.size.height);
    let travel = viewport.size.height.saturating_sub(thumb_height);
    let offset = scroll_offset.min(overflow);
    let thumb_y = if travel == 0 || overflow == 0 {
        0
    } else {
        u32::try_from((u64::from(offset) * u64::from(travel)) / u64::from(overflow))
            .unwrap_or(travel)
            .min(travel)
    };
    let thickness = thickness.min(viewport.size.width);
    Some(Rect {
        origin: Point::new(
            viewport
                .origin
                .x
                .saturating_add(to_i32(viewport.size.width.saturating_sub(thickness))),
            viewport.origin.y.saturating_add(to_i32(thumb_y)),
        ),
        size: Size::new(thickness, thumb_height),
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ScrollbarHit {
    Thumb {
        scroll_view: WidgetId,
        grab_offset_y: i32,
    },
    Track {
        scroll_view: WidgetId,
    },
}

fn hit_scrollbar(
    tree: &UiTree,
    layout: &UiLayout,
    state: &UiInputState,
    point: Point,
) -> Result<Option<ScrollbarHit>, UiError> {
    for id in layout.paint_order.iter().rev().copied() {
        let Some(metrics) = scroll_view_metrics(tree, layout, id)? else {
            continue;
        };
        if !metrics.viewport.contains(point)
            || layout.clip(id).is_some_and(|clip| !clip.contains(point))
        {
            continue;
        }
        let Some(thumb) = vertical_scroll_thumb_bounds(
            metrics.viewport,
            metrics.content_height,
            state.scroll_offset(id),
            SCROLL_THUMB_THICKNESS,
        ) else {
            continue;
        };
        let hit_thumb = thumb_hit_bounds(thumb, metrics.viewport);
        if hit_thumb.contains(point) {
            return Ok(Some(ScrollbarHit::Thumb {
                scroll_view: id,
                grab_offset_y: point.y.saturating_sub(thumb.origin.y),
            }));
        }
        if track_hit_bounds(metrics.viewport).contains(point) {
            return Ok(Some(ScrollbarHit::Track { scroll_view: id }));
        }
    }
    Ok(None)
}

fn thumb_hit_bounds(thumb: Rect, viewport: Rect) -> Rect {
    let width = SCROLL_THUMB_HIT_THICKNESS
        .max(thumb.size.width)
        .min(viewport.size.width);
    Rect {
        origin: Point::new(
            viewport
                .origin
                .x
                .saturating_add(to_i32(viewport.size.width.saturating_sub(width))),
            thumb.origin.y,
        ),
        size: Size::new(width, thumb.size.height),
    }
}

fn track_hit_bounds(viewport: Rect) -> Rect {
    let width = SCROLL_THUMB_HIT_THICKNESS
        .max(SCROLL_THUMB_THICKNESS)
        .min(viewport.size.width);
    Rect {
        origin: Point::new(
            viewport
                .origin
                .x
                .saturating_add(to_i32(viewport.size.width.saturating_sub(width))),
            viewport.origin.y,
        ),
        size: Size::new(width, viewport.size.height),
    }
}

struct ScrollViewMetrics {
    viewport: Rect,
    content_height: u32,
}

fn scroll_view_metrics(
    tree: &UiTree,
    layout: &UiLayout,
    id: WidgetId,
) -> Result<Option<ScrollViewMetrics>, UiError> {
    let Some(widget) = scroll_view_by_id(tree.root(), id) else {
        return Ok(None);
    };
    let viewport = layout.bounds(id).ok_or(UiError::UnknownLayoutWidget(id))?;
    let content = widget
        .children
        .first()
        .and_then(|child| layout.bounds(child.id))
        .ok_or(UiError::UnknownLayoutWidget(id))?;
    Ok(Some(ScrollViewMetrics {
        viewport,
        content_height: content.size.height,
    }))
}

fn scroll_thumb_bounds(
    tree: &UiTree,
    layout: &UiLayout,
    state: &UiInputState,
    id: WidgetId,
) -> Result<Option<Rect>, UiError> {
    let Some(metrics) = scroll_view_metrics(tree, layout, id)? else {
        return Ok(None);
    };
    Ok(vertical_scroll_thumb_bounds(
        metrics.viewport,
        metrics.content_height,
        state.scroll_offset(id),
        SCROLL_THUMB_THICKNESS,
    ))
}

fn apply_scroll_thumb_drag(
    tree: &UiTree,
    layout: &UiLayout,
    state: &mut UiInputState,
    drag: ScrollThumbDrag,
    point: Point,
) -> Result<UiResponse, UiError> {
    let Some(metrics) = scroll_view_metrics(tree, layout, drag.scroll_view)? else {
        state.scroll_drag = None;
        return Ok(UiResponse::default());
    };
    let overflow = metrics
        .content_height
        .saturating_sub(metrics.viewport.size.height);
    if overflow == 0 {
        state.scroll_drag = None;
        state.scroll_offsets.remove(&drag.scroll_view);
        return Ok(UiResponse {
            target: Some(drag.scroll_view),
            actions: Vec::new(),
        });
    }
    let Some(thumb) = vertical_scroll_thumb_bounds(
        metrics.viewport,
        metrics.content_height,
        state.scroll_offset(drag.scroll_view),
        SCROLL_THUMB_THICKNESS,
    ) else {
        state.scroll_drag = None;
        return Ok(UiResponse::default());
    };
    let travel = metrics
        .viewport
        .size
        .height
        .saturating_sub(thumb.size.height);
    let thumb_top = point.y.saturating_sub(drag.grab_offset_y);
    let relative = thumb_top.saturating_sub(metrics.viewport.origin.y);
    let thumb_y = relative.clamp(0, to_i32(travel));
    let next = if travel == 0 {
        0
    } else {
        let thumb_y = u32::try_from(thumb_y).unwrap_or(0);
        u32::try_from((u64::from(thumb_y) * u64::from(overflow)) / u64::from(travel))
            .unwrap_or(overflow)
            .min(overflow)
    };
    set_scroll_offset(state, drag.scroll_view, next)
}

fn jump_scroll_to_track_point(
    tree: &UiTree,
    layout: &UiLayout,
    state: &mut UiInputState,
    id: WidgetId,
    point: Point,
) -> Result<UiResponse, UiError> {
    let Some(metrics) = scroll_view_metrics(tree, layout, id)? else {
        return Ok(UiResponse::default());
    };
    let overflow = metrics
        .content_height
        .saturating_sub(metrics.viewport.size.height);
    if overflow == 0 {
        return Ok(UiResponse {
            target: Some(id),
            actions: Vec::new(),
        });
    }
    let Some(thumb) = vertical_scroll_thumb_bounds(
        metrics.viewport,
        metrics.content_height,
        state.scroll_offset(id),
        SCROLL_THUMB_THICKNESS,
    ) else {
        return Ok(UiResponse {
            target: Some(id),
            actions: Vec::new(),
        });
    };
    let travel = metrics
        .viewport
        .size
        .height
        .saturating_sub(thumb.size.height);
    let half = to_i32(thumb.size.height / 2);
    let relative = point
        .y
        .saturating_sub(metrics.viewport.origin.y)
        .saturating_sub(half);
    let thumb_y = relative.clamp(0, to_i32(travel));
    let next = if travel == 0 {
        0
    } else {
        let thumb_y = u32::try_from(thumb_y).unwrap_or(0);
        u32::try_from((u64::from(thumb_y) * u64::from(overflow)) / u64::from(travel))
            .unwrap_or(overflow)
            .min(overflow)
    };
    set_scroll_offset(state, id, next)
}

fn set_scroll_offset(
    state: &mut UiInputState,
    id: WidgetId,
    next: u32,
) -> Result<UiResponse, UiError> {
    let current = state.scroll_offset(id);
    if next == current {
        return Ok(UiResponse {
            target: Some(id),
            actions: Vec::new(),
        });
    }
    state.scroll_offsets.insert(id, next);
    Ok(UiResponse {
        target: Some(id),
        actions: vec![UiAction::Scrolled(id)],
    })
}

fn toggled_state_for_id(widget: &Widget, id: WidgetId) -> Option<bool> {
    if widget.id == id {
        return match &widget.kind {
            WidgetKind::Checkbox(_, checked) | WidgetKind::Toggle(_, checked) => Some(!checked),
            _ => None,
        };
    }
    widget
        .children
        .iter()
        .find_map(|child| toggled_state_for_id(child, id))
}

/// Handles one platform-normalised keyboard command.
///
/// Focus traversal follows [`UiLayout::focus_order`] and wraps in either
/// direction. A stale focus ID, for example after a host replaces its UI tree
/// while retaining [`UiInputState`], behaves like no focus: Tab selects the
/// first enabled interactive widget and Shift+Tab selects the last. Enter and
/// Space activate a currently focused interactive widget; checkbox/toggle
/// controls additionally emit [`UiAction::Toggled`].
///
/// Pointer input remains independent: [`handle_input`] does not change
/// keyboard focus or emit keyboard actions, preserving its existing semantics.
///
/// # Errors
///
/// Returns an error if layout does not represent every widget in tree.
pub fn handle_keyboard_input(
    tree: &UiTree,
    layout: &UiLayout,
    state: &mut UiInputState,
    input: KeyboardInput,
) -> Result<UiResponse, UiError> {
    validate_layout(tree.root(), layout)?;
    let focus_order: Vec<_> = layout.focus_order().collect();
    let current_index = state
        .focused
        .and_then(|focused| focus_order.iter().position(|id| *id == focused));
    let target = match input {
        KeyboardInput::Tab => focus_order
            .get(current_index.map_or(0, |index| (index + 1) % focus_order.len()))
            .copied(),
        KeyboardInput::ShiftTab => focus_order
            .get(current_index.map_or_else(
                || focus_order.len().saturating_sub(1),
                |index| {
                    index
                        .checked_sub(1)
                        .unwrap_or_else(|| focus_order.len() - 1)
                },
            ))
            .copied(),
        KeyboardInput::Enter | KeyboardInput::Space => current_index
            .and_then(|index| focus_order.get(index))
            .copied(),
    };
    let mut actions = Vec::new();
    match input {
        KeyboardInput::Tab | KeyboardInput::ShiftTab => {
            state.focused = target;
            if let Some(id) = target {
                actions.push(UiAction::Focused(id));
            }
        }
        KeyboardInput::Enter | KeyboardInput::Space => {
            if let Some(id) = target {
                actions.push(UiAction::Activated(id));
                if let Some(checked) = toggled_state_for_id(tree.root(), id) {
                    actions.push(UiAction::Toggled(id, checked));
                }
            }
        }
    }
    Ok(UiResponse { target, actions })
}

/// UI validation/layout/input failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UiError {
    /// Duplicate stable identifier.
    DuplicateWidgetId(WidgetId),
    /// Non-container has children.
    NonContainerHasChildren(WidgetId),
    /// Widget limit exceeded.
    TooManyWidgets {
        /// Configured limit.
        limit: usize,
    },
    /// Nesting depth exceeded.
    TooDeep {
        /// Configured limit.
        limit: usize,
    },
    /// Absolute child has no position.
    MissingAbsolutePosition(WidgetId),
    /// Signed coordinate overflow.
    CoordinateOverflow,
    /// Layout lacks a tree widget.
    UnknownLayoutWidget(WidgetId),
    /// A scroll viewport must contain exactly one column content child.
    InvalidScrollView(WidgetId),
}

impl std::fmt::Display for UiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DuplicateWidgetId(id) => write!(f, "duplicate UI widget ID {}", id.get()),
            Self::NonContainerHasChildren(id) => {
                write!(f, "non-container UI widget {} has children", id.get())
            }
            Self::TooManyWidgets { limit } => write!(f, "UI widget limit {limit} exceeded"),
            Self::TooDeep { limit } => write!(f, "UI depth limit {limit} exceeded"),
            Self::MissingAbsolutePosition(id) => {
                write!(f, "absolute UI child {} has no position", id.get())
            }
            Self::CoordinateOverflow => f.write_str("UI coordinate overflow"),
            Self::UnknownLayoutWidget(id) => write!(f, "layout misses UI widget {}", id.get()),
            Self::InvalidScrollView(id) => write!(
                f,
                "scroll view {} must contain exactly one column content child",
                id.get()
            ),
        }
    }
}

impl std::error::Error for UiError {}

fn validate_widget(
    widget: &Widget,
    depth: usize,
    limits: UiLimits,
    ids: &mut HashSet<WidgetId>,
    count: &mut usize,
) -> Result<(), UiError> {
    *count = count.checked_add(1).ok_or(UiError::TooManyWidgets {
        limit: limits.max_widgets,
    })?;
    if *count > limits.max_widgets {
        return Err(UiError::TooManyWidgets {
            limit: limits.max_widgets,
        });
    }
    if depth > limits.max_depth {
        return Err(UiError::TooDeep {
            limit: limits.max_depth,
        });
    }
    if !ids.insert(widget.id) {
        return Err(UiError::DuplicateWidgetId(widget.id));
    }
    if !matches!(
        widget.kind,
        WidgetKind::Container(_) | WidgetKind::ScrollView
    ) && !widget.children.is_empty()
    {
        return Err(UiError::NonContainerHasChildren(widget.id));
    }
    if matches!(widget.kind, WidgetKind::ScrollView)
        && (widget.children.len() != 1
            || !matches!(
                widget.children[0].kind,
                WidgetKind::Container(LayoutKind::Column)
            ))
    {
        return Err(UiError::InvalidScrollView(widget.id));
    }
    for child in &widget.children {
        validate_widget(child, depth + 1, limits, ids, count)?;
    }
    Ok(())
}

#[allow(clippy::too_many_lines)] // All layout cases stay adjacent for deterministic review.
fn layout_widget(widget: &Widget, rect: Rect, output: &mut UiLayout) -> Result<(), UiError> {
    output.bounds.insert(widget.id, rect);
    output.paint_order.push(widget.id);
    if widget.enabled && widget.kind.interactive() {
        output.interactive.insert(widget.id);
    }
    let kind = match widget.kind {
        WidgetKind::Container(kind) => kind,
        WidgetKind::ScrollView => LayoutKind::Column,
        _ => return Ok(()),
    };
    let inner = rect.inset(widget.style.padding);
    if kind == LayoutKind::Absolute {
        for child in &widget.children {
            let position = child
                .constraints
                .absolute_position
                .ok_or(UiError::MissingAbsolutePosition(child.id))?;
            let origin = Point::new(
                inner
                    .origin
                    .x
                    .checked_add(position.x)
                    .ok_or(UiError::CoordinateOverflow)?,
                inner
                    .origin
                    .y
                    .checked_add(position.y)
                    .ok_or(UiError::CoordinateOverflow)?,
            );
            let size = Size::new(
                dimension(
                    child.constraints.width,
                    child.style.min_size.width,
                    inner.size.width,
                ),
                dimension(
                    child.constraints.height,
                    child.style.min_size.height,
                    inner.size.height,
                ),
            );
            layout_widget(child, Rect { origin, size }, output)?;
        }
        return Ok(());
    }
    let horizontal = kind == LayoutKind::Row;
    let main_parent = if horizontal {
        inner.size.width
    } else {
        inner.size.height
    };
    let cross_parent = if horizontal {
        inner.size.height
    } else {
        inner.size.width
    };
    let gap_count = widget.children.len().saturating_sub(1);
    let gaps = widget
        .style
        .gap
        .saturating_mul(u32::try_from(gap_count).unwrap_or(u32::MAX));
    let available = main_parent.saturating_sub(gaps);
    let fills = widget
        .children
        .iter()
        .filter(|child| main_dimension(child, horizontal) == Dimension::Fill)
        .count();
    let fixed = widget
        .children
        .iter()
        .filter(|child| main_dimension(child, horizontal) != Dimension::Fill)
        .map(|child| main_size(child, horizontal, available))
        .fold(0_u32, u32::saturating_add);
    let fill = if fills == 0 {
        0
    } else {
        available.saturating_sub(fixed)
            / u32::try_from(fills).map_err(|_| UiError::CoordinateOverflow)?
    };
    let mut cursor = if horizontal {
        inner.origin.x
    } else {
        inner.origin.y
    };
    for child in &widget.children {
        let main = if main_dimension(child, horizontal) == Dimension::Fill {
            fill
        } else {
            main_size(child, horizontal, available)
        };
        let cross = if horizontal {
            dimension(
                child.constraints.height,
                child.style.min_size.height,
                cross_parent,
            )
        } else {
            dimension(
                child.constraints.width,
                child.style.min_size.width,
                cross_parent,
            )
        };
        let child_rect = if horizontal {
            Rect {
                origin: Point::new(cursor, inner.origin.y),
                size: Size::new(main, cross),
            }
        } else {
            Rect {
                origin: Point::new(inner.origin.x, cursor),
                size: Size::new(cross, main),
            }
        };
        layout_widget(child, child_rect, output)?;
        cursor = cursor
            .checked_add(to_i32(main))
            .and_then(|value| value.checked_add(to_i32(widget.style.gap)))
            .ok_or(UiError::CoordinateOverflow)?;
    }
    Ok(())
}

fn main_dimension(widget: &Widget, horizontal: bool) -> Dimension {
    if horizontal {
        widget.constraints.width
    } else {
        widget.constraints.height
    }
}

fn main_size(widget: &Widget, horizontal: bool, parent: u32) -> u32 {
    if horizontal {
        dimension(
            widget.constraints.width,
            widget.style.min_size.width,
            parent,
        )
    } else {
        dimension(
            widget.constraints.height,
            widget.style.min_size.height,
            parent,
        )
    }
}

fn dimension(dimension: Dimension, intrinsic: u32, parent: u32) -> u32 {
    match dimension {
        Dimension::Auto => intrinsic,
        Dimension::Points(value) => value,
        Dimension::Fill => parent,
    }
}

#[allow(clippy::too_many_lines)] // Measured layout mirrors the ordinary deterministic path.
fn layout_widget_with_measurer<M: UiMeasurer>(
    widget: &Widget,
    rect: Rect,
    output: &mut UiLayout,
    measurer: &mut M,
) -> Result<(), LayoutWithMeasureError<M::Error>> {
    output.bounds.insert(widget.id, rect);
    output.paint_order.push(widget.id);
    if widget.enabled && widget.kind.interactive() {
        output.interactive.insert(widget.id);
    }
    let kind = match widget.kind {
        WidgetKind::Container(kind) => kind,
        WidgetKind::ScrollView => LayoutKind::Column,
        _ => return Ok(()),
    };
    let inner = rect.inset(widget.style.padding);
    if kind == LayoutKind::Absolute {
        for child in &widget.children {
            let position =
                child
                    .constraints
                    .absolute_position
                    .ok_or(LayoutWithMeasureError::Layout(
                        UiError::MissingAbsolutePosition(child.id),
                    ))?;
            let origin = Point::new(
                inner
                    .origin
                    .x
                    .checked_add(position.x)
                    .ok_or(LayoutWithMeasureError::Layout(UiError::CoordinateOverflow))?,
                inner
                    .origin
                    .y
                    .checked_add(position.y)
                    .ok_or(LayoutWithMeasureError::Layout(UiError::CoordinateOverflow))?,
            );
            let intrinsic = measured_intrinsic(child, inner.size, measurer)?;
            let size = Size::new(
                dimension(child.constraints.width, intrinsic.width, inner.size.width),
                dimension(
                    child.constraints.height,
                    intrinsic.height,
                    inner.size.height,
                ),
            );
            layout_widget_with_measurer(child, Rect { origin, size }, output, measurer)?;
        }
        return Ok(());
    }
    let horizontal = kind == LayoutKind::Row;
    let main_parent = if horizontal {
        inner.size.width
    } else {
        inner.size.height
    };
    let cross_parent = if horizontal {
        inner.size.height
    } else {
        inner.size.width
    };
    let gap_count = widget.children.len().saturating_sub(1);
    let gaps = widget
        .style
        .gap
        .saturating_mul(u32::try_from(gap_count).unwrap_or(u32::MAX));
    let available = main_parent.saturating_sub(gaps);
    let fills = widget
        .children
        .iter()
        .filter(|child| main_dimension(child, horizontal) == Dimension::Fill)
        .count();
    let fixed = widget
        .children
        .iter()
        .filter(|child| main_dimension(child, horizontal) != Dimension::Fill)
        .try_fold(0_u32, |total, child| {
            let intrinsic = measured_intrinsic(child, inner.size, measurer)?;
            let main = if horizontal {
                dimension(child.constraints.width, intrinsic.width, available)
            } else {
                dimension(child.constraints.height, intrinsic.height, available)
            };
            Ok(total.saturating_add(main))
        })?;
    let fill = if fills == 0 {
        0
    } else {
        available.saturating_sub(fixed)
            / u32::try_from(fills)
                .map_err(|_| LayoutWithMeasureError::Layout(UiError::CoordinateOverflow))?
    };
    let mut cursor = if horizontal {
        inner.origin.x
    } else {
        inner.origin.y
    };
    for child in &widget.children {
        let intrinsic = measured_intrinsic(child, inner.size, measurer)?;
        let main = if main_dimension(child, horizontal) == Dimension::Fill {
            fill
        } else if horizontal {
            dimension(child.constraints.width, intrinsic.width, available)
        } else {
            dimension(child.constraints.height, intrinsic.height, available)
        };
        let cross = if horizontal {
            dimension(child.constraints.height, intrinsic.height, cross_parent)
        } else {
            dimension(child.constraints.width, intrinsic.width, cross_parent)
        };
        let child_rect = if horizontal {
            Rect {
                origin: Point::new(cursor, inner.origin.y),
                size: Size::new(main, cross),
            }
        } else {
            Rect {
                origin: Point::new(inner.origin.x, cursor),
                size: Size::new(cross, main),
            }
        };
        layout_widget_with_measurer(child, child_rect, output, measurer)?;
        cursor = cursor
            .checked_add(to_i32(main))
            .and_then(|value| value.checked_add(to_i32(widget.style.gap)))
            .ok_or(LayoutWithMeasureError::Layout(UiError::CoordinateOverflow))?;
    }
    Ok(())
}

fn measured_intrinsic<M: UiMeasurer>(
    widget: &Widget,
    available: Size,
    measurer: &mut M,
) -> Result<Size, LayoutWithMeasureError<M::Error>> {
    let mut intrinsic = widget.style.min_size;
    if widget.text().is_none()
        || (widget.constraints.width != Dimension::Auto
            && widget.constraints.height != Dimension::Auto)
    {
        return Ok(intrinsic);
    }
    let content =
        measurer
            .measure(widget, available)
            .map_err(|source| LayoutWithMeasureError::Measure {
                widget: widget.id,
                source,
            })?;
    let horizontal_padding = widget
        .style
        .padding
        .left
        .saturating_add(widget.style.padding.right);
    let vertical_padding = widget
        .style
        .padding
        .top
        .saturating_add(widget.style.padding.bottom);
    intrinsic.width = intrinsic
        .width
        .max(content.width.saturating_add(horizontal_padding));
    intrinsic.height = intrinsic
        .height
        .max(content.height.saturating_add(vertical_padding));
    Ok(intrinsic)
}

fn validate_layout(widget: &Widget, layout: &UiLayout) -> Result<(), UiError> {
    if !layout.bounds.contains_key(&widget.id) {
        return Err(UiError::UnknownLayoutWidget(widget.id));
    }
    for child in &widget.children {
        validate_layout(child, layout)?;
    }
    Ok(())
}

fn apply_scroll_views(
    widget: &Widget,
    layout: &mut UiLayout,
    state: &UiInputState,
    inherited_clip: Option<Rect>,
) -> Result<(), UiError> {
    let bounds = layout
        .bounds(widget.id)
        .ok_or(UiError::UnknownLayoutWidget(widget.id))?;
    let clip = if matches!(widget.kind, WidgetKind::ScrollView) {
        Some(
            intersect_rect(inherited_clip.unwrap_or(bounds), bounds).unwrap_or(Rect {
                origin: bounds.origin,
                size: Size::default(),
            }),
        )
    } else {
        inherited_clip
    };
    if let Some(clip) = clip {
        layout.clips.insert(widget.id, clip);
    }
    for child in &widget.children {
        if matches!(widget.kind, WidgetKind::ScrollView) {
            translate_bounds(child, layout, state.scroll_offset(widget.id))?;
        }
        apply_scroll_views(child, layout, state, clip)?;
    }
    Ok(())
}

fn translate_bounds(widget: &Widget, layout: &mut UiLayout, offset: u32) -> Result<(), UiError> {
    let bounds = layout
        .bounds
        .get_mut(&widget.id)
        .ok_or(UiError::UnknownLayoutWidget(widget.id))?;
    bounds.origin.y = bounds.origin.y.saturating_sub(to_i32(offset));
    for child in &widget.children {
        translate_bounds(child, layout, offset)?;
    }
    Ok(())
}

fn intersect_rect(first: Rect, second: Rect) -> Option<Rect> {
    let left = i64::from(first.origin.x).max(i64::from(second.origin.x));
    let top = i64::from(first.origin.y).max(i64::from(second.origin.y));
    let right = (i64::from(first.origin.x) + i64::from(first.size.width))
        .min(i64::from(second.origin.x) + i64::from(second.size.width));
    let bottom = (i64::from(first.origin.y) + i64::from(first.size.height))
        .min(i64::from(second.origin.y) + i64::from(second.size.height));
    (right > left && bottom > top).then(|| Rect {
        origin: Point::new(
            i32::try_from(left).unwrap_or(i32::MIN),
            i32::try_from(top).unwrap_or(i32::MIN),
        ),
        size: Size::new(
            u32::try_from(right - left).unwrap_or(u32::MAX),
            u32::try_from(bottom - top).unwrap_or(u32::MAX),
        ),
    })
}

fn scroll_view_by_id(widget: &Widget, id: WidgetId) -> Option<&Widget> {
    if widget.id == id && matches!(widget.kind, WidgetKind::ScrollView) {
        return Some(widget);
    }
    widget
        .children
        .iter()
        .find_map(|child| scroll_view_by_id(child, id))
}

fn to_i32(value: u32) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(value: &str) -> WidgetId {
        WidgetId::from_key(value)
    }

    fn keyboard_tree() -> UiTree {
        UiBuilder::new(id("root"), LayoutKind::Column)
            .child(Widget::button(id("first"), "First"))
            .child(Widget::button(id("disabled"), "Disabled").with_enabled(false))
            .child(Widget::button(id("last"), "Last"))
            .build()
            .expect("keyboard test tree")
    }

    fn controls_tree() -> UiTree {
        UiBuilder::new(id("root"), LayoutKind::Column)
            .child(Widget::checkbox(id("check"), "Checkbox", false).with_constraints(fill_width()))
            .child(Widget::toggle(id("toggle"), "Toggle", true).with_constraints(fill_width()))
            .build()
            .expect("controls test tree")
    }

    const fn fill_width() -> LayoutConstraints {
        LayoutConstraints::auto().with_width(Dimension::Fill)
    }

    struct FixedMeasurer {
        size: Size,
    }

    impl UiMeasurer for FixedMeasurer {
        type Error = ();

        fn measure(&mut self, _widget: &Widget, _available: Size) -> Result<Size, Self::Error> {
            Ok(self.size)
        }
    }

    struct FailingMeasurer;

    impl UiMeasurer for FailingMeasurer {
        type Error = &'static str;

        fn measure(&mut self, _widget: &Widget, _available: Size) -> Result<Size, Self::Error> {
            Err("font data is unavailable")
        }
    }

    #[test]
    fn row_layout_is_deterministic() {
        let tree = UiBuilder::new(id("root"), LayoutKind::Row)
            .child(
                Widget::button(id("fixed"), "Fixed")
                    .with_constraints(LayoutConstraints::auto().with_width(Dimension::Points(20))),
            )
            .child(
                Widget::button(id("fill"), "Fill")
                    .with_constraints(LayoutConstraints::auto().with_width(Dimension::Fill)),
            )
            .build()
            .expect("tree");
        let first = layout(&tree, Size::new(100, 40)).expect("layout");
        let second = layout(&tree, Size::new(100, 40)).expect("layout");
        assert_eq!(first, second);
        assert_eq!(first.bounds(id("fixed")).expect("fixed").size.width, 20);
        assert_eq!(first.bounds(id("fill")).expect("fill").origin.x, 20);
        assert_eq!(first.bounds(id("fill")).expect("fill").size.width, 80);
    }

    #[test]
    fn measured_auto_label_includes_content_padding_and_minimum() {
        let tree = UiBuilder::new(id("root"), LayoutKind::Column)
            .child(
                Widget::label(id("caption"), "Ready")
                    .with_style(WidgetStyle::default().with_padding(Insets::all(3))),
            )
            .build()
            .expect("tree");
        let mut measurer = FixedMeasurer {
            size: Size::new(30, 11),
        };

        let computed = layout_with_measurer(&tree, Size::new(100, 100), &mut measurer)
            .expect("measured layout");

        // `with_style(WidgetStyle::default()…)` replaces the label's default
        // min_size (0×20); measured height is content + padding only.
        assert_eq!(
            computed.bounds(id("caption")).expect("caption").size,
            Size::new(36, 17)
        );
    }

    #[test]
    fn explicit_dimensions_do_not_require_a_measurer() {
        let tree = UiBuilder::new(id("root"), LayoutKind::Column)
            .child(
                Widget::label(id("caption"), "Fixed").with_constraints(
                    LayoutConstraints::auto()
                        .with_width(Dimension::Points(40))
                        .with_height(Dimension::Points(20)),
                ),
            )
            .build()
            .expect("tree");

        let computed = layout_with_measurer(&tree, Size::new(100, 100), &mut FailingMeasurer)
            .expect("fixed dimensions do not measure content");

        assert_eq!(
            computed.bounds(id("caption")).expect("caption").size,
            Size::new(40, 20)
        );
    }

    #[test]
    fn measurement_failure_preserves_widget_identifier() {
        let caption = id("caption");
        let tree = UiBuilder::new(id("root"), LayoutKind::Column)
            .child(Widget::label(caption, "Measured"))
            .build()
            .expect("tree");

        assert!(matches!(
            layout_with_measurer(&tree, Size::new(100, 100), &mut FailingMeasurer),
            Err(LayoutWithMeasureError::Measure {
                widget,
                source: "font data is unavailable",
            }) if widget == caption
        ));
    }

    #[test]
    fn duplicate_id_is_rejected() {
        assert!(matches!(
            UiBuilder::new(id("root"), LayoutKind::Column)
                .child(Widget::label(id("same"), "one"))
                .child(Widget::button(id("same"), "two"))
                .build(),
            Err(UiError::DuplicateWidgetId(_))
        ));
    }

    #[test]
    fn topmost_button_receives_click() {
        let tree = UiBuilder::new(id("root"), LayoutKind::Absolute)
            .child(
                Widget::button(id("back"), "Back").with_constraints(
                    LayoutConstraints::auto()
                        .with_width(Dimension::Points(40))
                        .with_height(Dimension::Points(40))
                        .with_absolute_position(Point::new(0, 0)),
                ),
            )
            .child(
                Widget::button(id("front"), "Front").with_constraints(
                    LayoutConstraints::auto()
                        .with_width(Dimension::Points(40))
                        .with_height(Dimension::Points(40))
                        .with_absolute_position(Point::new(10, 10)),
                ),
            )
            .build()
            .expect("tree");
        let computed = layout(&tree, Size::new(100, 100)).expect("layout");
        let mut state = UiInputState::default();
        let down = handle_input(
            &tree,
            &computed,
            &mut state,
            PointerInput::PrimaryDown(Point::new(15, 15)),
        )
        .expect("down");
        assert_eq!(down.target(), Some(id("front")));
        assert_eq!(
            down.actions(),
            &[
                UiAction::Hovered(id("front")),
                UiAction::Pressed(id("front"))
            ]
        );
        let up = handle_input(
            &tree,
            &computed,
            &mut state,
            PointerInput::PrimaryUp(Point::new(15, 15)),
        )
        .expect("up");
        assert_eq!(up.actions(), &[UiAction::Clicked(id("front"))]);
        assert_eq!(state.focused(), None);
    }

    #[test]
    fn keyboard_tab_uses_tree_order_skips_disabled_buttons_and_wraps() {
        let tree = keyboard_tree();
        let computed = layout(&tree, Size::new(160, 120)).expect("layout");
        assert_eq!(
            computed.focus_order().collect::<Vec<_>>(),
            vec![id("first"), id("last")]
        );
        let mut pointer_state = UiInputState::default();
        let disabled_pointer = handle_input(
            &tree,
            &computed,
            &mut pointer_state,
            PointerInput::PrimaryDown(Point::new(1, 33)),
        )
        .expect("disabled pointer target");
        assert_eq!(disabled_pointer.target(), None);
        assert!(disabled_pointer.actions().is_empty());
        let mut state = UiInputState::default();

        let first = handle_keyboard_input(&tree, &computed, &mut state, KeyboardInput::Tab)
            .expect("first tab");
        assert_eq!(first.target(), Some(id("first")));
        assert_eq!(first.actions(), &[UiAction::Focused(id("first"))]);

        let last = handle_keyboard_input(&tree, &computed, &mut state, KeyboardInput::Tab)
            .expect("second tab");
        assert_eq!(last.target(), Some(id("last")));
        assert_eq!(last.actions(), &[UiAction::Focused(id("last"))]);

        let wrapped = handle_keyboard_input(&tree, &computed, &mut state, KeyboardInput::Tab)
            .expect("wrapping tab");
        assert_eq!(wrapped.target(), Some(id("first")));
        assert_eq!(state.focused(), Some(id("first")));
    }

    #[test]
    fn keyboard_shift_tab_wraps_backwards_from_first_focus() {
        let tree = keyboard_tree();
        let computed = layout(&tree, Size::new(160, 120)).expect("layout");
        let mut state = UiInputState::default();

        let response = handle_keyboard_input(&tree, &computed, &mut state, KeyboardInput::ShiftTab)
            .expect("reverse tab");
        assert_eq!(response.target(), Some(id("last")));
        assert_eq!(response.actions(), &[UiAction::Focused(id("last"))]);

        let previous = handle_keyboard_input(&tree, &computed, &mut state, KeyboardInput::ShiftTab)
            .expect("previous reverse tab");
        assert_eq!(previous.target(), Some(id("first")));
    }

    #[test]
    fn keyboard_activation_requires_current_enabled_focus() {
        let tree = keyboard_tree();
        let computed = layout(&tree, Size::new(160, 120)).expect("layout");
        let mut state = UiInputState::default();

        let no_focus = handle_keyboard_input(&tree, &computed, &mut state, KeyboardInput::Enter)
            .expect("unfocused enter");
        assert_eq!(no_focus.target(), None);
        assert!(no_focus.actions().is_empty());

        let _ = handle_keyboard_input(&tree, &computed, &mut state, KeyboardInput::Tab)
            .expect("focus first button");
        let activated = handle_keyboard_input(&tree, &computed, &mut state, KeyboardInput::Space)
            .expect("space activation");
        assert_eq!(activated.target(), Some(id("first")));
        assert_eq!(activated.actions(), &[UiAction::Activated(id("first"))]);
    }

    #[test]
    fn keyboard_rejects_layout_that_does_not_represent_tree() {
        let tree = keyboard_tree();
        let mut state = UiInputState::default();
        assert_eq!(
            handle_keyboard_input(&tree, &UiLayout::default(), &mut state, KeyboardInput::Tab),
            Err(UiError::UnknownLayoutWidget(id("root")))
        );
    }

    fn scroll_tree() -> UiTree {
        UiBuilder::new(id("root"), LayoutKind::Column)
            .child(
                Widget::scroll_view(id("scroll"))
                    .with_constraints(
                        LayoutConstraints::auto()
                            .with_width(Dimension::Fill)
                            .with_height(Dimension::Points(40)),
                    )
                    .with_children(vec![
                        Widget::container(id("content"), LayoutKind::Column)
                            .with_constraints(
                                LayoutConstraints::auto().with_height(Dimension::Points(120)),
                            )
                            .with_children(vec![
                                Widget::button(id("first"), "First"),
                                Widget::button(id("last"), "Last").with_constraints(
                                    LayoutConstraints::auto()
                                        .with_height(Dimension::Points(32))
                                        .with_absolute_position(Point::new(0, 80)),
                                ),
                            ]),
                    ]),
            )
            .build()
            .expect("scroll tree")
    }

    #[test]
    fn scroll_clamps_to_content_overflow() {
        let tree = scroll_tree();
        let initial = layout_with_input_state(&tree, Size::new(100, 100), &UiInputState::default())
            .expect("initial layout");
        let mut state = UiInputState::default();
        let response = handle_scroll_input(&tree, &initial, &mut state, Point::new(2, 2), -500)
            .expect("scroll");
        assert_eq!(response.actions(), &[UiAction::Scrolled(id("scroll"))]);
        assert_eq!(state.scroll_offset(id("scroll")), 80);
    }

    #[test]
    fn wheel_outside_scroll_view_is_ignored() {
        let tree = scroll_tree();
        let layout = layout_with_input_state(&tree, Size::new(100, 100), &UiInputState::default())
            .expect("layout");
        let mut state = UiInputState::default();
        assert!(
            handle_scroll_input(&tree, &layout, &mut state, Point::new(2, 60), -20)
                .expect("outside scroll")
                .actions()
                .is_empty()
        );
        assert_eq!(state.scroll_offset(id("scroll")), 0);
    }

    #[test]
    fn pointer_hit_testing_is_clipped_to_scroll_viewport() {
        let tree = scroll_tree();
        let layout = layout_with_input_state(&tree, Size::new(100, 100), &UiInputState::default())
            .expect("layout");
        let mut state = UiInputState::default();
        let response = handle_input(
            &tree,
            &layout,
            &mut state,
            PointerInput::PrimaryDown(Point::new(2, 45)),
        )
        .expect("clipped input");
        assert_eq!(response.target(), None);
    }

    #[test]
    fn scrollbar_thumb_drag_updates_offset_and_captures_pointer() {
        let tree = scroll_tree();
        let layout = layout_with_input_state(&tree, Size::new(100, 100), &UiInputState::default())
            .expect("layout");
        let mut state = UiInputState::default();
        let viewport = layout.bounds(id("scroll")).expect("viewport");
        let thumb =
            vertical_scroll_thumb_bounds(viewport, 120, 0, SCROLL_THUMB_THICKNESS).expect("thumb");
        let grab = Point::new(thumb.origin.x + 1, thumb.origin.y + 2);

        let down = handle_input(&tree, &layout, &mut state, PointerInput::PrimaryDown(grab))
            .expect("thumb down");
        assert_eq!(down.target(), Some(id("scroll")));
        assert_eq!(state.scroll_dragging(), Some(id("scroll")));
        assert!(down.actions().is_empty());

        let travel = viewport.size.height.saturating_sub(thumb.size.height);
        let bottom = Point::new(
            grab.x,
            viewport
                .origin
                .y
                .saturating_add(to_i32(travel))
                .saturating_add(2),
        );
        let dragged = handle_input(&tree, &layout, &mut state, PointerInput::Move(bottom))
            .expect("thumb drag");
        assert_eq!(dragged.actions(), &[UiAction::Scrolled(id("scroll"))]);
        assert_eq!(state.scroll_offset(id("scroll")), 80);

        let up = handle_input(&tree, &layout, &mut state, PointerInput::PrimaryUp(bottom))
            .expect("thumb up");
        assert_eq!(state.scroll_dragging(), None);
        assert_eq!(up.target(), Some(id("scroll")));
    }

    #[test]
    fn scrollbar_track_click_jumps_and_starts_drag() {
        let tree = scroll_tree();
        let layout = layout_with_input_state(&tree, Size::new(100, 100), &UiInputState::default())
            .expect("layout");
        let mut state = UiInputState::default();
        let viewport = layout.bounds(id("scroll")).expect("viewport");
        let track = Point::new(
            viewport
                .origin
                .x
                .saturating_add(to_i32(viewport.size.width.saturating_sub(2))),
            viewport
                .origin
                .y
                .saturating_add(to_i32(viewport.size.height.saturating_sub(4))),
        );

        let response = handle_input(&tree, &layout, &mut state, PointerInput::PrimaryDown(track))
            .expect("track click");
        assert_eq!(response.actions(), &[UiAction::Scrolled(id("scroll"))]);
        assert!(state.scroll_offset(id("scroll")) > 0);
        assert_eq!(state.scroll_dragging(), Some(id("scroll")));
    }

    #[test]
    fn scroll_view_requires_one_column_content_child() {
        assert!(matches!(
            UiBuilder::new(id("root"), LayoutKind::Column)
                .child(Widget::scroll_view(id("scroll")))
                .build(),
            Err(UiError::InvalidScrollView(_))
        ));
    }

    #[test]
    fn vertical_scroll_thumb_absent_without_overflow() {
        let viewport = Rect {
            origin: Point::new(10, 20),
            size: Size::new(100, 40),
        };
        assert_eq!(
            vertical_scroll_thumb_bounds(viewport, 40, 0, SCROLL_THUMB_THICKNESS),
            None
        );
        assert_eq!(
            vertical_scroll_thumb_bounds(viewport, 20, 0, SCROLL_THUMB_THICKNESS),
            None
        );
    }

    #[test]
    fn image_widget_uses_default_min_size_and_exposes_id() {
        let image = UiImageId::new(7);
        let tree = UiBuilder::new(id("root"), LayoutKind::Row)
            .child(Widget::image(id("icon"), image))
            .build()
            .expect("tree");
        let computed = layout(&tree, Size::new(200, 40)).expect("layout");
        let bounds = computed.bounds(id("icon")).expect("icon");
        assert_eq!(bounds.size, Size::new(24, 24));
        assert_eq!(tree.root().children()[0].image_id(), Some(image));
        assert_eq!(tree.root().children()[0].text(), None);
    }

    #[test]
    fn image_widget_rejects_children() {
        let result = UiBuilder::new(id("root"), LayoutKind::Column)
            .child(
                Widget::image(id("icon"), UiImageId::new(1))
                    .with_children(vec![Widget::label(id("nested"), "no")]),
            )
            .build();
        assert!(matches!(result, Err(UiError::NonContainerHasChildren(_))));
    }

    #[test]
    fn checkbox_toggle_have_text_and_checked_state_helpers() {
        let check = Widget::checkbox(id("check"), "Enable sound", false);
        let toggle = Widget::toggle(id("toggle"), "V-Sync", true);
        assert_eq!(check.text(), Some("Enable sound"));
        assert_eq!(check.is_checked(), Some(false));
        assert_eq!(toggle.text(), Some("V-Sync"));
        assert_eq!(toggle.is_checked(), Some(true));
    }

    #[test]
    fn controls_and_layout_elements_reject_children() {
        let result = UiBuilder::new(id("root"), LayoutKind::Column)
            .child(
                Widget::separator(id("sep")).with_children(vec![Widget::label(id("nested"), "no")]),
            )
            .build();
        assert!(matches!(result, Err(UiError::NonContainerHasChildren(_))));

        let spacer_result = UiBuilder::new(id("root2"), LayoutKind::Column)
            .child(Widget::spacer(id("gap")).with_children(vec![Widget::label(id("nested"), "no")]))
            .build();
        assert!(matches!(
            spacer_result,
            Err(UiError::NonContainerHasChildren(_))
        ));
    }

    #[test]
    fn checkbox_click_emits_toggled_action() {
        let tree = controls_tree();
        let layout = layout(&tree, Size::new(120, 80)).expect("controls layout");
        let mut state = UiInputState::default();

        let down = handle_input(
            &tree,
            &layout,
            &mut state,
            PointerInput::PrimaryDown(Point::new(4, 13)),
        )
        .expect("check down");
        assert_eq!(
            down.actions(),
            &[
                UiAction::Hovered(id("check")),
                UiAction::Pressed(id("check")),
            ]
        );

        let up = handle_input(
            &tree,
            &layout,
            &mut state,
            PointerInput::PrimaryUp(Point::new(4, 10)),
        )
        .expect("check up");
        assert_eq!(
            up.actions(),
            &[
                UiAction::Clicked(id("check")),
                UiAction::Toggled(id("check"), true)
            ]
        );
    }

    #[test]
    fn keyboard_activates_toggled_controls_with_updated_value_action() {
        let tree = controls_tree();
        let layout = layout(&tree, Size::new(160, 80)).expect("controls layout");
        let mut state = UiInputState::default();

        let focused = handle_keyboard_input(&tree, &layout, &mut state, KeyboardInput::Tab)
            .expect("focus check");
        assert_eq!(focused.actions(), &[UiAction::Focused(id("check"))]);

        let toggled = handle_keyboard_input(&tree, &layout, &mut state, KeyboardInput::Space)
            .expect("toggle via keyboard");
        assert_eq!(
            toggled.actions(),
            &[
                UiAction::Activated(id("check")),
                UiAction::Toggled(id("check"), true)
            ]
        );
    }

    #[test]
    fn vertical_scroll_thumb_tracks_offset_within_viewport() {
        let viewport = Rect {
            origin: Point::new(0, 0),
            size: Size::new(100, 40),
        };
        let top = vertical_scroll_thumb_bounds(viewport, 120, 0, SCROLL_THUMB_THICKNESS)
            .expect("overflow");
        let bottom = vertical_scroll_thumb_bounds(viewport, 120, 80, SCROLL_THUMB_THICKNESS)
            .expect("overflow");
        let mid = vertical_scroll_thumb_bounds(viewport, 120, 40, SCROLL_THUMB_THICKNESS)
            .expect("overflow");
        let clamped = vertical_scroll_thumb_bounds(viewport, 120, 500, SCROLL_THUMB_THICKNESS)
            .expect("overflow");

        assert_eq!(top.origin.x, 92);
        assert_eq!(top.origin.y, 0);
        assert_eq!(top.size.width, SCROLL_THUMB_THICKNESS);
        assert!(top.size.height >= SCROLL_THUMB_MIN_HEIGHT);
        assert!(top.size.height <= 40);

        assert_eq!(
            bottom.origin.y,
            to_i32(40_u32.saturating_sub(bottom.size.height))
        );
        assert_eq!(clamped, bottom);
        assert!(mid.origin.y > top.origin.y && mid.origin.y < bottom.origin.y);
        assert_eq!(mid.size, top.size);
    }
}
