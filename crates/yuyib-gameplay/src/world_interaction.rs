//! High-level, renderer-neutral world interaction lifecycle.
//!
//! A 2D pointer query, 3D raycast, trigger system, script, or authoritative
//! server selects an optional [`WorldInteractionTarget`]. This module only
//! turns that selection and a semantic active/inactive action sample into a
//! deterministic focus and interaction event stream. It does not read keys,
//! perform physics queries, mutate the ECS world, or accept a command on behalf
//! of game authority.

use std::{error::Error, fmt, slice, time::Duration, vec};

use yuyib_ecs::prelude::Entity;

use crate::{InteractionId, InteractionMethod, InteractionRequested};

/// Maximum number of events produced by one [`WorldInteractionState::step`].
///
/// The bound covers exit + enter + progress + interaction when a target and
/// action change during one simulation step.
pub const MAX_WORLD_INTERACTION_EVENTS_PER_STEP: usize = 4;

/// Activation policy attached to one world interaction target.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorldInteractionActivation(ActivationKind);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ActivationKind {
    Press,
    Hold(Duration),
}

impl WorldInteractionActivation {
    /// Activates once on a fresh semantic action press.
    #[must_use]
    pub const fn press() -> Self {
        Self(ActivationKind::Press)
    }

    /// Activates after a continuous hold of `duration`.
    ///
    /// # Errors
    ///
    /// Returns [`WorldInteractionConfigError::ZeroHoldDuration`] for a zero
    /// duration. Use [`Self::press`] for immediate activation.
    pub fn hold(duration: Duration) -> Result<Self, WorldInteractionConfigError> {
        if duration.is_zero() {
            return Err(WorldInteractionConfigError::ZeroHoldDuration);
        }
        Ok(Self(ActivationKind::Hold(duration)))
    }

    /// Returns the required hold duration, or `None` for press activation.
    #[must_use]
    pub const fn hold_duration(self) -> Option<Duration> {
        match self.0 {
            ActivationKind::Press => None,
            ActivationKind::Hold(duration) => Some(duration),
        }
    }
}

impl Default for WorldInteractionActivation {
    fn default() -> Self {
        Self::press()
    }
}

/// Invalid high-level interaction configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorldInteractionConfigError {
    /// Hold activation cannot use a zero duration.
    ZeroHoldDuration,
}

impl fmt::Display for WorldInteractionConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroHoldDuration => formatter.write_str(
                "world interaction hold duration must be non-zero; use press activation instead",
            ),
        }
    }
}

impl Error for WorldInteractionConfigError {}

/// A query-selected, strongly typed interaction target.
///
/// `Id` is application-defined and can be an enum, newtype, or the standard
/// [`InteractionId`]. ECS [`Entity`] identity remains local runtime state;
/// applications should use a serializable `Id` for saves or networking.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorldInteractionTarget<Id = InteractionId> {
    entity: Entity,
    id: Id,
    activation: WorldInteractionActivation,
}

impl<Id> WorldInteractionTarget<Id> {
    /// Creates a target activated by a fresh semantic press.
    #[must_use]
    pub const fn new(entity: Entity, id: Id) -> Self {
        Self {
            entity,
            id,
            activation: WorldInteractionActivation::press(),
        }
    }

    /// Applies a validated activation policy.
    #[must_use]
    pub const fn with_activation(mut self, activation: WorldInteractionActivation) -> Self {
        self.activation = activation;
        self
    }

    /// Returns the local ECS entity selected by the spatial adapter.
    #[must_use]
    pub const fn entity(&self) -> Entity {
        self.entity
    }

    /// Returns the application-defined stable target identifier.
    #[must_use]
    pub const fn id(&self) -> &Id {
        &self.id
    }

    /// Returns press/hold activation policy for this target.
    #[must_use]
    pub const fn activation(&self) -> WorldInteractionActivation {
        self.activation
    }
}

