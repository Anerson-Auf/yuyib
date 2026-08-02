//! Typed Transform3d authoring validation and scene materialization.
//!
//! Standalone authoring APIs with no editor, renderer, webview, or GPU dependency.
//! Editor integration should invoke these when wired.

#![forbid(unsafe_code)]

use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error as StdError,
    fmt,
    str::FromStr,
};

use serde::Deserialize;
use serde_json::Value;
use yuyib_authoring::{EntityGuid, SceneDocument, SceneFormatError};
use yuyib_ecs::{
    bevy_ecs::{entity::Entity, world::World},
    prelude::*,
};
use yuyib_game_3d::{LocalTransform3d, Parent3d, Transform3d, propagate_world_transforms};

/// Persisted schema for a world-space [`Transform3d`].
pub const TRANSFORM_3D_SCHEMA: &str = "yuyib.transform3d";
/// Persisted schema for a parent-relative Transform3d payload.
pub const LOCAL_TRANSFORM_3D_SCHEMA: &str = "yuyib.local-transform3d";
/// Persisted schema for a parent-relative entity reference.
pub const PARENT_3D_SCHEMA: &str = "yuyib.parent3d";
/// Persisted schema for a renderable 3D model projection.
pub const MODEL_3D_SCHEMA: &str = "yuyib.model3d";
/// Persisted schema for a directional 3D light projection.
pub const DIRECTIONAL_LIGHT_3D_SCHEMA: &str = "yuyib.directional-light3d";

/// A runtime world created from Transform3d components in an authored scene.
#[derive(Debug)]
pub struct MaterializedTransformScene {
    /// Runtime world containing materialized world and hierarchy transforms.
    pub world: World,
    /// Stable authored identity for each materialized runtime entity.
    pub entities: BTreeMap<EntityGuid, Entity>,
}

/// Explains a rejected Transform3d authoring value or scene.
#[derive(Debug)]
pub enum Error {
    /// The field belongs to a schema this crate does not validate.
    UnsupportedSchema {
        /// Schema supplied to [`validate_transform_field`].
        schema_id: String,
    },
    /// The field path is not a Transform3d scalar field.
    UnsupportedField {
        /// Schema supplied to [`validate_transform_field`].
        schema_id: String,
        /// Field path supplied to [`validate_transform_field`].
        field_path: String,
    },
    /// A scalar Transform3d field was not representable as a finite `f32`.
    InvalidFieldValue {
        /// Schema supplied to [`validate_transform_field`] or [`validate_parent_field`].
        schema_id: String,
        /// Field path supplied to [`validate_transform_field`] or [`validate_parent_field`].
        field_path: String,
        /// Stable reason suitable for diagnostics.
        reason: &'static str,
    },
    /// A `Parent3d` field value could not be parsed or resolved.
    InvalidParentValue {
        /// Schema supplied to [`validate_parent_field`].
        schema_id: String,
        /// Field path supplied to [`validate_parent_field`].
        field_path: String,
        /// Stable reason suitable for diagnostics.
        reason: &'static str,
    },
    /// The source document is structurally invalid.
    SceneFormat(SceneFormatError),
    /// A Transform3d payload could not be decoded or violates TRS invariants.
    InvalidTransform {
        /// Authored entity containing the invalid component.
        entity: EntityGuid,
        /// Stable reason suitable for diagnostics.
        reason: String,
    },
    /// A `Parent3d` relationship could not be resolved or propagated.
    InvalidHierarchy {
        /// Stable reason suitable for diagnostics.
        reason: String,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedSchema { schema_id } => {
                write!(formatter, "unsupported Transform3d schema {schema_id:?}")
            }
            Self::UnsupportedField {
                schema_id,
                field_path,
            } => write!(
                formatter,
                "field {field_path:?} is not supported by Transform3d schema {schema_id:?}"
            ),
            Self::InvalidFieldValue {
                schema_id,
                field_path,
                reason,
            } => write!(
                formatter,
                "invalid value for {schema_id}.{field_path}: {reason}"
            ),
            Self::InvalidParentValue {
                schema_id,
                field_path,
                reason,
            } => write!(
                formatter,
                "invalid value for {schema_id}.{field_path}: {reason}"
            ),
            Self::SceneFormat(error) => error.fmt(formatter),
            Self::InvalidTransform { entity, reason } => {
                write!(
                    formatter,
                    "invalid Transform3d for entity {entity}: {reason}"
                )
            }
            Self::InvalidHierarchy { reason } => {
                write!(formatter, "invalid Transform3d hierarchy: {reason}")
            }
        }
    }
}

impl StdError for Error {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::SceneFormat(error) => Some(error),
            Self::UnsupportedSchema { .. }
            | Self::UnsupportedField { .. }
            | Self::InvalidFieldValue { .. }
            | Self::InvalidParentValue { .. }
            | Self::InvalidTransform { .. }
            | Self::InvalidHierarchy { .. } => None,
        }
    }
}

