//! Renderer-neutral event-driven quest foundations.
//!
//! This module does not assume a UI, save file, network role or ECS schedule.

#![allow(
    clippy::doc_markdown,
    reason = "Rustdoc links are intentionally kept plain in this compact domain module."
)]

use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt,
};

use yuyib_ecs::bevy_ecs::prelude::Resource;

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

identifier!(QuestId, "Stable identifier for one quest definition.");
identifier!(
    ObjectiveId,
    "Stable identifier for one objective within a quest."
);
identifier!(
    QuestEventId,
    "Stable identifier for a semantic gameplay event."
);

/// Immutable positive counter objective.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QuestObjective {
    id: ObjectiveId,
    event: QuestEventId,
    target: u32,
}

impl QuestObjective {
    /// Creates an objective that advances from matching event signals.
    ///
    /// # Errors
    ///
    /// Returns QuestDefinitionError when target is zero.
    pub fn new(
        id: impl Into<ObjectiveId>,
        event: impl Into<QuestEventId>,
        target: u32,
    ) -> Result<Self, QuestDefinitionError> {
        if target == 0 {
            return Err(QuestDefinitionError::ZeroObjectiveTarget);
        }
        Ok(Self {
            id: id.into(),
            event: event.into(),
            target,
        })
    }

    /// Returns the identifier unique within its quest.
    #[must_use]
    pub fn id(&self) -> &ObjectiveId {
        &self.id
    }

    /// Returns the event that advances this objective.
    #[must_use]
    pub fn event(&self) -> &QuestEventId {
        &self.event
    }

    /// Returns the strictly positive completion target.
    #[must_use]
    pub const fn target(&self) -> u32 {
        self.target
    }
}

/// Validated immutable quest definition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QuestDefinition {
    id: QuestId,
    objectives: Vec<QuestObjective>,
}

impl QuestDefinition {
    /// Creates a definition with unique objectives in authoring order.
    ///
    /// # Errors
    ///
    /// Returns QuestDefinitionError for an empty or duplicate objective list.
    pub fn new(
        id: impl Into<QuestId>,
        objectives: Vec<QuestObjective>,
    ) -> Result<Self, QuestDefinitionError> {
        if objectives.is_empty() {
            return Err(QuestDefinitionError::NoObjectives);
        }
        let mut seen = BTreeSet::new();
        for objective in &objectives {
            if !seen.insert(objective.id.clone()) {
                return Err(QuestDefinitionError::DuplicateObjective {
                    objective: objective.id.clone(),
                });
            }
        }
        Ok(Self {
            id: id.into(),
            objectives,
        })
    }

    /// Returns the stable quest identifier.
    #[must_use]
    pub fn id(&self) -> &QuestId {
        &self.id
    }

    /// Returns objectives in authoring order.
    #[must_use]
    pub fn objectives(&self) -> &[QuestObjective] {
        &self.objectives
    }
}

/// Positive semantic progress submitted to a QuestBook.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QuestSignal {
    event: QuestEventId,
    amount: u32,
}

impl QuestSignal {
    /// Creates a nonzero progress signal.
    ///
    /// # Errors
    ///
    /// Returns QuestSignalError for zero amount.
    pub fn new(event: impl Into<QuestEventId>, amount: u32) -> Result<Self, QuestSignalError> {
        if amount == 0 {
            return Err(QuestSignalError::ZeroAmount);
        }
        Ok(Self {
            event: event.into(),
            amount,
        })
    }

    /// Returns the matching event identifier.
    #[must_use]
    pub fn event(&self) -> &QuestEventId {
        &self.event
    }

    /// Returns positive progress units.
    #[must_use]
    pub const fn amount(&self) -> u32 {
        self.amount
    }
}

/// Lifecycle state of one quest.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QuestStatus {
    /// Registered but not started.
    Inactive,
    /// Matching signals can advance objectives.
    Active,
    /// Every objective reached its target.
    Completed,
    /// Gameplay explicitly ended this quest unsuccessfully.
    Failed,
}

