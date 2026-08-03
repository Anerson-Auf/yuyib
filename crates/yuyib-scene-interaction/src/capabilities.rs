//! What a host adapter claims to support (discoverability without dual SoT).

use crate::intent::{
    KNOWN_3D_SCHEMAS, SCHEMA_DIRECTIONAL_LIGHT_3D, SCHEMA_LOCAL_TRANSFORM_3D, SCHEMA_MODEL_3D,
    SCHEMA_PARENT_3D, SCHEMA_TRANSFORM_3D, SceneInteractionIntent,
};

/// Where the bridge is running.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BridgeContext {
    /// Authored document mutations (undoable commands).
    Editor,
    /// Materialized runtime World (process-local handles).
    Play,
}

/// Kind of [`SceneInteractionIntent`] for capability queries.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IntentKind {
    /// [`SceneInteractionIntent::SetTranslation`].
    SetTranslation,
    /// [`SceneInteractionIntent::SetComponentField`].
    SetComponentField,
    /// [`SceneInteractionIntent::AddComponent`].
    AddComponent,
    /// [`SceneInteractionIntent::EmitSignal`].
    EmitSignal,
}

/// Static capability sheet for one adapter.
///
/// Scripts and tooling can ask before applying; unsupported ops fail loudly
/// rather than silently no-op.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BridgeCapabilities {
    /// Host context.
    pub context: BridgeContext,
    /// `SetTranslation` supported.
    pub set_translation: bool,
    /// Schemas accepted by `SetComponentField` (empty = none).
    pub set_component_field_schemas: &'static [&'static str],
    /// Schemas accepted by `AddComponent` with defaults / payload (empty = none).
    pub add_component_schemas: &'static [&'static str],
    /// `EmitSignal` queued for the host.
    pub emit_signal: bool,
}

impl BridgeCapabilities {
    /// Returns whether this capability sheet covers `intent`.
    #[must_use]
    pub fn supports(&self, intent: &SceneInteractionIntent) -> bool {
        match intent {
            SceneInteractionIntent::SetTranslation { .. } => self.set_translation,
            SceneInteractionIntent::SetComponentField { schema, .. } => self
                .set_component_field_schemas
                .iter()
                .any(|candidate| *candidate == schema.as_str()),
            SceneInteractionIntent::AddComponent { schema, .. } => self
                .add_component_schemas
                .iter()
                .any(|candidate| *candidate == schema.as_str()),
            SceneInteractionIntent::EmitSignal { .. } => self.emit_signal,
        }
    }

    /// Human-readable rejection when [`Self::supports`] is false.
    #[must_use]
    pub fn unsupported_message(&self, intent: &SceneInteractionIntent) -> String {
        match intent {
            SceneInteractionIntent::SetTranslation { .. } => {
                format!("{:?} bridge does not support SetTranslation", self.context)
            }
            SceneInteractionIntent::SetComponentField { schema, .. } => format!(
                "{:?} bridge does not support SetComponentField for `{schema}` (allowed: {:?})",
                self.context, self.set_component_field_schemas
            ),
            SceneInteractionIntent::AddComponent { schema, .. } => format!(
                "{:?} bridge does not support AddComponent for `{schema}` (allowed: {:?})",
                self.context, self.add_component_schemas
            ),
            SceneInteractionIntent::EmitSignal { .. } => {
                format!("{:?} bridge does not support EmitSignal", self.context)
            }
        }
    }
}

/// Editor adapter capabilities (parity with Inspector / projection known 3D set).
#[must_use]
pub fn editor_capabilities() -> BridgeCapabilities {
    BridgeCapabilities {
        context: BridgeContext::Editor,
        set_translation: true,
        set_component_field_schemas: KNOWN_3D_SCHEMAS,
        add_component_schemas: KNOWN_3D_SCHEMAS,
        emit_signal: true,
    }
}

/// Play adapter capabilities (TRS + model/light fields + signals).
///
/// `AddComponent` covers transform / local-transform / directional-light /
/// model3d (proxy handle) / parent3d (GUID → Entity resolve). Rapier trigger
/// volumes and shadow cascades are not Intent ops.
#[must_use]
pub fn play_capabilities() -> BridgeCapabilities {
    BridgeCapabilities {
        context: BridgeContext::Play,
        set_translation: true,
        set_component_field_schemas: &[
            SCHEMA_TRANSFORM_3D,
            SCHEMA_LOCAL_TRANSFORM_3D,
            SCHEMA_MODEL_3D,
            SCHEMA_DIRECTIONAL_LIGHT_3D,
        ],
        add_component_schemas: &[
            SCHEMA_TRANSFORM_3D,
            SCHEMA_LOCAL_TRANSFORM_3D,
            SCHEMA_DIRECTIONAL_LIGHT_3D,
            SCHEMA_MODEL_3D,
            SCHEMA_PARENT_3D,
        ],
        emit_signal: true,
    }
}