/// Validates one editable scalar Transform3d field.
///
/// Both `yuyib.transform3d` and `yuyib.local-transform3d` use the same TRS
/// field allowlist. A quaternion's non-zero invariant is checked only when
/// decoding the complete payload during materialization.
///
/// # Errors
///
/// Returns [`Error`] if the schema or field is unsupported, the value is not a
/// finite `f32`, or a scale axis is zero.
pub fn validate_transform_field(
    schema_id: &str,
    field_path: &str,
    value: &Value,
) -> Result<(), Error> {
    if !is_transform_schema(schema_id) {
        return Err(Error::UnsupportedSchema {
            schema_id: schema_id.to_owned(),
        });
    }

    let is_scale = matches!(field_path, "scale.x" | "scale.y" | "scale.z");
    if !matches!(
        field_path,
        "translation.x"
            | "translation.y"
            | "translation.z"
            | "rotation.x"
            | "rotation.y"
            | "rotation.z"
            | "rotation.w"
            | "scale.x"
            | "scale.y"
            | "scale.z"
    ) {
        return Err(Error::UnsupportedField {
            schema_id: schema_id.to_owned(),
            field_path: field_path.to_owned(),
        });
    }

    let scalar = json_f32(value).ok_or_else(|| Error::InvalidFieldValue {
        schema_id: schema_id.to_owned(),
        field_path: field_path.to_owned(),
        reason: "must be a number",
    })?;
    if !scalar.is_finite() {
        return Err(Error::InvalidFieldValue {
            schema_id: schema_id.to_owned(),
            field_path: field_path.to_owned(),
            reason: "must be a finite f32",
        });
    }
    if is_scale && scalar == 0.0 {
        return Err(Error::InvalidFieldValue {
            schema_id: schema_id.to_owned(),
            field_path: field_path.to_owned(),
            reason: "scale must be non-zero",
        });
    }
    Ok(())
}

/// Coerces a transform scalar field to a JSON number.
///
/// Inspector / bridge payloads sometimes arrive as numeric strings (`"0"`).
/// Validation already accepts those, but persistence must store real numbers
/// or rematerialize fails with `expected f32`.
#[must_use]
pub fn coerce_transform_field_value(value: &Value) -> Option<Value> {
    let scalar = json_f32(value)?;
    serde_json::Number::from_f64(f64::from(scalar)).map(Value::Number)
}

/// Validates one editable Model3d field.
///
/// Model paths are persisted strings; runtime mesh handles are represented by
/// optional unsigned integer slots.
///
/// # Errors
///
/// Returns [`Error`] if the schema or field is unsupported, or the value does
/// not match the persisted Model3d projection.
pub fn validate_model3d_field(
    schema_id: &str,
    field_path: &str,
    value: &Value,
) -> Result<(), Error> {
    if schema_id != MODEL_3D_SCHEMA {
        return Err(unsupported_schema(schema_id));
    }
    match field_path {
        "model" if value.is_null() => Ok(()),
        "model" if value.as_str().is_some_and(|path| !path.trim().is_empty()) => Ok(()),
        "mesh" if value.is_null() || value.as_u64().is_some() => Ok(()),
        "visible" if value.is_boolean() => Ok(()),
        "render_order" if value.as_i64().is_some() => Ok(()),
        "model" => Err(invalid_field_value(
            schema_id,
            field_path,
            "must be null or a non-empty asset path string",
        )),
        "mesh" => Err(invalid_field_value(
            schema_id,
            field_path,
            "must be null or an unsigned integer",
        )),
        "visible" => Err(invalid_field_value(
            schema_id,
            field_path,
            "must be a boolean",
        )),
        "render_order" => Err(invalid_field_value(
            schema_id,
            field_path,
            "must be a signed integer",
        )),
        _ => Err(unsupported_field(schema_id, field_path)),
    }
}

/// Validates one editable DirectionalLight3d field.
///
/// Vector fields accept both their scalar `x`/`y`/`z` paths and a complete
/// three-element JSON array, which matches `write_json_field` semantics.
///
/// # Errors
///
/// Returns [`Error`] if the schema or field is unsupported, or the value
/// cannot be represented by the persisted directional-light projection.
pub fn validate_directional_light_field(
    schema_id: &str,
    field_path: &str,
    value: &Value,
) -> Result<(), Error> {
    if schema_id != DIRECTIONAL_LIGHT_3D_SCHEMA {
        return Err(unsupported_schema(schema_id));
    }
    match field_path {
        "direction" => validate_vec3_field(schema_id, field_path, value, false),
        "direction.x" | "direction.y" | "direction.z" => {
            validate_finite_field(schema_id, field_path, value, false)
        }
        "color" => validate_vec3_field(schema_id, field_path, value, true),
        "color.x" | "color.y" | "color.z" => {
            validate_finite_field(schema_id, field_path, value, true)
        }
        "illuminance_lux" | "illuminance" => {
            validate_finite_field(schema_id, field_path, value, true)
        }
        "enabled" if value.is_boolean() => Ok(()),
        "enabled" => Err(invalid_field_value(
            schema_id,
            field_path,
            "must be a boolean",
        )),
        _ => Err(unsupported_field(schema_id, field_path)),
    }
}

fn unsupported_schema(schema_id: &str) -> Error {
    Error::UnsupportedSchema {
        schema_id: schema_id.to_owned(),
    }
}

