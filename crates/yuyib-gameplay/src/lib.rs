//! Backend-neutral gameplay foundations for Yuyib.
//!
//! Models *semantic* input and interaction events, not
//! keyboard, mouse, touch, raycast, or physics APIs. An application maps such
//! low-level sources to [`ActionId`]s, then submits them to [`ActionStates`].
//! Gameplay systems can consequently react to the same `game.use` action on
//! Windows keyboard, touch devices, gamepads, accessibility tools, or a
//! network-authoritative server.
//!
//! # Example
//!
//! ```
//! use yuyib_gameplay::{ActionId, ActionPhase, ActionStates, ActionValue};
//!
//! let use_action = ActionId::new("game.use");
//! let mut actions = ActionStates::default();
//!
//! let event = actions.submit(use_action.clone(), ActionValue::digital(true), 1);
//! assert_eq!(event.expect("press produces an event").phase, ActionPhase::Started);
//! assert!(actions.get(&use_action).expect("state was inserted").is_active());
//! ```
//!
//! A physics, renderer, or UI plugin is responsible for discovering targets.
//! It emits [`InteractionRequested`] and later [`InteractionResolved`] or
//! [`TriggerEvent`]. This keeps gameplay logic usable in 2D, 3D, headless, and
//! networked applications.

#![forbid(unsafe_code)]

/// Renderer-neutral 2D pointer/touch world-point interaction adapter.
pub mod interaction_2d;
/// Rendering-independent 3D semantic use-action raycast adapter.
pub mod interaction_3d;
/// Renderer-neutral quest definitions, progress and transitions.
pub mod quest;
/// Shared input-agnostic focus and hold-to-interact state machine for 2D/3D.
pub mod world_interaction;
/// Presentation hints (cursor / label / highlight) for a focused interaction.
pub mod interaction_prompt;
/// Renderer-neutral dialogue graphs, story flags, and choice sessions.
pub mod dialogue;

pub use world_interaction::{
    WorldInteractionActivation, WorldInteractionConfigError, WorldInteractionEvent,
    WorldInteractionEvents, WorldInteractionState, WorldInteractionTarget,
};
pub use interaction_prompt::{
    InteractionCursorHint, InteractionPrompt2d, InteractionPromptPresentation,
};
pub use dialogue::{
    DialogueChoice, DialogueChoiceId, DialogueCondition, DialogueDefinitionError, DialogueEffect,
    DialogueEvent, DialogueGraph, DialogueId, DialogueNode, DialogueNodeId, DialoguePresentation,
    DialoguePresentedChoice, DialogueSession, DialogueSessionError, StoryFlagId, StoryFlags,
};

pub use quest::{
    ObjectiveId, QuestBook, QuestBookError, QuestDefinition, QuestDefinitionError, QuestEventId,
    QuestId, QuestObjective, QuestProgress, QuestSignal, QuestSignalError, QuestSnapshot,
    QuestStatus, QuestTransition,
};

use std::{collections::BTreeMap, fmt};

use yuyib_ecs::bevy_ecs::prelude::{Component, Entity, Resource};

/// Stable semantic identifier for a player or application action.
///
/// Use reverse-domain or dotted names such as `"game.use"`, `"ui.confirm"`,
/// and `"editor.orbit_camera"`. The identifier intentionally contains no
/// physical binding information.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ActionId(Box<str>);

impl ActionId {
    /// Creates an action identifier.
    #[must_use]
    pub fn new(value: impl Into<Box<str>>) -> Self {
        Self(value.into())
    }

    /// Returns the canonical action name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for ActionId {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for ActionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl From<&str> for ActionId {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

impl From<String> for ActionId {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

/// A normalized scalar supplied by an action-binding backend.
///
/// Values are clamped to `[-1.0, 1.0]`; non-finite input becomes zero. A value
/// is active when its absolute value is at least [`Self::ACTIVE_THRESHOLD`].
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ActionValue(f32);

impl ActionValue {
    /// Smallest absolute value considered active.
    pub const ACTIVE_THRESHOLD: f32 = 0.000_1;

    /// Creates a normalized value, clamping it to `[-1.0, 1.0]`.
    #[must_use]
    pub fn new(value: f32) -> Self {
        Self(if value.is_finite() {
            value.clamp(-1.0, 1.0)
        } else {
            0.0
        })
    }

    /// Creates a digital pressed or released value.
    #[must_use]
    pub const fn digital(pressed: bool) -> Self {
        Self(if pressed { 1.0 } else { 0.0 })
    }

    /// Returns the normalized scalar value.
    #[must_use]
    pub const fn scalar(self) -> f32 {
        self.0
    }

    /// Reports whether this value is semantically active.
    #[must_use]
    pub fn is_active(self) -> bool {
        self.0.abs() >= Self::ACTIVE_THRESHOLD
    }
}

/// The semantic phase of an action.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActionPhase {
    /// The action became active this frame.
    Started,
    /// The action remained active this frame.
    Performed,
    /// The action became inactive this frame.
    Canceled,
}

/// A semantic action event emitted by [`ActionStates::submit`].
#[derive(Clone, Debug, PartialEq)]
pub struct ActionEvent {
    /// Action that changed.
    pub action: ActionId,
    /// Current lifecycle phase.
    pub phase: ActionPhase,
    /// Current normalized value.
    pub value: ActionValue,
    /// Monotonic frame supplied by the caller.
    pub frame: u64,
}

/// Current state of one semantic action.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ActionState {
    value: ActionValue,
    active: bool,
    changed_at_frame: u64,
}

