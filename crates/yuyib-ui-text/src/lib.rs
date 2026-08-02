//! Bounded renderer-agnostic native UI text shaping for Yuyib.
//!
//! [`TextEngine`] owns an isolated `cosmic-text` font database and loads only
//! application-provided font bytes or one explicit font-file path. It does not
//! scan system font directories, retain global font state, rasterize glyphs, or
//! depend on a window/GPU backend. Layout outputs positioned glyph IDs and
//! metrics for a later atlas renderer.

#![forbid(unsafe_code)]

use std::{
    collections::HashMap,
    error::Error,
    fmt,
    fs::File,
    io::{self, Read},
    ops::Range,
    path::PathBuf,
};

use cosmic_text::{
    Align, Attrs, Buffer, Family, FontSystem, Metrics, Shaping, Wrap,
    fontdb::{Database, ID},
};

/// Explicit source for one application-controlled font file or byte payload.
///
/// [`Self::File`] opens exactly the supplied path; it does not enumerate font
/// directories or ask the operating system to select a font. Prefer
/// [`Self::Bytes`] for packaged applications and reproducible builds.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FontSource {
    /// Complete font bytes owned by the application.
    Bytes(Vec<u8>),
    /// One exact application-selected font file path.
    File(PathBuf),
}

impl FontSource {
    /// Wraps owned font bytes.
    #[must_use]
    pub const fn bytes(bytes: Vec<u8>) -> Self {
        Self::Bytes(bytes)
    }

    /// Selects one exact font file without scanning any system directories.
    #[must_use]
    pub fn file(path: impl Into<PathBuf>) -> Self {
        Self::File(path.into())
    }
}

/// Hard limits for one isolated text engine and each layout request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TextLimits {
    /// Maximum bytes accepted from [`FontSource`].
    pub max_font_bytes: usize,
    /// Maximum UTF-8 bytes accepted by one [`TextEngine::shape`] request.
    pub max_text_bytes: usize,
    /// Maximum visual lines emitted by one layout request.
    pub max_lines: usize,
    /// Maximum glyphs emitted by one layout request.
    pub max_glyphs: usize,
}

impl Default for TextLimits {
    fn default() -> Self {
        Self {
            max_font_bytes: 64 * 1024 * 1024,
            max_text_bytes: 1024 * 1024,
            max_lines: 10_000,
            max_glyphs: 1_000_000,
        }
    }
}

impl TextLimits {
    /// Validates that every mandatory bound is non-zero.
    ///
    /// # Errors
    ///
    /// Returns [`TextError::InvalidLimit`] for a zero budget.
    pub const fn validate(self) -> Result<(), TextError> {
        if self.max_font_bytes == 0 {
            return Err(TextError::InvalidLimit("max_font_bytes"));
        }
        if self.max_text_bytes == 0 {
            return Err(TextError::InvalidLimit("max_text_bytes"));
        }
        if self.max_lines == 0 {
            return Err(TextError::InvalidLimit("max_lines"));
        }
        if self.max_glyphs == 0 {
            return Err(TextError::InvalidLimit("max_glyphs"));
        }
        Ok(())
    }
}

/// Wrapping policy applied before layout output is collected.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TextWrap {
    /// Preserve each paragraph on a single visual line.
    None,
    /// Break at individual glyph clusters.
    Glyph,
    /// Break at words only.
    Word,
    /// Break at words, falling back to glyph clusters for oversized words.
    #[default]
    WordOrGlyph,
}

impl From<TextWrap> for Wrap {
    fn from(value: TextWrap) -> Self {
        match value {
            TextWrap::None => Self::None,
            TextWrap::Glyph => Self::Glyph,
            TextWrap::Word => Self::Word,
            TextWrap::WordOrGlyph => Self::WordOrGlyph,
        }
    }
}

/// Horizontal alignment for a shaped visual line.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TextAlignment {
    /// Align to the leading edge of the paragraph.
    #[default]
    Left,
    /// Align to the trailing edge of the paragraph.
    Right,
    /// Centre within the wrap width.
    Center,
    /// Distribute spacing across non-final wrapped lines.
    Justified,
    /// Use paragraph-direction-aware end alignment.
    End,
}

