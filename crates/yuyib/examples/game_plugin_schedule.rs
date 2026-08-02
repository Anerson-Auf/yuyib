//! High-level game runtime: plugin registration and bounded fixed updates.
//!
//! Run from the workspace root:
//!
//! ```text
//! cargo run -p yuyib --example game_plugin_schedule
//! ```

use std::error::Error;

use yuyib::{
    ecs::prelude::{Res, ResMut, Resource},
    game::{FixedTime, FixedUpdateStats, Game, GamePlugin, GameSchedule, GameTime},
    platform::WindowConfig,
};

#[derive(Default, Resource)]
struct Simulation {
    position: f32,
    velocity: f32,
    fixed_ticks: u64,
}

struct MovementPlugin;

impl GamePlugin for MovementPlugin {
    fn build(self, game: &mut Game) {
        game.world_mut().insert_resource(Simulation {
            position: 0.0,
            velocity: 2.0,
            fixed_ticks: 0,
        });
        game.schedule_mut(GameSchedule::FixedUpdate)
            .add_systems(integrate_movement);
        game.schedule_mut(GameSchedule::Update)
            .add_systems(report_runtime);
    }
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "Bevy ECS systems receive query parameters by value"
)]
fn integrate_movement(time: Res<FixedTime>, mut simulation: ResMut<Simulation>) {
    simulation.position += simulation.velocity * time.delta.as_secs_f32();
    simulation.fixed_ticks += 1;
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "Bevy ECS systems receive query parameters by value"
)]
fn report_runtime(time: Res<GameTime>, fixed: Res<FixedUpdateStats>, simulation: Res<Simulation>) {
    if time.frame.index.is_multiple_of(60) {
        println!(
            "frame={} last_frame_fixed_steps={} total_fixed_ticks={} alpha={:.2} position={:.2}",
            time.frame.index,
            fixed.steps,
            simulation.fixed_ticks,
            fixed.interpolation_alpha,
            simulation.position
        );
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    Game::new()
        .window(WindowConfig {
            title: "Yuyib — plugin schedules".to_owned(),
            ..Default::default()
        })
        .add_plugin(MovementPlugin)
        .run()?;
    Ok(())
}
