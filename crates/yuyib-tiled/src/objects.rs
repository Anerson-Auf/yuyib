//! Tiled objectgroup import (points/rects + properties).

use std::fmt;

use serde::Deserialize;

use super::{RawProperty, TiledImportError, TiledImportLimits};

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

/// One imported point or axis-aligned rectangle from an object layer.
#[derive(Clone, Debug, PartialEq)]
pub struct ImportedTiledObject2d {
    name: String,
    class: String,
    /// Tiled pixel position (top-left for rects, point for points).
    position_px: [f32; 2],
    /// Tiled pixel size (`[0, 0]` for points).
    size_px: [f32; 2],
    properties: Vec<(String, TiledPropertyValue)>,
}

impl ImportedTiledObject2d {
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

    /// Top-left / point position in Tiled pixel space.
    #[must_use]
    pub const fn position_px(&self) -> [f32; 2] {
        self.position_px
    }

    /// Size in Tiled pixel space.
    #[must_use]
    pub const fn size_px(&self) -> [f32; 2] {
        self.size_px
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
}

/// One imported `objectgroup` layer.
#[derive(Clone, Debug, PartialEq)]
pub struct ImportedTiledObjectLayer2d {
    name: String,
    objects: Vec<ImportedTiledObject2d>,
}

impl ImportedTiledObjectLayer2d {
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
    name: String,
    #[serde(default, rename = "type")]
    type_name: String,
    #[serde(default)]
    class: String,
    #[serde(default)]
    x: f32,
    #[serde(default)]
    y: f32,
    #[serde(default)]
    width: f32,
    #[serde(default)]
    height: f32,
    #[serde(default)]
    point: bool,
    #[serde(default)]
    ellipse: bool,
    #[serde(default)]
    polygon: Option<serde_json::Value>,
    #[serde(default)]
    polyline: Option<serde_json::Value>,
    #[serde(default)]
    gid: Option<u32>,
    #[serde(default)]
    template: Option<String>,
    #[serde(default)]
    text: Option<serde_json::Value>,
    #[serde(default)]
    properties: Option<Vec<RawProperty>>,
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
    if raw.ellipse
        || raw.polygon.is_some()
        || raw.polyline.is_some()
        || raw.gid.is_some()
        || raw.template.is_some()
        || raw.text.is_some()
    {
        return Err(TiledImportError::Unsupported(
            "only point/rect tiled objects are supported in this slice",
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
    let is_point = raw.point || (raw.width == 0.0 && raw.height == 0.0);
    let size_px = if is_point {
        [0.0, 0.0]
    } else {
        [raw.width, raw.height]
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
        properties,
    })
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

impl fmt::Display for TiledPropertyValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bool(value) => write!(formatter, "{value}"),
            Self::String(value) => write!(formatter, "{value}"),
            Self::Number(value) => write!(formatter, "{value}"),
        }
    }
}
