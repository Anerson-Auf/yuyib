//! Renderer-neutral dialogue graphs, story flags, and choice sessions.
//!
//! This module owns branching narrative state only: nodes, choices, flag
//! conditions, and effects (`SetFlag` / `EmitQuestSignal`). It does not build
//! `UiTree`, own Winit, or load JSON assets (asset authoring is a later bite).

#![allow(
    clippy::doc_markdown,
    reason = "Rustdoc links are intentionally kept plain in this compact domain module."
)]

use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt,
};

use crate::quest::{QuestBook, QuestSignal, QuestTransition};

macro_rules! identifier {
    ($name:ident, $description:literal) => {
        #[doc = $description]
        #[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(Box<str>);

        impl $name {
            /// Creates an identifier from a canonical semantic name.
            #[must_use]
            pub fn new(value: impl Into<Box<str>>) -> Self {
                Self(value.into())
            }

            /// Returns the canonical semantic name.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(self.as_str())
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self::new(value)
            }
        }
    };
}

identifier!(DialogueId, "Stable identifier for one dialogue graph.");
identifier!(DialogueNodeId, "Stable identifier for one dialogue node.");
identifier!(DialogueChoiceId, "Stable identifier for one dialogue choice.");
identifier!(StoryFlagId, "Stable identifier for a story/quest branch flag or counter.");

impl DialogueChoiceId {
    /// Stable widget key for native UI mapping (`dlg-choice:<id>`).
    #[must_use]
    pub fn widget_key(&self) -> String {
        format!("dlg-choice:{}", self.as_str())
    }
}

/// Shared story blackboard for dialogue conditions and quest-adjacent branching.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StoryFlags {
    bools: BTreeMap<StoryFlagId, bool>,
    counters: BTreeMap<StoryFlagId, u32>,
}

impl StoryFlags {
    /// Returns an empty blackboard.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns whether a boolean flag is set (missing ⇒ false).
    #[must_use]
    pub fn flag(&self, id: &StoryFlagId) -> bool {
        self.bools.get(id).copied().unwrap_or(false)
    }

    /// Sets or clears a boolean flag.
    pub fn set_flag(&mut self, id: impl Into<StoryFlagId>, value: bool) {
        let id = id.into();
        if value {
            self.bools.insert(id, true);
        } else {
            self.bools.remove(&id);
        }
    }

    /// Returns a counter (missing ⇒ 0).
    #[must_use]
    pub fn counter(&self, id: &StoryFlagId) -> u32 {
        self.counters.get(id).copied().unwrap_or(0)
    }

    /// Replaces a counter value.
    pub fn set_counter(&mut self, id: impl Into<StoryFlagId>, value: u32) {
        let id = id.into();
        if value == 0 {
            self.counters.remove(&id);
        } else {
            self.counters.insert(id, value);
        }
    }

    /// Adds to a counter with saturating arithmetic.
    pub fn add_counter(&mut self, id: impl Into<StoryFlagId>, amount: u32) {
        let id = id.into();
        let next = self.counter(&id).saturating_add(amount);
        self.set_counter(id, next);
    }
}

/// Predicate evaluated against [`StoryFlags`] for choice visibility.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DialogueCondition {
    /// Boolean flag must be set.
    FlagSet(StoryFlagId),
    /// Boolean flag must be clear / missing.
    FlagClear(StoryFlagId),
    /// Counter must be ≥ `minimum`.
    CounterAtLeast {
        /// Counter id.
        id: StoryFlagId,
        /// Inclusive lower bound.
        minimum: u32,
    },
}

impl DialogueCondition {
    /// Returns whether this condition holds for `flags`.
    #[must_use]
    pub fn matches(&self, flags: &StoryFlags) -> bool {
        match self {
            Self::FlagSet(id) => flags.flag(id),
            Self::FlagClear(id) => !flags.flag(id),
            Self::CounterAtLeast { id, minimum } => flags.counter(id) >= *minimum,
        }
    }
}

