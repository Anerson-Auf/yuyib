//! Presentation hints for a focused world interaction target.
//!
//! Converts [`WorldInteractionState`] focus into cursor / label / highlight
//! signals. The host projects `world_anchor` to screen and draws UI; this module
//! does not own Winit cursors or `ApplicationUi` trees.

use yuyib_ecs::prelude::Entity;

use crate::{WorldInteractionState, WorldInteractionTarget};

/// Suggested hardware / software cursor while a target is focused.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum InteractionCursorHint {
    /// No interaction affordance.
    #[default]
    Default,
    /// Generic use / activate.
    Use,
    /// Dialogue / talk.
    Talk,
    /// Inspect / look.
    Look,
}

/// One frame of interaction presentation for HUD / cursor / outline.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct InteractionPromptPresentation {
    /// Whether any target is currently focused.
    pub visible: bool,
    /// Suggested cursor while focused.
    pub cursor: InteractionCursorHint,
    /// Entity to outline / tint (if any).
    pub highlight_entity: Option<Entity>,
    /// Optional prompt text (e.g. `"Press E"`).
    pub label: Option<String>,
    /// World-space anchor supplied by the host for screen projection.
    pub world_anchor: Option<[f32; 2]>,
    /// Hold progress in `[0, 1]` when the host forwards it.
    pub hold_fraction: Option<f32>,
}

/// Thin high-level bridge: focus state → presentation for UI/cursor.
#[derive(Clone, Debug, PartialEq)]
pub struct InteractionPrompt2d {
    label: Option<String>,
    cursor_when_focused: InteractionCursorHint,
    presentation: InteractionPromptPresentation,
}

impl Default for InteractionPrompt2d {
    fn default() -> Self {
        Self::new()
    }
}

impl InteractionPrompt2d {
    /// Creates an empty prompt (invisible until [`Self::sync`]).
    #[must_use]
    pub fn new() -> Self {
        Self {
            label: None,
            cursor_when_focused: InteractionCursorHint::Use,
            presentation: InteractionPromptPresentation::default(),
        }
    }

    /// Sets the prompt label shown while a target is focused.
    #[must_use]
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Sets the cursor hint used while a target is focused.
    #[must_use]
    pub const fn with_cursor(mut self, cursor: InteractionCursorHint) -> Self {
        self.cursor_when_focused = cursor;
        self
    }

    /// Returns the latest presentation snapshot.
    #[must_use]
    pub const fn presentation(&self) -> &InteractionPromptPresentation {
        &self.presentation
    }

    /// Clears presentation (no focus).
    pub fn clear(&mut self) {
        self.presentation = InteractionPromptPresentation::default();
    }

    /// Rebuilds presentation from the current focus target.
    ///
    /// `world_anchor` and `hold_fraction` are host-owned (spatial query / last
    /// [`crate::WorldInteractionEvent::Progress`]).
    pub fn sync<Id>(
        &mut self,
        state: &WorldInteractionState<Id>,
        world_anchor: Option<[f32; 2]>,
        hold_fraction: Option<f32>,
    ) where
        Id: Clone + Eq,
    {
        match state.current() {
            Some(target) => self.apply_target(target, world_anchor, hold_fraction),
            None => self.clear(),
        }
    }

    /// Applies presentation for an explicit target (without owning the SM).
    pub fn apply_target<Id>(
        &mut self,
        target: &WorldInteractionTarget<Id>,
        world_anchor: Option<[f32; 2]>,
        hold_fraction: Option<f32>,
    ) {
        self.presentation = InteractionPromptPresentation {
            visible: true,
            cursor: self.cursor_when_focused,
            highlight_entity: Some(target.entity()),
            label: self.label.clone(),
            world_anchor,
            hold_fraction,
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{WorldInteractionActivation, WorldInteractionTarget};
    use std::time::Duration;
    use yuyib_ecs::prelude::World;

    #[test]
    fn sync_shows_prompt_for_focus() {
        let mut world = World::new();
        let entity = world.spawn_empty().id();
        let mut state = WorldInteractionState::default();
        let target = WorldInteractionTarget::new(entity, crate::InteractionId::new("door"))
            .with_activation(WorldInteractionActivation::press());
        let _ = state.step(Some(target), false, Duration::ZERO);

        let mut prompt = InteractionPrompt2d::new().with_label("Press E");
        prompt.sync(&state, Some([10.0, 20.0]), None);
        let presentation = prompt.presentation();
        assert!(presentation.visible);
        assert_eq!(presentation.cursor, InteractionCursorHint::Use);
        assert_eq!(presentation.highlight_entity, Some(entity));
        assert_eq!(presentation.label.as_deref(), Some("Press E"));
        assert_eq!(presentation.world_anchor, Some([10.0, 20.0]));
    }

    #[test]
    fn sync_clears_without_focus() {
        let state = WorldInteractionState::<crate::InteractionId>::default();
        let mut prompt = InteractionPrompt2d::new().with_label("Press E");
        prompt.sync(&state, Some([1.0, 2.0]), Some(0.5));
        assert!(!prompt.presentation().visible);
        assert_eq!(prompt.presentation().cursor, InteractionCursorHint::Default);
    }
}
