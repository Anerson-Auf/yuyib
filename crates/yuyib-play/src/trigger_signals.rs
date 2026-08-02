//! Map Rapier sensor overlaps onto Intent Bridge `trigger.*` signals.
//!
//! Play locomotion stays on CharacterController / triangle mesh. When a host
//! also owns [`yuyib_physics::RapierDynamicsWorld3d`], it can feed
//! [`collect_trigger_overlaps`](yuyib_physics::RapierDynamicsWorld3d::collect_trigger_overlaps)
//! through [`diff_trigger_overlaps`] without switching physics modes.

use std::collections::{HashMap, HashSet};

use serde_json::json;
use yuyib_physics::BodyId3d;
use yuyib_scene_interaction::{
    ParsedTriggerPhase, SIGNAL_TRIGGER_PREFIX, SceneInteractionIntent,
};

/// Tracks last-frame sensor pairs to emit Entered / Stayed / Exited.
#[derive(Clone, Debug, Default)]
pub struct TriggerOverlapTracker {
    previous: HashSet<(BodyId3d, BodyId3d)>,
}

impl TriggerOverlapTracker {
    /// Diffs current Rapier intersection pairs into Intent Bridge signals.
    ///
    /// `trigger_ids` maps sensor body → semantic trigger id (`level.exit`).
    /// Pairs are `(trigger_body, other_body)` as returned by Rapier collect.
    pub fn diff_to_intents(
        &mut self,
        current_pairs: &[(BodyId3d, BodyId3d)],
        trigger_ids: &HashMap<BodyId3d, String>,
    ) -> Vec<SceneInteractionIntent> {
        let current: HashSet<_> = current_pairs.iter().copied().collect();
        let mut intents = Vec::new();

        for pair in &current {
            let Some(trigger_id) = trigger_ids.get(&pair.0) else {
                continue;
            };
            let phase = if self.previous.contains(pair) {
                ParsedTriggerPhase::Stayed
            } else {
                ParsedTriggerPhase::Entered
            };
            intents.push(trigger_intent(trigger_id, phase));
        }
        for pair in &self.previous {
            if current.contains(pair) {
                continue;
            }
            let Some(trigger_id) = trigger_ids.get(&pair.0) else {
                continue;
            };
            intents.push(trigger_intent(trigger_id, ParsedTriggerPhase::Exited));
        }

        self.previous = current;
        intents
    }
}

fn trigger_intent(trigger_id: &str, phase: ParsedTriggerPhase) -> SceneInteractionIntent {
    SceneInteractionIntent::EmitSignal {
        name: format!("{SIGNAL_TRIGGER_PREFIX}{trigger_id}"),
        payload: json!({
            "trigger": trigger_id,
            "phase": phase.as_str(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use yuyib_scene_interaction::try_parse_trigger_signal;

    #[test]
    fn entered_then_exited() {
        let trigger = BodyId3d::from_raw_parts(1, 0);
        let other = BodyId3d::from_raw_parts(2, 0);
        let mut ids = HashMap::new();
        ids.insert(trigger, "level.exit".to_owned());
        let mut tracker = TriggerOverlapTracker::default();

        let first = tracker.diff_to_intents(&[(trigger, other)], &ids);
        assert_eq!(first.len(), 1);
        let parsed = try_parse_trigger_signal(
            match &first[0] {
                SceneInteractionIntent::EmitSignal { name, .. } => name,
                _ => panic!("expected signal"),
            },
            match &first[0] {
                SceneInteractionIntent::EmitSignal { payload, .. } => payload,
                _ => panic!("expected signal"),
            },
        )
        .expect("parse");
        assert_eq!(parsed.phase, ParsedTriggerPhase::Entered);

        let second = tracker.diff_to_intents(&[(trigger, other)], &ids);
        assert_eq!(second.len(), 1);
        assert!(matches!(
            try_parse_trigger_signal(
                match &second[0] {
                    SceneInteractionIntent::EmitSignal { name, .. } => name,
                    _ => panic!("expected signal"),
                },
                match &second[0] {
                    SceneInteractionIntent::EmitSignal { payload, .. } => payload,
                    _ => panic!("expected signal"),
                },
            )
            .map(|value| value.phase),
            Some(ParsedTriggerPhase::Stayed)
        ));

        let third = tracker.diff_to_intents(&[], &ids);
        assert_eq!(third.len(), 1);
        assert!(matches!(
            try_parse_trigger_signal(
                match &third[0] {
                    SceneInteractionIntent::EmitSignal { name, .. } => name,
                    _ => panic!("expected signal"),
                },
                match &third[0] {
                    SceneInteractionIntent::EmitSignal { payload, .. } => payload,
                    _ => panic!("expected signal"),
                },
            )
            .map(|value| value.phase),
            Some(ParsedTriggerPhase::Exited)
        ));
    }
}