/// Side effect applied when entering a node or selecting a choice.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DialogueEffect {
    /// Sets a boolean story flag.
    SetFlag(StoryFlagId),
    /// Clears a boolean story flag.
    ClearFlag(StoryFlagId),
    /// Replaces a counter.
    SetCounter {
        /// Counter id.
        id: StoryFlagId,
        /// Absolute value.
        value: u32,
    },
    /// Adds to a counter.
    AddCounter {
        /// Counter id.
        id: StoryFlagId,
        /// Non-zero preferred; zero is a no-op.
        amount: u32,
    },
    /// Forwards a quest progress signal through an optional [`QuestBook`].
    EmitQuestSignal(QuestSignal),
}

/// One selectable edge from a dialogue node.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DialogueChoice {
    id: DialogueChoiceId,
    text: String,
    next: Option<DialogueNodeId>,
    requires: Vec<DialogueCondition>,
    effects: Vec<DialogueEffect>,
}

impl DialogueChoice {
    /// Creates a choice that jumps to `next`, or ends the session when `None`.
    #[must_use]
    pub fn new(
        id: impl Into<DialogueChoiceId>,
        text: impl Into<String>,
        next: Option<DialogueNodeId>,
    ) -> Self {
        Self {
            id: id.into(),
            text: text.into(),
            next,
            requires: Vec::new(),
            effects: Vec::new(),
        }
    }

    /// Adds a visibility condition (all conditions must match).
    #[must_use]
    pub fn with_require(mut self, condition: DialogueCondition) -> Self {
        self.requires.push(condition);
        self
    }

    /// Appends an effect applied when the player selects this choice.
    #[must_use]
    pub fn with_effect(mut self, effect: DialogueEffect) -> Self {
        self.effects.push(effect);
        self
    }

    /// Returns the choice id.
    #[must_use]
    pub fn id(&self) -> &DialogueChoiceId {
        &self.id
    }

    /// Returns the player-facing label.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Returns the next node, if any.
    #[must_use]
    pub fn next(&self) -> Option<&DialogueNodeId> {
        self.next.as_ref()
    }

    /// Returns visibility conditions.
    #[must_use]
    pub fn requires(&self) -> &[DialogueCondition] {
        &self.requires
    }

    /// Returns selection effects.
    #[must_use]
    pub fn effects(&self) -> &[DialogueEffect] {
        &self.effects
    }

    /// Returns whether every condition matches `flags`.
    #[must_use]
    pub fn is_visible(&self, flags: &StoryFlags) -> bool {
        self.requires.iter().all(|condition| condition.matches(flags))
    }
}

/// One spoken / narrated beat with zero or more choices.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DialogueNode {
    id: DialogueNodeId,
    speaker: Option<String>,
    text: String,
    enter_effects: Vec<DialogueEffect>,
    choices: Vec<DialogueChoice>,
}

impl DialogueNode {
    /// Creates a node with body text and no choices (terminal until acknowledged).
    #[must_use]
    pub fn new(id: impl Into<DialogueNodeId>, text: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            speaker: None,
            text: text.into(),
            enter_effects: Vec::new(),
            choices: Vec::new(),
        }
    }

    /// Sets an optional speaker label.
    #[must_use]
    pub fn with_speaker(mut self, speaker: impl Into<String>) -> Self {
        self.speaker = Some(speaker.into());
        self
    }

    /// Appends an effect applied when the session enters this node.
    #[must_use]
    pub fn with_enter_effect(mut self, effect: DialogueEffect) -> Self {
        self.enter_effects.push(effect);
        self
    }

    /// Replaces the choice list.
    #[must_use]
    pub fn with_choices(mut self, choices: Vec<DialogueChoice>) -> Self {
        self.choices = choices;
        self
    }

    /// Returns the node id.
    #[must_use]
    pub fn id(&self) -> &DialogueNodeId {
        &self.id
    }

    /// Returns the optional speaker.
    #[must_use]
    pub fn speaker(&self) -> Option<&str> {
        self.speaker.as_deref()
    }

    /// Returns the body text.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Returns enter effects.
    #[must_use]
    pub fn enter_effects(&self) -> &[DialogueEffect] {
        &self.enter_effects
    }

    /// Returns authored choices (not filtered by flags).
    #[must_use]
    pub fn choices(&self) -> &[DialogueChoice] {
        &self.choices
    }
}

