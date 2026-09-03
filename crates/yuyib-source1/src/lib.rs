//! Bounded Source 1 VMF brush adaptation.
//!
//! This crate converts parsed VMF solid sides into yuyib-vmf-model brush
//! solids and delegates convex geometry compilation to that crate. It does not
//! support Source 2, BSP, VMT, VTF or entities-to-ECS. The optional textured
//! compiler uses Source `uaxis`/`vaxis` plus caller-provided VTF dimensions.

#![forbid(unsafe_code)]

use std::{collections::BTreeSet, error::Error, fmt};

use yuyib_model::Model;
use yuyib_vmf::{VmfEntity, VmfMap, VmfSide, VmfSolid};
use yuyib_vmf_model::{
    BrushCompileError, BrushCompileLimits, BrushSide, BrushSolid, PlanePoints, TextureAxes,
    TextureAxis, compile_brushes, compile_brushes_with_texture_sizes,
};

/// Selects which world and entity solids are compiled.
///
/// Selected solids retain deterministic order: world solids first, then
/// selected entity indices in ascending source document order.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum MapBrushSelection {
    /// Select only world solids.
    WorldOnly,
    /// Select world solids then every entity solid.
    #[default]
    WorldAndEntities,
    /// Select every entity solid but no world solid.
    EntitiesOnly,
    /// Select world solids then unique entity indices.
    EntityIndices(Vec<usize>),
}

/// Work bounds applied while selecting and adapting VMF brush text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Source1AdapterLimits {
    /// Maximum selected solids.
    pub max_selected_solids: usize,
    /// Maximum side count per selected solid.
    pub max_sides_per_solid: usize,
    /// Maximum raw bytes in one VMF plane value.
    pub max_plane_bytes: usize,
}

impl Default for Source1AdapterLimits {
    fn default() -> Self {
        Self {
            max_selected_solids: 4_096,
            max_sides_per_solid: 128,
            max_plane_bytes: 4_096,
        }
    }
}

/// Original VMF solid location retained by a selected brush.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VmfBrushOrigin {
    /// World solid index.
    World {
        /// Zero-based solid index.
        solid: usize,
    },
    /// Entity and solid index.
    Entity {
        /// Zero-based entity index.
        entity: usize,
        /// Zero-based solid index within the entity.
        solid: usize,
    },
}

/// Normalized brush and its source map position.
#[derive(Clone, Debug, PartialEq)]
pub struct SelectedBrushSolid {
    origin: VmfBrushOrigin,
    solid: BrushSolid,
}

impl SelectedBrushSolid {
    /// Returns original VMF selection location.
    #[must_use]
    pub const fn origin(&self) -> VmfBrushOrigin {
        self.origin
    }

    /// Returns normalized brush geometry.
    #[must_use]
    pub fn solid(&self) -> &BrushSolid {
        &self.solid
    }
}

/// Adapts selected Source 1 VMF solids to yuyib-vmf-model brush data.
///
/// # Errors
///
/// Returns `Source1Error` for selection, limit, required property or plane
/// conversion failure.
pub fn adapt_map(
    map: &VmfMap,
    selection: &MapBrushSelection,
    limits: Source1AdapterLimits,
) -> Result<Vec<SelectedBrushSolid>, Source1Error> {
    let source = selected_solids(map, selection)?;
    if source.len() > limits.max_selected_solids {
        return Err(Source1Error::LimitExceeded {
            limit: Source1Limit::SelectedSolids,
            maximum: limits.max_selected_solids,
        });
    }
    source
        .into_iter()
        .map(|(origin, solid)| adapt_solid(origin, solid, limits))
        .collect()
}

