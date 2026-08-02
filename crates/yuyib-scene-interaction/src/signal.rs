//! Opaque script signals and optional quest-progress decode (host maps to gameplay).

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Conventional prefix for quest-related [`crate::SceneInteractionIntent::EmitSignal`] names.
pub const SIGNAL_QUEST_PREFIX: &str = "quest.";
/// Conventional prefix for trigger-volume style signals.
pub const SIGNAL_TRIGGER_PREFIX: &str = "trigger.";

/// One drained signal from a bridge batch (host publishes / routes).
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SceneInteractionSignal {
    /// Signal name (`quest.step_completed`, `trigger.entered`, …).
    pub name: String,
    /// Opaque JSON payload.
    pub payload: Value,
}

impl SceneInteractionSignal {
    /// Builds a signal record.
    #[must_use]
    pub fn new(name: impl Into<String>, payload: Value) -> Self {
        Self {
            name: name.into(),
            payload,
        }
    }

    /// True when the name uses the quest convention prefix.
    #[must_use]
    pub fn is_quest_prefixed(&self) -> bool {
        self.name.starts_with(SIGNAL_QUEST_PREFIX)
    }

    /// True when the name uses the trigger convention prefix.
    #[must_use]
    pub fn is_trigger_prefixed(&self) -> bool {
        self.name.starts_with(SIGNAL_TRIGGER_PREFIX)
    }
}

/// Quest progress shape extracted from an interaction signal (no gameplay dependency).
///
/// Hosts map this onto `yuyib_gameplay::QuestSignal` (`event` + nonzero `amount`).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParsedQuestProgressSignal {
    /// Event id for `QuestSignal` (often the EmitSignal name, or `payload.event`).
    pub event: String,
    /// Positive progress units.
    pub amount: u32,
}

/// Tries to decode quest progress from an EmitSignal name/payload.
///
/// Accepted shapes:
/// - name = event id, payload `{ "amount": N }` with `N > 0`
/// - payload `{ "event": "…", "amount": N }` (name may be `quest.apply` or similar)
///
/// Returns `None` when the payload is not quest-progress shaped (still a valid
/// opaque signal for other hosts).
#[must_use]
pub fn try_parse_quest_progress_signal(
    name: &str,
    payload: &Value,
) -> Option<ParsedQuestProgressSignal> {
    let amount = payload.get("amount").and_then(Value::as_u64)?;
    if amount == 0 || amount > u64::from(u32::MAX) {
        return None;
    }
    let amount = amount as u32;
    let event = payload
        .get("event")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .filter(|event| !event.is_empty())
        .unwrap_or_else(|| name.to_owned());
    if event.is_empty() || event.chars().any(char::is_control) {
        return None;
    }
    Some(ParsedQuestProgressSignal { event, amount })
}

/// Trigger phase decoded from an interaction signal (maps onto gameplay `TriggerPhase`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ParsedTriggerPhase {
    /// Entered the volume / condition.
    Entered,
    /// Remained inside this frame.
    Stayed,
    /// Left the volume / condition.
    Exited,
}

impl ParsedTriggerPhase {
    /// Parses `entered` / `stayed` / `exited` (case-insensitive).
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "entered" | "enter" => Some(Self::Entered),
            "stayed" | "stay" => Some(Self::Stayed),
            "exited" | "exit" => Some(Self::Exited),
            _ => None,
        }
    }

    /// Stable lowercase wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Entered => "entered",
            Self::Stayed => "stayed",
            Self::Exited => "exited",
        }
    }
}

/// Trigger-shaped signal for hosts that wire Rapier overlaps → gameplay later.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParsedTriggerSignal {
    /// Semantic trigger id (`level.exit`, …).
    pub trigger_id: String,
    /// Observed phase.
    pub phase: ParsedTriggerPhase,
}

/// Tries to decode a trigger transition from EmitSignal.
///
/// Accepted shapes:
/// - name `trigger.<id>`, payload `{ "phase": "entered"|"stayed"|"exited" }`
/// - payload `{ "trigger": "…", "phase": "…" }` (name may be `trigger.apply`)
#[must_use]
pub fn try_parse_trigger_signal(name: &str, payload: &Value) -> Option<ParsedTriggerSignal> {
    let phase = payload
        .get("phase")
        .and_then(Value::as_str)
        .and_then(ParsedTriggerPhase::parse)?;
    let trigger_id = payload
        .get("trigger")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .filter(|id| !id.is_empty())
        .or_else(|| {
            name.strip_prefix(SIGNAL_TRIGGER_PREFIX)
                .filter(|id| !id.is_empty())
                .map(str::to_owned)
        })?;
    if trigger_id.chars().any(char::is_control) {
        return None;
    }
    Some(ParsedTriggerSignal { trigger_id, phase })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_amount_with_name_as_event() {
        let parsed =
            try_parse_quest_progress_signal("quest.intro.talked", &json!({ "amount": 1 })).unwrap();
        assert_eq!(parsed.event, "quest.intro.talked");
        assert_eq!(parsed.amount, 1);
    }

    #[test]
    fn parses_explicit_event_field() {
        let parsed = try_parse_quest_progress_signal(
            "quest.apply",
            &json!({ "event": "intro.talked", "amount": 2 }),
        )
        .unwrap();
        assert_eq!(parsed.event, "intro.talked");
        assert_eq!(parsed.amount, 2);
    }

    #[test]
    fn rejects_zero_amount() {
        assert!(try_parse_quest_progress_signal("quest.x", &json!({ "amount": 0 })).is_none());
    }

    #[test]
    fn parses_trigger_prefix_and_phase() {
        let parsed = try_parse_trigger_signal(
            "trigger.level.exit",
            &json!({ "phase": "entered" }),
        )
        .unwrap();
        assert_eq!(parsed.trigger_id, "level.exit");
        assert_eq!(parsed.phase, ParsedTriggerPhase::Entered);
    }
}