/// Runtime progress suitable for a later save format.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QuestProgress {
    status: QuestStatus,
    objectives: BTreeMap<ObjectiveId, u32>,
}

impl QuestProgress {
    /// Returns lifecycle state.
    #[must_use]
    pub const fn status(&self) -> QuestStatus {
        self.status
    }

    /// Returns current units for a known objective.
    #[must_use]
    pub fn objective(&self, id: &ObjectiveId) -> Option<u32> {
        self.objectives.get(id).copied()
    }

    /// Returns objective progress in stable identifier order.
    pub fn objectives(&self) -> impl Iterator<Item = (&ObjectiveId, u32)> {
        self.objectives.iter().map(|(id, value)| (id, *value))
    }
}

/// Detached copy of all runtime quest progress for a later save layer.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct QuestSnapshot {
    quests: BTreeMap<QuestId, QuestProgress>,
}

impl QuestSnapshot {
    /// Returns saved state for one quest.
    #[must_use]
    pub fn get(&self, id: &QuestId) -> Option<&QuestProgress> {
        self.quests.get(id)
    }

    /// Returns saved state in stable quest identifier order.
    pub fn quests(&self) -> impl Iterator<Item = (&QuestId, &QuestProgress)> {
        self.quests.iter()
    }
}

/// One explicit state change emitted by QuestBook.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum QuestTransition {
    /// A quest became active.
    Started {
        /// Quest that became active.
        quest: QuestId,
    },
    /// A signal advanced an objective.
    ObjectiveProgressed {
        /// Parent quest.
        quest: QuestId,
        /// Changed objective.
        objective: ObjectiveId,
        /// Previous progress.
        previous: u32,
        /// Progress after clamping to target.
        current: u32,
        /// Objective completion target.
        target: u32,
    },
    /// A quest reached all targets.
    Completed {
        /// Quest that completed.
        quest: QuestId,
    },
    /// Gameplay failed an active quest.
    Failed {
        /// Quest that failed.
        quest: QuestId,
    },
}

/// Validated definitions and event-driven progress state.
#[derive(Debug, Default, Resource)]
pub struct QuestBook {
    definitions: BTreeMap<QuestId, QuestDefinition>,
    progress: BTreeMap<QuestId, QuestProgress>,
}

impl QuestBook {
    /// Registers an inactive quest definition.
    ///
    /// # Errors
    ///
    /// Returns QuestBookError when the identifier already exists.
    pub fn register(&mut self, definition: QuestDefinition) -> Result<(), QuestBookError> {
        let id = definition.id.clone();
        if self.definitions.contains_key(&id) {
            return Err(QuestBookError::DuplicateQuest { quest: id });
        }
        let objectives = definition
            .objectives
            .iter()
            .map(|objective| (objective.id.clone(), 0))
            .collect();
        self.progress.insert(
            id.clone(),
            QuestProgress {
                status: QuestStatus::Inactive,
                objectives,
            },
        );
        self.definitions.insert(id, definition);
        Ok(())
    }

    /// Starts an inactive quest.
    ///
    /// # Errors
    ///
    /// Returns QuestBookError for unknown quests or forbidden transitions.
    pub fn start(&mut self, id: &QuestId) -> Result<QuestTransition, QuestBookError> {
        self.transition(id, QuestStatus::Inactive, QuestStatus::Active, |quest| {
            QuestTransition::Started { quest }
        })
    }

    /// Fails an active quest.
    ///
    /// # Errors
    ///
    /// Returns QuestBookError for unknown quests or forbidden transitions.
    pub fn fail(&mut self, id: &QuestId) -> Result<QuestTransition, QuestBookError> {
        self.transition(id, QuestStatus::Active, QuestStatus::Failed, |quest| {
            QuestTransition::Failed { quest }
        })
    }