impl From<TextAlignment> for Align {
    fn from(value: TextAlignment) -> Self {
        match value {
            TextAlignment::Left => Self::Left,
            TextAlignment::Right => Self::Right,
            TextAlignment::Center => Self::Center,
            TextAlignment::Justified => Self::Justified,
            TextAlignment::End => Self::End,
        }
    }
}

/// One complete text shaping request.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextLayoutOptions {
    /// Font em size in logical pixels.
    pub font_size: f32,
    /// Baseline-to-baseline line height in logical pixels.
    pub line_height: f32,
    /// Optional logical wrap width. `None` means no horizontal clipping width.
    pub max_width: Option<f32>,
    /// Line-break policy.
    pub wrap: TextWrap,
    /// Horizontal alignment.
    pub alignment: TextAlignment,
}

impl Default for TextLayoutOptions {
    fn default() -> Self {
        Self {
            font_size: 16.0,
            line_height: 20.0,
            max_width: None,
            wrap: TextWrap::default(),
            alignment: TextAlignment::default(),
        }
    }
}

impl TextLayoutOptions {
    /// Validates finite positive metrics and an optional finite positive width.
    ///
    /// # Errors
    ///
    /// Returns [`TextError::InvalidMetric`] or [`TextError::InvalidWidth`].
    pub fn validate(self) -> Result<(), TextError> {
        if !self.font_size.is_finite() || self.font_size <= 0.0 {
            return Err(TextError::InvalidMetric("font_size"));
        }
        if !self.line_height.is_finite() || self.line_height <= 0.0 {
            return Err(TextError::InvalidMetric("line_height"));
        }
        if self
            .max_width
            .is_some_and(|width| !width.is_finite() || width <= 0.0)
        {
            return Err(TextError::InvalidWidth);
        }
        Ok(())
    }
}

/// Immutable measurement summary for a [`ShapedText`] result.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextMetrics {
    /// Greatest visual line width in logical pixels.
    pub width: f32,
    /// Bottom edge of the final visual line in logical pixels.
    pub height: f32,
    /// Visual line count.
    pub line_count: usize,
    /// Positioned glyph count.
    pub glyph_count: usize,
    /// Requested font em size in logical pixels.
    pub font_size: f32,
    /// Requested baseline-to-baseline height in logical pixels.
    pub line_height: f32,
}

/// Renderer-ready logical glyph placement without pixel rasterization.
#[derive(Clone, Debug, PartialEq)]
pub struct PositionedGlyph {
    /// Index of the font face in [`TextEngine::font_families`].
    pub font_index: u32,
    /// Glyph ID within `font_index`.
    pub glyph_id: u16,
    /// UTF-8 byte range in the original request text for this shaped cluster.
    pub text_range: Range<usize>,
    /// Logical horizontal glyph origin before its font-relative offset.
    pub x: f32,
    /// Logical vertical glyph origin before its font-relative offset.
    pub y: f32,
    /// Logical advance/hit width.
    pub advance: f32,
    /// Font-relative horizontal placement offset in em units.
    pub x_offset: f32,
    /// Font-relative vertical placement offset in em units.
    pub y_offset: f32,
    /// Actual font size selected by shaping for this glyph.
    pub font_size: f32,
}

/// One visual shaped line in deterministic layout order.
#[derive(Clone, Debug, PartialEq)]
pub struct PositionedLine {
    /// Original paragraph index in the input text.
    pub source_line: usize,
    /// Whether the source paragraph is right-to-left.
    pub rtl: bool,
    /// Logical top coordinate of this visual line.
    pub top: f32,
    /// Logical baseline coordinate of this visual line.
    pub baseline: f32,
    /// Logical height before the next visual line.
    pub height: f32,
    /// Logical line width.
    pub width: f32,
    /// Glyphs in the visual order supplied by the shaping engine.
    pub glyphs: Vec<PositionedGlyph>,
}