/// Validated immutable dialogue graph.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DialogueGraph {
    id: DialogueId,
    start: DialogueNodeId,
    nodes: BTreeMap<DialogueNodeId, DialogueNode>,
}

impl DialogueGraph {
    /// Validates and stores a graph.
    ///
    /// # Errors
    ///
    /// Returns [`DialogueDefinitionError`] for empty graphs, missing start,
    /// duplicate choice ids, or dangling `next` edges.
    pub fn new(
        id: impl Into<DialogueId>,
        start: impl Into<DialogueNodeId>,
        nodes: Vec<DialogueNode>,
    ) -> Result<Self, DialogueDefinitionError> {
        if nodes.is_empty() {
            return Err(DialogueDefinitionError::NoNodes);
        }
        let start = start.into();
        let mut map = BTreeMap::new();
        let mut choice_ids = BTreeSet::new();
        for node in nodes {
            if map.contains_key(&node.id) {
                return Err(DialogueDefinitionError::DuplicateNode {
                    node: node.id.clone(),
                });
            }
            for choice in &node.choices {
                if !choice_ids.insert(choice.id.clone()) {
                    return Err(DialogueDefinitionError::DuplicateChoice {
                        choice: choice.id.clone(),
                    });
                }
            }
            map.insert(node.id.clone(), node);
        }
        if !map.contains_key(&start) {
            return Err(DialogueDefinitionError::MissingStart {
                start: start.clone(),
            });
        }
        for node in map.values() {
            for choice in &node.choices {
                if let Some(next) = &choice.next
                    && !map.contains_key(next)
                {
                    return Err(DialogueDefinitionError::UnknownNext {
                        choice: choice.id.clone(),
                        next: next.clone(),
                    });
                }
            }
        }
        Ok(Self {
            id: id.into(),
            start,
            nodes: map,
        })
    }

    /// Returns the dialogue id.
    #[must_use]
    pub fn id(&self) -> &DialogueId {
        &self.id
    }

    /// Returns the start node id.
    #[must_use]
    pub fn start(&self) -> &DialogueNodeId {
        &self.start
    }

    /// Returns a node by id.
    #[must_use]
    pub fn node(&self, id: &DialogueNodeId) -> Option<&DialogueNode> {
        self.nodes.get(id)
    }
}

/// UI-facing snapshot of the active node after flag filtering.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DialoguePresentation {
    /// Active dialogue id.
    pub dialogue: DialogueId,
    /// Current node id.
    pub node: DialogueNodeId,
    /// Optional speaker label.
    pub speaker: Option<String>,
    /// Body text.
    pub text: String,
    /// Visible choices in authoring order.
    pub choices: Vec<DialoguePresentedChoice>,
}

/// One visible choice in a [`DialoguePresentation`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DialoguePresentedChoice {
    /// Choice id (use [`DialogueChoiceId::widget_key`] for UI ids).
    pub id: DialogueChoiceId,
    /// Player-facing label.
    pub text: String,
}

/// Lifecycle events emitted by [`DialogueSession`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DialogueEvent {
    /// Session entered a node (after enter effects).
    Entered {
        /// Dialogue id.
        dialogue: DialogueId,
        /// Node id.
        node: DialogueNodeId,
    },
    /// Player selected a choice.
    Chose {
        /// Dialogue id.
        dialogue: DialogueId,
        /// Choice id.
        choice: DialogueChoiceId,
    },
    /// Session ended (choice with `next = None`, acknowledge, or stop).
    Ended {
        /// Dialogue id.
        dialogue: DialogueId,
    },
}