fn unsupported_field(schema_id: &str, field_path: &str) -> Error {
    Error::UnsupportedField {
        schema_id: schema_id.to_owned(),
        field_path: field_path.to_owned(),
    }
}

fn invalid_field_value(schema_id: &str, field_path: &str, reason: &'static str) -> Error {
    Error::InvalidFieldValue {
        schema_id: schema_id.to_owned(),
        field_path: field_path.to_owned(),
        reason,
    }
}

fn validate_vec3_field(
    schema_id: &str,
    field_path: &str,
    value: &Value,
    non_negative: bool,
) -> Result<(), Error> {
    let Some(values) = value.as_array() else {
        return Err(invalid_field_value(
            schema_id,
            field_path,
            "must be a three-element numeric array",
        ));
    };
    if values.len() != 3
        || values
            .iter()
            .any(|value| !is_finite_value(value, non_negative))
    {
        return Err(invalid_field_value(
            schema_id,
            field_path,
            if non_negative {
                "components must be finite non-negative f32 values"
            } else {
                "components must be finite f32 values"
            },
        ));
    }
    Ok(())
}

fn validate_finite_field(
    schema_id: &str,
    field_path: &str,
    value: &Value,
    non_negative: bool,
) -> Result<(), Error> {
    if is_finite_value(value, non_negative) {
        Ok(())
    } else {
        Err(invalid_field_value(
            schema_id,
            field_path,
            if non_negative {
                "must be a finite non-negative f32"
            } else {
                "must be a finite f32"
            },
        ))
    }
}

fn is_finite_value(value: &Value, non_negative: bool) -> bool {
    let Some(number) = json_f32(value) else {
        return false;
    };
    number.is_finite() && (!non_negative || number >= 0.0)
}

/// Validates one editable `Parent3d` field.
///
/// The persisted payload stores the parent relationship in the `parent` field
/// as an [`EntityGuid`] string, or `null` to clear the parent (scene root).
/// When `known_entities` is supplied, a non-null parent must reference one of
/// those authored entities.
///
/// # Errors
///
/// Returns [`Error`] if the schema or field is unsupported, the value is not
/// null / a non-empty entity GUID string, or the parent does not exist in
/// `known_entities`.
pub fn validate_parent_field(
    schema_id: &str,
    field_path: &str,
    value: &Value,
    known_entities: Option<&BTreeSet<EntityGuid>>,
) -> Result<Option<EntityGuid>, Error> {
    if schema_id != PARENT_3D_SCHEMA {
        return Err(Error::UnsupportedSchema {
            schema_id: schema_id.to_owned(),
        });
    }
    if field_path != "parent" {
        return Err(Error::UnsupportedField {
            schema_id: schema_id.to_owned(),
            field_path: field_path.to_owned(),
        });
    }

    if value.is_null() {
        return Ok(None);
    }

    let Some(text) = value.as_str() else {
        return Err(Error::InvalidParentValue {
            schema_id: schema_id.to_owned(),
            field_path: field_path.to_owned(),
            reason: "must be null or a non-empty entity GUID string",
        });
    };
    let text = text.trim();
    if text.is_empty() {
        return Err(Error::InvalidParentValue {
            schema_id: schema_id.to_owned(),
            field_path: field_path.to_owned(),
            reason: "must be null or a non-empty entity GUID string",
        });
    }
    let parent = EntityGuid::from_str(text).map_err(|_| Error::InvalidParentValue {
        schema_id: schema_id.to_owned(),
        field_path: field_path.to_owned(),
        reason: "must be a valid entity GUID string",
    })?;
    if let Some(known_entities) = known_entities {
        if !known_entities.contains(&parent) {
            return Err(Error::InvalidParentValue {
                schema_id: schema_id.to_owned(),
                field_path: field_path.to_owned(),
                reason: "parent must reference an existing authored entity",
            });
        }
    }
    Ok(Some(parent))
}

