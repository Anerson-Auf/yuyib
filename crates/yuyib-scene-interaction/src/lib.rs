//! Intent bridge: full Rust scripts talk to scene objects by stable GUID.
//!
//! Editor and Play share one [`SceneInteractionIntent`] surface. Adapters map
//! intents onto command transactions (Editor) or ECS mutations (Play). Persist
//! from Play back into `.yscene` stays explicit (Apply whitelist) — this crate
//! never writes documents itself.
//!
//! Distinct from gameplay player-interactable commands (`InteractionRequested` in
//! `yuyib-gameplay`): those are world interaction; this bridge is
//! **script → object** mutation / signaling.
//!
//! # Foundation layout
//!
//! - [`intent`] — operations scripts may request
//! - [`capabilities`] — what a host adapter claims to support
//! - [`signal`] — opaque bus payloads + optional quest-progress parse helpers
//! - [`bridge`] — [`SceneInteractionBridge`] + [`SceneInteractionBatchResult`]

#![forbid(unsafe_code)]

mod bridge;
mod capabilities;
mod intent;
mod signal;

pub use bridge::{SceneInteractionBatchResult, SceneInteractionBridge};
pub use capabilities::{
    BridgeCapabilities, BridgeContext, IntentKind, editor_capabilities, play_capabilities,
};
pub use intent::{
    SCHEMA_DIRECTIONAL_LIGHT_3D, SCHEMA_LOCAL_TRANSFORM_3D, SCHEMA_MODEL_3D, SCHEMA_PARENT_3D,
    SCHEMA_TRANSFORM_3D, SceneInteractionIntent, TransformSpace, KNOWN_3D_SCHEMAS,
    translation_field_writes, translation_schemas, validate_intent,
};
pub use signal::{
    ParsedQuestProgressSignal, ParsedTriggerPhase, ParsedTriggerSignal, SceneInteractionSignal,
    SIGNAL_QUEST_PREFIX, SIGNAL_TRIGGER_PREFIX, try_parse_quest_progress_signal,
    try_parse_trigger_signal,
};

#[cfg(test)]
mod tests {
    use super::*;
    use yuyib_authoring::EntityGuid;

    #[test]
    fn translation_fields_match_inspector_paths() {
        let writes = translation_field_writes([1.0, 2.0, 3.0]);
        assert_eq!(writes[0].0, "translation.x");
        assert_eq!(writes[0].1, serde_json::Value::from(1.0));
        assert_eq!(writes[2].1, serde_json::Value::from(3.0));
    }

    #[test]
    fn intent_round_trips_json() {
        let entity = EntityGuid::new();
        let intent = SceneInteractionIntent::SetTranslation {
            entity,
            translation: [4.0, 5.0, 6.0],
            space: TransformSpace::Local,
        };
        let text = serde_json::to_string(&intent).expect("serialize");
        let parsed: SceneInteractionIntent = serde_json::from_str(&text).expect("parse");
        assert_eq!(parsed, intent);
    }

    #[test]
    fn reject_empty_signal_name() {
        let err = validate_intent(&SceneInteractionIntent::EmitSignal {
            name: String::new(),
            payload: serde_json::Value::Null,
        });
        assert!(err.is_err());
    }

    #[test]
    fn editor_capabilities_cover_known_3d() {
        let caps = editor_capabilities();
        assert!(caps.set_translation);
        assert!(caps.emit_signal);
        assert!(caps.supports(&SceneInteractionIntent::AddComponent {
            entity: EntityGuid::new(),
            schema: SCHEMA_MODEL_3D.to_owned(),
            version: None,
            payload: None,
        }));
        assert!(play_capabilities().supports(
            &SceneInteractionIntent::AddComponent {
                entity: EntityGuid::new(),
                schema: SCHEMA_TRANSFORM_3D.to_owned(),
                version: None,
                payload: None,
            }
        ));
        assert!(!play_capabilities().supports(
            &SceneInteractionIntent::AddComponent {
                entity: EntityGuid::new(),
                schema: SCHEMA_MODEL_3D.to_owned(),
                version: None,
                payload: None,
            }
        ));
    }
}