/// Complete positioned glyph output for one UTF-8 request.
#[derive(Clone, Debug, PartialEq)]
pub struct ShapedText {
    metrics: TextMetrics,
    lines: Vec<PositionedLine>,
}

impl ShapedText {
    /// Returns aggregate measurement information.
    #[must_use]
    pub const fn metrics(&self) -> TextMetrics {
        self.metrics
    }

    /// Returns visual lines in deterministic top-to-bottom layout order.
    #[must_use]
    pub fn lines(&self) -> &[PositionedLine] {
        &self.lines
    }
}

/// Isolated `cosmic-text` shaping state with no global or implicit font source.
pub struct TextEngine {
    font_system: FontSystem,
    primary_family: String,
    font_indices: HashMap<ID, u32>,
    font_families: Vec<String>,
    limits: TextLimits,
}

impl TextEngine {
    /// Creates an isolated text engine from a single explicit font source.
    ///
    /// No installed-font scan is performed. A font collection may provide
    /// several faces, all exposed through [`Self::font_families`]. The first
    /// declared family becomes the primary shaping family.
    ///
    /// # Errors
    ///
    /// Returns source I/O, byte-budget, font parsing, or invalid-limit errors.
    pub fn from_source(source: FontSource, limits: TextLimits) -> Result<Self, TextError> {
        limits.validate()?;
        let bytes = source.read_bounded(limits.max_font_bytes)?;
        let mut database = Database::new();
        database.load_font_data(bytes);
        let mut font_indices = HashMap::new();
        let mut font_families = Vec::new();
        for face in database.faces() {
            let family = face
                .families
                .first()
                .map(|(name, _)| name.clone())
                .ok_or(TextError::FontHasNoFamily)?;
            let index =
                u32::try_from(font_families.len()).map_err(|_| TextError::TooManyFontFaces)?;
            font_indices.insert(face.id, index);
            font_families.push(family);
        }
        let primary_family = font_families
            .first()
            .cloned()
            .ok_or(TextError::InvalidFontData)?;
        let font_system = FontSystem::new_with_locale_and_db(String::from("en-US"), database);
        Ok(Self {
            font_system,
            primary_family,
            font_indices,
            font_families,
            limits,
        })
    }

    /// Returns the configured bounded input policy.
    #[must_use]
    pub const fn limits(&self) -> TextLimits {
        self.limits
    }

    /// Returns deterministic face-family names loaded from the explicit source.
    #[must_use]
    pub fn font_families(&self) -> &[String] {
        &self.font_families
    }