impl WorldInteractionTarget<InteractionId> {
    /// Converts this selected target into the existing authority request type.
    #[must_use]
    pub fn interaction_request(
        &self,
        actor: Entity,
        method: InteractionMethod,
    ) -> InteractionRequested {
        InteractionRequested {
            actor,
            target: self.entity,
            interaction: self.id.clone(),
            method,
        }
    }
}

/// One high-level focus, progress, or activation event.
#[derive(Clone, Debug, PartialEq)]
pub enum WorldInteractionEvent<Id = InteractionId> {
    /// The target became the current focus/overlap candidate.
    Entered(WorldInteractionTarget<Id>),
    /// The same target remained selected for another simulation step.
    Stayed(WorldInteractionTarget<Id>),
    /// The target stopped being selected.
    Exited(WorldInteractionTarget<Id>),
    /// A hold interaction advanced but has not necessarily completed.
    Progress {
        /// Target whose action is being held.
        target: WorldInteractionTarget<Id>,
        /// Accumulated continuous hold time, clamped to `required`.
        elapsed: Duration,
        /// Hold duration required by the target.
        required: Duration,
        /// Normalized progress in the inclusive `[0.0, 1.0]` range.
        fraction: f32,
    },
    /// A fresh press or completed hold produced an authority request candidate.
    Interacted(WorldInteractionTarget<Id>),
}

impl WorldInteractionEvent<InteractionId> {
    /// Returns an authority request only for [`Self::Interacted`].
    #[must_use]
    pub fn interaction_request(
        &self,
        actor: Entity,
        method: InteractionMethod,
    ) -> Option<InteractionRequested> {
        match self {
            Self::Interacted(target) => Some(target.interaction_request(actor, method)),
            Self::Entered(_) | Self::Stayed(_) | Self::Exited(_) | Self::Progress { .. } => None,
        }
    }
}

/// Bounded events returned by one interaction state-machine step.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WorldInteractionEvents<Id> {
    events: Vec<WorldInteractionEvent<Id>>,
}

impl<Id> WorldInteractionEvents<Id> {
    /// Returns the events in deterministic emission order.
    #[must_use]
    pub fn as_slice(&self) -> &[WorldInteractionEvent<Id>] {
        &self.events
    }

    /// Returns whether no lifecycle event was emitted.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// Returns the number of events, never greater than
    /// [`MAX_WORLD_INTERACTION_EVENTS_PER_STEP`].
    #[must_use]
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// Iterates over events without consuming the batch.
    pub fn iter(&self) -> slice::Iter<'_, WorldInteractionEvent<Id>> {
        self.events.iter()
    }
}

impl<Id> IntoIterator for WorldInteractionEvents<Id> {
    type Item = WorldInteractionEvent<Id>;
    type IntoIter = vec::IntoIter<Self::Item>;

    fn into_iter(self) -> Self::IntoIter {
        self.events.into_iter()
    }
}

