//! Headless-проверка игрового вертикального среза: ввод, движение,
//! взаимодействие и прогресс задания.
//!
//! Запуск: `cargo run -p yuyib --example playable_vertical`.
//! Пример намеренно не открывает окно и завершается после проверки сценария.
//! Он печатает итог в консоль. Расписание, источник ввода и граница доверия
//! остаются ответственностью приложения.

use std::error::Error;

use yuyib::{
    character_3d::{CharacterInput3d, CharacterMotor3d, CharacterMotorConfig3d},
    ecs::prelude::World,
    gameplay::{
        ActionId, ActionStates, ActionValue, Interactable, InteractionOutcome, InteractionResolved,
        ObjectiveId, QuestBook, QuestDefinition, QuestId, QuestObjective, QuestSignal,
        interaction_3d::{UseRaycast3dConfig, UseRaycastOutcome3d, request_use_raycast_3d},
    },
    physics::{Position3d, Ray3d, SphereCollider3d, Vec2, Vec3},
};

fn main() -> Result<(), Box<dyn Error>> {
    let mut world = World::new();
    let actor = world.spawn_empty().id();
    let generator = world
        .spawn((
            Position3d::new(Vec3::new(0.0, 0.5, -2.0))?,
            SphereCollider3d::new(0.35)?,
            Interactable::new("world.activate_generator")
                .requiring_action("game.use")
                .with_max_distance(3.0)?,
        ))
        .id();

    // Application-owned fixed schedule: semantic movement input becomes one
    // deterministic motor step. A real app accumulates wall time here.
    let mut motor =
        CharacterMotor3d::new(CharacterMotorConfig3d::default(), Vec3::new(0.0, 0.5, 0.0))?;
    let _motor_events = motor.step(CharacterInput3d::new(Vec2::ZERO, false)?)?;

    // Application-owned input adapter would submit this event from its own
    // frame boundary. This example supplies the semantic press directly.
    let use_action = ActionId::new("game.use");
    let mut actions = ActionStates::default();
    let use_event = actions
        .submit(use_action, ActionValue::digital(true), 1)
        .expect("a semantic press starts game.use");
    let ray = Ray3d::new(motor.position(), Vec3::new(0.0, 0.0, -1.0))?;
    let outcome = request_use_raycast_3d(
        &mut world,
        &actions,
        &use_event,
        actor,
        ray,
        &UseRaycast3dConfig::default(),
    )?;
    let UseRaycastOutcome3d::Requested(selected) = outcome else {
        return Err("the vertical-slice generator should be selectable".into());
    };
    assert_eq!(selected.request.target, generator);

    // Authority boundary: a client only requests. Server/game rules decide
    // whether to accept before a quest observes the resulting domain signal.
    let resolution = InteractionResolved {
        request: selected.request,
        outcome: InteractionOutcome::Accepted,
    };
    assert_eq!(resolution.outcome, InteractionOutcome::Accepted);

    let quest_id = QuestId::new("tutorial.power_up");
    let objective_id = ObjectiveId::new("activate_generator");
    let mut quests = QuestBook::default();
    quests.register(QuestDefinition::new(
        quest_id.clone(),
        vec![QuestObjective::new(
            objective_id.clone(),
            "world.generator_activated",
            1,
        )?],
    )?)?;
    let _started = quests.start(&quest_id)?;

    if resolution.outcome == InteractionOutcome::Accepted {
        let signal = QuestSignal::new("world.generator_activated", 1)?;
        let transitions = quests.apply_signal(&signal);
        assert!(!transitions.is_empty());
    }
    assert_eq!(
        quests
            .progress(&quest_id)
            .expect("registered quest has progress")
            .objective(&objective_id),
        Some(1)
    );

    println!(
        "playable_vertical: сценарий успешно завершён\n  движение: fixed tick\n  действие: game.use → генератор\n  задание: tutorial.power_up (1/1)"
    );

    Ok(())
}