    /// Shapes and measures one UTF-8 string without rasterizing it.
    ///
    /// The result is deterministic for equal engine font bytes, text, options,
    /// and `cosmic-text` version. It intentionally does not add fallback fonts
    /// from the operating system; missing glyph behavior is therefore defined
    /// solely by the explicit source.
    ///
    /// # Errors
    ///
    /// Returns a validation error, text/line/glyph budget error, or an error
    /// when shaping selects a face outside this engine's explicit source.
    pub fn shape(
        &mut self,
        text: &str,
        options: TextLayoutOptions,
    ) -> Result<ShapedText, TextError> {
        options.validate()?;
        if text.len() > self.limits.max_text_bytes {
            return Err(TextError::TextTooLarge {
                actual: text.len(),
                limit: self.limits.max_text_bytes,
            });
        }
        let line_offsets = source_line_offsets(text);
        let metrics = Metrics::new(options.font_size, options.line_height);
        let mut buffer = Buffer::new(&mut self.font_system, metrics);
        buffer.set_size(options.max_width, None);
        buffer.set_wrap(options.wrap.into());
        let attrs = Attrs::new().family(Family::Name(&self.primary_family));
        buffer.set_text(
            text,
            &attrs,
            Shaping::Advanced,
            Some(options.alignment.into()),
        );
        buffer.shape_until_scroll(&mut self.font_system, false);

        let mut lines = Vec::new();
        let mut glyph_count = 0_usize;
        let mut max_width = 0.0_f32;
        let mut height = 0.0_f32;
        for run in buffer.layout_runs() {
            if lines.len() == self.limits.max_lines {
                return Err(TextError::TooManyLines {
                    limit: self.limits.max_lines,
                });
            }
            glyph_count =
                glyph_count
                    .checked_add(run.glyphs.len())
                    .ok_or(TextError::TooManyGlyphs {
                        limit: self.limits.max_glyphs,
                    })?;
            if glyph_count > self.limits.max_glyphs {
                return Err(TextError::TooManyGlyphs {
                    limit: self.limits.max_glyphs,
                });
            }
            let source_offset = *line_offsets
                .get(run.line_i)
                .ok_or(TextError::InvalidShapingLine(run.line_i))?;
            let mut glyphs = Vec::with_capacity(run.glyphs.len());
            for glyph in run.glyphs {
                let font_index = self
                    .font_indices
                    .get(&glyph.font_id)
                    .copied()
                    .ok_or(TextError::UnexpectedShapedFont)?;
                glyphs.push(PositionedGlyph {
                    font_index,
                    glyph_id: glyph.glyph_id,
                    text_range: source_offset.saturating_add(glyph.start)
                        ..source_offset.saturating_add(glyph.end),
                    x: glyph.x,
                    y: run.line_y + glyph.y,
                    advance: glyph.w,
                    x_offset: glyph.x_offset,
                    y_offset: glyph.y_offset,
                    font_size: glyph.font_size,
                });
            }
            max_width = max_width.max(run.line_w);
            height = height.max(run.line_top + run.line_height);
            lines.push(PositionedLine {
                source_line: run.line_i,
                rtl: run.rtl,
                top: run.line_top,
                baseline: run.line_y,
                height: run.line_height,
                width: run.line_w,
                glyphs,
            });
        }
        Ok(ShapedText {
            metrics: TextMetrics {
                width: max_width,
                height,
                line_count: lines.len(),
                glyph_count,
                font_size: options.font_size,
                line_height: options.line_height,
            },
            lines,
        })
    }
}

impl FontSource {
    fn read_bounded(self, max_bytes: usize) -> Result<Vec<u8>, TextError> {
        match self {
            Self::Bytes(bytes) => {
                if bytes.len() > max_bytes {
                    return Err(TextError::FontTooLarge {
                        actual: bytes.len(),
                        limit: max_bytes,
                    });
                }
                Ok(bytes)
            }
            Self::File(path) => {
                let file = File::open(&path).map_err(|source| TextError::FontRead {
                    path: path.clone(),
                    source,
                })?;
                let mut bytes = Vec::new();
                let read_limit = u64::try_from(max_bytes.saturating_add(1)).unwrap_or(u64::MAX);
                file.take(read_limit)
                    .read_to_end(&mut bytes)
                    .map_err(|source| TextError::FontRead { path, source })?;
                if bytes.len() > max_bytes {
                    return Err(TextError::FontTooLarge {
                        actual: bytes.len(),
                        limit: max_bytes,
                    });
                }
                Ok(bytes)
            }
        }
    }
}