impl<'a, Id> IntoIterator for &'a WorldInteractionEvents<Id> {
    type Item = &'a WorldInteractionEvent<Id>;
    type IntoIter = slice::Iter<'a, WorldInteractionEvent<Id>>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// Input-agnostic state machine shared by 2D, 3D, UI, and headless gameplay.
///
/// Call [`Self::step`] once per deterministic simulation step with the target
/// already selected by a caller-owned spatial adapter and the current semantic
/// action state. Changing target while the action is already held never carries
/// progress or activates the new target; the user must release and press again.
#[derive(Clone, Debug)]
pub struct WorldInteractionState<Id = InteractionId> {
    current: Option<WorldInteractionTarget<Id>>,
    action_was_active: bool,
    holding: bool,
    hold_elapsed: Duration,
    completed: bool,
}

impl<Id> Default for WorldInteractionState<Id> {
    fn default() -> Self {
        Self {
            current: None,
            action_was_active: false,
            holding: false,
            hold_elapsed: Duration::ZERO,
            completed: false,
        }
    }
}

impl<Id> WorldInteractionState<Id>
where
    Id: Clone + Eq,
{
    /// Returns the currently selected target.
    #[must_use]
    pub const fn current(&self) -> Option<&WorldInteractionTarget<Id>> {
        self.current.as_ref()
    }

    /// Advances focus and action state by one deterministic simulation step.
    ///
    /// `delta` should come from a fixed schedule for deterministic hold timing.
    /// The returned batch is ordered as exit, enter/stay, progress, interact and
    /// contains at most [`MAX_WORLD_INTERACTION_EVENTS_PER_STEP`] events.
    #[must_use]
    pub fn step(
        &mut self,
        target: Option<WorldInteractionTarget<Id>>,
        action_active: bool,
        delta: Duration,
    ) -> WorldInteractionEvents<Id> {
        let mut events = Vec::with_capacity(MAX_WORLD_INTERACTION_EVENTS_PER_STEP);
        let target_changed = self.current != target;

        if target_changed {
            if let Some(previous) = self.current.take() {
                events.push(WorldInteractionEvent::Exited(previous));
            }
            self.reset_activation();
            if let Some(next) = target.as_ref() {
                events.push(WorldInteractionEvent::Entered(next.clone()));
            }
            self.current = target;
        } else if let Some(current) = self.current.as_ref() {
            events.push(WorldInteractionEvent::Stayed(current.clone()));
        }

        let action_started = action_active && !self.action_was_active;
        if !action_active {
            self.reset_activation();
        } else if let Some(current) = self.current.as_ref() {
            match current.activation.0 {
                ActivationKind::Press if action_started => {
                    events.push(WorldInteractionEvent::Interacted(current.clone()));
                    self.completed = true;
                }
                ActivationKind::Hold(required) => {
                    if action_started {
                        self.holding = true;
                    }
                    if self.holding && !self.completed {
                        self.hold_elapsed = self.hold_elapsed.saturating_add(delta).min(required);
                        let fraction = self.hold_elapsed.as_secs_f32() / required.as_secs_f32();
                        events.push(WorldInteractionEvent::Progress {
                            target: current.clone(),
                            elapsed: self.hold_elapsed,
                            required,
                            fraction,
                        });
                        if self.hold_elapsed >= required {
                            events.push(WorldInteractionEvent::Interacted(current.clone()));
                            self.completed = true;
                            self.holding = false;
                        }
                    }
                }
                ActivationKind::Press => {}
            }
        }

        self.action_was_active = action_active;
        debug_assert!(events.len() <= MAX_WORLD_INTERACTION_EVENTS_PER_STEP);
        WorldInteractionEvents { events }
    }

    /// Clears focus and action history, for example after input focus loss.
    ///
    /// This deliberately emits no `Exited` event. Call [`Self::step`] with no
    /// target first when gameplay consumers must observe an exit transition.
    pub fn clear(&mut self) {
        self.current = None;
        self.action_was_active = false;
        self.reset_activation();
    }

    fn reset_activation(&mut self) {
        self.holding = false;
        self.hold_elapsed = Duration::ZERO;
        self.completed = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum TargetId {
        Door,
        Terminal,
    }

    fn target(entity: Entity, id: TargetId) -> WorldInteractionTarget<TargetId> {
        WorldInteractionTarget::new(entity, id)
    }

    #[test]
    fn focus_lifecycle_is_deterministic() {
        let first_entity = Entity::from_bits(1);
        let second_entity = Entity::from_bits(2);
        let first = target(first_entity, TargetId::Door);
        let second = target(second_entity, TargetId::Terminal);
        let mut state = WorldInteractionState::default();

        assert!(matches!(
            state.step(Some(first.clone()), false, Duration::ZERO).as_slice(),
            [WorldInteractionEvent::Entered(observed)] if observed == &first
        ));
        assert!(matches!(
            state.step(Some(first.clone()), false, Duration::ZERO).as_slice(),
            [WorldInteractionEvent::Stayed(observed)] if observed == &first
        ));
        assert!(matches!(
            state.step(Some(second.clone()), false, Duration::ZERO).as_slice(),
            [WorldInteractionEvent::Exited(exited), WorldInteractionEvent::Entered(entered)]
                if exited == &first && entered == &second
        ));
        assert!(matches!(
            state.step(None, false, Duration::ZERO).as_slice(),
            [WorldInteractionEvent::Exited(observed)] if observed == &second
        ));
    }

    #[test]
    fn hold_progress_completes_once_and_resets_after_release() {
        let selected = target(Entity::from_bits(7), TargetId::Terminal).with_activation(
            WorldInteractionActivation::hold(Duration::from_secs(1)).expect("valid hold"),
        );
        let mut state = WorldInteractionState::default();

        let first = state.step(Some(selected.clone()), true, Duration::from_millis(400));
        assert!(matches!(
            first.as_slice(),
            [WorldInteractionEvent::Entered(_), WorldInteractionEvent::Progress { fraction, .. }]
                if (*fraction - 0.4).abs() < f32::EPSILON
        ));
        let completed = state.step(Some(selected.clone()), true, Duration::from_millis(600));
        assert!(matches!(
            completed.as_slice(),
            [WorldInteractionEvent::Stayed(_), WorldInteractionEvent::Progress { fraction, .. }, WorldInteractionEvent::Interacted(_)]
                if (*fraction - 1.0).abs() < f32::EPSILON
        ));
        assert!(matches!(
            state
                .step(Some(selected.clone()), true, Duration::from_secs(1))
                .as_slice(),
            [WorldInteractionEvent::Stayed(_)]
        ));

        let _released = state.step(Some(selected.clone()), false, Duration::ZERO);
        let repeated = state.step(Some(selected), true, Duration::from_secs(1));
        assert!(matches!(
            repeated.as_slice(),
            [
                WorldInteractionEvent::Stayed(_),
                WorldInteractionEvent::Progress { .. },
                WorldInteractionEvent::Interacted(_)
            ]
        ));
    }

    #[test]
    fn held_action_does_not_transfer_to_a_new_target() {
        let first = target(Entity::from_bits(1), TargetId::Door);
        let second = target(Entity::from_bits(2), TargetId::Terminal).with_activation(
            WorldInteractionActivation::hold(Duration::from_millis(100)).expect("valid hold"),
        );
        let mut state = WorldInteractionState::default();

        let pressed = state.step(Some(first), true, Duration::ZERO);
        assert!(
            pressed
                .iter()
                .any(|event| matches!(event, WorldInteractionEvent::Interacted(_)))
        );
        let switched = state.step(Some(second.clone()), true, Duration::from_secs(1));
        assert!(!switched.iter().any(|event| matches!(
            event,
            WorldInteractionEvent::Progress { .. } | WorldInteractionEvent::Interacted(_)
        )));
        let _released = state.step(Some(second.clone()), false, Duration::ZERO);
        let pressed_again = state.step(Some(second), true, Duration::from_millis(100));
        assert!(
            pressed_again
                .iter()
                .any(|event| matches!(event, WorldInteractionEvent::Interacted(_)))
        );
    }

    #[test]
    fn standard_interaction_id_builds_existing_authority_request() {
        let actor = Entity::from_bits(3);
        let target = Entity::from_bits(4);
        let event = WorldInteractionEvent::Interacted(WorldInteractionTarget::new(
            target,
            InteractionId::new("world.open_door"),
        ));

        let request = event
            .interaction_request(actor, InteractionMethod::Proximity)
            .expect("interacted event creates a request");
        assert_eq!(request.actor, actor);
        assert_eq!(request.target, target);
        assert_eq!(request.interaction.as_str(), "world.open_door");
    }

    #[test]
    fn zero_hold_is_rejected() {
        assert_eq!(
            WorldInteractionActivation::hold(Duration::ZERO),
            Err(WorldInteractionConfigError::ZeroHoldDuration)
        );
    }
}