/// Active playthrough of one [`DialogueGraph`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DialogueSession {
    graph: DialogueGraph,
    current: Option<DialogueNodeId>,
}

impl DialogueSession {
    /// Starts a session on `graph.start`, applying enter effects.
    pub fn start(
        graph: DialogueGraph,
        flags: &mut StoryFlags,
        quests: Option<&mut QuestBook>,
    ) -> (Self, Vec<DialogueEvent>, Vec<QuestTransition>) {
        let start = graph.start.clone();
        let mut session = Self {
            graph,
            current: None,
        };
        let (events, quest_events) = session.enter_node(start, flags, quests);
        (session, events, quest_events)
    }

    /// Returns whether a node is currently presented.
    #[must_use]
    pub const fn is_active(&self) -> bool {
        self.current.is_some()
    }

    /// Returns the dialogue id.
    #[must_use]
    pub fn dialogue_id(&self) -> &DialogueId {
        self.graph.id()
    }

    /// Returns the current node id, if active.
    #[must_use]
    pub fn current_node_id(&self) -> Option<&DialogueNodeId> {
        self.current.as_ref()
    }

    /// Returns the current node, if active.
    #[must_use]
    pub fn current_node(&self) -> Option<&DialogueNode> {
        self.current.as_ref().and_then(|id| self.graph.node(id))
    }

    /// Builds a UI snapshot with flag-filtered choices.
    #[must_use]
    pub fn presentation(&self, flags: &StoryFlags) -> Option<DialoguePresentation> {
        let node = self.current_node()?;
        let choices = node
            .choices
            .iter()
            .filter(|choice| choice.is_visible(flags))
            .map(|choice| DialoguePresentedChoice {
                id: choice.id.clone(),
                text: choice.text.clone(),
            })
            .collect();
        Some(DialoguePresentation {
            dialogue: self.graph.id.clone(),
            node: node.id.clone(),
            speaker: node.speaker.clone(),
            text: node.text.clone(),
            choices,
        })
    }

    /// Selects a visible choice by id.
    ///
    /// # Errors
    ///
    /// Returns [`DialogueSessionError`] when inactive, unknown, or hidden.
    pub fn choose(
        &mut self,
        choice_id: &DialogueChoiceId,
        flags: &mut StoryFlags,
        quests: Option<&mut QuestBook>,
    ) -> Result<(Vec<DialogueEvent>, Vec<QuestTransition>), DialogueSessionError> {
        let node = self
            .current_node()
            .ok_or(DialogueSessionError::Inactive)?
            .clone();
        let choice = node
            .choices
            .iter()
            .find(|choice| choice.id() == choice_id)
            .cloned()
            .ok_or_else(|| DialogueSessionError::UnknownChoice {
                choice: choice_id.clone(),
            })?;
        if !choice.is_visible(flags) {
            return Err(DialogueSessionError::ChoiceNotVisible {
                choice: choice_id.clone(),
            });
        }

        let mut events = vec![DialogueEvent::Chose {
            dialogue: self.graph.id.clone(),
            choice: choice.id.clone(),
        }];
        let mut quests = quests;
        let mut quest_events =
            apply_effects(&choice.effects, flags, quests.as_deref_mut());

        match choice.next {
            Some(next) => {
                let (entered, more_quests) = self.enter_node(next, flags, quests);
                events.extend(entered);
                quest_events.extend(more_quests);
            }
            None => {
                self.current = None;
                events.push(DialogueEvent::Ended {
                    dialogue: self.graph.id.clone(),
                });
            }
        }
        Ok((events, quest_events))
    }

