//! Tiled objectgroup import (point/rect/ellipse/polygon + properties).

use std::fmt;

use serde::Deserialize;

use super::{RawProperty, TiledImportError, TiledImportLimits};

/// Hard cap on polygon vertices per object (trust budget).
pub const MAX_POLYGON_POINTS: usize = 64;

/// Typed Tiled custom property value (bounded import set).
#[derive(Clone, Debug, PartialEq)]
pub enum TiledPropertyValue {
    /// JSON bool.
    Bool(bool),
    /// JSON string.
    String(String),
    /// Finite JSON number stored as `f64`.
    Number(f64),
}

impl TiledPropertyValue {
    /// Returns the string value when this is a string property.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(value) => Some(value.as_str()),
            _ => None,
        }
    }
}

/// Geometry kind for one imported Tiled object.
#[derive(Clone, Debug, PartialEq)]
pub enum TiledObjectShape2d {
    /// Zero-size point marker.
    Point,
    /// Axis-aligned rectangle (`position_px` top-left + `size_px`).
    Rect,
    /// Ellipse inscribed in the object's AABB.
    Ellipse,
    /// Closed polygon; vertices are relative to `position_px`.
    Polygon {
        /// Relative vertex list (at least 3).
        points_px: Vec<[f32; 2]>,
    },
}

/// One imported object from an object layer.
#[derive(Clone, Debug, PartialEq)]
pub struct ImportedTiledObject2d {
    name: String,
    class: String,
    /// Tiled pixel position (top-left for rects/ellipses, origin for polygons/points).
    position_px: [f32; 2],
    /// Tiled pixel size (`[0, 0]` for points; AABB for polygons).
    size_px: [f32; 2],
    shape: TiledObjectShape2d,
    properties: Vec<(String, TiledPropertyValue)>,
}

impl ImportedTiledObject2d {
    /// Builds one imported object (authoring / LDtk bridge).
    ///
    /// Shape is [`TiledObjectShape2d::Point`] when `size_px` is zero, otherwise
    /// [`TiledObjectShape2d::Rect`].
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        class: impl Into<String>,
        position_px: [f32; 2],
        size_px: [f32; 2],
        properties: Vec<(String, TiledPropertyValue)>,
    ) -> Self {
        let shape = if size_px == [0.0, 0.0] {
            TiledObjectShape2d::Point
        } else {
            TiledObjectShape2d::Rect
        };
        Self {
            name: name.into(),
            class: class.into(),
            position_px,
            size_px,
            shape,
            properties,
        }
    }

    /// Overrides geometry kind (ellipse / polygon).
    #[must_use]
    pub fn with_shape(mut self, shape: TiledObjectShape2d) -> Self {
        self.shape = shape;
        self
    }

    /// Object name (may be empty when Tiled omitted it).
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Object class (`class`, falling back to legacy `type`).
    #[must_use]
    pub fn class(&self) -> &str {
        &self.class
    }

    /// Top-left / point / polygon origin in Tiled pixel space.
    #[must_use]
    pub const fn position_px(&self) -> [f32; 2] {
        self.position_px
    }

    /// Size in Tiled pixel space (AABB for polygons).
    #[must_use]
    pub const fn size_px(&self) -> [f32; 2] {
        self.size_px
    }

    /// Geometry kind.
    #[must_use]
    pub const fn shape(&self) -> &TiledObjectShape2d {
        &self.shape
    }

    /// Custom properties in document order.
    #[must_use]
    pub fn properties(&self) -> &[(String, TiledPropertyValue)] {
        &self.properties
    }

    /// Looks up a property by name.
    #[must_use]
    pub fn property(&self, name: &str) -> Option<&TiledPropertyValue> {
        self.properties
            .iter()
            .find_map(|(key, value)| (key == name).then_some(value))
    }

    /// Returns whether `point_px` lies inside this object's shape (pixel space).
    #[must_use]
    pub fn contains_point_px(&self, point_px: [f32; 2]) -> bool {
        match &self.shape {
            TiledObjectShape2d::Point => {
                (point_px[0] - self.position_px[0]).abs() < f32::EPSILON
                    && (point_px[1] - self.position_px[1]).abs() < f32::EPSILON
            }
            TiledObjectShape2d::Rect => point_in_aabb(point_px, self.position_px, self.size_px),
            TiledObjectShape2d::Ellipse => point_in_ellipse(point_px, self.position_px, self.size_px),
            TiledObjectShape2d::Polygon { points_px } => {
                let absolute: Vec<[f32; 2]> = points_px
                    .iter()
                    .map(|p| [self.position_px[0] + p[0], self.position_px[1] + p[1]])
                    .collect();
                point_in_polygon(point_px, &absolute)
            }
        }
    }
}