/// Materializes authored 3D transform components into a new world.
///
/// World-space `yuyib.transform3d` components are materialized directly.
/// `yuyib.local-transform3d` components participate when they have a resolved
/// `yuyib.parent3d` relationship; local-only ancestors required by those
/// relationships are materialized as roots. A world-space parent is adapted to
/// a local root using the same TRS values so that hierarchy propagation can
/// derive its child's world transform.
///
/// The operation is transactional: all payloads and parent references are
/// decoded and validated before any runtime entity is spawned, and hierarchy
/// propagation completes before the world is returned. Unknown component
/// schemas are deliberately ignored.
///
/// # Errors
///
/// Returns [`Error`] if the document is structurally invalid, a recognized
/// payload is malformed, a parent does not resolve to a transform-bearing
/// entity, or hierarchy propagation fails.
pub fn materialize_transform_scene(
    document: &SceneDocument,
) -> Result<MaterializedTransformScene, Error> {
    document.validate().map_err(Error::SceneFormat)?;

    let decoded = document
        .entities
        .iter()
        .map(|entity| {
            let world = component_payload(entity, TRANSFORM_3D_SCHEMA)
                .map(|payload| decode_transform(entity.guid, payload))
                .transpose()?;
            let local = component_payload(entity, LOCAL_TRANSFORM_3D_SCHEMA)
                .map(|payload| decode_transform(entity.guid, payload).map(LocalTransform3d::from))
                .transpose()?;
            let parent = component_payload(entity, PARENT_3D_SCHEMA)
                .map(|payload| decode_parent(entity.guid, payload))
                .transpose()?;
            Ok((
                entity.guid,
                DecodedEntity {
                    world,
                    local,
                    parent,
                },
            ))
        })
        .collect::<Result<BTreeMap<_, _>, Error>>()?;

    let mut local_entities = BTreeMap::new();
    for (guid, entity) in &decoded {
        // Inspector reparent writes `yuyib.parent3d` onto entities that still
        // carry world `yuyib.transform3d` (no separate local component). Treat
        // that world TRS as the local offset under the parent.
        if entity.parent.is_none() {
            continue;
        }
        if entity.local.is_none() && entity.world.is_none() {
            return Err(invalid_hierarchy(
                *guid,
                "parented entity must have Transform3d or LocalTransform3d",
            ));
        }
        let parent = entity.parent.expect("checked above");
        let Some(parent_entity) = decoded.get(&parent) else {
            return Err(invalid_hierarchy(
                *guid,
                "parent does not reference an authored entity",
            ));
        };
        if parent == *guid {
            return Err(invalid_hierarchy(*guid, "entity cannot parent itself"));
        }
        if parent_entity.world.is_none() && parent_entity.local.is_none() {
            return Err(invalid_hierarchy(
                *guid,
                "parent must have Transform3d or LocalTransform3d",
            ));
        }
        local_entities.insert(*guid, ());
    }

    let mut changed = true;
    while changed {
        changed = false;
        for guid in local_entities.keys().copied().collect::<Vec<_>>() {
            let entity = decoded.get(&guid).expect("local entity must be decoded");
            let Some(parent) = entity.parent else {
                continue;
            };
            let parent_entity = decoded
                .get(&parent)
                .expect("resolved parents must be decoded");
            if parent_entity.local.is_some() && local_entities.insert(parent, ()).is_none() {
                changed = true;
            }
        }
    }

    let mut hierarchy_entities = local_entities.clone();
    for guid in local_entities.keys() {
        let Some(parent) = decoded.get(guid).and_then(|entity| entity.parent) else {
            continue;
        };
        hierarchy_entities.insert(parent, ());
    }

    let mut world = World::new();
    let mut entities = BTreeMap::new();
    for (guid, entity) in &decoded {
        let Some(transform) = entity.world else {
            continue;
        };
        let entity = world.spawn(transform).id();
        entities.insert(*guid, entity);
    }
    for guid in hierarchy_entities.keys() {
        let entity = decoded.get(guid).expect("hierarchy entity must be decoded");
        let local = entity
            .local
            .or_else(|| entity.world.map(LocalTransform3d::from))
            .expect("hierarchy entity must have a transform");
        if let Some(runtime) = entities.get(guid).copied() {
            world.entity_mut(runtime).insert(local);
        } else {
            let runtime = world.spawn(local).id();
            entities.insert(*guid, runtime);
        }
    }
    for guid in local_entities.keys() {
        let Some(parent_guid) = decoded.get(guid).and_then(|entity| entity.parent) else {
            continue;
        };
        let child = *entities
            .get(guid)
            .expect("participating local entity must be materialized");
        let parent = *entities
            .get(&parent_guid)
            .expect("resolved parent must be materialized");
        world.entity_mut(child).insert(Parent3d::new(parent));
    }
    if !hierarchy_entities.is_empty() {
        propagate_world_transforms(&mut world).map_err(|error| Error::InvalidHierarchy {
            reason: error.to_string(),
        })?;
    }
    Ok(MaterializedTransformScene { world, entities })
}

fn is_transform_schema(schema_id: &str) -> bool {
    matches!(schema_id, TRANSFORM_3D_SCHEMA | LOCAL_TRANSFORM_3D_SCHEMA)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthoredTransform3d {
    translation: [f32; 3],
    rotation: [f32; 4],
    scale: [f32; 3],
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthoredParent3d {
    parent: EntityGuid,
}

#[derive(Clone, Copy)]
struct DecodedEntity {
    world: Option<Transform3d>,
    local: Option<LocalTransform3d>,
    parent: Option<EntityGuid>,
}

fn component_payload<'a>(
    entity: &'a yuyib_authoring::SceneEntityRecord,
    schema: &str,
) -> Option<&'a Value> {
    entity
        .components
        .iter()
        .find(|component| component.schema().as_str() == schema)
        .map(|component| component.payload())
}

fn decode_transform(entity: EntityGuid, payload: &Value) -> Result<Transform3d, Error> {
    let normalized = normalize_trs_payload(payload)
        .map_err(|reason| Error::InvalidTransform { entity, reason })?;
    let authored: AuthoredTransform3d =
        serde_json::from_value(normalized).map_err(|error| Error::InvalidTransform {
            entity,
            reason: format!("payload must contain only translation, rotation, and scale: {error}"),
        })?;
    validate_transform(entity, authored)
}

