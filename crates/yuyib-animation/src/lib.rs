//! Named animation clip sets and an opt-in state machine.
//!
//! This crate is renderer- and dimension-agnostic: clip payloads are caller-
//! defined (`SpriteAnimation`, glTF clip indices, asset handles, …). High-level
//! 2D/3D facades own playback sync; this module owns naming, `play("walk")`,
//! and finished → next-state transitions.

#![forbid(unsafe_code)]

use std::{collections::HashMap, error::Error, fmt};

/// Named collection of animation clips.
///
/// Clip type `C` is opaque to this crate so the same set shape works for 2D
/// sprites and later 3D skeletal players.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AnimationSet<C> {
    clips: HashMap<String, C>,
}

impl<C> AnimationSet<C> {
    /// Creates an empty set.
    #[must_use]
    pub fn new() -> Self {
        Self {
            clips: HashMap::new(),
        }
    }

    /// Inserts or replaces a named clip. Returns the previous clip if any.
    pub fn insert(&mut self, name: impl Into<String>, clip: C) -> Option<C> {
        self.clips.insert(name.into(), clip)
    }

    /// Builder-style insert that keeps ownership of `self`.
    #[must_use]
    pub fn with(mut self, name: impl Into<String>, clip: C) -> Self {
        self.insert(name, clip);
        self
    }

    /// Returns a clip by name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&C> {
        self.clips.get(name)
    }

    /// Returns whether `name` is registered.
    #[must_use]
    pub fn contains(&self, name: &str) -> bool {
        self.clips.contains_key(name)
    }

    /// Number of registered clips.
    #[must_use]
    pub fn len(&self) -> usize {
        self.clips.len()
    }

    /// Returns true when no clips are registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.clips.is_empty()
    }

    /// Iterates `(name, clip)` pairs in arbitrary hash order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &C)> {
        self.clips.iter().map(|(name, clip)| (name.as_str(), clip))
    }
}

/// Authored definition of one animation state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnimationStateDef {
    /// Clip key looked up in an [`AnimationSet`]. Often equal to the state name.
    pub clip: String,
    /// When a once-clip finishes, auto-transition to this state (if present).
    pub on_finished: Option<String>,
}

impl AnimationStateDef {
    /// State that plays `clip` with no auto-transition.
    #[must_use]
    pub fn clip(clip: impl Into<String>) -> Self {
        Self {
            clip: clip.into(),
            on_finished: None,
        }
    }

    /// Sets the finished auto-transition target.
    #[must_use]
    pub fn on_finished(mut self, next: impl Into<String>) -> Self {
        self.on_finished = Some(next.into());
        self
    }
}

/// Outcome of [`AnimationStateMachine::play`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlayOutcome {
    /// Requested state was already current; no clip change.
    Unchanged,
    /// Active state switched; consumers should (re)bind the clip.
    Changed,
}

/// Opt-in animation state machine driven by explicit [`Self::play`] calls.
///
/// States map to clip names. This is a high-level control surface, not a full
/// blend tree: no parameters graph, no crossfade weights. Those stay end-game.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnimationStateMachine {
    states: HashMap<String, AnimationStateDef>,
    current: String,
}

impl AnimationStateMachine {
    /// Creates a machine with a single initial state that plays a clip of the
    /// same name.
    ///
    /// # Errors
    ///
    /// Returns [`AnimationError::EmptyName`] when `initial` is empty.
    pub fn new(initial: impl Into<String>) -> Result<Self, AnimationError> {
        let initial = initial.into();
        validate_name(&initial)?;
        let mut states = HashMap::new();
        states.insert(initial.clone(), AnimationStateDef::clip(initial.clone()));
        Ok(Self {
            states,
            current: initial,
        })
    }

    /// Registers or replaces a state definition.
    ///
    /// # Errors
    ///
    /// Returns [`AnimationError::EmptyName`] for an empty state or clip name, or
    /// [`AnimationError::EmptyName`] when `on_finished` is `Some("")`.
    pub fn insert(
        &mut self,
        name: impl Into<String>,
        def: AnimationStateDef,
    ) -> Result<&mut Self, AnimationError> {
        let name = name.into();
        validate_name(&name)?;
        validate_name(&def.clip)?;
        if let Some(next) = def.on_finished.as_deref() {
            validate_name(next)?;
        }
        self.states.insert(name, def);
        Ok(self)
    }

    /// Builder-style [`Self::insert`].
    ///
    /// # Errors
    ///
    /// Forwards [`Self::insert`] failures.
    pub fn with_state(
        mut self,
        name: impl Into<String>,
        def: AnimationStateDef,
    ) -> Result<Self, AnimationError> {
        self.insert(name, def)?;
        Ok(self)
    }