/// Text source, shaping, or resource-limit error.
#[derive(Debug)]
pub enum TextError {
    /// One configured limit was zero.
    InvalidLimit(&'static str),
    /// An exact explicit font path could not be read.
    FontRead {
        /// Requested path.
        path: PathBuf,
        /// Operating-system read failure.
        source: io::Error,
    },
    /// Font bytes exceeded the configured input budget.
    FontTooLarge {
        /// Supplied byte length.
        actual: usize,
        /// Configured byte limit.
        limit: usize,
    },
    /// Font parser did not load any usable face.
    InvalidFontData,
    /// A parsed font face did not declare a family name.
    FontHasNoFamily,
    /// More loaded faces existed than can fit the public font index.
    TooManyFontFaces,
    /// Font em size or line height was non-finite or non-positive.
    InvalidMetric(&'static str),
    /// Wrap width was non-finite or non-positive.
    InvalidWidth,
    /// UTF-8 input exceeded the configured byte budget.
    TextTooLarge {
        /// Supplied UTF-8 byte length.
        actual: usize,
        /// Configured byte limit.
        limit: usize,
    },
    /// Shaping would emit more visual lines than the configured budget.
    TooManyLines {
        /// Configured line limit.
        limit: usize,
    },
    /// Shaping would emit more glyphs than the configured budget.
    TooManyGlyphs {
        /// Configured glyph limit.
        limit: usize,
    },
    /// Shaping referenced an unexpected original paragraph index.
    InvalidShapingLine(usize),
    /// Shaping selected a face outside the explicit source database.
    UnexpectedShapedFont,
}

impl fmt::Display for TextError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLimit(name) => write!(formatter, "text limit {name} must be non-zero"),
            Self::FontRead { path, source } => {
                write!(
                    formatter,
                    "failed to read font {}: {source}",
                    path.display()
                )
            }
            Self::FontTooLarge { actual, limit } => {
                write!(formatter, "font byte length {actual} exceeds limit {limit}")
            }
            Self::InvalidFontData => formatter.write_str("font bytes contain no usable face"),
            Self::FontHasNoFamily => formatter.write_str("font face declares no family"),
            Self::TooManyFontFaces => formatter.write_str("font source contains too many faces"),
            Self::InvalidMetric(name) => {
                write!(formatter, "text {name} must be finite and positive")
            }
            Self::InvalidWidth => {
                formatter.write_str("text wrap width must be finite and positive")
            }
            Self::TextTooLarge { actual, limit } => {
                write!(formatter, "text byte length {actual} exceeds limit {limit}")
            }
            Self::TooManyLines { limit } => write!(formatter, "text line limit {limit} exceeded"),
            Self::TooManyGlyphs { limit } => write!(formatter, "text glyph limit {limit} exceeded"),
            Self::InvalidShapingLine(line) => {
                write!(formatter, "shaping returned unknown source line {line}")
            }
            Self::UnexpectedShapedFont => {
                formatter.write_str("shaping selected a font outside the explicit source")
            }
        }
    }
}

impl Error for TextError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::FontRead { source, .. } => Some(source),
            _ => None,
        }
    }
}

fn source_line_offsets(text: &str) -> Vec<usize> {
    let mut offsets = vec![0];
    for (index, byte) in text.bytes().enumerate() {
        if byte == b'\n' {
            offsets.push(index.saturating_add(1));
        }
    }
    offsets
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_zero_limits_and_invalid_layout_metrics() {
        assert!(matches!(
            TextLimits {
                max_font_bytes: 0,
                ..TextLimits::default()
            }
            .validate(),
            Err(TextError::InvalidLimit("max_font_bytes"))
        ));
        assert!(matches!(
            TextLayoutOptions {
                font_size: 0.0,
                ..TextLayoutOptions::default()
            }
            .validate(),
            Err(TextError::InvalidMetric("font_size"))
        ));
        assert!(matches!(
            TextLayoutOptions {
                max_width: Some(f32::NAN),
                ..TextLayoutOptions::default()
            }
            .validate(),
            Err(TextError::InvalidWidth)
        ));
    }

    #[test]
    fn rejects_font_bytes_before_parsing_when_budget_is_exceeded() {
        let limits = TextLimits {
            max_font_bytes: 2,
            ..TextLimits::default()
        };
        assert!(matches!(
            TextEngine::from_source(FontSource::bytes(vec![0, 1, 2]), limits),
            Err(TextError::FontTooLarge {
                actual: 3,
                limit: 2
            })
        ));
    }

    #[test]
    fn rejects_invalid_font_bytes_without_any_system_font_fallback() {
        assert!(matches!(
            TextEngine::from_source(FontSource::bytes(vec![0, 1, 2]), TextLimits::default()),
            Err(TextError::InvalidFontData)
        ));
    }

    #[test]
    fn source_line_offsets_preserve_utf8_byte_boundaries() {
        assert_eq!(source_line_offsets("a\nёж\n"), vec![0, 2, 7]);
    }
}