/// Coerces numeric-string TRS payloads (`"0"` → `0.0`) across the document.
///
/// Inspector and older fixtures sometimes persist transform components as JSON
/// strings. Runtime materialize already tolerates that, but Play binaries and
/// on-disk `.yscene` files stay cleaner when authoring rewrites real numbers.
///
/// Returns how many component payloads were rewritten.
pub fn coerce_document_transform_payloads(document: &mut SceneDocument) -> usize {
    let mut rewritten = 0_usize;
    for entity in &mut document.entities {
        for component in &mut entity.components {
            let schema = component.schema().as_str();
            if !is_transform_schema(schema) {
                continue;
            }
            let Ok(normalized) = normalize_trs_payload(component.payload()) else {
                continue;
            };
            if &normalized != component.payload() {
                component.replace_payload(component.version(), normalized);
                rewritten = rewritten.saturating_add(1);
            }
        }
    }
    rewritten
}

/// Parses a JSON number or numeric string into `f32`.
#[must_use]
pub fn json_f32(value: &Value) -> Option<f32> {
    match value {
        Value::Number(number) => number.as_f64().map(|value| value as f32),
        Value::String(text) => text.trim().parse::<f32>().ok(),
        _ => None,
    }
    .filter(|value| value.is_finite())
}

fn normalize_trs_payload(payload: &Value) -> Result<Value, String> {
    let Value::Object(object) = payload else {
        return Err("payload must be a JSON object".to_owned());
    };
    let mut normalized = serde_json::Map::new();
    for key in ["translation", "rotation", "scale"] {
        let Some(Value::Array(components)) = object.get(key) else {
            return Err(format!("payload.{key} must be a JSON array"));
        };
        let expected = if key == "rotation" { 4 } else { 3 };
        if components.len() != expected {
            return Err(format!(
                "payload.{key} must contain {expected} components, got {}",
                components.len()
            ));
        }
        let mut coerced = Vec::with_capacity(expected);
        for (index, component) in components.iter().enumerate() {
            let Some(scalar) = json_f32(component) else {
                return Err(format!(
                    "payload.{key}[{index}] must be a finite number (got {component})"
                ));
            };
            let number = serde_json::Number::from_f64(f64::from(scalar))
                .ok_or_else(|| format!("payload.{key}[{index}] is not a finite f32"))?;
            coerced.push(Value::Number(number));
        }
        normalized.insert(key.to_owned(), Value::Array(coerced));
    }
    if object.len() != 3
        || object
            .keys()
            .any(|key| !matches!(key.as_str(), "translation" | "rotation" | "scale"))
    {
        // Preserve deny_unknown_fields semantics after coercion.
        for key in object.keys() {
            if !matches!(key.as_str(), "translation" | "rotation" | "scale") {
                return Err(format!("unknown field `{key}`"));
            }
        }
    }
    Ok(Value::Object(normalized))
}

fn decode_parent(entity: EntityGuid, payload: &Value) -> Result<EntityGuid, Error> {
    serde_json::from_value::<AuthoredParent3d>(payload.clone())
        .map(|parent| parent.parent)
        .map_err(|error| Error::InvalidHierarchy {
            reason: format!(
                "invalid Parent3d for entity {entity}: payload must contain only parent: {error}"
            ),
        })
}

fn validate_transform(
    entity: EntityGuid,
    transform: AuthoredTransform3d,
) -> Result<Transform3d, Error> {
    let components = transform
        .translation
        .iter()
        .chain(transform.rotation.iter())
        .chain(transform.scale.iter());
    if !components.copied().all(f32::is_finite) {
        return Err(invalid_transform(
            entity,
            "all TRS components must be finite",
        ));
    }
    if transform.scale.contains(&0.0) {
        return Err(invalid_transform(entity, "scale axes must be non-zero"));
    }
    if transform.rotation.iter().all(|component| *component == 0.0) {
        return Err(invalid_transform(
            entity,
            "rotation quaternion must be non-zero",
        ));
    }
    Ok(Transform3d {
        translation: transform.translation,
        rotation: transform.rotation,
        scale: transform.scale,
    })
}

fn invalid_transform(entity: EntityGuid, reason: &'static str) -> Error {
    Error::InvalidTransform {
        entity,
        reason: reason.to_owned(),
    }
}