impl ActionState {
    /// Returns the latest normalized value.
    #[must_use]
    pub const fn value(self) -> ActionValue {
        self.value
    }

    /// Reports whether the action is active.
    #[must_use]
    pub const fn is_active(self) -> bool {
        self.active
    }

    /// Returns the frame where the state last changed phase or value.
    #[must_use]
    pub const fn changed_at_frame(self) -> u64 {
        self.changed_at_frame
    }
}

/// Resource that stores semantic action state.
///
/// Call [`Self::submit`] once per action per frame that has an input sample.
/// Active samples emit [`ActionPhase::Performed`] after their initial
/// [`ActionPhase::Started`]; repeated inactive samples do not emit events.
/// Input adapters own physical-device binding, dead zones, and repeat policy.
#[derive(Debug, Default, Resource)]
pub struct ActionStates {
    states: BTreeMap<ActionId, ActionState>,
}

impl ActionStates {
    /// Submits the latest semantic value and returns its transition event.
    #[must_use]
    pub fn submit(
        &mut self,
        action: ActionId,
        value: ActionValue,
        frame: u64,
    ) -> Option<ActionEvent> {
        let state = self.states.entry(action.clone()).or_default();
        let was_active = state.active;
        let is_active = value.is_active();
        let value_changed = state.value != value;
        state.value = value;
        state.active = is_active;

        let phase = match (was_active, is_active) {
            (false, true) => Some(ActionPhase::Started),
            (true, true) => Some(ActionPhase::Performed),
            (true, false) => Some(ActionPhase::Canceled),
            (false, false) => None,
        };

        if phase.is_some() || value_changed {
            state.changed_at_frame = frame;
        }

        phase.map(|phase| ActionEvent {
            action,
            phase,
            value,
            frame,
        })
    }

    /// Returns state for an action that has received at least one sample.
    #[must_use]
    pub fn get(&self, action: &ActionId) -> Option<ActionState> {
        self.states.get(action).copied()
    }

    /// Clears all action state, for example after focus loss.
    ///
    /// This does not produce cancellation events. Backends which must notify
    /// gameplay should submit released values before clearing the resource.
    pub fn clear(&mut self) {
        self.states.clear();
    }
}

/// Stable semantic identifier for an interaction provided by an entity.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct InteractionId(Box<str>);

impl InteractionId {
    /// Creates an interaction identifier such as `"world.open_door"`.
    #[must_use]
    pub fn new(value: impl Into<Box<str>>) -> Self {
        Self(value.into())
    }

    /// Returns the canonical interaction name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for InteractionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl From<&str> for InteractionId {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

/// How an interaction candidate was discovered.
///
/// The associated physics, rendering, touch, and UI systems remain external.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InteractionMethod {
    /// A semantic action was used, normally after a raycast or focus query.
    Action(ActionId),
    /// Two world shapes overlapped or touched.
    Contact,
    /// A proximity/trigger query found the target.
    Proximity,
    /// A ray or shape cast found the target.
    Raycast,
    /// A pointer, UI cursor, or touch hit-test found the target.
    Pointer,
    /// A domain-specific method, such as a scripted or network operation.
    Custom(Box<str>),
}

/// Configures an entity as a possible world interaction target.
///
/// This component has no collision shape and does not perform range checks.
/// A query plugin resolves candidate entities and applies its distance, line of
/// sight, authority, or physics rules before emitting [`InteractionRequested`].
#[derive(Clone, Debug, Component, PartialEq)]
pub struct Interactable {
    /// Semantic capability provided by the entity.
    pub interaction: InteractionId,
    /// Whether the entity may currently receive interaction requests.
    pub enabled: bool,
    /// Optional semantic action required to activate the entity.
    pub required_action: Option<ActionId>,
    /// Optional maximum query distance in world units.
    pub max_distance: Option<f32>,
}

impl Interactable {
    /// Creates an enabled interaction with no action or range restriction.
    #[must_use]
    pub fn new(interaction: impl Into<InteractionId>) -> Self {
        Self {
            interaction: interaction.into(),
            enabled: true,
            required_action: None,
            max_distance: None,
        }
    }

    /// Requires a semantic action, such as `game.use`.
    #[must_use]
    pub fn requiring_action(mut self, action: impl Into<ActionId>) -> Self {
        self.required_action = Some(action.into());
        self
    }

    /// Sets a finite non-negative maximum distance.
    ///
    /// Returns [`InteractionConfigError`] for invalid distances instead of
    /// allowing NaN values to leak into a physics or visibility query.
    ///
    /// # Errors
    ///
    /// Returns [`InteractionConfigError::InvalidDistance`] when `distance` is
    /// negative, NaN, or infinite.
    pub fn with_max_distance(mut self, distance: f32) -> Result<Self, InteractionConfigError> {
        if !distance.is_finite() || distance < 0.0 {
            return Err(InteractionConfigError::InvalidDistance(distance));
        }
        self.max_distance = Some(distance);
        Ok(self)
    }
}