    /// Acknowledges a terminal node that has no visible choices, ending the session.
    ///
    /// # Errors
    ///
    /// Returns an error when inactive or when visible choices still remain.
    pub fn acknowledge(
        &mut self,
        flags: &StoryFlags,
    ) -> Result<Vec<DialogueEvent>, DialogueSessionError> {
        let presentation = self
            .presentation(flags)
            .ok_or(DialogueSessionError::Inactive)?;
        if !presentation.choices.is_empty() {
            return Err(DialogueSessionError::ChoicesRemain);
        }
        self.current = None;
        Ok(vec![DialogueEvent::Ended {
            dialogue: presentation.dialogue,
        }])
    }

    /// Forces the session closed without selecting a choice.
    pub fn stop(&mut self) -> Option<DialogueEvent> {
        self.current.take().map(|_| DialogueEvent::Ended {
            dialogue: self.graph.id.clone(),
        })
    }

    fn enter_node(
        &mut self,
        node_id: DialogueNodeId,
        flags: &mut StoryFlags,
        quests: Option<&mut QuestBook>,
    ) -> (Vec<DialogueEvent>, Vec<QuestTransition>) {
        let Some(node) = self.graph.node(&node_id).cloned() else {
            self.current = None;
            return (
                vec![DialogueEvent::Ended {
                    dialogue: self.graph.id.clone(),
                }],
                Vec::new(),
            );
        };
        self.current = Some(node_id.clone());
        let quest_events = apply_effects(&node.enter_effects, flags, quests);
        (
            vec![DialogueEvent::Entered {
                dialogue: self.graph.id.clone(),
                node: node_id,
            }],
            quest_events,
        )
    }
}

fn apply_effects(
    effects: &[DialogueEffect],
    flags: &mut StoryFlags,
    mut quests: Option<&mut QuestBook>,
) -> Vec<QuestTransition> {
    let mut quest_events = Vec::new();
    for effect in effects {
        match effect {
            DialogueEffect::SetFlag(id) => flags.set_flag(id.clone(), true),
            DialogueEffect::ClearFlag(id) => flags.set_flag(id.clone(), false),
            DialogueEffect::SetCounter { id, value } => flags.set_counter(id.clone(), *value),
            DialogueEffect::AddCounter { id, amount } => flags.add_counter(id.clone(), *amount),
            DialogueEffect::EmitQuestSignal(signal) => {
                if let Some(book) = quests.as_deref_mut() {
                    quest_events.extend(book.apply_signal(signal));
                }
            }
        }
    }
    quest_events
}

/// Invalid dialogue graph authoring.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DialogueDefinitionError {
    /// Graph contained no nodes.
    NoNodes,
    /// Two nodes shared an id.
    DuplicateNode {
        /// Conflicting node.
        node: DialogueNodeId,
    },
    /// Two choices shared an id within the graph.
    DuplicateChoice {
        /// Conflicting choice.
        choice: DialogueChoiceId,
    },
    /// Start node was missing from the node list.
    MissingStart {
        /// Requested start.
        start: DialogueNodeId,
    },
    /// A choice pointed at an unknown node.
    UnknownNext {
        /// Choice with the bad edge.
        choice: DialogueChoiceId,
        /// Missing node.
        next: DialogueNodeId,
    },
}

impl fmt::Display for DialogueDefinitionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoNodes => formatter.write_str("dialogue graph has no nodes"),
            Self::DuplicateNode { node } => write!(formatter, "duplicate dialogue node `{node}`"),
            Self::DuplicateChoice { choice } => {
                write!(formatter, "duplicate dialogue choice `{choice}`")
            }
            Self::MissingStart { start } => {
                write!(formatter, "dialogue start node `{start}` is missing")
            }
            Self::UnknownNext { choice, next } => {
                write!(
                    formatter,
                    "dialogue choice `{choice}` points to unknown node `{next}`"
                )
            }
        }
    }
}

impl Error for DialogueDefinitionError {}

/// Invalid session operation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DialogueSessionError {
    /// No active node.
    Inactive,
    /// Choice id is not on the current node.
    UnknownChoice {
        /// Requested choice.
        choice: DialogueChoiceId,
    },
    /// Choice exists but fails visibility conditions.
    ChoiceNotVisible {
        /// Hidden choice.
        choice: DialogueChoiceId,
    },
    /// Acknowledge called while choices remain visible.
    ChoicesRemain,
}