/// Adapts selected Source 1 solids and compiles them into one renderer-neutral model.
///
/// Geometry intersection and output budgets are delegated to yuyib-vmf-model
/// through `compiler_limits`.
///
/// # Errors
///
/// Returns `Source1Error` for adapter failure or downstream brush compilation.
pub fn compile_map(
    map: &VmfMap,
    selection: &MapBrushSelection,
    adapter_limits: Source1AdapterLimits,
    compiler_limits: BrushCompileLimits,
) -> Result<Model, Source1Error> {
    let selected = adapt_map(map, selection, adapter_limits)?;
    let solids: Vec<_> = selected
        .into_iter()
        .map(|selected| selected.solid)
        .collect();
    compile_brushes(&solids, compiler_limits).map_err(Source1Error::Compile)
}

/// Compiles a map with normalized UVs for materials whose VTF size is known.
///
/// # Errors
/// Returns the same adapter and geometry errors as [`compile_map`].
pub fn compile_map_with_texture_sizes(
    map: &VmfMap,
    selection: &MapBrushSelection,
    adapter_limits: Source1AdapterLimits,
    compiler_limits: BrushCompileLimits,
    texture_size: impl Fn(&str) -> Option<[u16; 2]>,
) -> Result<Model, Source1Error> {
    let selected = adapt_map(map, selection, adapter_limits)?;
    let solids: Vec<_> = selected
        .into_iter()
        .map(|selected| selected.solid)
        .collect();
    compile_brushes_with_texture_sizes(&solids, compiler_limits, texture_size)
        .map_err(Source1Error::Compile)
}

fn selected_solids<'a>(
    map: &'a VmfMap,
    selection: &MapBrushSelection,
) -> Result<Vec<(VmfBrushOrigin, &'a VmfSolid)>, Source1Error> {
    let mut selected = Vec::new();
    if !matches!(selection, MapBrushSelection::EntitiesOnly)
        && let Some(world) = map.world()
    {
        append_solids(&mut selected, world, |solid| VmfBrushOrigin::World {
            solid,
        });
    }
    let entities = match selection {
        MapBrushSelection::WorldOnly => Vec::new(),
        MapBrushSelection::WorldAndEntities | MapBrushSelection::EntitiesOnly => {
            (0..map.entities().len()).collect()
        }
        MapBrushSelection::EntityIndices(indices) => {
            normalize_indices(indices, map.entities().len())?
        }
    };
    for entity in entities {
        append_solids(&mut selected, &map.entities()[entity], |solid| {
            VmfBrushOrigin::Entity { entity, solid }
        });
    }
    Ok(selected)
}

fn append_solids<'a>(
    output: &mut Vec<(VmfBrushOrigin, &'a VmfSolid)>,
    entity: &'a VmfEntity,
    origin: impl Fn(usize) -> VmfBrushOrigin,
) {
    output.extend(
        entity
            .solids()
            .iter()
            .enumerate()
            .map(|(index, solid)| (origin(index), solid)),
    );
}

fn normalize_indices(indices: &[usize], count: usize) -> Result<Vec<usize>, Source1Error> {
    let mut unique = BTreeSet::new();
    for &index in indices {
        if index >= count {
            return Err(Source1Error::EntityIndexOutOfRange {
                index,
                entity_count: count,
            });
        }
        if !unique.insert(index) {
            return Err(Source1Error::DuplicateEntityIndex { index });
        }
    }
    Ok(unique.into_iter().collect())
}

fn adapt_solid(
    origin: VmfBrushOrigin,
    solid: &VmfSolid,
    limits: Source1AdapterLimits,
) -> Result<SelectedBrushSolid, Source1Error> {
    if solid.sides().len() > limits.max_sides_per_solid {
        return Err(Source1Error::LimitExceeded {
            limit: Source1Limit::SidesPerSolid,
            maximum: limits.max_sides_per_solid,
        });
    }
    let sides = solid
        .sides()
        .iter()
        .enumerate()
        .map(|(index, side)| adapt_side(origin, index, side, limits))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(SelectedBrushSolid {
        origin,
        solid: BrushSolid::new(solid.id().map(str::to_owned), sides),
    })
}

