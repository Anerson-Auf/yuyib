//! Fixture-free high-level world interaction lifecycle.
//!
//! Run with:
//! `cargo run -p yuyib --example world_interaction_flow --no-default-features --features gameplay`

use std::{error::Error, time::Duration};

use yuyib::{
    ecs::prelude::World,
    gameplay::{
        InteractionId, InteractionMethod, WorldInteractionActivation, WorldInteractionEvent,
        WorldInteractionState, WorldInteractionTarget,
    },
};

fn main() -> Result<(), Box<dyn Error>> {
    let mut world = World::new();
    let actor = world.spawn_empty().id();
    let terminal_entity = world.spawn_empty().id();
    let terminal =
        WorldInteractionTarget::new(terminal_entity, InteractionId::new("world.hack_terminal"))
            .with_activation(WorldInteractionActivation::hold(Duration::from_millis(
                600,
            ))?);
    let mut interaction = WorldInteractionState::default();

    // A 2D hit-test, 3D raycast, trigger, or server query supplies `terminal`.
    let entered = interaction.step(Some(terminal.clone()), false, Duration::from_millis(200));
    assert!(matches!(
        entered.as_slice(),
        [WorldInteractionEvent::Entered(_)]
    ));

    // A keyboard, gamepad, touch, accessibility, or network adapter supplies
    // only semantic active/inactive state. Hold timing belongs to fixed update.
    for fixed_tick in 1..=3 {
        let events = interaction.step(Some(terminal.clone()), true, Duration::from_millis(200));
        for event in events {
            match &event {
                WorldInteractionEvent::Progress { fraction, .. } => {
                    println!("fixed_tick={fixed_tick} hold={:.0}%", fraction * 100.0);
                }
                WorldInteractionEvent::Interacted(_) => {
                    let request = event
                        .interaction_request(actor, InteractionMethod::Proximity)
                        .expect("Interacted always converts to a request");
                    assert_eq!(request.target, terminal_entity);
                    println!(
                        "request: actor={:?} target={:?} interaction={}",
                        request.actor, request.target, request.interaction
                    );
                }
                WorldInteractionEvent::Entered(_)
                | WorldInteractionEvent::Stayed(_)
                | WorldInteractionEvent::Exited(_) => {}
            }
        }
    }

    // Leaving the query result emits Exit and cancels/reset any hold state.
    let exited = interaction.step(None, false, Duration::from_millis(200));
    assert!(matches!(
        exited.as_slice(),
        [WorldInteractionEvent::Exited(_)]
    ));
    println!("world_interaction_flow: Enter → Stay/Progress → Interact → Exit");

    Ok(())
}