fn point_in_aabb(point: [f32; 2], origin: [f32; 2], size: [f32; 2]) -> bool {
    point[0] >= origin[0]
        && point[1] >= origin[1]
        && point[0] <= origin[0] + size[0]
        && point[1] <= origin[1] + size[1]
}

fn point_in_ellipse(point: [f32; 2], origin: [f32; 2], size: [f32; 2]) -> bool {
    if size[0] <= 0.0 || size[1] <= 0.0 {
        return false;
    }
    let cx = origin[0] + size[0] * 0.5;
    let cy = origin[1] + size[1] * 0.5;
    let rx = size[0] * 0.5;
    let ry = size[1] * 0.5;
    let nx = (point[0] - cx) / rx;
    let ny = (point[1] - cy) / ry;
    nx * nx + ny * ny <= 1.0
}

fn point_in_polygon(point: [f32; 2], vertices: &[[f32; 2]]) -> bool {
    if vertices.len() < 3 {
        return false;
    }
    // Ray cast +X; count crossings (even-odd).
    let mut inside = false;
    let mut j = vertices.len() - 1;
    for i in 0..vertices.len() {
        let pi = vertices[i];
        let pj = vertices[j];
        let intersect = ((pi[1] > point[1]) != (pj[1] > point[1]))
            && (point[0]
                < (pj[0] - pi[0]) * (point[1] - pi[1]) / (pj[1] - pi[1] + f32::EPSILON) + pi[0]);
        if intersect {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// One imported object layer.
#[derive(Clone, Debug, PartialEq)]
pub struct ImportedTiledObjectLayer2d {
    name: String,
    objects: Vec<ImportedTiledObject2d>,
}

impl ImportedTiledObjectLayer2d {
    /// Builds one object layer.
    #[must_use]
    pub fn new(name: impl Into<String>, objects: Vec<ImportedTiledObject2d>) -> Self {
        Self {
            name: name.into(),
            objects,
        }
    }

    /// Layer name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Objects in document order.
    #[must_use]
    pub fn objects(&self) -> &[ImportedTiledObject2d] {
        &self.objects
    }
}

/// Converts Tiled pixel geometry into world centre + size using the bind scale.
///
/// World Y grows downward, matching [`super::TileMap2d`] / Tiled right-down.
#[must_use]
pub fn world_from_tiled_px(
    position_px: [f32; 2],
    size_px: [f32; 2],
    tile_pixel_size: [u32; 2],
    world_tile_size: [f32; 2],
) -> ([f32; 2], [f32; 2]) {
    let scale = [
        world_tile_size[0] / tile_pixel_size[0] as f32,
        world_tile_size[1] / tile_pixel_size[1] as f32,
    ];
    let size = [size_px[0] * scale[0], size_px[1] * scale[1]];
    let center = [
        position_px[0] * scale[0] + size[0] * 0.5,
        position_px[1] * scale[1] + size[1] * 0.5,
    ];
    (center, size)
}

#[derive(Debug, Deserialize)]
pub(super) struct RawObject {
    #[serde(default)]
    pub(super) name: String,
    #[serde(default, rename = "type")]
    pub(super) type_name: String,
    #[serde(default)]
    pub(super) class: String,
    #[serde(default)]
    pub(super) x: f32,
    #[serde(default)]
    pub(super) y: f32,
    #[serde(default)]
    pub(super) width: f32,
    #[serde(default)]
    pub(super) height: f32,
    #[serde(default)]
    pub(super) point: bool,
    #[serde(default)]
    pub(super) ellipse: bool,
    #[serde(default)]
    pub(super) polygon: Option<Vec<RawPolygonPoint>>,
    #[serde(default)]
    pub(super) polyline: Option<serde_json::Value>,
    #[serde(default)]
    pub(super) gid: Option<u32>,
    #[serde(default)]
    pub(super) template: Option<String>,
    #[serde(default)]
    pub(super) text: Option<serde_json::Value>,
    #[serde(default)]
    pub(super) properties: Option<Vec<RawProperty>>,
}

#[derive(Debug, Deserialize, Clone)]
pub(super) struct RawPolygonPoint {
    pub(super) x: f32,
    pub(super) y: f32,
}

pub(super) fn parse_object_layers(
    layers: &[super::RawLayer],
    limits: &TiledImportLimits,
) -> Result<Vec<ImportedTiledObjectLayer2d>, TiledImportError> {
    let mut out = Vec::new();
    for layer in layers {
        if layer.layer_type != "objectgroup" {
            continue;
        }
        super::validate_layer_name(&layer.name, limits)?;
        if out.len() >= limits.max_object_layers {
            return Err(TiledImportError::ObjectLayerLimit {
                limit: limits.max_object_layers,
            });
        }
        let objects_raw = layer.objects.as_deref().unwrap_or(&[]);
        if objects_raw.len() > limits.max_objects_per_layer {
            return Err(TiledImportError::ObjectLimit {
                limit: limits.max_objects_per_layer,
                actual: objects_raw.len(),
            });
        }
        let mut objects = Vec::with_capacity(objects_raw.len());
        for raw in objects_raw {
            objects.push(convert_object(raw, limits)?);
        }
        out.push(ImportedTiledObjectLayer2d {
            name: layer.name.clone(),
            objects,
        });
    }
    Ok(out)
}

fn convert_object(
    raw: &RawObject,
    limits: &TiledImportLimits,
) -> Result<ImportedTiledObject2d, TiledImportError> {
    if raw.name.len() > limits.max_object_name_bytes {
        return Err(TiledImportError::ObjectName);
    }
    if raw.polyline.is_some() || raw.gid.is_some() || raw.template.is_some() || raw.text.is_some()
    {
        return Err(TiledImportError::Unsupported(
            "polyline/tile-gid/template/text tiled objects are not supported in this slice",
        ));
    }
    if !raw.x.is_finite() || !raw.y.is_finite() || !raw.width.is_finite() || !raw.height.is_finite()
    {
        return Err(TiledImportError::Unsupported(
            "object geometry must be finite",
        ));
    }
    if raw.width < 0.0 || raw.height < 0.0 {
        return Err(TiledImportError::Unsupported(
            "object size must be non-negative",
        ));
    }

    let shape = if let Some(points) = raw.polygon.as_ref() {
        if raw.ellipse || raw.point {
            return Err(TiledImportError::Unsupported(
                "object cannot combine polygon with ellipse/point",
            ));
        }
        if points.len() < 3 {
            return Err(TiledImportError::Unsupported(
                "polygon requires at least 3 points",
            ));
        }
        if points.len() > MAX_POLYGON_POINTS {
            return Err(TiledImportError::Unsupported(
                "polygon exceeds vertex limit",
            ));
        }
        let mut points_px = Vec::with_capacity(points.len());
        for point in points {
            if !point.x.is_finite() || !point.y.is_finite() {
                return Err(TiledImportError::Unsupported(
                    "polygon points must be finite",
                ));
            }
            points_px.push([point.x, point.y]);
        }
        TiledObjectShape2d::Polygon { points_px }
    } else if raw.ellipse {
        if raw.width == 0.0 || raw.height == 0.0 {
            return Err(TiledImportError::Unsupported(
                "ellipse requires positive width and height",
            ));
        }
        TiledObjectShape2d::Ellipse
    } else if raw.point || (raw.width == 0.0 && raw.height == 0.0) {
        TiledObjectShape2d::Point
    } else {
        TiledObjectShape2d::Rect
    };

    let size_px = match &shape {
        TiledObjectShape2d::Point => [0.0, 0.0],
        TiledObjectShape2d::Rect | TiledObjectShape2d::Ellipse => [raw.width, raw.height],
        TiledObjectShape2d::Polygon { points_px } => polygon_aabb_size(points_px),
    };

    let class = if !raw.class.is_empty() {
        raw.class.clone()
    } else {
        raw.type_name.clone()
    };
    if class.len() > limits.max_object_name_bytes {
        return Err(TiledImportError::ObjectName);
    }
    let properties = convert_properties(raw.properties.as_deref().unwrap_or(&[]), limits)?;
    Ok(ImportedTiledObject2d {
        name: raw.name.clone(),
        class,
        position_px: [raw.x, raw.y],
        size_px,
        shape,
        properties,
    })
}

fn polygon_aabb_size(points_px: &[[f32; 2]]) -> [f32; 2] {
    let mut min_x = f32::INFINITY;
    let mut min_y = f32::INFINITY;
    let mut max_x = f32::NEG_INFINITY;
    let mut max_y = f32::NEG_INFINITY;
    for point in points_px {
        min_x = min_x.min(point[0]);
        min_y = min_y.min(point[1]);
        max_x = max_x.max(point[0]);
        max_y = max_y.max(point[1]);
    }
    [(max_x - min_x).max(0.0), (max_y - min_y).max(0.0)]
}

fn convert_properties(
    raw: &[RawProperty],
    limits: &TiledImportLimits,
) -> Result<Vec<(String, TiledPropertyValue)>, TiledImportError> {
    if raw.len() > limits.max_properties_per_object {
        return Err(TiledImportError::ObjectPropertyLimit {
            limit: limits.max_properties_per_object,
            actual: raw.len(),
        });
    }
    let mut out = Vec::with_capacity(raw.len());
    for property in raw {
        if property.name.is_empty() || property.name.len() > limits.max_object_name_bytes {
            return Err(TiledImportError::ObjectName);
        }
        let value = match &property.value {
            serde_json::Value::Bool(value) => TiledPropertyValue::Bool(*value),
            serde_json::Value::String(value) => {
                if value.len() > limits.max_property_string_bytes {
                    return Err(TiledImportError::ObjectPropertyString);
                }
                TiledPropertyValue::String(value.clone())
            }
            serde_json::Value::Number(value) => {
                let number = value.as_f64().ok_or(TiledImportError::Unsupported(
                    "object property number must be finite f64",
                ))?;
                if !number.is_finite() {
                    return Err(TiledImportError::Unsupported(
                        "object property number must be finite",
                    ));
                }
                TiledPropertyValue::Number(number)
            }
            _ => {
                return Err(TiledImportError::Unsupported(
                    "object properties support bool/string/number only",
                ));
            }
        };
        out.push((property.name.clone(), value));
    }
    Ok(out)
}

/// Parses TMX `points="x,y x,y ..."` into relative polygon vertices.
pub(super) fn parse_tmx_points(text: &str) -> Result<Vec<RawPolygonPoint>, TiledImportError> {
    let mut points = Vec::new();
    for token in text.split_whitespace() {
        let (x_text, y_text) = token.split_once(',').ok_or(TiledImportError::Unsupported(
            "tmx polygon points must be x,y pairs",
        ))?;
        let x: f32 = x_text
            .parse()
            .map_err(|_| TiledImportError::Unsupported("invalid tmx polygon x"))?;
        let y: f32 = y_text
            .parse()
            .map_err(|_| TiledImportError::Unsupported("invalid tmx polygon y"))?;
        if !x.is_finite() || !y.is_finite() {
            return Err(TiledImportError::Unsupported(
                "tmx polygon points must be finite",
            ));
        }
        points.push(RawPolygonPoint { x, y });
    }
    Ok(points)
}

impl fmt::Display for TiledPropertyValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bool(value) => write!(formatter, "{value}"),
            Self::String(value) => formatter.write_str(value),
            Self::Number(value) => write!(formatter, "{value}"),
        }
    }
}