impl fmt::Display for DialogueSessionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Inactive => formatter.write_str("dialogue session is inactive"),
            Self::UnknownChoice { choice } => {
                write!(formatter, "unknown dialogue choice `{choice}`")
            }
            Self::ChoiceNotVisible { choice } => {
                write!(formatter, "dialogue choice `{choice}` is not visible")
            }
            Self::ChoicesRemain => {
                formatter.write_str("cannot acknowledge dialogue while choices remain")
            }
        }
    }
}

impl Error for DialogueSessionError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quest::{QuestDefinition, QuestObjective};

    fn sample_graph() -> DialogueGraph {
        DialogueGraph::new(
            "demo.talk",
            "intro",
            vec![
                DialogueNode::new("intro", "Need a favour?")
                    .with_speaker("Npc")
                    .with_choices(vec![
                        DialogueChoice::new("accept", "I'll help.", Some(DialogueNodeId::new("thanks")))
                            .with_effect(DialogueEffect::SetFlag(StoryFlagId::new("helped_npc")))
                            .with_effect(DialogueEffect::EmitQuestSignal(
                                QuestSignal::new("world.talk_npc", 1).expect("signal"),
                            )),
                        DialogueChoice::new("refuse", "Not now.", None),
                        DialogueChoice::new(
                            "secret",
                            "About the vault…",
                            Some(DialogueNodeId::new("vault")),
                        )
                        .with_require(DialogueCondition::FlagSet(StoryFlagId::new("knows_vault"))),
                    ]),
                DialogueNode::new("thanks", "Appreciate it.").with_speaker("Npc"),
                DialogueNode::new("vault", "Keep it quiet.").with_speaker("Npc"),
            ],
        )
        .expect("graph")
    }

    #[test]
    fn choice_filters_and_sets_flags_and_quest_signal() {
        let graph = sample_graph();
        let mut flags = StoryFlags::new();
        let mut book = QuestBook::default();
        book.register(
            QuestDefinition::new(
                "q.talk",
                vec![QuestObjective::new("talk", "world.talk_npc", 1).expect("obj")],
            )
            .expect("def"),
        )
        .expect("register");
        book.start(&"q.talk".into()).expect("start");

        let (mut session, events, _) = DialogueSession::start(graph, &mut flags, Some(&mut book));
        assert!(matches!(
            events.as_slice(),
            [DialogueEvent::Entered { node, .. }] if node.as_str() == "intro"
        ));
        let presentation = session.presentation(&flags).expect("pres");
        assert_eq!(presentation.choices.len(), 2);

        flags.set_flag("knows_vault", true);
        assert_eq!(session.presentation(&flags).expect("pres").choices.len(), 3);

        let (events, quests) = session
            .choose(&DialogueChoiceId::new("accept"), &mut flags, Some(&mut book))
            .expect("choose");
        assert!(flags.flag(&StoryFlagId::new("helped_npc")));
        assert!(quests.iter().any(|event| matches!(
            event,
            QuestTransition::Completed { .. } | QuestTransition::ObjectiveProgressed { .. }
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            DialogueEvent::Entered { node, .. } if node.as_str() == "thanks"
        )));
        assert!(session
            .acknowledge(&flags)
            .expect("ack")
            .iter()
            .any(|event| matches!(event, DialogueEvent::Ended { .. })));
        assert!(!session.is_active());
    }

    #[test]
    fn hidden_choice_cannot_be_selected() {
        let graph = sample_graph();
        let mut flags = StoryFlags::new();
        let (mut session, _, _) = DialogueSession::start(graph, &mut flags, None);
        assert_eq!(
            session.choose(&DialogueChoiceId::new("secret"), &mut flags, None),
            Err(DialogueSessionError::ChoiceNotVisible {
                choice: DialogueChoiceId::new("secret"),
            })
        );
    }
}