fn invalid_hierarchy(entity: EntityGuid, reason: &'static str) -> Error {
    Error::InvalidHierarchy {
        reason: format!("entity {entity}: {reason}"),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use yuyib_authoring::{
        ComponentRecord, ComponentSchemaId, SceneDocument, SceneEntityRecord, SchemaVersion,
    };
    use yuyib_game_3d::WorldTransform3d;

    use super::*;

    #[test]
    fn spawns_one_entity_per_transform() {
        let mut scene = SceneDocument::new(SchemaVersion::INITIAL);
        scene
            .entities
            .push(entity_with_component(transform_component(json!({
                "translation": [1.0, 2.0, 3.0],
                "rotation": [0.0, 0.0, 0.0, 1.0],
                "scale": [1.0, 1.0, 1.0]
            }))));
        scene
            .entities
            .push(entity_with_component(ComponentRecord::new(
                ComponentSchemaId::new("example.unknown").expect("valid schema"),
                SchemaVersion::INITIAL,
                json!({"value": true}),
            )));

        let mut materialized = materialize_transform_scene(&scene).expect("materializes");
        assert_eq!(materialized.entities.len(), 1);
        assert_eq!(
            materialized
                .world
                .query::<&Transform3d>()
                .iter(&materialized.world)
                .count(),
            1
        );
    }

    #[test]
    fn preserves_guid_map() {
        let authored = entity_with_component(transform_component(valid_transform()));
        let guid = authored.guid;
        let mut scene = SceneDocument::new(SchemaVersion::INITIAL);
        scene.entities.push(authored);

        let materialized = materialize_transform_scene(&scene).expect("materializes");
        let runtime = materialized.entities.get(&guid).expect("guid is mapped");
        assert!(materialized.world.get::<Transform3d>(*runtime).is_some());
    }

    #[test]
    fn materializes_parent3d_on_world_transform_without_local_component() {
        let parent = entity_with_component(transform_component(json!({
            "translation": [5.0, 0.0, 0.0],
            "rotation": [0.0, 0.0, 0.0, 1.0],
            "scale": [1.0, 1.0, 1.0]
        })));
        let parent_guid = parent.guid;
        let child = SceneEntityRecord {
            guid: EntityGuid::new(),
            name: None,
            components: vec![
                transform_component(json!({
                    "translation": [1.0, 2.0, 0.0],
                    "rotation": [0.0, 0.0, 0.0, 1.0],
                    "scale": [1.0, 1.0, 1.0]
                })),
                parent_component(parent_guid),
            ],
            extensions: BTreeMap::new(),
        };
        let child_guid = child.guid;
        let mut scene = SceneDocument::new(SchemaVersion::INITIAL);
        scene.entities.extend([parent, child]);

        let materialized =
            materialize_transform_scene(&scene).expect("promotes world TRS under parent");
        let parent = materialized.entities[&parent_guid];
        let child = materialized.entities[&child_guid];
        assert_eq!(
            materialized.world.get::<Parent3d>(child),
            Some(&Parent3d::new(parent))
        );
        assert!(materialized.world.get::<LocalTransform3d>(parent).is_some());
        assert_eq!(
            materialized
                .world
                .get::<WorldTransform3d>(child)
                .expect("world transform")
                .translation(),
            [6.0, 2.0, 0.0]
        );
    }

    #[test]
    fn materializes_resolved_local_parent_hierarchy() {
        let parent = entity_with_component(transform_component(json!({
            "translation": [10.0, 2.0, 3.0],
            "rotation": [0.0, 0.0, 0.0, 1.0],
            "scale": [1.0, 1.0, 1.0]
        })));
        let parent_guid = parent.guid;
        let child = SceneEntityRecord {
            guid: EntityGuid::new(),
            name: None,
            components: vec![
                local_transform_component(json!({
                    "translation": [1.0, 0.0, 0.0],
                    "rotation": [0.0, 0.0, 0.0, 1.0],
                    "scale": [1.0, 1.0, 1.0]
                })),
                parent_component(parent_guid),
            ],
            extensions: BTreeMap::new(),
        };
        let child_guid = child.guid;
        let mut scene = SceneDocument::new(SchemaVersion::INITIAL);
        scene.entities.extend([parent, child]);

        let materialized = materialize_transform_scene(&scene).expect("materializes hierarchy");
        let parent = materialized.entities[&parent_guid];
        let child = materialized.entities[&child_guid];

        assert!(materialized.world.get::<Transform3d>(parent).is_some());
        assert!(materialized.world.get::<LocalTransform3d>(parent).is_some());
        assert_eq!(
            materialized.world.get::<Parent3d>(child),
            Some(&Parent3d::new(parent))
        );
        assert_eq!(
            materialized
                .world
                .get::<WorldTransform3d>(child)
                .expect("world transform")
                .translation(),
            [11.0, 2.0, 3.0]
        );
    }

    #[test]
    fn materializes_local_only_parent_required_by_child() {
        let parent = entity_with_component(local_transform_component(json!({
            "translation": [4.0, 0.0, 0.0],
            "rotation": [0.0, 0.0, 0.0, 1.0],
            "scale": [1.0, 1.0, 1.0]
        })));
        let parent_guid = parent.guid;
        let child = SceneEntityRecord {
            guid: EntityGuid::new(),
            name: None,
            components: vec![
                local_transform_component(json!({
                    "translation": [2.0, 0.0, 0.0],
                    "rotation": [0.0, 0.0, 0.0, 1.0],
                    "scale": [1.0, 1.0, 1.0]
                })),
                parent_component(parent_guid),
            ],
            extensions: BTreeMap::new(),
        };
        let child_guid = child.guid;
        let mut scene = SceneDocument::new(SchemaVersion::INITIAL);
        scene.entities.extend([parent, child]);

        let materialized = materialize_transform_scene(&scene).expect("materializes hierarchy");
        let parent = materialized.entities[&parent_guid];
        let child = materialized.entities[&child_guid];

        assert!(materialized.world.get::<Parent3d>(parent).is_none());
        assert_eq!(
            materialized
                .world
                .get::<WorldTransform3d>(child)
                .expect("world transform")
                .translation(),
            [6.0, 0.0, 0.0]
        );
    }

    #[test]
    fn rejects_local_transform_with_unresolved_parent() {
        let child = SceneEntityRecord {
            guid: EntityGuid::new(),
            name: None,
            components: vec![
                local_transform_component(valid_transform()),
                parent_component(EntityGuid::new()),
            ],
            extensions: BTreeMap::new(),
        };
        let mut scene = SceneDocument::new(SchemaVersion::INITIAL);
        scene.entities.push(child);

        assert!(matches!(
            materialize_transform_scene(&scene),
            Err(Error::InvalidHierarchy { .. })
        ));
    }

    #[test]
    fn rejects_non_finite_zero_scale_and_zero_quaternion() {
        for payload in [
            json!({
                "translation": [1e100, 0.0, 0.0],
                "rotation": [0.0, 0.0, 0.0, 1.0],
                "scale": [1.0, 1.0, 1.0]
            }),
            json!({
                "translation": [0.0, 0.0, 0.0],
                "rotation": [0.0, 0.0, 0.0, 1.0],
                "scale": [1.0, 0.0, 1.0]
            }),
            json!({
                "translation": [0.0, 0.0, 0.0],
                "rotation": [0.0, 0.0, 0.0, 0.0],
                "scale": [1.0, 1.0, 1.0]
            }),
        ] {
            let mut scene = SceneDocument::new(SchemaVersion::INITIAL);
            scene
                .entities
                .push(entity_with_component(transform_component(payload)));
            assert!(matches!(
                materialize_transform_scene(&scene),
                Err(Error::InvalidTransform { .. })
            ));
        }
    }

    #[test]
    fn coerce_document_rewrites_numeric_string_trs() {
        let mut scene = SceneDocument::new(SchemaVersion::INITIAL);
        scene
            .entities
            .push(entity_with_component(transform_component(json!({
                "translation": ["0", 1.0, "2"],
                "rotation": ["0", "0", "0", "1"],
                "scale": [1.0, "1", 1.0]
            }))));
        assert_eq!(coerce_document_transform_payloads(&mut scene), 1);
        let payload = scene.entities[0].components[0].payload();
        assert_eq!(payload["translation"][0], json!(0.0));
        assert_eq!(payload["rotation"][3], json!(1.0));
        assert_eq!(coerce_document_transform_payloads(&mut scene), 0);
    }

    #[test]
    fn materializes_numeric_string_trs_components() {
        let mut scene = SceneDocument::new(SchemaVersion::INITIAL);
        scene
            .entities
            .push(entity_with_component(transform_component(json!({
                "translation": ["1", "2.5", 0],
                "rotation": ["0", "0", "0", "1"],
                "scale": ["1", 1.0, "1"]
            }))));
        let materialized = materialize_transform_scene(&scene).expect("coerces string numbers");
        assert_eq!(materialized.entities.len(), 1);
    }

    #[test]
    fn coerce_transform_field_value_promotes_numeric_strings() {
        assert_eq!(coerce_transform_field_value(&json!("0")), Some(json!(0.0)));
        assert_eq!(
            coerce_transform_field_value(&json!(1.25)),
            Some(json!(1.25))
        );
        assert!(coerce_transform_field_value(&json!("nope")).is_none());
    }

    #[test]
    fn validator_allows_only_trs_scalar_paths() {
        assert!(
            validate_transform_field(TRANSFORM_3D_SCHEMA, "translation.x", &json!(1.0)).is_ok()
        );
        assert!(
            validate_transform_field(LOCAL_TRANSFORM_3D_SCHEMA, "rotation.w", &json!(1.0)).is_ok()
        );
        assert!(matches!(
            validate_transform_field(TRANSFORM_3D_SCHEMA, "translation", &json!(1.0)),
            Err(Error::UnsupportedField { .. })
        ));
        assert!(matches!(
            validate_transform_field("example.transform", "translation.x", &json!(1.0)),
            Err(Error::UnsupportedSchema { .. })
        ));
    }

    #[test]
    fn model3d_validator_accepts_persisted_projection() {
        for (field, value) in [
            ("model", json!(null)),
            ("model", json!("builtin:cube")),
            ("mesh", json!(42)),
            ("visible", json!(true)),
            ("render_order", json!(-3)),
        ] {
            assert!(validate_model3d_field(MODEL_3D_SCHEMA, field, &value).is_ok());
        }
        assert!(validate_model3d_field(MODEL_3D_SCHEMA, "model", &json!("  ")).is_err());
        assert!(validate_model3d_field(MODEL_3D_SCHEMA, "mesh", &json!(-1)).is_err());
    }

    #[test]
    fn directional_light_validator_accepts_scalar_and_vector_paths() {
        assert!(
            validate_directional_light_field(
                DIRECTIONAL_LIGHT_3D_SCHEMA,
                "direction",
                &json!([0.0, -1.0, 0.0])
            )
            .is_ok()
        );
        assert!(
            validate_directional_light_field(DIRECTIONAL_LIGHT_3D_SCHEMA, "color.z", &json!(0.5))
                .is_ok()
        );
        assert!(
            validate_directional_light_field(
                DIRECTIONAL_LIGHT_3D_SCHEMA,
                "illuminance_lux",
                &json!(1500.0)
            )
            .is_ok()
        );
        assert!(
            validate_directional_light_field(
                DIRECTIONAL_LIGHT_3D_SCHEMA,
                "color",
                &json!([1.0, -0.1, 0.0])
            )
            .is_err()
        );
        assert!(
            validate_directional_light_field(DIRECTIONAL_LIGHT_3D_SCHEMA, "enabled", &json!(1))
                .is_err()
        );
    }

    #[test]
    fn parent_validator_requires_entity_guid_string() {
        let parent = EntityGuid::new();
        assert_eq!(
            validate_parent_field(PARENT_3D_SCHEMA, "parent", &json!(parent.to_string()), None)
                .expect("valid guid"),
            Some(parent)
        );
        assert_eq!(
            validate_parent_field(PARENT_3D_SCHEMA, "parent", &Value::Null, None)
                .expect("null clears parent"),
            None
        );
        assert!(matches!(
            validate_parent_field(PARENT_3D_SCHEMA, "parent", &json!(""), None),
            Err(Error::InvalidParentValue { .. })
        ));
        assert!(matches!(
            validate_parent_field(PARENT_3D_SCHEMA, "parent", &json!("not-a-guid"), None),
            Err(Error::InvalidParentValue { .. })
        ));
        assert!(matches!(
            validate_parent_field(
                PARENT_3D_SCHEMA,
                "parent_guid",
                &json!(parent.to_string()),
                None
            ),
            Err(Error::UnsupportedField { .. })
        ));
        assert!(matches!(
            validate_parent_field("example.parent", "parent", &json!(parent.to_string()), None),
            Err(Error::UnsupportedSchema { .. })
        ));
    }

    #[test]
    fn parent_validator_can_require_existing_authored_entity() {
        let parent = EntityGuid::new();
        let missing = EntityGuid::new();
        let known = BTreeSet::from([parent]);

        assert_eq!(
            validate_parent_field(
                PARENT_3D_SCHEMA,
                "parent",
                &json!(parent.to_string()),
                Some(&known)
            )
            .expect("known parent"),
            Some(parent)
        );
        assert!(matches!(
            validate_parent_field(
                PARENT_3D_SCHEMA,
                "parent",
                &json!(missing.to_string()),
                Some(&known)
            ),
            Err(Error::InvalidParentValue { .. })
        ));
    }

    #[test]
    fn rejects_parent3d_payload_with_invalid_parent_guid() {
        let mut scene = SceneDocument::new(SchemaVersion::INITIAL);
        scene.entities.push(SceneEntityRecord {
            guid: EntityGuid::new(),
            name: None,
            components: vec![ComponentRecord::new(
                ComponentSchemaId::new(PARENT_3D_SCHEMA).expect("valid schema"),
                SchemaVersion::INITIAL,
                json!({ "parent": "not-a-guid" }),
            )],
            extensions: BTreeMap::new(),
        });

        assert!(matches!(
            materialize_transform_scene(&scene),
            Err(Error::InvalidHierarchy { .. })
        ));
    }

    #[test]
    fn rejects_self_parent_reference() {
        let guid = EntityGuid::new();
        let mut scene = SceneDocument::new(SchemaVersion::INITIAL);
        scene.entities.push(SceneEntityRecord {
            guid,
            name: None,
            components: vec![
                local_transform_component(valid_transform()),
                parent_component(guid),
            ],
            extensions: BTreeMap::new(),
        });

        assert!(matches!(
            materialize_transform_scene(&scene),
            Err(Error::InvalidHierarchy { .. })
        ));
    }

    fn valid_transform() -> Value {
        json!({
            "translation": [0.0, 0.0, 0.0],
            "rotation": [0.0, 0.0, 0.0, 1.0],
            "scale": [1.0, 1.0, 1.0]
        })
    }

    fn transform_component(payload: Value) -> ComponentRecord {
        ComponentRecord::new(
            ComponentSchemaId::new(TRANSFORM_3D_SCHEMA).expect("valid schema"),
            SchemaVersion::INITIAL,
            payload,
        )
    }

    fn local_transform_component(payload: Value) -> ComponentRecord {
        ComponentRecord::new(
            ComponentSchemaId::new(LOCAL_TRANSFORM_3D_SCHEMA).expect("valid schema"),
            SchemaVersion::INITIAL,
            payload,
        )
    }

    fn parent_component(parent: EntityGuid) -> ComponentRecord {
        ComponentRecord::new(
            ComponentSchemaId::new(PARENT_3D_SCHEMA).expect("valid schema"),
            SchemaVersion::INITIAL,
            json!({ "parent": parent }),
        )
    }

    fn entity_with_component(component: ComponentRecord) -> SceneEntityRecord {
        SceneEntityRecord {
            guid: EntityGuid::new(),
            name: None,
            components: vec![component],
            extensions: BTreeMap::new(),
        }
    }
}