fn adapt_side(
    origin: VmfBrushOrigin,
    side: usize,
    source: &VmfSide,
    limits: Source1AdapterLimits,
) -> Result<BrushSide, Source1Error> {
    let plane = source
        .plane()
        .ok_or(Source1Error::MissingPlane { origin, side })?;
    if plane.len() > limits.max_plane_bytes {
        return Err(Source1Error::LimitExceeded {
            limit: Source1Limit::PlaneBytes,
            maximum: limits.max_plane_bytes,
        });
    }
    let material = source
        .material()
        .ok_or(Source1Error::MissingMaterial { origin, side })?;
    let points = parse_plane(plane).map_err(|source| Source1Error::InvalidPlane {
        origin,
        side,
        source,
    })?;
    let mut output = BrushSide::new(points, material);
    if let (Some(uaxis), Some(vaxis)) = (source.uaxis(), source.vaxis()) {
        let u = parse_texture_axis(uaxis).map_err(|source| Source1Error::InvalidTextureAxis {
            origin,
            side,
            source,
        })?;
        let v = parse_texture_axis(vaxis).map_err(|source| Source1Error::InvalidTextureAxis {
            origin,
            side,
            source,
        })?;
        output = output.with_texture_axes(TextureAxes { u, v });
    }
    Ok(output)
}

fn parse_texture_axis(input: &str) -> Result<TextureAxis, TextureAxisParseError> {
    let (values, scale) = input
        .trim()
        .strip_prefix('[')
        .and_then(|text| text.split_once(']'))
        .ok_or(TextureAxisParseError)?;
    let values: Vec<_> = values
        .split_whitespace()
        .map(str::parse::<f32>)
        .collect::<Result<_, _>>()
        .map_err(|_| TextureAxisParseError)?;
    let scale = scale
        .trim()
        .parse::<f32>()
        .map_err(|_| TextureAxisParseError)?;
    if values.len() != 4
        || !values.iter().all(|value| value.is_finite())
        || !scale.is_finite()
        || scale == 0.0
    {
        return Err(TextureAxisParseError);
    }
    Ok(TextureAxis {
        direction: [values[0], values[1], values[2]],
        shift: values[3],
        scale,
    })
}

/// A VMF texture-axis value was not exactly `[x y z shift] scale`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TextureAxisParseError;
impl fmt::Display for TextureAxisParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("invalid texture axis")
    }
}
impl Error for TextureAxisParseError {}

/// Parses exactly three finite VMF point tuples.
///
/// The accepted grammar is three whitespace-separated parenthesized triples
/// and no trailing text.
///
/// # Errors
///
/// Returns `PlaneParseError` for missing syntax, invalid finite numbers or
/// trailing data.
pub fn parse_plane(input: &str) -> Result<PlanePoints, PlaneParseError> {
    let mut parser = PlaneParser { input, offset: 0 };
    let first = parser.point()?;
    let second = parser.point()?;
    let third = parser.point()?;
    parser.space();
    if parser.peek().is_some() {
        return Err(PlaneParseError::new(
            PlaneParseErrorKind::TrailingText,
            parser.offset,
        ));
    }
    Ok(PlanePoints::new(first, second, third))
}

/// Structured plane conversion error independent of VMF line locations.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlaneParseError {
    kind: PlaneParseErrorKind,
    byte_offset: usize,
}

impl PlaneParseError {
    fn new(kind: PlaneParseErrorKind, byte_offset: usize) -> Self {
        Self { kind, byte_offset }
    }
    /// Returns structured failure kind.
    #[must_use]
    pub const fn kind(&self) -> &PlaneParseErrorKind {
        &self.kind
    }
    /// Returns zero-based UTF-8 byte offset in the plane value.
    #[must_use]
    pub const fn byte_offset(&self) -> usize {
        self.byte_offset
    }
}
impl fmt::Display for PlaneParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "invalid plane at byte {}: {:?}",
            self.byte_offset, self.kind
        )
    }
}
impl Error for PlaneParseError {}