    /// Applies one signal to every active definition in stable order.
    #[must_use]
    pub fn apply_signal(&mut self, signal: &QuestSignal) -> Vec<QuestTransition> {
        let mut events = Vec::new();
        for (quest, definition) in &self.definitions {
            let Some(progress) = self.progress.get_mut(quest) else {
                continue;
            };
            if progress.status != QuestStatus::Active {
                continue;
            }
            for objective in definition
                .objectives
                .iter()
                .filter(|objective| objective.event == signal.event)
            {
                let previous = progress.objectives.get(&objective.id).copied().unwrap_or(0);
                let current = previous.saturating_add(signal.amount).min(objective.target);
                if current == previous {
                    continue;
                }
                progress.objectives.insert(objective.id.clone(), current);
                events.push(QuestTransition::ObjectiveProgressed {
                    quest: quest.clone(),
                    objective: objective.id.clone(),
                    previous,
                    current,
                    target: objective.target,
                });
            }
            let complete = definition.objectives.iter().all(|objective| {
                progress.objectives.get(&objective.id).copied().unwrap_or(0) >= objective.target
            });
            if complete {
                progress.status = QuestStatus::Completed;
                events.push(QuestTransition::Completed {
                    quest: quest.clone(),
                });
            }
        }
        events
    }

    /// Returns immutable definition data.
    #[must_use]
    pub fn definition(&self, id: &QuestId) -> Option<&QuestDefinition> {
        self.definitions.get(id)
    }

    /// Returns current runtime state.
    #[must_use]
    pub fn progress(&self, id: &QuestId) -> Option<&QuestProgress> {
        self.progress.get(id)
    }

    /// Creates a detached save-ready state snapshot.
    #[must_use]
    pub fn snapshot(&self) -> QuestSnapshot {
        QuestSnapshot {
            quests: self.progress.clone(),
        }
    }

    fn transition(
        &mut self,
        id: &QuestId,
        expected: QuestStatus,
        requested: QuestStatus,
        build: impl FnOnce(QuestId) -> QuestTransition,
    ) -> Result<QuestTransition, QuestBookError> {
        let progress = self
            .progress
            .get_mut(id)
            .ok_or_else(|| QuestBookError::UnknownQuest { quest: id.clone() })?;
        if progress.status != expected {
            return Err(QuestBookError::InvalidTransition {
                quest: id.clone(),
                from: progress.status,
                requested,
            });
        }
        progress.status = requested;
        Ok(build(id.clone()))
    }
}

/// Definition validation failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum QuestDefinitionError {
    /// No objectives were supplied.
    NoObjectives,
    /// An objective target was zero.
    ZeroObjectiveTarget,
    /// An objective identity appeared more than once.
    DuplicateObjective {
        /// Repeated objective identifier.
        objective: ObjectiveId,
    },
}

impl fmt::Display for QuestDefinitionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoObjectives => formatter.write_str("quest needs at least one objective"),
            Self::ZeroObjectiveTarget => formatter.write_str("objective target must be positive"),
            Self::DuplicateObjective { objective } => {
                write!(formatter, "duplicate objective: {objective}")
            }
        }
    }
}
impl Error for QuestDefinitionError {}

/// Invalid signal construction request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QuestSignalError {
    /// A signal amount was zero.
    ZeroAmount,
}
impl fmt::Display for QuestSignalError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("quest signal amount must be positive")
    }
}
impl Error for QuestSignalError {}

