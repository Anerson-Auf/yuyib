use std::{
    collections::{BTreeMap, HashSet},
    error::Error,
    fmt,
};

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;

use crate::{ComponentSchemaId, EntityGuid, SceneGuid, SchemaVersion};

/// Stable discriminator for Yuyib authored scene JSON.
pub const SCENE_FORMAT: &str = "yuyib.scene";
/// Current scene-container version understood for authored mutation.
pub const SCENE_FORMAT_VERSION: u32 = 1;

/// One persisted component payload.
///
/// Payloads remain generic JSON until a registered adapter elects to decode
/// them. Consequently, an older editor can load and save unknown component
/// schemas without discarding their data.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ComponentRecord {
    schema: ComponentSchemaId,
    version: SchemaVersion,
    payload: Value,
    /// Component-envelope data unknown to this editor version.
    #[serde(flatten)]
    extensions: BTreeMap<String, Value>,
}

impl ComponentRecord {
    /// Creates an opaque component record.
    #[must_use]
    pub const fn new(schema: ComponentSchemaId, version: SchemaVersion, payload: Value) -> Self {
        Self {
            schema,
            version,
            payload,
            extensions: BTreeMap::new(),
        }
    }

    /// Returns the stable component schema identifier.
    #[must_use]
    pub const fn schema(&self) -> &ComponentSchemaId {
        &self.schema
    }

    /// Returns the persisted schema version.
    #[must_use]
    pub const fn version(&self) -> SchemaVersion {
        self.version
    }

    /// Returns the untouched opaque JSON value.
    #[must_use]
    pub const fn payload(&self) -> &Value {
        &self.payload
    }

    /// Returns unknown component-envelope fields preserved for forward compatibility.
    #[must_use]
    pub const fn extensions(&self) -> &BTreeMap<String, Value> {
        &self.extensions
    }

    /// Decodes the payload for a known authoring adapter without modifying it.
    ///
    /// # Errors
    ///
    /// Returns the underlying JSON type error when the payload does not match
    /// `T`.
    pub fn decode<T: DeserializeOwned>(&self) -> Result<T, serde_json::Error> {
        serde_json::from_value(self.payload.clone())
    }

    /// Explicitly replaces payload and version after a known edit or migration.
    pub fn replace_payload(&mut self, version: SchemaVersion, payload: Value) {
        self.version = version;
        self.payload = payload;
    }
}

/// One authored entity and all of its persisted components.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SceneEntityRecord {
    /// Persistent entity identity.
    pub guid: EntityGuid,
    /// Optional human-facing label, not identity.
    pub name: Option<String>,
    /// Known and unknown component records.
    pub components: Vec<ComponentRecord>,
    /// Entity-envelope data unknown to this editor version.
    #[serde(flatten)]
    pub extensions: BTreeMap<String, Value>,
}

/// Versioned, editor-neutral authored scene document.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SceneDocument {
    /// Stable container discriminator; must equal the scene format constant.
    pub format: String,
    /// Version of the scene container itself.
    pub format_version: SchemaVersion,
    /// Persistent scene identity, independent from path and contents.
    pub scene_guid: SceneGuid,
    /// Authored entities in author-controlled order.
    pub entities: Vec<SceneEntityRecord>,
    /// Scene-envelope data unknown to this editor version.
    #[serde(flatten)]
    pub extensions: BTreeMap<String, Value>,
}

impl SceneDocument {
    /// Creates an empty scene using the current container discriminator.
    #[must_use]
    pub fn new(format_version: SchemaVersion) -> Self {
        Self {
            format: SCENE_FORMAT.to_owned(),
            format_version,
            scene_guid: SceneGuid::new(),
            entities: Vec::new(),
            extensions: BTreeMap::new(),
        }
    }

    /// Parses and structurally validates a JSON scene.
    ///
    /// # Errors
    ///
    /// Rejects malformed JSON, duplicate entity GUIDs, and duplicate component
    /// schema IDs on one entity.
    pub fn from_json(json: &str) -> Result<Self, SceneFormatError> {
        let scene: Self = serde_json::from_str(json).map_err(SceneFormatError::Json)?;
        scene.validate()?;
        Ok(scene)
    }

    /// Serializes a structurally valid scene.
    ///
    /// # Errors
    ///
    /// Rejects duplicate identities or a JSON serialization failure.
    pub fn to_json(&self) -> Result<String, SceneFormatError> {
        self.validate()?;
        serde_json::to_string_pretty(self).map_err(SceneFormatError::Json)
    }