/// Specific strict plane grammar failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PlaneParseErrorKind {
    /// Opening parenthesis was required.
    ExpectedOpenParen,
    /// Closing parenthesis was required.
    ExpectedCloseParen,
    /// A coordinate was absent.
    ExpectedNumber,
    /// Coordinate text was not finite f32.
    InvalidNumber {
        /// Rejected text.
        token: String,
    },
    /// Extra text followed the third point.
    TrailingText,
}

/// Source 1 adapter or geometry compiler failure.
#[derive(Debug)]
pub enum Source1Error {
    /// Selected entity index was absent.
    EntityIndexOutOfRange {
        /// Requested index.
        index: usize,
        /// VMF entity count.
        entity_count: usize,
    },
    /// Explicit selection repeated an entity index.
    DuplicateEntityIndex {
        /// Repeated index.
        index: usize,
    },
    /// Side has no plane property.
    MissingPlane {
        /// Owning solid.
        origin: VmfBrushOrigin,
        /// Side index.
        side: usize,
    },
    /// Side has no material property.
    MissingMaterial {
        /// Owning solid.
        origin: VmfBrushOrigin,
        /// Side index.
        side: usize,
    },
    /// Plane text conversion failed.
    InvalidPlane {
        /// Owning solid.
        origin: VmfBrushOrigin,
        /// Side index.
        side: usize,
        /// Underlying conversion error.
        source: PlaneParseError,
    },
    /// Texture-axis text conversion failed.
    InvalidTextureAxis {
        /// Owning solid.
        origin: VmfBrushOrigin,
        /// Side index.
        side: usize,
        /// Underlying conversion error.
        source: TextureAxisParseError,
    },
    /// Adapter work limit was exceeded.
    LimitExceeded {
        /// Limited resource.
        limit: Source1Limit,
        /// Configured maximum.
        maximum: usize,
    },
    /// Convex brush compiler rejected the normalized input.
    Compile(BrushCompileError),
}
impl fmt::Display for Source1Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EntityIndexOutOfRange {
                index,
                entity_count,
            } => write!(formatter, "entity index {index} outside {entity_count}"),
            Self::DuplicateEntityIndex { index } => {
                write!(formatter, "duplicate entity index {index}")
            }
            Self::MissingPlane { origin, side } => {
                write!(formatter, "missing plane at {origin:?} side {side}")
            }
            Self::MissingMaterial { origin, side } => {
                write!(formatter, "missing material at {origin:?} side {side}")
            }
            Self::InvalidPlane {
                origin,
                side,
                source,
            } => write!(
                formatter,
                "invalid plane at {origin:?} side {side}: {source}"
            ),
            Self::InvalidTextureAxis {
                origin,
                side,
                source,
            } => write!(
                formatter,
                "invalid texture axis at {origin:?} side {side}: {source}"
            ),
            Self::LimitExceeded { limit, maximum } => {
                write!(formatter, "{limit:?} exceeds limit {maximum}")
            }
            Self::Compile(source) => write!(formatter, "brush compile failure: {source}"),
        }
    }
}
impl Error for Source1Error {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidPlane { source, .. } => Some(source),
            Self::InvalidTextureAxis { source, .. } => Some(source),
            Self::Compile(source) => Some(source),
            _ => None,
        }
    }
}

/// Bounded resource controlled by `Source1AdapterLimits`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Source1Limit {
    /// Selected solid count.
    SelectedSolids,
    /// Side count in one solid.
    SidesPerSolid,
    /// Raw bytes in one plane property.
    PlaneBytes,
}