/// Invalid QuestBook operation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum QuestBookError {
    /// A definition identifier already exists.
    DuplicateQuest {
        /// Repeated quest identifier.
        quest: QuestId,
    },
    /// A requested quest does not exist.
    UnknownQuest {
        /// Absent quest identifier.
        quest: QuestId,
    },
    /// Lifecycle transition is forbidden.
    InvalidTransition {
        /// Quest being changed.
        quest: QuestId,
        /// Current state.
        from: QuestStatus,
        /// Requested state.
        requested: QuestStatus,
    },
}
impl fmt::Display for QuestBookError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateQuest { quest } => write!(formatter, "quest already exists: {quest}"),
            Self::UnknownQuest { quest } => write!(formatter, "unknown quest: {quest}"),
            Self::InvalidTransition {
                quest,
                from,
                requested,
            } => write!(
                formatter,
                "quest {quest} cannot move from {from:?} to {requested:?}"
            ),
        }
    }
}
impl Error for QuestBookError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn definition() -> QuestDefinition {
        QuestDefinition::new(
            "main.power",
            vec![
                QuestObjective::new("generators", "world.generator", 2).expect("valid"),
                QuestObjective::new("exit", "world.exit", 1).expect("valid"),
            ],
        )
        .expect("definition")
    }

    #[test]
    fn signals_complete_active_quest_once() {
        let mut book = QuestBook::default();
        book.register(definition()).expect("register");
        let id = QuestId::new("main.power");
        book.start(&id).expect("start");
        let generator = QuestSignal::new("world.generator", 1).expect("signal");
        assert_eq!(book.apply_signal(&generator).len(), 1);
        assert_eq!(book.apply_signal(&generator).len(), 1);
        let done = book.apply_signal(&QuestSignal::new("world.exit", 3).expect("signal"));
        assert!(matches!(
            done.as_slice(),
            [
                QuestTransition::ObjectiveProgressed { .. },
                QuestTransition::Completed { .. }
            ]
        ));
        assert_eq!(
            book.progress(&id).expect("state").status(),
            QuestStatus::Completed
        );
        assert!(book.apply_signal(&generator).is_empty());
    }

    #[test]
    fn validation_and_snapshot_are_explicit() {
        assert!(matches!(
            QuestSignal::new("x", 0),
            Err(QuestSignalError::ZeroAmount)
        ));
        assert!(matches!(
            QuestDefinition::new("x", Vec::new()),
            Err(QuestDefinitionError::NoObjectives)
        ));
        let mut book = QuestBook::default();
        book.register(definition()).expect("register");
        let id = QuestId::new("main.power");
        book.start(&id).expect("start");
        let snapshot = book.snapshot();
        let _ = book.apply_signal(&QuestSignal::new("world.generator", 2).expect("signal"));
        assert_eq!(
            snapshot
                .get(&id)
                .expect("snapshot")
                .objective(&ObjectiveId::new("generators")),
            Some(0)
        );
        assert!(matches!(book.fail(&id), Ok(QuestTransition::Failed { .. })));
    }

    #[test]
    fn accepted_interaction_can_be_mapped_to_a_quest_signal() {
        use crate::{
            InteractionId, InteractionMethod, InteractionOutcome, InteractionRequested,
            InteractionResolved,
        };
        use yuyib_ecs::bevy_ecs::entity::Entity;

        let mut book = QuestBook::default();
        book.register(definition()).expect("register");
        let id = QuestId::new("main.power");
        book.start(&id).expect("start");
        let resolution = InteractionResolved {
            request: InteractionRequested {
                actor: Entity::PLACEHOLDER,
                target: Entity::PLACEHOLDER,
                interaction: InteractionId::new("world.activate_generator"),
                method: InteractionMethod::Custom("script.confirmed".into()),
            },
            outcome: InteractionOutcome::Accepted,
        };
        if resolution.outcome == InteractionOutcome::Accepted {
            let signal = QuestSignal::new("world.generator", 1).expect("positive signal");
            let transitions = book.apply_signal(&signal);
            assert!(matches!(
                transitions.as_slice(),
                [QuestTransition::ObjectiveProgressed {
                    current: 1,
                    target: 2,
                    ..
                }]
            ));
        } else {
            panic!("fixture must model an accepted interaction");
        }
    }
}