    /// Validates identities required for deterministic editing.
    ///
    /// # Errors
    ///
    /// Rejects duplicate entity GUIDs or duplicate component schema IDs on one
    /// entity.
    pub fn validate(&self) -> Result<(), SceneFormatError> {
        if self.format != SCENE_FORMAT {
            return Err(SceneFormatError::UnsupportedFormat(self.format.clone()));
        }
        let mut entities = HashSet::new();
        for entity in &self.entities {
            if !entities.insert(entity.guid) {
                return Err(SceneFormatError::DuplicateEntity(entity.guid));
            }
            let mut schemas = HashSet::new();
            for component in &entity.components {
                if !schemas.insert(component.schema().clone()) {
                    return Err(SceneFormatError::DuplicateComponent {
                        entity: entity.guid,
                        schema: component.schema().clone(),
                    });
                }
            }
        }
        Ok(())
    }
}

/// A scene container failed parsing or structural validation.
#[derive(Debug)]
pub enum SceneFormatError {
    /// JSON syntax or data type mismatch.
    Json(serde_json::Error),
    /// Container discriminator is not a Yuyib scene.
    UnsupportedFormat(String),
    /// The same persistent entity identity occurred twice.
    DuplicateEntity(EntityGuid),
    /// One entity contained the same component schema more than once.
    DuplicateComponent {
        /// Entity containing the duplicate.
        entity: EntityGuid,
        /// Repeated stable component schema.
        schema: ComponentSchemaId,
    },
}

impl fmt::Display for SceneFormatError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json(error) => write!(formatter, "invalid scene JSON: {error}"),
            Self::UnsupportedFormat(format) => {
                write!(formatter, "unsupported scene format {format:?}")
            }
            Self::DuplicateEntity(entity) => write!(formatter, "duplicate entity GUID {entity}"),
            Self::DuplicateComponent { entity, schema } => {
                write!(
                    formatter,
                    "entity {entity} contains duplicate component {schema}"
                )
            }
        }
    }
}

impl Error for SceneFormatError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Json(error) => Some(error),
            Self::UnsupportedFormat(_)
            | Self::DuplicateEntity(_)
            | Self::DuplicateComponent { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn unknown_component_payload_survives_a_semantic_round_trip() {
        let entity = EntityGuid::new();
        let scene = SceneGuid::new();
        let input = format!(
            r#"{{
                "format": "yuyib.scene",
                "format_version": 1,
                "scene_guid": "{scene}",
                "future_scene": {{"keep": true}},
                "entities": [{{
                    "guid": "{entity}",
                    "name": "unknown owner",
                    "future_entity": [1, 2, 3],
                    "components": [{{
                        "schema": "third-party.quantum-widget",
                        "version": 7,
                        "future_component": "opaque-envelope",
                        "payload": {{"nested": [3, true, null], "future": {{"x": 1.25}}}}
                    }}]
                }}]
            }}"#
        );
        let first = SceneDocument::from_json(&input).expect("load unknown component");
        let output = first.to_json().expect("save unknown component");
        let second = SceneDocument::from_json(&output).expect("reload unknown component");

        assert_eq!(first, second);
        assert_eq!(
            second.entities[0].components[0].payload(),
            &json!({"nested": [3, true, null], "future": {"x": 1.25}})
        );
        assert_eq!(second.extensions["future_scene"], json!({"keep": true}));
        assert_eq!(
            second.entities[0].extensions["future_entity"],
            json!([1, 2, 3])
        );
        assert_eq!(
            second.entities[0].components[0].extensions()["future_component"],
            "opaque-envelope"
        );
        assert!(!output.contains("source"));
    }

    #[test]
    fn duplicate_component_schema_is_rejected() {
        let schema = ComponentSchemaId::new("yuyib.transform3d").expect("id");
        let record =
            ComponentRecord::new(schema, SchemaVersion::new(1).expect("version"), json!({}));
        let scene = SceneDocument {
            format: SCENE_FORMAT.to_owned(),
            format_version: SchemaVersion::new(1).expect("version"),
            scene_guid: SceneGuid::new(),
            entities: vec![SceneEntityRecord {
                guid: EntityGuid::new(),
                name: None,
                components: vec![record.clone(), record],
                extensions: BTreeMap::new(),
            }],
            extensions: BTreeMap::new(),
        };
        assert!(matches!(
            scene.validate(),
            Err(SceneFormatError::DuplicateComponent { .. })
        ));
    }
}