struct PlaneParser<'a> {
    input: &'a str,
    offset: usize,
}
impl PlaneParser<'_> {
    fn point(&mut self) -> Result<[f32; 3], PlaneParseError> {
        self.space();
        if self.take() != Some('(') {
            return Err(PlaneParseError::new(
                PlaneParseErrorKind::ExpectedOpenParen,
                self.offset,
            ));
        }
        let values = [self.number()?, self.number()?, self.number()?];
        self.space();
        if self.take() != Some(')') {
            return Err(PlaneParseError::new(
                PlaneParseErrorKind::ExpectedCloseParen,
                self.offset,
            ));
        }
        Ok(values)
    }
    fn number(&mut self) -> Result<f32, PlaneParseError> {
        self.space();
        let start = self.offset;
        while self
            .peek()
            .is_some_and(|value| !value.is_whitespace() && value != ')')
        {
            self.take();
        }
        if start == self.offset {
            return Err(PlaneParseError::new(
                PlaneParseErrorKind::ExpectedNumber,
                start,
            ));
        }
        let token = &self.input[start..self.offset];
        token
            .parse::<f32>()
            .ok()
            .filter(|value| value.is_finite())
            .ok_or_else(|| {
                PlaneParseError::new(
                    PlaneParseErrorKind::InvalidNumber {
                        token: token.to_owned(),
                    },
                    start,
                )
            })
    }
    fn space(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            self.take();
        }
    }
    fn peek(&self) -> Option<char> {
        self.input[self.offset..].chars().next()
    }
    fn take(&mut self) -> Option<char> {
        let value = self.peek()?;
        self.offset += value.len_utf8();
        Some(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use yuyib_vmf::parse;

    const CUBE: &str = r#"
world { solid {
 side { "plane" "(-1 -1 -1) (-1 -1 1) (-1 1 1)" "material" "brick/wall" }
 side { "plane" "(1 -1 -1) (1 1 1) (1 -1 1)" "material" "brick/wall" }
 side { "plane" "(-1 -1 -1) (1 -1 -1) (1 -1 1)" "material" "brick/floor" }
 side { "plane" "(-1 1 -1) (1 1 1) (1 1 -1)" "material" "brick/ceiling" }
 side { "plane" "(-1 -1 -1) (-1 1 -1) (1 1 -1)" "material" "brick/wall" }
 side { "plane" "(-1 -1 1) (1 1 1) (-1 1 1)" "material" "brick/wall" }
} }
entity { solid { "id" "entity" } }
"#;

    #[test]
    fn cube_vmf_compiles_to_model() {
        let map = parse(CUBE).expect("parse");
        let model = compile_map(
            &map,
            &MapBrushSelection::WorldOnly,
            Source1AdapterLimits::default(),
            BrushCompileLimits::default(),
        )
        .expect("compile");
        assert_eq!(model.meshes().len(), 1);
        assert_eq!(model.meshes()[0].primitives().len(), 6);
    }
    #[test]
    fn plane_and_selection_failures_are_structured() {
        assert!(matches!(
            parse_plane("(0 0) (0 1 0) (1 1 0)"),
            Err(PlaneParseError { .. })
        ));
        let map = parse(CUBE).expect("parse");
        assert!(matches!(
            adapt_map(
                &map,
                &MapBrushSelection::EntityIndices(vec![0, 0]),
                Source1AdapterLimits::default()
            ),
            Err(Source1Error::DuplicateEntityIndex { .. })
        ));
    }
    #[test]
    fn order_is_world_then_entity() {
        let map = parse(CUBE).expect("parse");
        let selected = adapt_map(
            &map,
            &MapBrushSelection::WorldAndEntities,
            Source1AdapterLimits::default(),
        )
        .expect("adapt");
        assert_eq!(selected.len(), 2);
        assert_eq!(selected[0].origin(), VmfBrushOrigin::World { solid: 0 });
        assert_eq!(
            selected[1].origin(),
            VmfBrushOrigin::Entity {
                entity: 0,
                solid: 0
            }
        );
    }
}
