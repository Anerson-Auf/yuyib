//! Headless high-level dialogue / choice / story-flag flow.
//!
//! Run with:
//! `cargo run -p yuyib --example dialogue_choice_flow --no-default-features --features gameplay`
//!
//! Domain lives in `yuyib::gameplay` (`DialogueGraph`, `StoryFlags`,
//! `DialogueSession`). Native UI overlay helpers live in `yuyib::app`
//! (`dialogue_overlay_tree`, `DialogueOverlayContent`, `ApplicationUi::replace_tree`)
//! and stay optional — this smoke stays headless.

use std::error::Error;

use yuyib::gameplay::{
    DialogueChoice, DialogueChoiceId, DialogueCondition, DialogueEffect, DialogueEvent,
    DialogueGraph, DialogueNode, DialogueNodeId, DialogueSession, QuestBook, QuestDefinition,
    QuestObjective, QuestSignal, QuestStatus, StoryFlagId, StoryFlags,
};

fn main() -> Result<(), Box<dyn Error>> {
    let graph = DialogueGraph::new(
        "demo.gate_guard",
        "intro",
        vec![
            DialogueNode::new("intro", "Halt. State your business.")
                .with_speaker("Gate Guard")
                .with_choices(vec![
                    DialogueChoice::new(
                        "bribe",
                        "Maybe this coin will help?",
                        Some(DialogueNodeId::new("bribed")),
                    )
                    .with_effect(DialogueEffect::SetFlag(StoryFlagId::new("bribed_guard")))
                    .with_effect(DialogueEffect::EmitQuestSignal(QuestSignal::new(
                        "world.talk_npc",
                        1,
                    )?)),
                    DialogueChoice::new("leave", "I'll come back later.", None),
                    DialogueChoice::new(
                        "password",
                        "The raven flies at midnight.",
                        Some(DialogueNodeId::new("opened")),
                    )
                    .with_require(DialogueCondition::FlagSet(StoryFlagId::new(
                        "knows_password",
                    ))),
                ]),
            DialogueNode::new("bribed", "…Fine. Don't make trouble.")
                .with_speaker("Gate Guard"),
            DialogueNode::new("opened", "Pass, quietly.")
                .with_speaker("Gate Guard")
                .with_enter_effect(DialogueEffect::SetFlag(StoryFlagId::new("gate_open"))),
        ],
    )?;

    let mut flags = StoryFlags::new();
    let mut book = QuestBook::default();
    book.register(QuestDefinition::new(
        "q.talk_guard",
        vec![QuestObjective::new("talk", "world.talk_npc", 1)?],
    )?)?;
    book.start(&"q.talk_guard".into())?;

    let (mut session, events, _) = DialogueSession::start(graph, &mut flags, Some(&mut book));
    println!("start: {events:?}");
    let presentation = session.presentation(&flags).expect("active");
    println!(
        "visible choices: {:?}",
        presentation
            .choices
            .iter()
            .map(|choice| choice.id.as_str())
            .collect::<Vec<_>>()
    );
    assert_eq!(presentation.choices.len(), 2, "password hidden without flag");

    // Host UI would call dialogue_overlay_tree from this presentation.
    println!(
        "overlay keys: {:?}",
        presentation
            .choices
            .iter()
            .map(|choice| choice.id.widget_key())
            .collect::<Vec<_>>()
    );

    let (events, quest_events) =
        session.choose(&DialogueChoiceId::new("bribe"), &mut flags, Some(&mut book))?;
    println!("bribe: dialogue={events:?} quest={quest_events:?}");
    assert!(flags.flag(&StoryFlagId::new("bribed_guard")));
    assert_eq!(
        book.progress(&"q.talk_guard".into())
            .expect("progress")
            .status(),
        QuestStatus::Completed
    );

    let ended = session.acknowledge(&flags)?;
    println!("ack: {ended:?}");
    assert!(!session.is_active());

    // Branch that required a flag becomes available later in the playthrough.
    flags.set_flag("knows_password", true);
    let graph = DialogueGraph::new(
        "demo.gate_guard",
        "intro",
        vec![
            DialogueNode::new("intro", "Halt. State your business.")
                .with_speaker("Gate Guard")
                .with_choices(vec![DialogueChoice::new(
                    "password",
                    "The raven flies at midnight.",
                    Some(DialogueNodeId::new("opened")),
                )
                .with_require(DialogueCondition::FlagSet(StoryFlagId::new(
                    "knows_password",
                )))]),
            DialogueNode::new("opened", "Pass, quietly.")
                .with_speaker("Gate Guard")
                .with_enter_effect(DialogueEffect::SetFlag(StoryFlagId::new("gate_open"))),
        ],
    )?;
    let (mut session, _, _) = DialogueSession::start(graph, &mut flags, None);
    assert_eq!(session.presentation(&flags).expect("p").choices.len(), 1);
    let (events, _) = session.choose(&DialogueChoiceId::new("password"), &mut flags, None)?;
    assert!(events.iter().any(|event| matches!(
        event,
        DialogueEvent::Entered { node, .. } if node.as_str() == "opened"
    )));
    assert!(flags.flag(&StoryFlagId::new("gate_open")));
    println!("dialogue_choice_flow: ok");
    Ok(())
}