/// Configuration error for an [`Interactable`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum InteractionConfigError {
    /// Distance was negative, NaN, or infinite.
    InvalidDistance(f32),
}

impl fmt::Display for InteractionConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidDistance(distance) => {
                write!(
                    formatter,
                    "interaction distance must be finite and non-negative, got {distance}"
                )
            }
        }
    }
}

impl std::error::Error for InteractionConfigError {}

/// A command-like request to interact with an entity.
///
/// This is intentionally not proof that an interaction happened. A game,
/// server, or authority system validates it and emits [`InteractionResolved`].
#[derive(Clone, Debug, PartialEq)]
pub struct InteractionRequested {
    /// Entity requesting the interaction.
    pub actor: Entity,
    /// Candidate target entity.
    pub target: Entity,
    /// Capability requested from the target.
    pub interaction: InteractionId,
    /// How the candidate was discovered.
    pub method: InteractionMethod,
}

/// Result of validating an [`InteractionRequested`] command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InteractionOutcome {
    /// The authority accepted the interaction.
    Accepted,
    /// The target was disabled.
    Disabled,
    /// The action did not meet the target's required action.
    WrongAction,
    /// The candidate exceeded the configured range.
    OutOfRange,
    /// The candidate was blocked by line of sight, permissions, or game rules.
    Rejected,
}

/// A domain event emitted after an interaction request has been validated.
#[derive(Clone, Debug, PartialEq)]
pub struct InteractionResolved {
    /// Request that was evaluated.
    pub request: InteractionRequested,
    /// Authority decision.
    pub outcome: InteractionOutcome,
}

/// Stable semantic identifier for a trigger volume or condition.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TriggerId(Box<str>);

impl TriggerId {
    /// Creates a trigger identifier such as `"level.exit"`.
    #[must_use]
    pub fn new(value: impl Into<Box<str>>) -> Self {
        Self(value.into())
    }

    /// Returns the canonical trigger name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for TriggerId {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

/// Configuration for a passive trigger entity.
///
/// A trigger is metadata only. A physics, tilemap, navigation, or scripted
/// system decides overlap semantics and emits [`TriggerEvent`].
#[derive(Clone, Debug, Component, PartialEq)]
pub struct Trigger {
    /// Semantic trigger capability.
    pub trigger: TriggerId,
    /// Whether the trigger currently accepts events.
    pub enabled: bool,
}

impl Trigger {
    /// Creates an enabled trigger.
    #[must_use]
    pub fn new(trigger: impl Into<TriggerId>) -> Self {
        Self {
            trigger: trigger.into(),
            enabled: true,
        }
    }
}

/// Transition observed for a [`Trigger`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TriggerPhase {
    /// Another entity entered the trigger.
    Entered,
    /// Another entity remained in the trigger during this frame.
    Stayed,
    /// Another entity left the trigger.
    Exited,
}

/// Domain event describing a trigger transition.
#[derive(Clone, Debug, PartialEq)]
pub struct TriggerEvent {
    /// Entity carrying the [`Trigger`] component.
    pub trigger_entity: Entity,
    /// Semantic trigger identity read from the component.
    pub trigger: TriggerId,
    /// Entity which crossed the trigger boundary.
    pub other: Entity,
    /// Observed transition.
    pub phase: TriggerPhase,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_state_has_stable_lifecycle() {
        let action = ActionId::new("game.use");
        let mut states = ActionStates::default();

        assert_eq!(
            states
                .submit(action.clone(), ActionValue::digital(true), 3)
                .expect("press event")
                .phase,
            ActionPhase::Started
        );
        assert_eq!(
            states
                .submit(action.clone(), ActionValue::digital(true), 4)
                .expect("held event")
                .phase,
            ActionPhase::Performed
        );
        assert_eq!(
            states
                .submit(action.clone(), ActionValue::digital(false), 5)
                .expect("release event")
                .phase,
            ActionPhase::Canceled
        );
        assert!(
            states
                .submit(action, ActionValue::digital(false), 6)
                .is_none()
        );
    }

    #[test]
    fn action_values_are_safe_for_external_input() {
        assert!((ActionValue::new(3.5).scalar() - 1.0).abs() <= f32::EPSILON);
        assert!(ActionValue::new(f32::NAN).scalar().abs() <= f32::EPSILON);
        assert!(!ActionValue::new(0.000_01).is_active());
    }

    #[test]
    fn interactable_rejects_invalid_distance() {
        assert!(
            Interactable::new("world.open")
                .with_max_distance(-1.0)
                .is_err()
        );
        assert!(
            Interactable::new("world.open")
                .with_max_distance(f32::INFINITY)
                .is_err()
        );
        assert_eq!(
            Interactable::new("world.open")
                .with_max_distance(0.0)
                .expect("zero range is valid")
                .max_distance,
            Some(0.0)
        );
    }
}
