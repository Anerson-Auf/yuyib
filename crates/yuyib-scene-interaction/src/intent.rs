//! Script→object operations.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use yuyib_authoring::EntityGuid;

/// Authored / projected Transform3d schema id.
pub const SCHEMA_TRANSFORM_3D: &str = "yuyib.transform3d";
/// Authored / projected LocalTransform3d schema id.
pub const SCHEMA_LOCAL_TRANSFORM_3D: &str = "yuyib.local-transform3d";
/// Authored Parent3d schema id.
pub const SCHEMA_PARENT_3D: &str = "yuyib.parent3d";
/// Authored Model3d schema id.
pub const SCHEMA_MODEL_3D: &str = "yuyib.model3d";
/// Authored DirectionalLight3d schema id.
pub const SCHEMA_DIRECTIONAL_LIGHT_3D: &str = "yuyib.directional-light3d";

/// Known 3D schemas already closed in Editor visual authoring / projection.
pub const KNOWN_3D_SCHEMAS: &[&str] = &[
    SCHEMA_TRANSFORM_3D,
    SCHEMA_LOCAL_TRANSFORM_3D,
    SCHEMA_PARENT_3D,
    SCHEMA_MODEL_3D,
    SCHEMA_DIRECTIONAL_LIGHT_3D,
];

/// Which transform component an intent should prefer.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TransformSpace {
    /// Prefer [`SCHEMA_TRANSFORM_3D`] (falls back to local when only local exists).
    #[default]
    World,
    /// Prefer [`SCHEMA_LOCAL_TRANSFORM_3D`].
    Local,
}

/// One script→object operation.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SceneInteractionIntent {
    /// Set translation on Transform3d and/or LocalTransform3d.
    SetTranslation {
        /// Target authored entity.
        entity: EntityGuid,
        /// World/local meters.
        translation: [f32; 3],
        /// Which schema family to prefer.
        #[serde(default)]
        space: TransformSpace,
    },
    /// Set one dotted JSON field on an existing component (Inspector parity).
    SetComponentField {
        /// Target authored entity.
        entity: EntityGuid,
        /// Component schema id (`yuyib.model3d`, …).
        schema: String,
        /// Dotted field path (`visible`, `translation.x`, …).
        field_path: String,
        /// Replacement JSON value.
        value: Value,
    },
    /// Add a component. `payload = None` means adapter default for known schemas.
    AddComponent {
        /// Target authored entity.
        entity: EntityGuid,
        /// Component schema id.
        schema: String,
        /// Schema version (default 1).
        #[serde(default)]
        version: Option<u32>,
        /// Full JSON payload; omit for typed defaults on the Editor adapter.
        #[serde(default)]
        payload: Option<Value>,
    },
    /// Decoupled signal for quests/triggers (host drains; see [`crate::signal`]).
    EmitSignal {
        /// Signal name (`quest.…`, `trigger.…`, or host-defined).
        name: String,
        /// Opaque JSON payload.
        #[serde(default)]
        payload: Value,
    },
}

impl SceneInteractionIntent {
    /// Classifies this intent for capability checks.
    #[must_use]
    pub const fn kind(&self) -> crate::capabilities::IntentKind {
        match self {
            Self::SetTranslation { .. } => crate::capabilities::IntentKind::SetTranslation,
            Self::SetComponentField { .. } => crate::capabilities::IntentKind::SetComponentField,
            Self::AddComponent { .. } => crate::capabilities::IntentKind::AddComponent,
            Self::EmitSignal { .. } => crate::capabilities::IntentKind::EmitSignal,
        }
    }
}

/// Schema ids touched by [`SceneInteractionIntent::SetTranslation`].
#[must_use]
pub fn translation_schemas(space: TransformSpace) -> &'static [&'static str] {
    match space {
        TransformSpace::World => &[SCHEMA_TRANSFORM_3D, SCHEMA_LOCAL_TRANSFORM_3D],
        TransformSpace::Local => &[SCHEMA_LOCAL_TRANSFORM_3D, SCHEMA_TRANSFORM_3D],
    }
}

/// Dotted field writes for a translation triple (Inspector / command parity).
#[must_use]
pub fn translation_field_writes(translation: [f32; 3]) -> [(String, Value); 3] {
    [
        ("translation.x".to_owned(), Value::from(translation[0])),
        ("translation.y".to_owned(), Value::from(translation[1])),
        ("translation.z".to_owned(), Value::from(translation[2])),
    ]
}

/// Validates intent shape before an adapter mutates state.
///
/// # Errors
///
/// Empty signal names, empty schema/field paths, or control characters.
pub fn validate_intent(intent: &SceneInteractionIntent) -> Result<(), String> {
    match intent {
        SceneInteractionIntent::SetTranslation { .. } => Ok(()),
        SceneInteractionIntent::SetComponentField {
            schema,
            field_path,
            ..
        } => {
            validate_token(schema, "schema")?;
            validate_token(field_path, "field_path")?;
            Ok(())
        }
        SceneInteractionIntent::AddComponent { schema, .. } => {
            validate_token(schema, "schema")?;
            Ok(())
        }
        SceneInteractionIntent::EmitSignal { name, .. } => {
            validate_token(name, "signal name")?;
            Ok(())
        }
    }
}

fn validate_token(value: &str, label: &str) -> Result<(), String> {
    if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
        return Err(format!(
            "{label} must contain 1..=256 bytes and no controls"
        ));
    }
    Ok(())
}