    /// Convenience: state name equals clip name, no auto-transition.
    ///
    /// # Errors
    ///
    /// Forwards [`Self::insert`] failures.
    pub fn with_clip(self, name: impl Into<String>) -> Result<Self, AnimationError> {
        let name = name.into();
        self.with_state(name.clone(), AnimationStateDef::clip(name))
    }

    /// Requests a state transition by name (`play("walk")`).
    ///
    /// # Errors
    ///
    /// Returns [`AnimationError::UnknownState`] when `name` is not registered.
    pub fn play(&mut self, name: &str) -> Result<PlayOutcome, AnimationError> {
        if !self.states.contains_key(name) {
            return Err(AnimationError::UnknownState(name.to_owned()));
        }
        if self.current == name {
            return Ok(PlayOutcome::Unchanged);
        }
        self.current = name.to_owned();
        Ok(PlayOutcome::Changed)
    }

    /// Forces the current state even if already active (restart semantics).
    ///
    /// # Errors
    ///
    /// Returns [`AnimationError::UnknownState`] when `name` is not registered.
    pub fn play_restart(&mut self, name: &str) -> Result<PlayOutcome, AnimationError> {
        if !self.states.contains_key(name) {
            return Err(AnimationError::UnknownState(name.to_owned()));
        }
        self.current = name.to_owned();
        Ok(PlayOutcome::Changed)
    }

    /// Returns the active state name.
    #[must_use]
    pub fn current_state(&self) -> &str {
        &self.current
    }

    /// Returns the clip key for the active state.
    ///
    /// # Panics
    ///
    /// Never panics for a well-formed machine created through this API: the
    /// current state is always present in `states`.
    #[must_use]
    pub fn current_clip(&self) -> &str {
        self.states
            .get(&self.current)
            .map(|def| def.clip.as_str())
            .expect("current state always registered")
    }

    /// Applies `on_finished` when a once-clip reports completion.
    ///
    /// Returns [`PlayOutcome::Changed`] when a transition happened.
    ///
    /// # Errors
    ///
    /// Returns [`AnimationError::UnknownState`] when `on_finished` targets a
    /// missing state.
    pub fn on_clip_finished(&mut self) -> Result<PlayOutcome, AnimationError> {
        let Some(next) = self
            .states
            .get(&self.current)
            .and_then(|def| def.on_finished.clone())
        else {
            return Ok(PlayOutcome::Unchanged);
        };
        self.play(&next)
    }

    /// Returns whether `name` is a registered state.
    #[must_use]
    pub fn contains_state(&self, name: &str) -> bool {
        self.states.contains_key(name)
    }
}

fn validate_name(name: &str) -> Result<(), AnimationError> {
    if name.is_empty() {
        Err(AnimationError::EmptyName)
    } else {
        Ok(())
    }
}

/// Failure while authoring or driving an animation set / state machine.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AnimationError {
    /// State or clip name was empty.
    EmptyName,
    /// [`AnimationStateMachine::play`] targeted an unknown state.
    UnknownState(String),
    /// Clip required by the active state is missing from the set.
    MissingClip(String),
}

impl fmt::Display for AnimationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyName => formatter.write_str("animation name must be non-empty"),
            Self::UnknownState(name) => write!(formatter, "unknown animation state '{name}'"),
            Self::MissingClip(name) => write!(formatter, "missing animation clip '{name}'"),
        }
    }
}

impl Error for AnimationError {}

#[cfg(test)]
mod tests {
    use super::{AnimationError, AnimationSet, AnimationStateDef, AnimationStateMachine, PlayOutcome};

    #[test]
    fn play_walk_changes_clip() {
        let mut machine = AnimationStateMachine::new("idle")
            .expect("initial")
            .with_clip("walk")
            .expect("walk");
        assert_eq!(machine.current_clip(), "idle");
        assert_eq!(machine.play("walk"), Ok(PlayOutcome::Changed));
        assert_eq!(machine.current_state(), "walk");
        assert_eq!(machine.play("walk"), Ok(PlayOutcome::Unchanged));
    }

    #[test]
    fn on_finished_transitions() {
        let mut machine = AnimationStateMachine::new("idle")
            .expect("initial")
            .with_state(
                "attack",
                AnimationStateDef::clip("attack").on_finished("idle"),
            )
            .expect("attack");
        assert_eq!(machine.play("attack"), Ok(PlayOutcome::Changed));
        assert_eq!(machine.on_clip_finished(), Ok(PlayOutcome::Changed));
        assert_eq!(machine.current_state(), "idle");
    }

    #[test]
    fn set_stores_typed_clips() {
        let set = AnimationSet::new().with("idle", 1_u32).with("walk", 2_u32);
        assert_eq!(set.get("walk"), Some(&2));
        assert!(set.contains("idle"));
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn unknown_play_errors() {
        let mut machine = AnimationStateMachine::new("idle").expect("initial");
        assert_eq!(
            machine.play("missing"),
            Err(AnimationError::UnknownState("missing".into()))
        );
    }
}
